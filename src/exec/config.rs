//! Budget and timeout limits for the exec tool.
//!
//! [`ExecConfig`] stores every byte limit and wall-clock timeout an exec call
//! uses. Each setter rejects an unusable value instead of clamping it, so a
//! host learns about a bad budget when it builds the tool, not on the first
//! call.
//!
//! [`ExecCeilings`] bounds what a setter may store. A host that needs a larger
//! budget must say so with [`ExecConfig::with_ceilings`].

use std::fmt;
use std::time::Duration;

use crate::ToolError;

/// Lowest accepted `max_output_bytes` and the lowest accepted ceiling for it.
const MIN_MAX_OUTPUT_BYTES: usize = 320;
/// Lowest accepted `max_line_bytes` and the lowest accepted ceiling for it.
const MIN_MAX_LINE_BYTES: usize = 32;
/// Lowest accepted `chunk_bytes` and the lowest accepted ceiling for it.
const MIN_CHUNK_BYTES: usize = 256;
/// Lowest accepted timeout and the lowest accepted ceiling for one.
const MIN_TIMEOUT: Duration = Duration::from_millis(1);

const DEFAULT_MAX_OUTPUT_BYTES: usize = 48 * 1024;
const DEFAULT_HEAD_BYTES: usize = 16 * 1024;
const DEFAULT_MAX_LINE_BYTES: usize = 2_000;
const DEFAULT_CHUNK_BYTES: usize = 8 * 1024;
const DEFAULT_DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_MAX_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const DEFAULT_DRAIN_TIMEOUT: Duration = Duration::from_secs(2);

const DEFAULT_CEILING_MAX_OUTPUT_BYTES: usize = 1024 * 1024;
const DEFAULT_CEILING_MAX_LINE_BYTES: usize = 64 * 1024;
const DEFAULT_CEILING_CHUNK_BYTES: usize = 64 * 1024;
const DEFAULT_CEILING_DEFAULT_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const DEFAULT_CEILING_MAX_TIMEOUT: Duration = Duration::from_secs(24 * 60 * 60);
const DEFAULT_CEILING_DRAIN_TIMEOUT: Duration = Duration::from_secs(30);

/// Highest value an [`ExecConfig`] setter may store for each knob.
///
/// The defaults match the ceilings of [`ExecConfig::new`]. A host replaces
/// them with [`ExecConfig::with_ceilings`]. `head_bytes` has no field here: its
/// ceiling is the current `max_output_bytes`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExecCeilings {
    /// Highest value [`ExecConfig::with_max_output_bytes`] may store.
    pub max_output_bytes: usize,
    /// Highest value [`ExecConfig::with_max_line_bytes`] may store.
    pub max_line_bytes: usize,
    /// Highest value [`ExecConfig::with_chunk_bytes`] may store.
    pub chunk_bytes: usize,
    /// Highest value [`ExecConfig::with_default_timeout`] may store.
    pub default_timeout: Duration,
    /// Highest value [`ExecConfig::with_max_timeout`] may store.
    pub max_timeout: Duration,
    /// Highest value [`ExecConfig::with_drain_timeout`] may store.
    pub drain_timeout: Duration,
}

impl Default for ExecCeilings {
    fn default() -> Self {
        default_ceilings()
    }
}

/// Upper bounds for the knobs. These are not the default knob values.
const fn default_ceilings() -> ExecCeilings {
    ExecCeilings {
        max_output_bytes: DEFAULT_CEILING_MAX_OUTPUT_BYTES,
        max_line_bytes: DEFAULT_CEILING_MAX_LINE_BYTES,
        chunk_bytes: DEFAULT_CEILING_CHUNK_BYTES,
        default_timeout: DEFAULT_CEILING_DEFAULT_TIMEOUT,
        max_timeout: DEFAULT_CEILING_MAX_TIMEOUT,
        drain_timeout: DEFAULT_CEILING_DRAIN_TIMEOUT,
    }
}

