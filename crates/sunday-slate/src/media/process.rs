//! Image processing for uploaded logos and cached avatars.

use image::{DynamicImage, ImageFormat, ImageReader, Limits, imageops::FilterType};
use std::io::Cursor;

/// Decodes larger than this per axis are rejected before the pixel buffer is
/// allocated — the 5 MB body limit bounds encoded bytes, not decoded pixels.
const MAX_DECODE_DIMENSION: u32 = 8192;
/// Cap on decoder allocations. Avatar sources are untrusted CDN responses of up
/// to 20 MB encoded, which a solid 8192x8192 PNG fits in while decoding to
/// 268 MB. 96 MiB still admits a 4096x4096 RGBA image.
const MAX_DECODE_ALLOC_BYTES: u64 = 96 * 1024 * 1024;
const AVATAR_MAX_AXIS: u32 = 256;

#[derive(Debug, thiserror::Error)]
pub enum ProcessError {
    #[error("Unsupported image format. Upload a PNG, JPEG, or WebP.")]
    UnsupportedFormat,
    #[error("That file could not be read as an image.")]
    Decode,
    #[error("Failed to process the image.")]
    Encode,
}

#[derive(Debug)]
pub struct ProcessedImage {
    pub png: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

fn decode(bytes: &[u8]) -> Result<DynamicImage, ProcessError> {
    let format = match image::guess_format(bytes) {
        Ok(format @ (ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::WebP)) => format,
        _ => return Err(ProcessError::UnsupportedFormat),
    };

    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_DECODE_DIMENSION);
    limits.max_image_height = Some(MAX_DECODE_DIMENSION);
    limits.max_alloc = Some(MAX_DECODE_ALLOC_BYTES);

    let mut reader = ImageReader::new(Cursor::new(bytes));
    reader.set_format(format);
    reader.limits(limits);
    reader.decode().map_err(|_| ProcessError::Decode)
}

fn encode_png(image: &DynamicImage) -> Result<Vec<u8>, ProcessError> {
    let mut buf = Cursor::new(Vec::new());
    image
        .write_to(&mut buf, ImageFormat::Png)
        .map_err(|_| ProcessError::Encode)?;
    Ok(buf.into_inner())
}

/// Validate the upload is a supported format, center-crop it to a square,
/// resize to 256×256, and re-encode as PNG. Returns the PNG bytes.
pub fn process_logo(bytes: &[u8]) -> Result<Vec<u8>, ProcessError> {
    let img = decode(bytes)?;
    let (w, h) = (img.width(), img.height());
    let side = w.min(h);
    let x = (w - side) / 2;
    let y = (h - side) / 2;
    let square = img.crop_imm(x, y, side, side);
    let resized = square.resize_exact(256, 256, FilterType::Lanczos3);
    encode_png(&resized)
}

pub fn process_avatar(bytes: &[u8]) -> Result<ProcessedImage, ProcessError> {
    let img = decode(bytes)?;
    let (w, h) = (img.width(), img.height());
    let processed = if w.max(h) > AVATAR_MAX_AXIS {
        let max_axis = w.max(h);
        let width = ((u64::from(w) * u64::from(AVATAR_MAX_AXIS) + u64::from(max_axis) / 2)
            / u64::from(max_axis)) as u32;
        let height = ((u64::from(h) * u64::from(AVATAR_MAX_AXIS) + u64::from(max_axis) / 2)
            / u64::from(max_axis)) as u32;
        img.resize_exact(width.max(1), height.max(1), FilterType::Lanczos3)
    } else {
        img
    };
    let width = processed.width();
    let height = processed.height();
    Ok(ProcessedImage {
        png: encode_png(&processed)?,
        width,
        height,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{DynamicImage, ImageFormat, RgbImage};
    use std::io::Cursor;

    fn encode(w: u32, h: u32, format: ImageFormat) -> Vec<u8> {
        let img = DynamicImage::ImageRgb8(RgbImage::new(w, h));
        let mut buf = Cursor::new(Vec::new());
        img.write_to(&mut buf, format).expect("encode");
        buf.into_inner()
    }

    fn decode_dims(bytes: &[u8]) -> (u32, u32) {
        let img = image::load_from_memory(bytes).expect("decode output");
        (img.width(), img.height())
    }

    #[test]
    fn resizes_non_square_png_to_256_square() {
        let input = encode(400, 200, ImageFormat::Png);
        let out = process_logo(&input).expect("process");
        assert_eq!(image::guess_format(&out).unwrap(), ImageFormat::Png);
        assert_eq!(decode_dims(&out), (256, 256));
    }

    #[test]
    fn accepts_jpeg_and_outputs_png() {
        let input = encode(300, 300, ImageFormat::Jpeg);
        let out = process_logo(&input).expect("process");
        assert_eq!(image::guess_format(&out).unwrap(), ImageFormat::Png);
        assert_eq!(decode_dims(&out), (256, 256));
    }

    #[test]
    fn rejects_non_image_bytes() {
        let err = process_logo(b"this is not an image").unwrap_err();
        assert!(matches!(
            err,
            ProcessError::UnsupportedFormat | ProcessError::Decode
        ));
    }

    /// 1px tall keeps the fixture cheap while still tripping the width cap.
    #[test]
    fn rejects_images_exceeding_the_decode_dimension_cap() {
        let input = encode(MAX_DECODE_DIMENSION + 1, 1, ImageFormat::Png);
        assert!(matches!(process_logo(&input), Err(ProcessError::Decode)));
    }

    #[test]
    fn rejects_unsupported_format() {
        // GIF89a magic — `guess_format` recognizes it from the header alone,
        // no `gif` encode feature required.
        let gif = b"GIF89a\x01\x00\x01\x00\x00\x00\x00;";
        assert!(matches!(
            process_logo(gif),
            Err(ProcessError::UnsupportedFormat)
        ));
    }
    #[test]
    fn process_avatar_wide_preserves_aspect_ratio() {
        let processed = process_avatar(&encode(400, 200, ImageFormat::Png)).expect("process");
        assert_eq!((processed.width, processed.height), (256, 128));
        assert!(processed.width.max(processed.height) <= 256);
    }

    #[test]
    fn process_avatar_tall_preserves_aspect_ratio() {
        let processed = process_avatar(&encode(200, 400, ImageFormat::Jpeg)).expect("process");
        assert_eq!((processed.width, processed.height), (128, 256));
        assert!(processed.width.max(processed.height) <= 256);
    }

    #[test]
    fn process_avatar_small_image_keeps_original_dimensions() {
        let processed = process_avatar(&encode(120, 80, ImageFormat::Png)).expect("process");
        assert_eq!((processed.width, processed.height), (120, 80));
    }

    #[test]
    fn process_avatar_outputs_png() {
        let processed = process_avatar(&encode(64, 64, ImageFormat::WebP)).expect("process");
        assert_eq!(
            image::guess_format(&processed.png).expect("format"),
            ImageFormat::Png
        );
    }

    #[test]
    fn process_avatar_rejects_unsupported_format() {
        let err = process_avatar(b"not an image").unwrap_err();
        assert!(matches!(err, ProcessError::UnsupportedFormat));
    }
}
