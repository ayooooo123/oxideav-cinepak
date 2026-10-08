//! The registry decoder (FFmpeg's, `ffdec`) reports the size and pixel
//! layout of the frames it returns (oxideav-core
//! `Decoder::output_video_dimensions` / `output_pixel_format`): FFmpeg's
//! RGB24 at the container's picture size, gray Cinepak too (R = G = B),
//! and nothing before the first frame.
//!
//! `decoded_format/*.cvid` are three-frame FFmpeg cinepak encodes of
//! `testsrc` (RGB 36×20, then gray 48×32), each frame as written by
//! `-f rawvideo`; the 24-bit `frame_length` in each frame header splits
//! them.

#![cfg(feature = "registry")]

use oxideav_core::{CodecId, CodecParameters, Error, Frame, Packet, PixelFormat, TimeBase};

/// The frames of one stream.
fn frames(bytes: &[u8]) -> Vec<Vec<u8>> {
    let mut frames = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        let len = usize::from(bytes[at + 1]) << 16
            | usize::from(bytes[at + 2]) << 8
            | usize::from(bytes[at + 3]);
        frames.push(bytes[at..at + len].to_vec());
        at += len;
    }
    assert_eq!(frames.len(), 3);
    frames
}

#[test]
fn frames_are_ffmpegs_rgb24_at_the_container_size() {
    let streams: [(&[u8], u32, u32, bool); 2] = [
        (
            include_bytes!("decoded_format/a_rgb24_36x20.cvid"),
            36,
            20,
            false,
        ),
        (
            include_bytes!("decoded_format/b_gray_48x32.cvid"),
            48,
            32,
            true,
        ),
    ];
    for (bytes, w, h, gray) in streams {
        let mut params = CodecParameters::video(CodecId::new("cinepak"));
        params.width = Some(w);
        params.height = Some(h);
        let mut dec = oxideav_cinepak::registry::make_decoder(&params).expect("decoder");
        assert_eq!(
            (dec.output_video_dimensions(), dec.output_pixel_format()),
            (None, None),
            "{w}x{h}: before a frame"
        );
        for (at, data) in frames(bytes).into_iter().enumerate() {
            dec.send_packet(&Packet::new(0, TimeBase::new(1, 25), data))
                .expect("send");
            let frame = match dec.receive_frame() {
                Ok(Frame::Video(frame)) => frame,
                other => panic!("{w}x{h} frame {at}: {other:?}"),
            };
            assert_eq!(
                dec.output_video_dimensions(),
                Some((w, h)),
                "{w}x{h} frame {at}"
            );
            assert_eq!(
                dec.output_pixel_format(),
                Some(PixelFormat::Rgb24),
                "{w}x{h} frame {at}"
            );
            let planes = frame.image_planes();
            assert_eq!(planes.len(), 1, "{w}x{h} frame {at}: planes");
            assert_eq!(
                planes[0].stride,
                w as usize * 3,
                "{w}x{h} frame {at}: stride"
            );
            assert_eq!(
                planes[0].data.len(),
                planes[0].stride * h as usize,
                "{w}x{h} frame {at}: rows"
            );
            if gray {
                assert!(
                    planes[0]
                        .data
                        .chunks_exact(3)
                        .all(|p| p[0] == p[1] && p[1] == p[2]),
                    "{w}x{h} frame {at}: gray as R = G = B"
                );
            }
            assert!(
                matches!(dec.receive_frame(), Err(Error::NeedMore)),
                "{w}x{h} frame {at}: one frame per packet"
            );
        }
    }
}