/// Budget and timeout limits for one exec tool.
///
/// Build a value with [`ExecConfig::new`] and change one knob at a time. Each
/// setter fails when the value is below its floor or above the active ceiling;
/// it never clamps. [`ExecConfig::with_ceilings`] replaces the ceilings after
/// proving every stored value still fits.
///
/// The cross-field rules `head_bytes <= max_output_bytes` and
/// `max_timeout >= default_timeout` are checked by the setters that could break
/// them, so a stored config always satisfies both.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecConfig {
    max_output_bytes: usize,
    head_bytes: usize,
    max_line_bytes: usize,
    chunk_bytes: usize,
    default_timeout: Duration,
    max_timeout: Duration,
    drain_timeout: Duration,
    allow_unbounded_timeout: bool,
    ceilings: ExecCeilings,
}

impl ExecConfig {
    /// Builds the documented defaults.
    ///
    /// `max_output_bytes` is 48 KiB, `head_bytes` is 16 KiB, `max_line_bytes`
    /// is 2000, `chunk_bytes` is 8 KiB, `default_timeout` is 30 seconds,
    /// `max_timeout` is 10 minutes, `drain_timeout` is 2 seconds, and
    /// unbounded timeouts are off. The ceilings are [`ExecCeilings::default`].
    #[must_use]
    pub const fn new() -> Self {
        Self {
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
            head_bytes: DEFAULT_HEAD_BYTES,
            max_line_bytes: DEFAULT_MAX_LINE_BYTES,
            chunk_bytes: DEFAULT_CHUNK_BYTES,
            default_timeout: DEFAULT_DEFAULT_TIMEOUT,
            max_timeout: DEFAULT_MAX_TIMEOUT,
            drain_timeout: DEFAULT_DRAIN_TIMEOUT,
            allow_unbounded_timeout: false,
            ceilings: default_ceilings(),
        }
    }

    /// Maximum bytes of merged output one call returns.
    #[must_use]
    pub const fn max_output_bytes(&self) -> usize {
        self.max_output_bytes
    }

    /// Leading bytes kept before the omission marker.
    ///
    /// Zero keeps the tail only. A value equal to `max_output_bytes` keeps the
    /// head only.
    #[must_use]
    pub const fn head_bytes(&self) -> usize {
        self.head_bytes
    }

    /// Maximum bytes kept from one line.
    #[must_use]
    pub const fn max_line_bytes(&self) -> usize {
        self.max_line_bytes
    }

    /// Bytes read per streamed read.
    #[must_use]
    pub const fn chunk_bytes(&self) -> usize {
        self.chunk_bytes
    }

    /// Wall-clock limit used when the model omits `timeout_ms`.
    #[must_use]
    pub const fn default_timeout(&self) -> Duration {
        self.default_timeout
    }

    /// Highest `timeout_ms` a model call may request.
    #[must_use]
    pub const fn max_timeout(&self) -> Duration {
        self.max_timeout
    }

    /// How long the runner may block in pipe reads after the tree is killed.
    #[must_use]
    pub const fn drain_timeout(&self) -> Duration {
        self.drain_timeout
    }

    /// Whether a model may request no deadline at all.
    #[must_use]
    pub const fn allow_unbounded_timeout(&self) -> bool {
        self.allow_unbounded_timeout
    }

    /// Copy of the ceilings in force.
    #[must_use]
    pub const fn ceilings(&self) -> ExecCeilings {
        self.ceilings
    }

    /// Replaces the maximum bytes of merged output one call returns.
    ///
    /// The stored `head_bytes` must still fit, so shrinking
    /// `max_output_bytes` below `head_bytes` is rejected.
    ///
    /// # Errors
    ///
    /// Returns [`crate::ErrorCode::SpecInvalid`] when `max_output_bytes` is
    /// below 320, above the active [`ExecCeilings::max_output_bytes`], or below
    /// the stored `head_bytes`. The value is not clamped.
    pub fn with_max_output_bytes(mut self, max_output_bytes: usize) -> Result<Self, ToolError> {
        check_knob(
            "max_output_bytes",
            max_output_bytes,
            MIN_MAX_OUTPUT_BYTES,
            self.ceilings.max_output_bytes,
        )?;
        if self.head_bytes > max_output_bytes {
            return Err(ToolError::spec_invalid(format!(
                "field \"max_output_bytes\" ({max_output_bytes}) is below the stored head_bytes ({})",
                self.head_bytes
            )));
        }
        self.max_output_bytes = max_output_bytes;
        Ok(self)
    }

