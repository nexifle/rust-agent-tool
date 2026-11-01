//! Budget limits for the read tool.
//!
//! [`ReadConfig`] stores every line, byte, chunk, and image limit a read uses.
//! Each numeric setter rejects an unusable value instead of clamping it, so a
//! host learns about a bad budget when it builds the tool, not on the first
//! call.
//!
//! [`ReadCeilings`] bounds what a setter may store. A host that needs a larger
//! budget must say so with [`ReadConfig::with_ceilings`].

use std::fmt;

use crate::ToolError;

/// Lowest accepted `max_lines` and the lowest accepted ceiling for it.
const MIN_MAX_LINES: u32 = 1;
/// Lowest accepted `max_line_bytes` and the lowest accepted ceiling for it.
const MIN_MAX_LINE_BYTES: u32 = 1;
/// Lowest accepted `max_output_bytes` and the lowest accepted ceiling for it.
const MIN_MAX_OUTPUT_BYTES: usize = 320;
/// Lowest accepted `chunk_bytes` and the lowest accepted ceiling for it.
const MIN_CHUNK_BYTES: usize = 12;
/// Lowest accepted `binary_sample_bytes`. WebP magic ends at byte 12.
const MIN_BINARY_SAMPLE_BYTES: usize = 12;
/// Lowest accepted `max_image_bytes` and the lowest accepted ceiling for it.
const MIN_MAX_IMAGE_BYTES: u64 = 1;

const DEFAULT_MAX_LINES: u32 = 2_000;
const DEFAULT_MAX_LINE_BYTES: u32 = 2_000;
const DEFAULT_MAX_OUTPUT_BYTES: usize = 48 * 1024;
const DEFAULT_CHUNK_BYTES: usize = 64 * 1024;
const DEFAULT_BINARY_SAMPLE_BYTES: usize = 8 * 1024;
const DEFAULT_MAX_IMAGE_BYTES: u64 = 5 * 1024 * 1024;

const DEFAULT_CEILING_MAX_LINES: u32 = 100_000;
const DEFAULT_CEILING_MAX_LINE_BYTES: u32 = 1024 * 1024;
const DEFAULT_CEILING_MAX_OUTPUT_BYTES: usize = 1024 * 1024;
const DEFAULT_CEILING_CHUNK_BYTES: usize = 1024 * 1024;
const DEFAULT_CEILING_MAX_IMAGE_BYTES: u64 = 20 * 1024 * 1024;

/// Highest value a [`ReadConfig`] setter may store for each knob.
///
/// The defaults match the ceilings of [`ReadConfig::new`]. A host replaces
/// them with [`ReadConfig::with_ceilings`]. `binary_sample_bytes` has no field
/// here: its ceiling is the current `chunk_bytes`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadCeilings {
    /// Highest value [`ReadConfig::with_max_lines`] may store.
    pub max_lines: u32,
    /// Highest value [`ReadConfig::with_max_line_bytes`] may store.
    pub max_line_bytes: u32,
    /// Highest value [`ReadConfig::with_max_output_bytes`] may store.
    pub max_output_bytes: usize,
    /// Highest value [`ReadConfig::with_chunk_bytes`] may store.
    pub chunk_bytes: usize,
    /// Highest value [`ReadConfig::with_max_image_bytes`] may store.
    pub max_image_bytes: u64,
}

impl Default for ReadCeilings {
    fn default() -> Self {
        default_ceilings()
    }
}

/// Upper bounds for the knobs. These are not the default knob values.
const fn default_ceilings() -> ReadCeilings {
    ReadCeilings {
        max_lines: DEFAULT_CEILING_MAX_LINES,
        max_line_bytes: DEFAULT_CEILING_MAX_LINE_BYTES,
        max_output_bytes: DEFAULT_CEILING_MAX_OUTPUT_BYTES,
        chunk_bytes: DEFAULT_CEILING_CHUNK_BYTES,
        max_image_bytes: DEFAULT_CEILING_MAX_IMAGE_BYTES,
    }
}

