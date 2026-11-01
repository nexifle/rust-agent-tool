//! Classification of a file from its length and a leading byte sample.
//!
//! The caller reads the sample once and passes it in. This module performs no
//! I/O and never reads more than the sample it is given. It deliberately does
//! not validate UTF-8 sequences; the window scanner does that later. A sample
//! with no NUL byte is reported as [`Class::Utf8`] even when it is not valid
//! UTF-8.

/// PNG file signature, including the eight-byte magic.
pub(crate) const PNG_MAGIC: &[u8] = &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

/// JPEG segment start marker.
const JPEG_MAGIC: &[u8] = &[0xFF, 0xD8, 0xFF];

/// GIF header for version 87a.
const GIF87A_MAGIC: &[u8] = b"GIF87a";

/// GIF header for version 89a.
const GIF89A_MAGIC: &[u8] = b"GIF89a";

/// RIFF container signature.
const RIFF_MAGIC: &[u8] = b"RIFF";

/// WebP form type inside a RIFF container.
const WEBP_FORM: &[u8] = b"WEBP";

/// Offset of the form type inside a RIFF container.
const WEBP_FORM_OFFSET: usize = 8;

/// Media type reported for PNG images.
const PNG_MEDIA_TYPE: &str = "image/png";

/// Media type reported for JPEG images.
const JPEG_MEDIA_TYPE: &str = "image/jpeg";

/// Media type reported for GIF images.
const GIF_MEDIA_TYPE: &str = "image/gif";

/// Media type reported for WebP images.
const WEBP_MEDIA_TYPE: &str = "image/webp";

/// How a file is treated after its leading sample has been inspected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Class {
    /// The file has no bytes.
    Empty,
    /// The sample starts with a recognised image magic.
    Image {
        /// Media type matching the detected magic.
        media_type: &'static str,
    },
    /// The sample contains a NUL byte and is not an image.
    Binary,
    /// The sample has no NUL byte and is not an image.
    ///
    /// UTF-8 validity is not checked here.
    Utf8,
}

/// Detects an image magic at the start of `sample`.
///
/// Only the start of the sample is inspected, and a format is reported only
/// when the sample is long enough to hold that format's magic. Returns the
/// matching media type, or `None` when the sample does not start with a
/// recognised image magic.
#[must_use]
pub(crate) fn is_image_magic(sample: &[u8]) -> Option<&'static str> {
    if sample.starts_with(PNG_MAGIC) {
        return Some(PNG_MEDIA_TYPE);
    }

    if sample.starts_with(JPEG_MAGIC) {
        return Some(JPEG_MEDIA_TYPE);
    }

    if sample.starts_with(GIF87A_MAGIC) || sample.starts_with(GIF89A_MAGIC) {
        return Some(GIF_MEDIA_TYPE);
    }

    let is_webp = sample.starts_with(RIFF_MAGIC)
        && sample
            .get(WEBP_FORM_OFFSET..)
            .is_some_and(|form| form.starts_with(WEBP_FORM));
    if is_webp {
        return Some(WEBP_MEDIA_TYPE);
    }

    None
}