    /// Replaces the leading bytes kept before the omission marker.
    ///
    /// Zero is legal and means tail-only. A value equal to
    /// `max_output_bytes` is legal and means head-only.
    ///
    /// # Errors
    ///
    /// Returns [`crate::ErrorCode::SpecInvalid`] when `head_bytes` is above the
    /// stored `max_output_bytes`. The value is not clamped.
    pub fn with_head_bytes(mut self, head_bytes: usize) -> Result<Self, ToolError> {
        if head_bytes > self.max_output_bytes {
            return Err(ToolError::spec_invalid(format!(
                "field \"head_bytes\" ({head_bytes}) is above max_output_bytes ({})",
                self.max_output_bytes
            )));
        }
        self.head_bytes = head_bytes;
        Ok(self)
    }

    /// Replaces the maximum bytes kept from one line.
    ///
    /// # Errors
    ///
    /// Returns [`crate::ErrorCode::SpecInvalid`] when `max_line_bytes` is below
    /// 32 or above the active [`ExecCeilings::max_line_bytes`]. The value is
    /// not clamped.
    pub fn with_max_line_bytes(mut self, max_line_bytes: usize) -> Result<Self, ToolError> {
        check_knob(
            "max_line_bytes",
            max_line_bytes,
            MIN_MAX_LINE_BYTES,
            self.ceilings.max_line_bytes,
        )?;
        self.max_line_bytes = max_line_bytes;
        Ok(self)
    }

    /// Replaces the bytes read per streamed read.
    ///
    /// # Errors
    ///
    /// Returns [`crate::ErrorCode::SpecInvalid`] when `chunk_bytes` is below
    /// 256 or above the active [`ExecCeilings::chunk_bytes`]. The value is not
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

    /// Replaces the wall-clock limit used when the model omits `timeout_ms`.
    ///
    /// # Errors
    ///
    /// Returns [`crate::ErrorCode::SpecInvalid`] when `default_timeout` is
    /// below 1 millisecond, above the active
    /// [`ExecCeilings::default_timeout`], or above the stored `max_timeout`.
    /// Raise `max_timeout` first instead; the value is not clamped.
    pub fn with_default_timeout(mut self, default_timeout: Duration) -> Result<Self, ToolError> {
        check_knob(
            "default_timeout",
            default_timeout,
            MIN_TIMEOUT,
            self.ceilings.default_timeout,
        )?;
        if default_timeout > self.max_timeout {
            return Err(ToolError::spec_invalid(format!(
                "field \"default_timeout\" is above max_timeout ({:?})",
                self.max_timeout
            )));
        }
        self.default_timeout = default_timeout;
        Ok(self)
    }

    /// Replaces the highest timeout a model call may request.
    ///
    /// # Errors
    ///
    /// Returns [`crate::ErrorCode::SpecInvalid`] when `max_timeout` is below 1
    /// millisecond, above the active [`ExecCeilings::max_timeout`], or below
    /// the stored `default_timeout`. The value is not clamped.
    pub fn with_max_timeout(mut self, max_timeout: Duration) -> Result<Self, ToolError> {
        check_knob(
            "max_timeout",
            max_timeout,
            MIN_TIMEOUT,
            self.ceilings.max_timeout,
        )?;
        if max_timeout < self.default_timeout {
            return Err(ToolError::spec_invalid(format!(
                "field \"max_timeout\" ({max_timeout:?}) is below the stored default_timeout ({:?})",
                self.default_timeout
            )));
        }
        self.max_timeout = max_timeout;
        Ok(self)
    }