/// Budget limits for one read tool.
///
/// Build a value with [`ReadConfig::new`] and change one knob at a time. A
/// numeric setter fails when the value is below its floor or above the active
/// ceiling; it never clamps. [`ReadConfig::with_ceilings`] replaces the
/// ceilings after proving every stored value still fits.
///
/// Cross-field rules, such as `binary_sample_bytes <= chunk_bytes`, are not
/// enforced by these setters except where noted. The read tool checks them
/// once when it is constructed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadConfig {
    max_lines: u32,
    max_line_bytes: u32,
    max_output_bytes: usize,
    chunk_bytes: usize,
    binary_sample_bytes: usize,
    max_image_bytes: u64,
    images: bool,
    ceilings: ReadCeilings,
}

impl ReadConfig {
    /// Builds the documented defaults.
    ///
    /// `max_lines` is 2000, `max_line_bytes` is 2000, `max_output_bytes` is
    /// 48 KiB, `chunk_bytes` is 64 KiB, `binary_sample_bytes` is 8 KiB,
    /// `max_image_bytes` is 5 MiB, and images are enabled. The ceilings are
    /// [`ReadCeilings::default`].
    #[must_use]
    pub const fn new() -> Self {
        Self {
            max_lines: DEFAULT_MAX_LINES,
            max_line_bytes: DEFAULT_MAX_LINE_BYTES,
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
            chunk_bytes: DEFAULT_CHUNK_BYTES,
            binary_sample_bytes: DEFAULT_BINARY_SAMPLE_BYTES,
            max_image_bytes: DEFAULT_MAX_IMAGE_BYTES,
            images: true,
            ceilings: default_ceilings(),
        }
    }

    /// Maximum number of lines one call returns.
    #[must_use]
    pub const fn max_lines(&self) -> u32 {
        self.max_lines
    }

    /// Maximum bytes kept from one line.
    #[must_use]
    pub const fn max_line_bytes(&self) -> u32 {
        self.max_line_bytes
    }

    /// Maximum bytes of numbered text one call returns.
    #[must_use]
    pub const fn max_output_bytes(&self) -> usize {
        self.max_output_bytes
    }

    /// Bytes requested from the host per streamed read.
    #[must_use]
    pub const fn chunk_bytes(&self) -> usize {
        self.chunk_bytes
    }

    /// Leading bytes classified before the rest of a file is read.
    #[must_use]
    pub const fn binary_sample_bytes(&self) -> usize {
        self.binary_sample_bytes
    }

    /// Maximum bytes of one image returned to the host.
    #[must_use]
    pub const fn max_image_bytes(&self) -> u64 {
        self.max_image_bytes
    }

    /// Whether image bytes are returned beside the text.
    #[must_use]
    pub const fn images(&self) -> bool {
        self.images
    }

    /// Copy of the ceilings in force.
    #[must_use]
    pub const fn ceilings(&self) -> ReadCeilings {
        self.ceilings
    }

    /// Replaces the maximum number of lines one call returns.
    ///
    /// # Errors
    ///
    /// Returns [`crate::ErrorCode::SpecInvalid`] when `max_lines` is below 1 or
    /// above the active [`ReadCeilings::max_lines`]. The value is not clamped.
    pub fn with_max_lines(mut self, max_lines: u32) -> Result<Self, ToolError> {
        check_knob(
            "max_lines",
            max_lines,
            MIN_MAX_LINES,
            self.ceilings.max_lines,
        )?;
        self.max_lines = max_lines;
        Ok(self)
    }

    /// Replaces the maximum bytes kept from one line.
    ///
    /// # Errors
    ///
    /// Returns [`crate::ErrorCode::SpecInvalid`] when `max_line_bytes` is below
    /// 1 or above the active [`ReadCeilings::max_line_bytes`]. The value is
    /// not clamped.
    pub fn with_max_line_bytes(mut self, max_line_bytes: u32) -> Result<Self, ToolError> {
        check_knob(
            "max_line_bytes",
            max_line_bytes,
            MIN_MAX_LINE_BYTES,
            self.ceilings.max_line_bytes,
        )?;
        self.max_line_bytes = max_line_bytes;
        Ok(self)
    }

