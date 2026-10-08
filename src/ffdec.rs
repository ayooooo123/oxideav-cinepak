// SPDX-License-Identifier: LGPL-2.1-or-later
// Port of FFmpeg 2da55bf libavcodec/cinepak.c, the decoder the registry
// returns (`registry::make_decoder`).
// Copyright (C) 2003 The FFmpeg project; Cinepak colorspace support (c)
// 2013 Rl, Aetey Global Technologies AB.
//
// This file is free software; you can redistribute it and/or modify it under
// the terms of the GNU Lesser General Public License as published by the
// Free Software Foundation; either version 2.1 of the License, or (at your
// option) any later version. See LICENSE-LGPL.

//! FFmpeg's Cinepak decoder, frame for frame. What it reproduces:
//!
//! * the picture is the container's size (`avctx->width × height`), decoded
//!   into a buffer whose width is rounded up to 4; a strip's left and right
//!   edges come from its header, its top is the previous strip's bottom
//!   when its own is 0;
//! * one buffer for the whole stream: a block a frame skips keeps the
//!   previous frame's pixels;
//! * each strip index keeps its own codebooks from frame to frame; a strip
//!   other than the first starts from the strip above when bit 0 of the
//!   frame flags is clear;
//! * a frame with no strip yields nothing; a frame that fails to decode
//!   part-way is still returned, as far as it got; one with less than 5% of
//!   its declared size (`discard_damaged_percentage` 95) is refused;
//! * Sega FILM's extra header bytes, detected on the first frame.
//!
//! Output is RGB24. FFmpeg's palette mode for 8-bit Cinepak (chosen by the
//! container's bits per coded sample, which the stream parameters do not
//! carry) is not reproduced.

/// A codebook entry: four pixels of three bytes (R, G, B; or Y three
/// times).
type Entry = [u8; 12];

const MAX_STRIPS: usize = 32;

#[derive(Clone)]
struct Strip {
    id: u8,
    x1: usize,
    y1: usize,
    x2: usize,
    y2: usize,
    v4: Box<[Entry; 256]>,
    v1: Box<[Entry; 256]>,
}

impl Default for Strip {
    fn default() -> Self {
        Self {
            id: 0,
            x1: 0,
            y1: 0,
            x2: 0,
            y2: 0,
            v4: Box::new([[0; 12]; 256]),
            v1: Box::new([[0; 12]; 256]),
        }
    }
}