    /// Replaces how long the runner may block in pipe reads after the tree is
    /// killed.
    ///
    /// This is not the command timeout.
    ///
    /// # Errors
    ///
    /// Returns [`crate::ErrorCode::SpecInvalid`] when `drain_timeout` is below
    /// 1 millisecond or above the active [`ExecCeilings::drain_timeout`]. The
    /// value is not clamped.
    pub fn with_drain_timeout(mut self, drain_timeout: Duration) -> Result<Self, ToolError> {
        check_knob(
            "drain_timeout",
            drain_timeout,
            MIN_TIMEOUT,
            self.ceilings.drain_timeout,
        )?;
        self.drain_timeout = drain_timeout;
        Ok(self)
    }

    /// Enables or disables model requests with no deadline. Limits are
    /// unchanged.
    #[must_use]
    pub fn with_allow_unbounded_timeout(mut self, enabled: bool) -> Self {
        self.allow_unbounded_timeout = enabled;
        self
    }

    /// Replaces the ceilings after proving every stored value still fits.
    ///
    /// Every new ceiling must also be at or above the floor of its knob, and
    /// `max_timeout` must be at or above `default_timeout`.
    ///
    /// # Errors
    ///
    /// Returns [`crate::ErrorCode::SpecInvalid`] when a ceiling is below its
    /// floor, when a stored knob is above its new ceiling, or when the pair
    /// `max_timeout` / `default_timeout` is out of order. Nothing is changed:
    /// the checks run before the new ceilings are stored, so the returned value
    /// is dropped without a partial update.
    pub fn with_ceilings(mut self, ceilings: ExecCeilings) -> Result<Self, ToolError> {
        check_ceiling_floor(
            "max_output_bytes",
            ceilings.max_output_bytes,
            MIN_MAX_OUTPUT_BYTES,
        )?;
        check_ceiling_floor(
            "max_line_bytes",
            ceilings.max_line_bytes,
            MIN_MAX_LINE_BYTES,
        )?;
        check_ceiling_floor("chunk_bytes", ceilings.chunk_bytes, MIN_CHUNK_BYTES)?;
        check_ceiling_floor("default_timeout", ceilings.default_timeout, MIN_TIMEOUT)?;
        check_ceiling_floor("max_timeout", ceilings.max_timeout, MIN_TIMEOUT)?;
        check_ceiling_floor("drain_timeout", ceilings.drain_timeout, MIN_TIMEOUT)?;
        if ceilings.max_timeout < ceilings.default_timeout {
            return Err(ToolError::spec_invalid(format!(
                "ceiling \"max_timeout\" ({:?}) is below ceiling \"default_timeout\" ({:?})",
                ceilings.max_timeout, ceilings.default_timeout
            )));
        }

        check_knob(
            "max_output_bytes",
            self.max_output_bytes,
            MIN_MAX_OUTPUT_BYTES,
            ceilings.max_output_bytes,
        )?;
        check_knob(
            "max_line_bytes",
            self.max_line_bytes,
            MIN_MAX_LINE_BYTES,
            ceilings.max_line_bytes,
        )?;
        check_knob(
            "chunk_bytes",
            self.chunk_bytes,
            MIN_CHUNK_BYTES,
            ceilings.chunk_bytes,
        )?;
        check_knob(
            "default_timeout",
            self.default_timeout,
            MIN_TIMEOUT,
            ceilings.default_timeout,
        )?;
        check_knob(
            "max_timeout",
            self.max_timeout,
            MIN_TIMEOUT,
            ceilings.max_timeout,
        )?;
        check_knob(
            "drain_timeout",
            self.drain_timeout,
            MIN_TIMEOUT,
            ceilings.drain_timeout,
        )?;

        self.ceilings = ceilings;
        Ok(self)
    }
}

impl Default for ExecConfig {
    fn default() -> Self {
        Self::new()
    }
}