    /// Replaces the maximum bytes of numbered text one call returns.
    ///
    /// # Errors
    ///
    /// Returns [`crate::ErrorCode::SpecInvalid`] when `max_output_bytes` is
    /// below 320 or above the active [`ReadCeilings::max_output_bytes`]. The
    /// value is not clamped.
    pub fn with_max_output_bytes(mut self, max_output_bytes: usize) -> Result<Self, ToolError> {
        check_knob(
            "max_output_bytes",
            max_output_bytes,
            MIN_MAX_OUTPUT_BYTES,
            self.ceilings.max_output_bytes,
        )?;
        self.max_output_bytes = max_output_bytes;
        Ok(self)
    }

    /// Replaces the bytes requested from the host per streamed read.
    ///
    /// This setter does not look at the stored `binary_sample_bytes`, so a
    /// smaller chunk is accepted here. The read tool checks that relation when
    /// it is constructed.
    ///
    /// # Errors
    ///
    /// Returns [`crate::ErrorCode::SpecInvalid`] when `chunk_bytes` is below 12
    /// or above the active [`ReadCeilings::chunk_bytes`]. The value is not
    /// clamped.
    pub fn with_chunk_bytes(mut self, chunk_bytes: usize) -> Result<Self, ToolError> {
        check_knob(
            "chunk_bytes",
            chunk_bytes,
            MIN_CHUNK_BYTES,
            self.ceilings.chunk_bytes,
        )?;
        self.chunk_bytes = chunk_bytes;
        Ok(self)
    }

    /// Replaces the leading bytes classified before the rest of a file is read.
    ///
    /// # Errors
    ///
    /// Returns [`crate::ErrorCode::SpecInvalid`] when `binary_sample_bytes` is
    /// below 12 or above the current `chunk_bytes`. The value is not clamped.
    pub fn with_binary_sample_bytes(
        mut self,
        binary_sample_bytes: usize,
    ) -> Result<Self, ToolError> {
        check_knob(
            "binary_sample_bytes",
            binary_sample_bytes,
            MIN_BINARY_SAMPLE_BYTES,
            self.chunk_bytes,
        )?;
        self.binary_sample_bytes = binary_sample_bytes;
        Ok(self)
    }

    /// Replaces the maximum bytes of one image returned to the host.
    ///
    /// # Errors
    ///
    /// Returns [`crate::ErrorCode::SpecInvalid`] when `max_image_bytes` is
    /// below 1 or above the active [`ReadCeilings::max_image_bytes`]. The
    /// value is not clamped.
    pub fn with_max_image_bytes(mut self, max_image_bytes: u64) -> Result<Self, ToolError> {
        check_knob(
            "max_image_bytes",
            max_image_bytes,
            MIN_MAX_IMAGE_BYTES,
            self.ceilings.max_image_bytes,
        )?;
        self.max_image_bytes = max_image_bytes;
        Ok(self)
    }

    /// Enables or disables image returns. Numeric limits are unchanged.
    #[must_use]
    pub fn with_images(mut self, enabled: bool) -> Self {
        self.images = enabled;
        self
    }