/// Why a frame was not returned.
#[derive(Debug, PartialEq, Eq)]
pub enum FfError {
    /// FFmpeg's AVERROR_INVALIDDATA.
    InvalidData(&'static str),
    /// FFmpeg's AVERROR_PATCHWELCOME.
    Unsupported(&'static str),
}

/// FFmpeg's `CinepakContext`.
pub struct FfCinepak {
    width: usize,
    height: usize,
    aligned_width: usize,
    aligned_height: usize,
    strips: Vec<Strip>,
    sega_film_skip_bytes: Option<usize>,
    /// The picture, `aligned_width * 3` bytes a row, `height` rows.
    frame: Vec<u8>,
}

fn rb16(d: &[u8], at: usize) -> usize {
    usize::from(u16::from_be_bytes([d[at], d[at + 1]]))
}

fn rb24(d: &[u8], at: usize) -> usize {
    (usize::from(d[at]) << 16) | (usize::from(d[at + 1]) << 8) | usize::from(d[at + 2])
}

fn rb32(d: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([d[at], d[at + 1], d[at + 2], d[at + 3]])
}

/// `cinepak_decode_codebook`.
fn decode_codebook(codebook: &mut [Entry; 256], chunk_id: u8, data: &[u8]) {
    let n = if chunk_id & 0x04 != 0 { 4 } else { 6 };
    let (mut flag, mut mask) = (0u32, 0u32);
    let mut d = 0;
    for entry in codebook.iter_mut() {
        if chunk_id & 0x01 != 0 {
            mask >>= 1;
            if mask == 0 {
                if d + 4 > data.len() {
                    break;
                }
                flag = rb32(data, d);
                d += 4;
                mask = 0x8000_0000;
            }
        }
        if chunk_id & 0x01 == 0 || flag & mask != 0 {
            if d + n > data.len() {
                break;
            }
            for k in 0..4 {
                entry[3 * k..3 * k + 3].fill(data[d + k]);
            }
            if n == 6 {
                let u = i32::from(data[d + 4] as i8);
                let v = i32::from(data[d + 5] as i8);
                for k in 0..4 {
                    let y = i32::from(entry[3 * k]);
                    entry[3 * k] = (y + v * 2).clamp(0, 255) as u8;
                    entry[3 * k + 1] = (y - u / 2 - v).clamp(0, 255) as u8;
                    entry[3 * k + 2] = (y + u * 2).clamp(0, 255) as u8;
                }
            }
            d += n;
        }
    }
}

impl FfCinepak {
    /// `cinepak_decode_init` for a picture of `width × height`.
    pub fn new(width: usize, height: usize) -> Self {
        let aligned_width = (width + 3) & !3;
        let aligned_height = (height + 3) & !3;
        Self {
            width,
            height,
            aligned_width,
            aligned_height,
            strips: vec![Strip::default(); MAX_STRIPS],
            sega_film_skip_bytes: None,
            frame: vec![0; aligned_width * 3 * height],
        }
    }

    /// The picture's size.
    pub fn size(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    /// The picture: `width × height` RGB24, rows `width * 3` bytes.
    pub fn picture(&self) -> Vec<u8> {
        let stride = self.aligned_width * 3;
        let mut out = Vec::with_capacity(self.width * 3 * self.height);
        for y in 0..self.height {
            out.extend_from_slice(&self.frame[y * stride..y * stride + self.width * 3]);
        }
        out
    }

    /// `cinepak_decode_vectors` into the picture.
    fn decode_vectors(&mut self, si: usize, chunk_id: u8, data: &[u8]) -> Result<(), FfError> {
        let stride = self.aligned_width * 3;
        let strip = &self.strips[si];
        let (x1, x2, y1, y2) = (strip.x1, strip.x2, strip.y1, strip.y2);
        let (mut flag, mut mask) = (0u32, 0u32);
        let mut d = 0;
        let bad = FfError::InvalidData("cinepak: vectors run past the chunk");
        for y in (y1..y2).step_by(4) {
            // Rows past the picture's bottom write onto its last row, the
            // bottom of the block first (FFmpeg's ip0..ip3 aliasing); rows
            // starting past it land outside the picture.
            let rows: [Option<usize>; 4] = std::array::from_fn(|k| {
                let r = if self.height > y + k { y + k } else { y };
                (r < self.height).then_some(r)
            });
            for x in (x1..x2).step_by(4) {
                if chunk_id & 0x01 != 0 {
                    mask >>= 1;
                    if mask == 0 {
                        if d + 4 > data.len() {
                            return Err(bad);
                        }
                        flag = rb32(data, d);
                        d += 4;
                        mask = 0x8000_0000;
                    }
                }
                if chunk_id & 0x01 == 0 || flag & mask != 0 {
                    if chunk_id & 0x02 == 0 {
                        mask >>= 1;
                        if mask == 0 {
                            if d + 4 > data.len() {
                                return Err(bad);
                            }
                            flag = rb32(data, d);
                            d += 4;
                            mask = 0x8000_0000;
                        }
                    }
                    // Each row's 12 bytes: (row, first entry offset, second).
                    let mut put = |rows: &[Option<usize>; 4], parts: [(usize, &Entry, usize, &Entry, usize); 4]| {
                        for (row, a, ao, b, bo) in parts {
                            if let Some(r) = rows[row] {
                                let at = r * stride + x * 3;
                                self.frame[at..at + 6].copy_from_slice(&a[ao..ao + 6]);
                                self.frame[at + 6..at + 12].copy_from_slice(&b[bo..bo + 6]);
                            }
                        }
                    };
                    let strip = &self.strips[si];
                    if chunk_id & 0x02 != 0 || !flag & mask != 0 {
                        if d >= data.len() {
                            return Err(bad);
                        }
                        let p = &strip.v1[usize::from(data[d])];
                        d += 1;
                        // Each codebook pixel doubled into a 2×2 square.
                        let dup = |i: usize| -> Entry {
                            let mut e = [0u8; 12];
                            e[..3].copy_from_slice(&p[i..i + 3]);
                            e[3..6].copy_from_slice(&p[i..i + 3]);
                            e[6..9].copy_from_slice(&p[i + 3..i + 6]);
                            e[9..12].copy_from_slice(&p[i + 3..i + 6]);
                            e
                        };
                        let (top, bottom) = (dup(0), dup(6));
                        put(
                            &rows,
                            [
                                (3, &bottom, 0, &bottom, 6),
                                (2, &bottom, 0, &bottom, 6),
                                (1, &top, 0, &top, 6),
                                (0, &top, 0, &top, 6),
                            ],
                        );
                    } else if flag & mask != 0 {
                        if d + 4 > data.len() {
                            return Err(bad);
                        }
                        let cb: [&Entry; 4] =
                            std::array::from_fn(|k| &strip.v4[usize::from(data[d + k])]);
                        d += 4;
                        put(
                            &rows,
                            [
                                (3, cb[2], 6, cb[3], 6),
                                (2, cb[2], 0, cb[3], 0),
                                (1, cb[0], 6, cb[1], 6),
                                (0, cb[0], 0, cb[1], 0),
                            ],
                        );
                    }
                }
            }
        }
        Ok(())
    }

    /// `cinepak_decode_strip`.
    fn decode_strip(&mut self, si: usize, data: &[u8]) -> Result<(), FfError> {
        let s = &self.strips[si];
        if s.x2 > self.aligned_width || s.y2 > self.aligned_height || s.x1 >= s.x2 || s.y1 >= s.y2 {
            return Err(FfError::InvalidData("cinepak: strip outside the picture"));
        }
        let mut d = 0;
        while d + 4 <= data.len() {
            let chunk_id = data[d];
            let size = rb24(data, d + 1)
                .checked_sub(4)
                .ok_or(FfError::InvalidData("cinepak: chunk size"))?;
            d += 4;
            let size = size.min(data.len() - d);
            let chunk = &data[d..d + size];
            match chunk_id {
                0x20 | 0x21 | 0x24 | 0x25 => {
                    decode_codebook(&mut self.strips[si].v4, chunk_id, chunk)
                }
                0x22 | 0x23 | 0x26 | 0x27 => {
                    decode_codebook(&mut self.strips[si].v1, chunk_id, chunk)
                }
                0x30..=0x32 => return self.decode_vectors(si, chunk_id, chunk),
                _ => {}
            }
            d += size;
        }
        Err(FfError::InvalidData("cinepak: strip without vectors"))
    }

    /// `cinepak_predecode_check`.
    fn predecode_check(&mut self, data: &[u8]) -> Result<(), FfError> {
        let num_strips = rb16(data, 8);
        let encoded_buf_size = rb24(data, 1);
        // `s->size < encoded_buf_size * (100 - 95) / 100`
        if (data.len() as i64) < encoded_buf_size as i64 * 5 / 100 {
            return Err(FfError::InvalidData("cinepak: frame too damaged"));
        }
        if self.sega_film_skip_bytes.is_none() {
            if encoded_buf_size == 0 {
                return Err(FfError::Unsupported("cinepak: encoded_buf_size 0"));
            }
            self.sega_film_skip_bytes = Some(
                if encoded_buf_size != data.len() && data.len() % encoded_buf_size != 0 {
                    if data.len() >= 16 && data[10..16] == [0xFE, 0x00, 0x00, 0x06, 0x00, 0x00] {
                        6
                    } else {
                        2
                    }
                } else {
                    0
                },
            );
        }
        let skip = self.sega_film_skip_bytes.unwrap_or(0);
        if data.len() < 10 + skip + num_strips * 12 {
            return Err(FfError::InvalidData("cinepak: strip headers truncated"));
        }
        if num_strips > 0 {
            let strip_size = rb24(data, 10 + skip + 1);
            if strip_size < 12 || strip_size > encoded_buf_size {
                return Err(FfError::InvalidData("cinepak: strip size"));
            }
        }
        Ok(())
    }

    /// `cinepak_decode`: decodes into the picture; an error leaves what was
    /// decoded before it.
    fn decode_strips(&mut self, data: &[u8]) -> Result<(), FfError> {
        let frame_flags = data[0];
        let num_strips = rb16(data, 8).min(MAX_STRIPS);
        let mut d = 10 + self.sega_film_skip_bytes.unwrap_or(0);
        let mut y0 = 0;
        for i in 0..num_strips {
            if d + 12 > data.len() {
                return Err(FfError::InvalidData("cinepak: strip header past the end"));
            }
            let h = &data[d..d + 12];
            let s = &mut self.strips[i];
            s.id = h[0];
            // A zero top means "relative to the previous strip".
            s.y1 = rb16(h, 4);
            if s.y1 == 0 {
                s.y1 = y0;
                s.y2 = y0 + rb16(h, 8);
            } else {
                s.y2 = rb16(h, 8);
            }
            s.x1 = rb16(h, 6);
            s.x2 = rb16(h, 10);
            let strip_size = rb24(h, 1)
                .checked_sub(12)
                .ok_or(FfError::InvalidData("cinepak: strip size"))?;
            d += 12;
            let strip_size = strip_size.min(data.len() - d);
            if i > 0 && frame_flags & 0x01 == 0 {
                let (above, this) = self.strips.split_at_mut(i);
                this[0].v4.copy_from_slice(&above[i - 1].v4[..]);
                this[0].v1.copy_from_slice(&above[i - 1].v1[..]);
            }
            self.decode_strip(i, &data[d..d + strip_size])?;
            d += strip_size;
            y0 = self.strips[i].y2;
        }
        Ok(())
    }

    /// `cinepak_decode_frame`: `Ok(true)` when the packet updated the
    /// picture and FFmpeg returns it, `Ok(false)` for an empty frame.
    pub fn decode(&mut self, data: &[u8]) -> Result<bool, FfError> {
        if data.len() < 10 {
            return Err(FfError::InvalidData("cinepak: packet under 10 bytes"));
        }
        if rb16(data, 8) == 0 {
            return Ok(false);
        }
        self.predecode_check(data)?;
        // A decoding error is logged by FFmpeg and the frame still returned.
        let _ = self.decode_strips(data);
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One 4×4 key frame: a V1 codebook entry (Y 10/20/30/40, U 0, V 0)
    /// drawn by one vector: each codebook pixel fills a 2×2 square.
    #[test]
    fn a_v1_block_doubles_each_pixel() {
        let mut f = vec![0x00, 0x00, 0x00, 0x00, 0x00, 4, 0x00, 4, 0x00, 1];
        let chunks = [
            &[0x22u8, 0, 0, 10, 10, 20, 30, 40, 0, 0][..],
            &[0x32, 0, 0, 5, 0][..],
        ];
        let strip_len = 12 + chunks.iter().map(|c| c.len()).sum::<usize>();
        f.extend_from_slice(&[0x10, 0, 0, strip_len as u8, 0, 0, 0, 0, 0, 4, 0, 4]);
        for c in chunks {
            f.extend_from_slice(c);
        }
        let len = f.len();
        f[3] = len as u8;
        let mut dec = FfCinepak::new(4, 4);
        assert_eq!(dec.decode(&f), Ok(true));
        let p = dec.picture();
        let row =
            |y: usize| -> Vec<u8> { p[y * 12..y * 12 + 12].iter().step_by(3).copied().collect() };
        assert_eq!(
            (row(0), row(1), row(2), row(3)),
            (
                vec![10, 10, 20, 20],
                vec![10, 10, 20, 20],
                vec![30, 30, 40, 40],
                vec![30, 30, 40, 40]
            )
        );
    }
}