/// Classifies a file from its length and a leading sample.
///
/// `sample` is the bytes at offset 0, with length
/// `min(byte_len, binary_sample_bytes)`. This function does not read more.
///
/// The rules are applied in this order:
///
/// 1. A zero `byte_len` is [`Class::Empty`], regardless of `sample`.
/// 2. A recognised image magic at the start of `sample` is [`Class::Image`],
///    even when the sample also contains a NUL byte.
/// 3. A NUL byte anywhere in `sample` is [`Class::Binary`].
/// 4. Anything else is [`Class::Utf8`]. UTF-8 validity is not checked.
#[must_use]
pub(crate) fn classify(byte_len: u64, sample: &[u8]) -> Class {
    if byte_len == 0 {
        return Class::Empty;
    }

    if let Some(media_type) = is_image_magic(sample) {
        return Class::Image { media_type };
    }

    if sample.contains(&0) {
        return Class::Binary;
    }

    Class::Utf8
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a length value for a sample so tests do not repeat the cast.
    fn sample_len(sample: &[u8]) -> u64 {
        sample.len() as u64
    }

    #[test]
    fn zero_length_is_empty_even_with_a_sample() {
        assert_eq!(classify(0, b"hello"), Class::Empty);
        assert_eq!(classify(0, PNG_MAGIC), Class::Empty);
        assert_eq!(classify(0, &[0x00, 0x01]), Class::Empty);
    }

    #[test]
    fn interior_nul_is_binary() {
        assert_eq!(classify(5, b"AB\0CD"), Class::Binary);
    }

    #[test]
    fn png_magic_wins_over_a_later_nul() {
        let mut sample = Vec::from(PNG_MAGIC);
        sample.extend_from_slice(&[0x00, 0x01, 0x02]);
        assert_eq!(is_image_magic(&sample), Some("image/png"));
        assert_eq!(
            classify(sample_len(&sample), &sample),
            Class::Image {
                media_type: "image/png"
            }
        );
    }

    #[test]
    fn recognised_images_report_their_media_type() {
        let cases: [(&[u8], &str); 5] = [
            (PNG_MAGIC, "image/png"),
            (b"\xFF\xD8\xFF", "image/jpeg"),
            (b"GIF87a", "image/gif"),
            (b"GIF89a", "image/gif"),
            (b"RIFFabcdWEBP", "image/webp"),
        ];

        for (sample, media_type) in cases {
            assert_eq!(is_image_magic(sample), Some(media_type));
            assert_eq!(
                classify(sample_len(sample), sample),
                Class::Image { media_type }
            );
        }
    }

    #[test]
    fn riff_without_webp_form_is_not_an_image() {
        let sample = b"RIFFabcdWAVE";
        assert_eq!(is_image_magic(sample), None);
        assert_eq!(classify(sample_len(sample), sample), Class::Utf8);
    }

    #[test]
    fn riff_with_binary_length_is_not_an_image() {
        let sample = b"RIFF\x00\x00\x00\x00WAVE";
        assert_eq!(is_image_magic(sample), None);
        assert_eq!(classify(sample_len(sample), sample), Class::Binary);
    }

    #[test]
    fn jpeg_magic_of_exactly_three_bytes_is_an_image() {
        let sample = [0xFF_u8, 0xD8, 0xFF];
        assert_eq!(is_image_magic(&sample), Some("image/jpeg"));
        assert_eq!(
            classify(3, &sample),
            Class::Image {
                media_type: "image/jpeg"
            }
        );
    }

    #[test]
    fn short_jpeg_prefix_is_not_an_image() {
        assert_eq!(is_image_magic(&[0xFF, 0xD8]), None);
    }

    #[test]
    fn extension_is_not_an_input() {
        // There is no filename parameter, so a text sample is just text.
        assert_eq!(is_image_magic(b"hello"), None);
        assert_eq!(classify(5, b"hello"), Class::Utf8);
        assert_eq!(classify(12, b"hello, world"), Class::Utf8);
    }

    #[test]
    fn invalid_utf8_without_nul_is_utf8_here() {
        let sample = [0xFF_u8, 0xFE, 0xFD];
        assert_eq!(classify(3, &sample), Class::Utf8);
    }

    #[test]
    fn partial_gif_header_is_not_a_gif() {
        assert_eq!(is_image_magic(b"GIF8"), None);
        assert_eq!(is_image_magic(b"GIF87"), None);
        assert_eq!(classify(4, b"GIF8"), Class::Utf8);
    }

    #[test]
    fn eleven_byte_webp_sample_is_not_webp() {
        let sample = b"RIFF\x00\x00\x00\x00WEB";
        assert_eq!(sample.len(), 11);
        assert_eq!(is_image_magic(sample), None);
    }

    #[test]
    fn twelve_byte_webp_sample_is_webp() {
        let sample = b"RIFF\x00\x00\x00\x00WEBP";
        assert_eq!(sample.len(), 12);
        assert_eq!(is_image_magic(sample), Some("image/webp"));
        // The NUL length bytes do not matter: image magic is checked first.
        assert_eq!(
            classify(12, sample),
            Class::Image {
                media_type: "image/webp"
            }
        );
    }

    #[test]
    fn empty_sample_with_nonzero_length_is_utf8() {
        assert_eq!(classify(10, &[]), Class::Utf8);
    }
}