    /// Replaces the ceilings after proving every stored value still fits.
    ///
    /// Every new ceiling must also be at or above the floor of its knob.
    ///
    /// # Errors
    ///
    /// Returns [`crate::ErrorCode::SpecInvalid`] when a ceiling is below its
    /// floor or when a stored knob is above its new ceiling. Nothing is
    /// changed: the checks run before the new ceilings are stored, so the
    /// returned value is dropped without a partial update. Because the method
    /// consumes `self`, the caller can keep using the previous config only if
    /// it did not move that value into this call (for example, by calling this
    /// method on a clone).
    pub fn with_ceilings(mut self, ceilings: ReadCeilings) -> Result<Self, ToolError> {
        check_ceiling_floor("max_lines", ceilings.max_lines, MIN_MAX_LINES)?;
        check_ceiling_floor(
            "max_line_bytes",
            ceilings.max_line_bytes,
            MIN_MAX_LINE_BYTES,
        )?;
        check_ceiling_floor(
            "max_output_bytes",
            ceilings.max_output_bytes,
            MIN_MAX_OUTPUT_BYTES,
        )?;
        check_ceiling_floor("chunk_bytes", ceilings.chunk_bytes, MIN_CHUNK_BYTES)?;
        check_ceiling_floor(
            "max_image_bytes",
            ceilings.max_image_bytes,
            MIN_MAX_IMAGE_BYTES,
        )?;

        check_knob(
            "max_lines",
            self.max_lines,
            MIN_MAX_LINES,
            ceilings.max_lines,
        )?;
        check_knob(
            "max_line_bytes",
            self.max_line_bytes,
            MIN_MAX_LINE_BYTES,
            ceilings.max_line_bytes,
        )?;
        check_knob(
            "max_output_bytes",
            self.max_output_bytes,
            MIN_MAX_OUTPUT_BYTES,
            ceilings.max_output_bytes,
        )?;
        check_knob(
            "chunk_bytes",
            self.chunk_bytes,
            MIN_CHUNK_BYTES,
            ceilings.chunk_bytes,
        )?;
        check_knob(
            "max_image_bytes",
            self.max_image_bytes,
            MIN_MAX_IMAGE_BYTES,
            ceilings.max_image_bytes,
        )?;
        let chunk_cap = self.chunk_bytes.min(ceilings.chunk_bytes);
        if self.binary_sample_bytes > chunk_cap {
            return Err(ToolError::spec_invalid(format!(
                "binary_sample_bytes ({}) is above chunk_bytes ({chunk_cap})",
                self.binary_sample_bytes
            )));
        }

        self.ceilings = ceilings;
        Ok(self)
    }
}

impl Default for ReadConfig {
    fn default() -> Self {
        Self::new()
    }
}

/// Checks one numeric knob against its floor and its ceiling.
///
/// The error names the field and the boundary that rejected the value.
fn check_knob<T>(field: &str, value: T, floor: T, ceiling: T) -> Result<(), ToolError>
where
    T: Copy + Ord + fmt::Display,
{
    if value < floor {
        return Err(ToolError::spec_invalid(format!(
            "field \"{field}\" must be at least {floor}, got {value}"
        )));
    }
    if value > ceiling {
        return Err(ToolError::spec_invalid(format!(
            "field \"{field}\" must be at most {ceiling}, got {value}"
        )));
    }
    Ok(())
}

