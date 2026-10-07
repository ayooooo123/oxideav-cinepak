//! The registry decoder reports the size and pixel layout of the frame it
//! last returned (oxideav-core `Decoder::output_video_dimensions` /
//! `output_pixel_format`), including a change of size and layout from
//! one frame to the next.
//!
//! `decoded_format/*.cvid` are three-frame FFmpeg cinepak encodes of
//! `testsrc` (RGB 36×20, then gray 48×32), each frame as written by
//! `-f rawvideo`; the 24-bit `frame_length` in each frame header splits
//! them.

#![cfg(feature = "registry")]

use oxideav_core::{CodecId, CodecParameters, Error, Frame, Packet, PixelFormat, TimeBase};

/// A frame's width, height and pixel format.
type Layout = (u32, u32, PixelFormat);

/// Every frame of both streams, in order, with the layout it decodes to.
fn frames() -> Vec<(Vec<u8>, Layout)> {
    let streams: [(&[u8], Layout); 2] = [
        (
            include_bytes!("decoded_format/a_rgb24_36x20.cvid"),
            (36, 20, PixelFormat::Rgb24),
        ),
        (
            include_bytes!("decoded_format/b_gray_48x32.cvid"),
            (48, 32, PixelFormat::Gray8),
        ),
    ];
    let mut frames = Vec::new();
    for (bytes, layout) in streams {
        let mut at = 0;
        while at < bytes.len() {
            let len = usize::from(bytes[at + 1]) << 16
                | usize::from(bytes[at + 2]) << 8
                | usize::from(bytes[at + 3]);
            frames.push((bytes[at..at + len].to_vec(), layout));
            at += len;
        }
    }
    assert_eq!(frames.len(), 6);
    frames
}

#[test]
fn each_frame_reports_its_own_size_and_layout() {
    let mut dec =
        oxideav_cinepak::registry::make_decoder(&CodecParameters::video(CodecId::new("cinepak")))
            .expect("decoder");
    for (at, (data, (w, h, format))) in frames().into_iter().enumerate() {
        dec.send_packet(&Packet::new(0, TimeBase::new(1, 25), data))
            .expect("send");
        let frame = match dec.receive_frame() {
            Ok(Frame::Video(frame)) => frame,
            other => panic!("frame {at}: {other:?}"),
        };
        assert_eq!(dec.output_video_dimensions(), Some((w, h)), "frame {at}");
        assert_eq!(dec.output_pixel_format(), Some(format), "frame {at}");
        let bytes = if format == PixelFormat::Rgb24 { 3 } else { 1 };
        let planes = frame.image_planes();
        assert_eq!(planes.len(), 1, "frame {at}: planes");
        assert_eq!(planes[0].stride, w as usize * bytes, "frame {at}: stride");
        assert_eq!(
            planes[0].data.len(),
            planes[0].stride * h as usize,
            "frame {at}: rows"
        );
        assert!(
            matches!(dec.receive_frame(), Err(Error::NeedMore)),
            "frame {at}: one frame per packet"
        );
    }
}