/// Checks one numeric knob against its floor and its ceiling.
///
/// The error names the field and the boundary that rejected the value.
fn check_knob<T>(field: &str, value: T, floor: T, ceiling: T) -> Result<(), ToolError>
where
    T: Copy + Ord + fmt::Debug,
{
    if value < floor {
        return Err(ToolError::spec_invalid(format!(
            "field \"{field}\" must be at least {floor:?}, got {value:?}"
        )));
    }
    if value > ceiling {
        return Err(ToolError::spec_invalid(format!(
            "field \"{field}\" must be at most {ceiling:?}, got {value:?}"
        )));
    }
    Ok(())
}

/// Checks one ceiling against the floor of its knob.
fn check_ceiling_floor<T>(field: &str, ceiling: T, floor: T) -> Result<(), ToolError>
where
    T: Copy + Ord + fmt::Debug,
{
    if ceiling < floor {
        return Err(ToolError::spec_invalid(format!(
            "ceiling \"{field}\" must be at least {floor:?}, got {ceiling:?}"
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

    fn milliseconds(value: u64) -> Duration {
        Duration::from_millis(value)
    }

    #[test]
    fn new_uses_every_documented_default() {
        let config = ExecConfig::new();
        assert_eq!(config.max_output_bytes(), 48 * 1024);
        assert_eq!(config.head_bytes(), 16 * 1024);
        assert_eq!(config.max_line_bytes(), 2_000);
        assert_eq!(config.chunk_bytes(), 8 * 1024);
        assert_eq!(config.default_timeout(), Duration::from_secs(30));
        assert_eq!(config.max_timeout(), Duration::from_secs(10 * 60));
        assert_eq!(config.drain_timeout(), Duration::from_secs(2));
        assert!(!config.allow_unbounded_timeout());
        assert_eq!(config.ceilings(), ExecCeilings::default());
        assert_eq!(ExecConfig::default(), config);
    }

    #[test]
    fn default_ceilings_match_the_documented_maxima() {
        let ceilings = ExecCeilings::default();
        assert_eq!(ceilings.max_output_bytes, 1024 * 1024);
        assert_eq!(ceilings.max_line_bytes, 64 * 1024);
        assert_eq!(ceilings.chunk_bytes, 64 * 1024);
        assert_eq!(ceilings.default_timeout, Duration::from_secs(10 * 60));
        assert_eq!(ceilings.max_timeout, Duration::from_secs(24 * 60 * 60));
        assert_eq!(ceilings.drain_timeout, Duration::from_secs(30));
    }

    #[test]
    fn each_numeric_setter_rejects_a_value_below_its_floor() {
        let cases = [
            ExecConfig::new().with_max_output_bytes(319),
            ExecConfig::new().with_max_line_bytes(31),
            ExecConfig::new().with_chunk_bytes(255),
            ExecConfig::new().with_default_timeout(Duration::ZERO),
            ExecConfig::new().with_max_timeout(Duration::ZERO),
            ExecConfig::new().with_drain_timeout(Duration::ZERO),
        ];
        for case in cases {
            let error = case.expect_err("a value below the floor must be rejected");
            assert_spec_invalid(&error);
        }
    }

    #[test]
    fn a_timeout_below_one_millisecond_is_rejected() {
        let error = ExecConfig::new()
            .with_default_timeout(Duration::from_micros(999))
            .expect_err("999 microseconds is below the floor");
        assert_spec_invalid(&error);

        let error = ExecConfig::new()
            .with_drain_timeout(Duration::from_nanos(1))
            .expect_err("one nanosecond is below the floor");
        assert_spec_invalid(&error);

        let config = ExecConfig::new()
            .with_default_timeout(milliseconds(1))
            .expect("one millisecond is the floor");
        assert_eq!(config.default_timeout(), Duration::from_millis(1));
    }

    #[test]
    fn each_numeric_setter_rejects_a_value_above_the_active_ceiling() {
        let cases = [
            ExecConfig::new().with_max_output_bytes(1024 * 1024 + 1),
            ExecConfig::new().with_max_line_bytes(64 * 1024 + 1),
            ExecConfig::new().with_chunk_bytes(64 * 1024 + 1),
            ExecConfig::new().with_default_timeout(Duration::from_secs(10 * 60 + 1)),
            ExecConfig::new().with_max_timeout(Duration::from_secs(24 * 60 * 60 + 1)),
            ExecConfig::new().with_drain_timeout(Duration::from_secs(31)),
        ];
        for case in cases {
            let error = case.expect_err("a value above the ceiling must be rejected");
            assert_spec_invalid(&error);
        }
    }

    #[test]
    fn error_messages_name_the_field_and_the_limit() {
        let error = ExecConfig::new()
            .with_max_line_bytes(31)
            .expect_err("below the line floor");
        assert_eq!(
            error.to_string(),
            "AGT_SPEC_INVALID: field \"max_line_bytes\" must be at least 32, got 31"
        );

        let error = ExecConfig::new()
            .with_chunk_bytes(64 * 1024 + 1)
            .expect_err("above the chunk ceiling");
        assert_eq!(
            error.to_string(),
            "AGT_SPEC_INVALID: field \"chunk_bytes\" must be at most 65536, got 65537"
        );
    }

    #[test]
    fn head_bytes_zero_and_max_output_bytes_are_accepted() {
        let tail_only = ExecConfig::new()
            .with_head_bytes(0)
            .expect("zero means tail only");
        assert_eq!(tail_only.head_bytes(), 0);

        let head_only = ExecConfig::new()
            .with_head_bytes(48 * 1024)
            .expect("the full budget means head only");
        assert_eq!(head_only.head_bytes(), 48 * 1024);
        assert_eq!(head_only.max_output_bytes(), 48 * 1024);
    }

    #[test]
    fn head_bytes_above_max_output_bytes_is_rejected() {
        let error = ExecConfig::new()
            .with_head_bytes(48 * 1024 + 1)
            .expect_err("head is above the output budget");
        assert_spec_invalid(&error);
        assert!(error.message().contains("head_bytes"));
        assert!(error.message().contains("max_output_bytes"));
    }

    #[test]
    fn shrinking_max_output_bytes_below_head_bytes_is_rejected() {
        let error = ExecConfig::new()
            .with_max_output_bytes(16 * 1024 - 1)
            .expect_err("the default head no longer fits");
        assert_spec_invalid(&error);

        let config = ExecConfig::new()
            .with_head_bytes(0)
            .expect("drop the head first")
            .with_max_output_bytes(320)
            .expect("320 is the output floor");
        assert_eq!(config.max_output_bytes(), 320);
        assert_eq!(config.head_bytes(), 0);
    }

    #[test]
    fn default_timeout_above_max_timeout_is_rejected() {
        let error = ExecConfig::new()
            .with_default_timeout(Duration::from_secs(10 * 60 + 1))
            .expect_err("above max_timeout and above the ceiling");
        assert_spec_invalid(&error);

        let error = ExecConfig::new()
            .with_max_timeout(Duration::from_secs(5 * 60))
            .expect("lower max_timeout above the default")
            .with_default_timeout(Duration::from_secs(5 * 60 + 1))
            .expect_err("default is now above the stored max");
        assert_spec_invalid(&error);
        assert!(error.message().contains("max_timeout"));
    }

    #[test]
    fn max_timeout_below_default_timeout_is_rejected() {
        let error = ExecConfig::new()
            .with_max_timeout(Duration::from_secs(29))
            .expect_err("below the stored default of 30 seconds");
        assert_spec_invalid(&error);

        let config = ExecConfig::new()
            .with_default_timeout(Duration::from_secs(5))
            .expect("lower the default first")
            .with_max_timeout(Duration::from_secs(5))
            .expect("equal to the default");
        assert_eq!(config.default_timeout(), config.max_timeout());
    }

    #[test]
    fn a_raised_default_timeout_ceiling_allows_a_larger_default() {
        // The default ceiling for `default_timeout` is 10 minutes, so raising
        // the stored default past it needs a new ceiling first.
        let error = ExecConfig::new()
            .with_default_timeout(Duration::from_secs(11 * 60))
            .expect_err("above the 10-minute default ceiling");
        assert_spec_invalid(&error);

        let config = ExecConfig::new()
            .with_ceilings(ExecCeilings {
                default_timeout: Duration::from_secs(20 * 60),
                ..ExecCeilings::default()
            })
            .expect("raise the default timeout ceiling")
            .with_max_timeout(Duration::from_secs(20 * 60))
            .expect("raise max_timeout")
            .with_default_timeout(Duration::from_secs(15 * 60))
            .expect("raise the default after the max");
        assert_eq!(config.default_timeout(), Duration::from_secs(15 * 60));
        assert_eq!(config.max_timeout(), Duration::from_secs(20 * 60));
    }

    #[test]
    fn allow_unbounded_timeout_flips_only_the_flag() {
        let defaults = ExecConfig::new();
        let config = defaults.clone().with_allow_unbounded_timeout(true);

        assert!(config.allow_unbounded_timeout());
        assert_eq!(config.max_output_bytes(), defaults.max_output_bytes());
        assert_eq!(config.head_bytes(), defaults.head_bytes());
        assert_eq!(config.max_line_bytes(), defaults.max_line_bytes());
        assert_eq!(config.chunk_bytes(), defaults.chunk_bytes());
        assert_eq!(config.default_timeout(), defaults.default_timeout());
        assert_eq!(config.max_timeout(), defaults.max_timeout());
        assert_eq!(config.drain_timeout(), defaults.drain_timeout());
        assert_eq!(config.ceilings(), defaults.ceilings());
    }

    #[test]
    fn with_ceilings_rejects_every_ceiling_below_a_stored_value() {
        let cases = [
            (
                ExecConfig::new()
                    .with_max_output_bytes(48 * 1024 + 1)
                    .expect("valid"),
                ExecCeilings {
                    max_output_bytes: 48 * 1024,
                    ..ExecCeilings::default()
                },
            ),
            (
                ExecConfig::new().with_max_line_bytes(2_001).expect("valid"),
                ExecCeilings {
                    max_line_bytes: 2_000,
                    ..ExecCeilings::default()
                },
            ),
            (
                ExecConfig::new()
                    .with_chunk_bytes(8 * 1024 + 1)
                    .expect("valid"),
                ExecCeilings {
                    chunk_bytes: 8 * 1024,
                    ..ExecCeilings::default()
                },
            ),
            (
                ExecConfig::new()
                    .with_default_timeout(Duration::from_secs(31))
                    .expect("valid"),
                ExecCeilings {
                    default_timeout: Duration::from_secs(30),
                    max_timeout: Duration::from_secs(30),
                    ..ExecCeilings::default()
                },
            ),
            (
                ExecConfig::new()
                    .with_drain_timeout(Duration::from_secs(3))
                    .expect("valid"),
                ExecCeilings {
                    drain_timeout: Duration::from_secs(2),
                    ..ExecCeilings::default()
                },
            ),
        ];

        for (config, ceilings) in cases {
            let before = config.clone();
            let error = config
                .with_ceilings(ceilings)
                .expect_err("stored value is above the new ceiling");
            assert_spec_invalid(&error);
            assert_eq!(before.ceilings(), ExecCeilings::default());
        }
    }

    #[test]
    fn with_ceilings_revalidates_head_bytes_against_a_smaller_output_ceiling() {
        let config = ExecConfig::new()
            .with_max_output_bytes(16 * 1024)
            .expect("lower the output budget to the head size")
            .with_head_bytes(16 * 1024)
            .expect("head equals the output budget");
        let ceilings = ExecCeilings {
            max_output_bytes: 16 * 1024,
            ..ExecCeilings::default()
        };
        let config = config
            .with_ceilings(ceilings)
            .expect("head_bytes fits the new ceiling");

        assert_eq!(config.max_output_bytes(), 16 * 1024);
        assert_eq!(config.head_bytes(), 16 * 1024);
        let error = config
            .with_max_output_bytes(16 * 1024 - 1)
            .expect_err("below the stored head");
        assert_spec_invalid(&error);
    }

    #[test]
    fn with_ceilings_rejects_a_ceiling_below_its_floor() {
        let cases = [
            ExecCeilings {
                max_output_bytes: 319,
                ..ExecCeilings::default()
            },
            ExecCeilings {
                max_line_bytes: 31,
                ..ExecCeilings::default()
            },
            ExecCeilings {
                chunk_bytes: 255,
                ..ExecCeilings::default()
            },
            ExecCeilings {
                default_timeout: Duration::ZERO,
                ..ExecCeilings::default()
            },
            ExecCeilings {
                max_timeout: Duration::ZERO,
                ..ExecCeilings::default()
            },
            ExecCeilings {
                drain_timeout: Duration::ZERO,
                ..ExecCeilings::default()
            },
        ];

        for ceilings in cases {
            let error = ExecConfig::new()
                .with_ceilings(ceilings)
                .expect_err("ceiling below the floor");
            assert_spec_invalid(&error);
        }
    }

    #[test]
    fn with_ceilings_rejects_a_max_timeout_ceiling_below_the_default_ceiling() {
        let ceilings = ExecCeilings {
            default_timeout: Duration::from_secs(10),
            max_timeout: Duration::from_secs(9),
            ..ExecCeilings::default()
        };
        let error = ExecConfig::new()
            .with_ceilings(ceilings)
            .expect_err("the ceiling pair is out of order");
        assert_spec_invalid(&error);
    }

    #[test]
    fn with_ceilings_replaces_the_limit_for_later_setters() {
        let raised = ExecCeilings {
            chunk_bytes: 128 * 1024,
            ..ExecCeilings::default()
        };
        let config = ExecConfig::new()
            .with_ceilings(raised)
            .expect("raise the chunk ceiling");
        assert_eq!(config.ceilings().chunk_bytes, 128 * 1024);
        assert_eq!(
            config
                .clone()
                .with_chunk_bytes(128 * 1024)
                .expect("at the new ceiling")
                .chunk_bytes(),
            128 * 1024
        );

        let error = config
            .with_chunk_bytes(128 * 1024 + 1)
            .expect_err("above the new ceiling");
        assert_spec_invalid(&error);
    }

    #[test]
    fn with_ceilings_accepts_a_ceiling_equal_to_a_stored_value() {
        let config = ExecConfig::new().with_max_line_bytes(2_000).expect("valid");
        let ceilings = ExecCeilings {
            max_line_bytes: 2_000,
            ..ExecCeilings::default()
        };
        let config = config
            .with_ceilings(ceilings)
            .expect("a ceiling equal to the stored value is allowed");
        assert_eq!(config.ceilings().max_line_bytes, 2_000);

        let error = config
            .with_max_line_bytes(2_001)
            .expect_err("above the new ceiling");
        assert_spec_invalid(&error);
    }

    #[test]
    fn values_exactly_at_the_default_ceiling_are_accepted() {
        let config = ExecConfig::new()
            .with_max_output_bytes(1024 * 1024)
            .expect("output ceiling")
            .with_head_bytes(1024 * 1024)
            .expect("head may equal the output")
            .with_max_line_bytes(64 * 1024)
            .expect("line ceiling")
            .with_chunk_bytes(64 * 1024)
            .expect("chunk ceiling")
            .with_max_timeout(Duration::from_secs(24 * 60 * 60))
            .expect("max timeout ceiling")
            .with_default_timeout(Duration::from_secs(10 * 60))
            .expect("default timeout ceiling")
            .with_drain_timeout(Duration::from_secs(30))
            .expect("drain ceiling");

        assert_eq!(config.max_output_bytes(), 1024 * 1024);
        assert_eq!(config.head_bytes(), 1024 * 1024);
        assert_eq!(config.max_line_bytes(), 64 * 1024);
        assert_eq!(config.chunk_bytes(), 64 * 1024);
        assert_eq!(config.max_timeout(), Duration::from_secs(24 * 60 * 60));
        assert_eq!(config.default_timeout(), Duration::from_secs(10 * 60));
        assert_eq!(config.drain_timeout(), Duration::from_secs(30));
    }
}