/// Checks one ceiling against the floor of its knob.
fn check_ceiling_floor<T>(field: &str, ceiling: T, floor: T) -> Result<(), ToolError>
where
    T: Copy + Ord + fmt::Display,
{
    if ceiling < floor {
        return Err(ToolError::spec_invalid(format!(
            "ceiling \"{field}\" must be at least {floor}, got {ceiling}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ErrorCode;

    fn assert_spec_invalid(error: &ToolError) {
        assert_eq!(error.code(), ErrorCode::SpecInvalid);
        assert!(
            error.to_string().starts_with("AGT_SPEC_INVALID:"),
            "unexpected display: {error}"
        );
    }

    #[test]
    fn new_uses_every_documented_default() {
        let config = ReadConfig::new();
        assert_eq!(config.max_lines(), 2_000);
        assert_eq!(config.max_line_bytes(), 2_000);
        assert_eq!(config.max_output_bytes(), 48 * 1024);
        assert_eq!(config.chunk_bytes(), 64 * 1024);
        assert_eq!(config.binary_sample_bytes(), 8 * 1024);
        assert_eq!(config.max_image_bytes(), 5 * 1024 * 1024);
        assert!(config.images());
        assert_eq!(config.ceilings(), ReadCeilings::default());
        assert_eq!(ReadConfig::default(), config);
    }

    #[test]
    fn default_ceilings_match_the_documented_maxima() {
        let ceilings = ReadCeilings::default();
        assert_eq!(ceilings.max_lines, 100_000);
        assert_eq!(ceilings.max_line_bytes, 1024 * 1024);
        assert_eq!(ceilings.max_output_bytes, 1024 * 1024);
        assert_eq!(ceilings.chunk_bytes, 1024 * 1024);
        assert_eq!(ceilings.max_image_bytes, 20 * 1024 * 1024);
    }

    #[test]
    fn each_numeric_setter_rejects_a_value_below_its_floor() {
        let cases = [
            ReadConfig::new().with_max_lines(0),
            ReadConfig::new().with_max_line_bytes(0),
            ReadConfig::new().with_max_output_bytes(319),
            ReadConfig::new().with_chunk_bytes(11),
            ReadConfig::new().with_binary_sample_bytes(11),
            ReadConfig::new().with_max_image_bytes(0),
        ];
        for case in cases {
            let error = case.expect_err("a value below the floor must be rejected");
            assert_spec_invalid(&error);
        }
    }

    #[test]
    fn each_numeric_setter_rejects_a_value_above_the_active_ceiling() {
        let cases = [
            ReadConfig::new().with_max_lines(100_001),
            ReadConfig::new().with_max_line_bytes(1024 * 1024 + 1),
            ReadConfig::new().with_max_output_bytes(1024 * 1024 + 1),
            ReadConfig::new().with_chunk_bytes(1024 * 1024 + 1),
            ReadConfig::new().with_binary_sample_bytes(64 * 1024 + 1),
            ReadConfig::new().with_max_image_bytes(20 * 1024 * 1024 + 1),
        ];
        for case in cases {
            let error = case.expect_err("a value above the ceiling must be rejected");
            assert_spec_invalid(&error);
        }
    }

    #[test]
    fn error_messages_name_the_field_and_the_limit() {
        let error = ReadConfig::new()
            .with_max_lines(100_001)
            .expect_err("above the line ceiling");
        assert_eq!(
            error.to_string(),
            "AGT_SPEC_INVALID: field \"max_lines\" must be at most 100000, got 100001"
        );

        let error = ReadConfig::new()
            .with_max_output_bytes(0)
            .expect_err("below the output floor");
        assert_eq!(
            error.to_string(),
            "AGT_SPEC_INVALID: field \"max_output_bytes\" must be at least 320, got 0"
        );

        let error = ReadConfig::new()
            .with_binary_sample_bytes(64 * 1024 + 1)
            .expect_err("above the current chunk");
        assert_eq!(
            error.to_string(),
            "AGT_SPEC_INVALID: field \"binary_sample_bytes\" must be at most 65536, got 65537"
        );
    }

    #[test]
    fn binary_sample_below_the_floor_is_rejected() {
        let error = ReadConfig::new()
            .with_binary_sample_bytes(4)
            .expect_err("4 is below the 12-byte floor");
        assert_spec_invalid(&error);
    }

    #[test]
    fn binary_sample_may_equal_chunk_bytes_but_not_exceed_it() {
        let config = ReadConfig::new()
            .with_binary_sample_bytes(64 * 1024)
            .expect("the sample may equal chunk_bytes");
        assert_eq!(config.binary_sample_bytes(), 64 * 1024);

        let error = config
            .with_binary_sample_bytes(64 * 1024 + 1)
            .expect_err("one byte above chunk_bytes");
        assert_spec_invalid(&error);
    }

    #[test]
    fn a_smaller_chunk_is_allowed_even_when_the_stored_sample_is_larger() {
        let config = ReadConfig::new()
            .with_binary_sample_bytes(8 * 1024)
            .expect("sample fits the default chunk")
            .with_chunk_bytes(100)
            .expect("the chunk setter checks only its own floor and ceiling");
        assert_eq!(config.chunk_bytes(), 100);
        assert_eq!(config.binary_sample_bytes(), 8 * 1024);

        let error = config
            .with_binary_sample_bytes(101)
            .expect_err("the sample is now above chunk_bytes");
        assert_spec_invalid(&error);
    }

    #[test]
    fn values_exactly_at_the_floor_are_accepted() {
        let config = ReadConfig::new()
            .with_max_lines(1)
            .expect("line floor")
            .with_max_line_bytes(1)
            .expect("line byte floor")
            .with_max_output_bytes(320)
            .expect("output floor")
            .with_chunk_bytes(12)
            .expect("chunk floor")
            .with_max_image_bytes(1)
            .expect("image floor");

        assert_eq!(config.max_lines(), 1);
        assert_eq!(config.max_line_bytes(), 1);
        assert_eq!(config.max_output_bytes(), 320);
        assert_eq!(config.chunk_bytes(), 12);
        assert_eq!(config.max_image_bytes(), 1);
        // The chunk setter does not recheck the stored sample.
        assert_eq!(config.binary_sample_bytes(), 8 * 1024);
    }

    #[test]
    fn values_exactly_at_the_default_ceiling_are_accepted() {
        let config = ReadConfig::new()
            .with_max_lines(100_000)
            .expect("line ceiling")
            .with_max_line_bytes(1024 * 1024)
            .expect("line byte ceiling")
            .with_max_output_bytes(1024 * 1024)
            .expect("output ceiling")
            .with_chunk_bytes(1024 * 1024)
            .expect("chunk ceiling")
            .with_binary_sample_bytes(1024 * 1024)
            .expect("the sample ceiling is the current chunk")
            .with_max_image_bytes(20 * 1024 * 1024)
            .expect("image ceiling");

        assert_eq!(config.max_lines(), 100_000);
        assert_eq!(config.max_line_bytes(), 1024 * 1024);
        assert_eq!(config.max_output_bytes(), 1024 * 1024);
        assert_eq!(config.chunk_bytes(), 1024 * 1024);
        assert_eq!(config.binary_sample_bytes(), 1024 * 1024);
        assert_eq!(config.max_image_bytes(), 20 * 1024 * 1024);
    }

    #[test]
    fn with_ceilings_rejects_every_ceiling_below_a_stored_value() {
        let cases = [
            (
                ReadConfig::new().with_max_lines(2_001).expect("valid"),
                ReadCeilings {
                    max_lines: 2_000,
                    ..ReadCeilings::default()
                },
            ),
            (
                ReadConfig::new().with_max_line_bytes(2_001).expect("valid"),
                ReadCeilings {
                    max_line_bytes: 2_000,
                    ..ReadCeilings::default()
                },
            ),
            (
                ReadConfig::new()
                    .with_max_output_bytes(48 * 1024 + 1)
                    .expect("valid"),
                ReadCeilings {
                    max_output_bytes: 48 * 1024,
                    ..ReadCeilings::default()
                },
            ),
            (
                ReadConfig::new()
                    .with_chunk_bytes(64 * 1024 + 1)
                    .expect("valid"),
                ReadCeilings {
                    chunk_bytes: 64 * 1024,
                    ..ReadCeilings::default()
                },
            ),
            (
                ReadConfig::new()
                    .with_max_image_bytes(5 * 1024 * 1024 + 1)
                    .expect("valid"),
                ReadCeilings {
                    max_image_bytes: 5 * 1024 * 1024,
                    ..ReadCeilings::default()
                },
            ),
        ];

        for (config, ceilings) in cases {
            let before = config.clone();
            let error = config
                .with_ceilings(ceilings)
                .expect_err("stored value is above the new ceiling");
            assert_spec_invalid(&error);
            // The caller that kept a clone still sees the previous config,
            // because the checks run before anything is stored.
            assert_eq!(before.ceilings(), ReadCeilings::default());
        }
    }

    #[test]
    fn a_failed_with_ceilings_leaves_another_config_untouched() {
        let config = ReadConfig::new()
            .with_max_lines(5_000)
            .expect("below the default ceiling");
        let before = config.clone();
        let untouched = ReadConfig::new();
        let lowered = ReadCeilings {
            max_lines: 4_999,
            ..ReadCeilings::default()
        };

        let error = config
            .with_ceilings(lowered)
            .expect_err("stored max_lines is above the new ceiling");
        assert_spec_invalid(&error);
        assert!(error.message().contains("max_lines"));
        assert!(error.message().contains("4999"));

        assert_eq!(before.max_lines(), 5_000);
        assert_eq!(
            before,
            ReadConfig::new().with_max_lines(5_000).expect("valid")
        );
        assert_eq!(untouched, ReadConfig::new());
    }

    #[test]
    fn with_ceilings_rejects_a_ceiling_below_its_floor() {
        let cases = [
            ReadCeilings {
                max_lines: 0,
                ..ReadCeilings::default()
            },
            ReadCeilings {
                max_line_bytes: 0,
                ..ReadCeilings::default()
            },
            ReadCeilings {
                max_output_bytes: 319,
                ..ReadCeilings::default()
            },
            ReadCeilings {
                chunk_bytes: 11,
                ..ReadCeilings::default()
            },
            ReadCeilings {
                max_image_bytes: 0,
                ..ReadCeilings::default()
            },
        ];

        for ceilings in cases {
            let error = ReadConfig::new()
                .with_ceilings(ceilings)
                .expect_err("ceiling below the floor");
            assert_spec_invalid(&error);
        }
    }

    #[test]
    fn with_ceilings_replaces_the_limit_for_later_setters() {
        let raised = ReadCeilings {
            max_lines: 200_000,
            ..ReadCeilings::default()
        };
        let config = ReadConfig::new()
            .with_ceilings(raised)
            .expect("raise the line ceiling");
        assert_eq!(config.ceilings().max_lines, 200_000);
        assert_eq!(
            config
                .clone()
                .with_max_lines(200_000)
                .expect("at the new ceiling")
                .max_lines(),
            200_000
        );

        let error = config
            .with_max_lines(200_001)
            .expect_err("above the new ceiling");
        assert_spec_invalid(&error);
    }

    #[test]
    fn with_ceilings_accepts_a_ceiling_equal_to_a_stored_value() {
        let config = ReadConfig::new().with_max_lines(2_000).expect("valid");
        let ceilings = ReadCeilings {
            max_lines: 2_000,
            ..ReadCeilings::default()
        };
        let config = config
            .with_ceilings(ceilings)
            .expect("a ceiling equal to the stored value is allowed");
        assert_eq!(config.ceilings().max_lines, 2_000);

        let error = config
            .with_max_lines(2_001)
            .expect_err("above the new ceiling");
        assert_spec_invalid(&error);
    }

    #[test]
    fn with_ceilings_rejects_a_sample_above_the_new_chunk_ceiling() {
        let config = ReadConfig::new()
            .with_chunk_bytes(100)
            .expect("chunk above the sample is a later check");
        let ceilings = ReadCeilings {
            chunk_bytes: 100,
            ..ReadCeilings::default()
        };
        let error = config
            .clone()
            .with_ceilings(ceilings)
            .expect_err("sample 8192 does not fit a chunk ceiling of 100");
        assert_spec_invalid(&error);

        let ceilings = ReadCeilings {
            chunk_bytes: 100,
            ..ReadCeilings::default()
        };
        let config = config
            .with_binary_sample_bytes(12)
            .expect("sample fits the stored chunk")
            .with_ceilings(ceilings)
            .expect("sample fits the new chunk ceiling");
        assert_eq!(config.binary_sample_bytes(), 12);
    }

    #[test]
    fn with_images_flips_only_the_flag() {
        let defaults = ReadConfig::new();
        let config = defaults.clone().with_images(false);

        assert!(!config.images());
        assert_eq!(config.max_lines(), defaults.max_lines());
        assert_eq!(config.max_line_bytes(), defaults.max_line_bytes());
        assert_eq!(config.max_output_bytes(), defaults.max_output_bytes());
        assert_eq!(config.chunk_bytes(), defaults.chunk_bytes());
        assert_eq!(config.binary_sample_bytes(), defaults.binary_sample_bytes());
        assert_eq!(config.max_image_bytes(), defaults.max_image_bytes());
        assert_eq!(config.ceilings(), defaults.ceilings());
    }
}
