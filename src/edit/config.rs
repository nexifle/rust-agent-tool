//! Configuration and matching policy for the edit tool.
//!
//! [`EditConfig`] holds the byte budgets, the matching tier and line-ending
//! policy, the NUL policy, the fuzzy threshold, and the tool name and
//! descriptions. Every fallible setter rejects an unusable value instead of
//! clamping it, so a host learns about a bad setting when it builds the tool,
//! not on the first call.
//!
//! [`EditCeilings`] bounds what a setter may store. A host that needs a larger
//! budget must say so with [`EditConfig::with_ceilings`].
//!
//! [`MatchMode`] selects which matching tiers are enabled and
//! [`NewlinePolicy`] selects the line endings written into a replaced region.
//! Both default to the conservative choice, so a host that sets nothing gets
//! byte-exact matching and keeps the endings the file already had.

use std::fmt;

use crate::ToolError;

/// Lowest accepted value of every byte and count knob, and the lowest accepted
/// ceiling for one.
const MIN_KNOB: usize = 1;

/// Default `max_file_bytes`: 2 MiB.
const DEFAULT_MAX_FILE_BYTES: usize = 2 * 1024 * 1024;
/// Default ceiling for `max_file_bytes`.
const DEFAULT_CEILING_MAX_FILE_BYTES: usize = 32 * 1024 * 1024;

/// Default `max_old_text_bytes`: 256 KiB.
const DEFAULT_MAX_OLD_TEXT_BYTES: usize = 256 * 1024;
/// Default ceiling for `max_old_text_bytes`.
const DEFAULT_CEILING_MAX_OLD_TEXT_BYTES: usize = 4 * 1024 * 1024;

/// Default `max_occurrences`.
const DEFAULT_MAX_OCCURRENCES: usize = 10_000;
/// Default ceiling for `max_occurrences`.
const DEFAULT_CEILING_MAX_OCCURRENCES: usize = 1_000_000;

/// Lowest accepted `fuzzy_threshold`.
///
/// The threshold has no ceiling field: it is a similarity score, so its bounds
/// are fixed here rather than exposed as a knob a host could widen.
const MIN_FUZZY_THRESHOLD: f32 = 0.5;
/// Highest accepted `fuzzy_threshold`.
const MAX_FUZZY_THRESHOLD: f32 = 1.0;
/// Default `fuzzy_threshold`.
const DEFAULT_FUZZY_THRESHOLD: f32 = 0.95;

/// Default tool name.
const DEFAULT_NAME: &str = "edit";

/// Default tool description.
const DEFAULT_DESCRIPTION: &str = "Replace one exact region of an existing file. `old_text` must occur exactly once unless `replace_all` is true. This tool does not create a file; use the write tool for a new file and for a whole-file rewrite.";

/// Default description of the `path` field.
const DEFAULT_PATH_DESCRIPTION: &str =
    "Path of the file to edit. Relative paths resolve against the workspace root the host uses.";

/// Default description of the `old_text` field.
const DEFAULT_OLD_TEXT_DESCRIPTION: &str = "Exact text to find, copied from the file, including indentation. Add surrounding lines until it occurs once.";

/// Default description of the `new_text` field.
const DEFAULT_NEW_TEXT_DESCRIPTION: &str = "Text that replaces `old_text`. An empty string deletes the region. Indentation is preserved relative to the matched line.";

/// Default description of the `replace_all` field.
///
/// Occurrence counting is left-to-right and non-overlapping, so two adjacent
/// hits of one `old_text` are two occurrences. A fuzzy match still replaces one
/// window only, whatever this flag says: a window has no stable occurrence
/// identity, so the flag just decides that one replacement is acceptable.
const DEFAULT_REPLACE_ALL_DESCRIPTION: &str =
    "Replace every occurrence of `old_text` instead of requiring exactly one. Defaults to false.";

/// Highest value an [`EditConfig`] setter may store.
///
/// The default matches the ceiling of [`EditConfig::new`]. A host replaces it
/// with [`EditConfig::with_ceilings`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EditCeilings {
    /// Highest value [`EditConfig::with_max_file_bytes`] may store.
    pub max_file_bytes: usize,
    /// Highest value [`EditConfig::with_max_old_text_bytes`] may store.
    pub max_old_text_bytes: usize,
    /// Highest value [`EditConfig::with_max_occurrences`] may store.
    pub max_occurrences: usize,
}

impl Default for EditCeilings {
    fn default() -> Self {
        default_ceilings()
    }
}

/// Upper bounds for the knobs. These are not the default knob values.
const fn default_ceilings() -> EditCeilings {
    EditCeilings {
        max_file_bytes: DEFAULT_CEILING_MAX_FILE_BYTES,
        max_old_text_bytes: DEFAULT_CEILING_MAX_OLD_TEXT_BYTES,
        max_occurrences: DEFAULT_CEILING_MAX_OCCURRENCES,
    }
}

/// Configuration for one edit tool.
///
/// Build a value with [`EditConfig::new`] and change one knob at a time.
/// Setters take `self` by value and return the changed value, so a failing
/// setter drops the value it was called on. A caller that still needs the old
/// config clones before the call. Numeric setters fail when the value is below
/// its floor or above the active ceiling; they never clamp.
///
/// The tool name and descriptions are stored here so the edit tool can build
/// its specification from this config.
#[derive(Debug, Clone, PartialEq)]
pub struct EditConfig {
    max_file_bytes: usize,
    max_old_text_bytes: usize,
    max_occurrences: usize,
    match_mode: MatchMode,
    allow_nul: bool,
    newline: NewlinePolicy,
    fuzzy_threshold: f32,
    name: String,
    description: String,
    path_description: String,
    old_text_description: String,
    new_text_description: String,
    replace_all_description: String,
    ceilings: EditCeilings,
}

impl EditConfig {
    /// Builds the documented defaults.
    ///
    /// `max_file_bytes` is 2 MiB, `max_old_text_bytes` is 256 KiB,
    /// `max_occurrences` is 10 000, the matching tier is [`MatchMode::Exact`],
    /// NUL bytes are refused, the line-ending policy is
    /// [`NewlinePolicy::Preserve`], and the fuzzy threshold is `0.95`. The name
    /// is `edit`. The ceilings are [`EditCeilings::default`].
    #[must_use]
    pub fn new() -> Self {
        Self {
            max_file_bytes: DEFAULT_MAX_FILE_BYTES,
            max_old_text_bytes: DEFAULT_MAX_OLD_TEXT_BYTES,
            max_occurrences: DEFAULT_MAX_OCCURRENCES,
            match_mode: MatchMode::Exact,
            allow_nul: false,
            newline: NewlinePolicy::Preserve,
            fuzzy_threshold: DEFAULT_FUZZY_THRESHOLD,
            name: DEFAULT_NAME.to_string(),
            description: DEFAULT_DESCRIPTION.to_string(),
            path_description: DEFAULT_PATH_DESCRIPTION.to_string(),
            old_text_description: DEFAULT_OLD_TEXT_DESCRIPTION.to_string(),
            new_text_description: DEFAULT_NEW_TEXT_DESCRIPTION.to_string(),
            replace_all_description: DEFAULT_REPLACE_ALL_DESCRIPTION.to_string(),
            ceilings: default_ceilings(),
        }
    }

    /// Largest file the tool accepts from `open`, and the largest final file.
    #[must_use]
    pub const fn max_file_bytes(&self) -> usize {
        self.max_file_bytes
    }

    /// Largest accepted `old_text`, in UTF-8 bytes.
    #[must_use]
    pub const fn max_old_text_bytes(&self) -> usize {
        self.max_old_text_bytes
    }

    /// Largest `replace_all` fan-out one call accepts.
    ///
    /// Ignored when `replace_all` is false, where counting stops at the number
    /// of line numbers a failure message can show. Occurrence counting is
    /// left-to-right and non-overlapping, so two adjacent hits of one `old_text`
    /// count as two. The fuzzy tier never fans out, whatever this says.
    #[must_use]
    pub const fn max_occurrences(&self) -> usize {
        self.max_occurrences
    }

    /// Which matching tiers may run.
    #[must_use]
    pub const fn match_mode(&self) -> MatchMode {
        self.match_mode
    }

    /// Whether NUL bytes are allowed in the file, in `old_text`, and in
    /// `new_text`.
    #[must_use]
    pub const fn allow_nul(&self) -> bool {
        self.allow_nul
    }

    /// Line endings written into a replaced region.
    #[must_use]
    pub const fn newline(&self) -> NewlinePolicy {
        self.newline
    }

    /// Minimum similarity the fuzzy tier accepts.
    ///
    /// Ignored unless [`EditConfig::match_mode`] is [`MatchMode::Fuzzy`].
    #[must_use]
    pub const fn fuzzy_threshold(&self) -> f32 {
        self.fuzzy_threshold
    }

    /// Tool name shown to the model.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// What the tool does, shown to the model.
    #[must_use]
    pub fn description(&self) -> &str {
        &self.description
    }

    /// Description of the `path` input field.
    #[must_use]
    pub fn path_description(&self) -> &str {
        &self.path_description
    }

    /// Description of the `old_text` input field.
    #[must_use]
    pub fn old_text_description(&self) -> &str {
        &self.old_text_description
    }

    /// Description of the `new_text` input field.
    #[must_use]
    pub fn new_text_description(&self) -> &str {
        &self.new_text_description
    }

    /// Description of the `replace_all` input field.
    #[must_use]
    pub fn replace_all_description(&self) -> &str {
        &self.replace_all_description
    }

    /// Copy of the ceilings in force.
    #[must_use]
    pub const fn ceilings(&self) -> EditCeilings {
        self.ceilings
    }

    /// Replaces the largest file the tool accepts or writes.
    ///
    /// # Errors
    ///
    /// Returns [`crate::ErrorCode::SpecInvalid`] when `max_file_bytes` is below
    /// 1 or above the active [`EditCeilings::max_file_bytes`]. The value is not
    /// clamped.
    pub fn with_max_file_bytes(mut self, max_file_bytes: usize) -> Result<Self, ToolError> {
        check_knob(
            "max_file_bytes",
            max_file_bytes,
            MIN_KNOB,
            self.ceilings.max_file_bytes,
        )?;
        self.max_file_bytes = max_file_bytes;
        Ok(self)
    }

    /// Replaces the largest accepted `old_text`.
    ///
    /// # Errors
    ///
    /// Returns [`crate::ErrorCode::SpecInvalid`] when `max_old_text_bytes` is
    /// below 1 or above the active [`EditCeilings::max_old_text_bytes`]. The
    /// value is not clamped.
    pub fn with_max_old_text_bytes(mut self, max_old_text_bytes: usize) -> Result<Self, ToolError> {
        check_knob(
            "max_old_text_bytes",
            max_old_text_bytes,
            MIN_KNOB,
            self.ceilings.max_old_text_bytes,
        )?;
        self.max_old_text_bytes = max_old_text_bytes;
        Ok(self)
    }

    /// Replaces the largest `replace_all` fan-out one call accepts.
    ///
    /// # Errors
    ///
    /// Returns [`crate::ErrorCode::SpecInvalid`] when `max_occurrences` is below
    /// 1 or above the active [`EditCeilings::max_occurrences`]. The value is not
    /// clamped.
    pub fn with_max_occurrences(mut self, max_occurrences: usize) -> Result<Self, ToolError> {
        check_knob(
            "max_occurrences",
            max_occurrences,
            MIN_KNOB,
            self.ceilings.max_occurrences,
        )?;
        self.max_occurrences = max_occurrences;
        Ok(self)
    }

    /// Selects which matching tiers may run. Other knobs are unchanged.
    #[must_use]
    pub fn with_match_mode(mut self, mode: MatchMode) -> Self {
        self.match_mode = mode;
        self
    }

    /// Enables or disables NUL bytes in the file and in both text fields.
    /// Other knobs are unchanged.
    #[must_use]
    pub fn with_allow_nul(mut self, enabled: bool) -> Self {
        self.allow_nul = enabled;
        self
    }

    /// Replaces the line-ending policy applied to the replaced region. Other
    /// knobs are unchanged.
    #[must_use]
    pub fn with_newline(mut self, newline: NewlinePolicy) -> Self {
        self.newline = newline;
        self
    }

    /// Replaces the minimum similarity the fuzzy tier accepts.
    ///
    /// # Errors
    ///
    /// Returns [`crate::ErrorCode::SpecInvalid`] when `fuzzy_threshold` is not
    /// finite, is below `0.5`, or is above `1.0`. The value is not clamped.
    pub fn with_fuzzy_threshold(mut self, fuzzy_threshold: f32) -> Result<Self, ToolError> {
        if !fuzzy_threshold.is_finite()
            || !(MIN_FUZZY_THRESHOLD..=MAX_FUZZY_THRESHOLD).contains(&fuzzy_threshold)
        {
            return Err(ToolError::spec_invalid(format!(
                "field \"fuzzy_threshold\" must be a finite number between \
                 {MIN_FUZZY_THRESHOLD} and {MAX_FUZZY_THRESHOLD}, got {fuzzy_threshold}"
            )));
        }
        self.fuzzy_threshold = fuzzy_threshold;
        Ok(self)
    }

    /// Replaces the tool name.
    ///
    /// The rules are the same as [`crate::ToolSpec::new`]: the name must be
    /// non-empty and must not contain whitespace.
    ///
    /// # Errors
    ///
    /// Returns [`crate::ErrorCode::SpecInvalid`] when the name is empty or
    /// contains whitespace.
    pub fn with_name(mut self, name: impl Into<String>) -> Result<Self, ToolError> {
        let name = name.into();
        if name.is_empty() || name.chars().any(char::is_whitespace) {
            return Err(ToolError::spec_invalid(
                "tool name must be non-empty and must not contain whitespace",
            ));
        }
        self.name = name;
        Ok(self)
    }

    /// Replaces the whole tool description.
    ///
    /// The description is trimmed before it is stored. It does not affect the
    /// parameter descriptions.
    ///
    /// # Errors
    ///
    /// Returns [`crate::ErrorCode::SpecInvalid`] when the description is empty
    /// after trimming.
    pub fn with_description(mut self, description: impl Into<String>) -> Result<Self, ToolError> {
        let description = description.into();
        let description = description.trim();
        if description.is_empty() {
            return Err(ToolError::spec_invalid(
                "tool description must not be empty",
            ));
        }
        self.description = description.to_string();
        Ok(self)
    }

    /// Replaces the description of one input field.
    ///
    /// Only `path`, `old_text`, `new_text`, and `replace_all` are accepted. The
    /// description is trimmed before it is stored.
    ///
    /// # Errors
    ///
    /// Returns [`crate::ErrorCode::SpecInvalid`] when the description is empty
    /// after trimming or when `parameter` is not one of the four field names.
    pub fn with_parameter_description(
        mut self,
        parameter: &str,
        description: impl Into<String>,
    ) -> Result<Self, ToolError> {
        let description = description.into();
        let description = description.trim();
        if description.is_empty() {
            return Err(ToolError::spec_invalid(
                "parameter description must not be empty",
            ));
        }
        let description = description.to_string();
        match parameter {
            "path" => self.path_description = description,
            "old_text" => self.old_text_description = description,
            "new_text" => self.new_text_description = description,
            "replace_all" => self.replace_all_description = description,
            other => {
                return Err(ToolError::spec_invalid(format!(
                    "unknown edit parameter: {other}"
                )));
            }
        }
        Ok(self)
    }

    /// Replaces the ceilings after proving every stored value still fits.
    ///
    /// Every new ceiling must also be at or above the floor of its knob.
    /// Raising a ceiling does not raise a stored budget: a host that wants a
    /// larger budget calls this method and then the matching setter.
    ///
    /// # Errors
    ///
    /// Returns [`crate::ErrorCode::SpecInvalid`] when a ceiling is below its
    /// floor or when a stored knob is above its new ceiling. Nothing is
    /// changed: the checks run before the new ceilings are stored, so the
    /// returned value is dropped without a partial update. Because the method
    /// consumes `self`, the caller keeps using the previous config only if it
    /// did not move that value into this call.
    pub fn with_ceilings(mut self, ceilings: EditCeilings) -> Result<Self, ToolError> {
        check_ceiling_floor("max_file_bytes", ceilings.max_file_bytes, MIN_KNOB)?;
        check_ceiling_floor("max_old_text_bytes", ceilings.max_old_text_bytes, MIN_KNOB)?;
        check_ceiling_floor("max_occurrences", ceilings.max_occurrences, MIN_KNOB)?;
        check_knob(
            "max_file_bytes",
            self.max_file_bytes,
            MIN_KNOB,
            ceilings.max_file_bytes,
        )?;
        check_knob(
            "max_old_text_bytes",
            self.max_old_text_bytes,
            MIN_KNOB,
            ceilings.max_old_text_bytes,
        )?;
        check_knob(
            "max_occurrences",
            self.max_occurrences,
            MIN_KNOB,
            ceilings.max_occurrences,
        )?;
        self.ceilings = ceilings;
        Ok(self)
    }
}

impl Default for EditConfig {
    fn default() -> Self {
        Self::new()
    }
}

/// Which matching tiers the edit tool may use.
///
/// The tiers run in order and the first tier that finds a match wins. Every
/// tier after [`MatchMode::Exact`] is opt-in, because a match that is not
/// byte-exact can land the replacement in the wrong place.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MatchMode {
    /// Byte-for-byte matching only. This is the default.
    ///
    /// Two passes run: a raw pass over the original text when the needle itself
    /// contains a `\r`, then a pass over the file text with every line ending
    /// normalized to `\n`, whose hits are mapped back to the original offsets.
    /// No character is folded, no whitespace is trimmed, and no window slides,
    /// so a match is always a substring of the file or of its LF view.
    #[default]
    Exact,
    /// Also accepts per-line trailing-whitespace differences and folded Unicode
    /// punctuation.
    ///
    /// Leading whitespace is never normalized, so a match always starts at the
    /// indentation the file has. The lenient tier runs only where the exact tier
    /// found nothing.
    Lenient,
    /// Also accepts a near match scored by string similarity.
    ///
    /// A fuzzy match is only used when the cheaper tiers miss, and it reports
    /// its own tier so a host can tell the model to re-read the file. It never
    /// fans out past one window, whatever `replace_all` says.
    Fuzzy,
}

/// Line-ending policy for the text that replaces a matched region.
///
/// This is not the write tool's [`crate::Newline`]. [`crate::Newline::AsIs`]
/// means "write the bytes as sent", which is right for a whole-file write and
/// wrong for a replacement: a model that sends `\n` into a CRLF file would leave
/// one region mixing two endings. [`NewlinePolicy::Preserve`] has no equivalent
/// on the write side, and it is the default here.
///
/// Both types feed the same conversion table in `crate::text::normalize_newlines`,
/// so the crate has one line-ending implementation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum NewlinePolicy {
    /// Use the ending found inside the replaced region, and fall back to the
    /// file's dominant ending when the region holds none. This is the default.
    ///
    /// The choice is made per region, so a CRLF region in a mostly-LF file stays
    /// CRLF and an LF region in the same file stays LF.
    #[default]
    Preserve,
    /// End every line of the replacement with `\n`.
    Lf,
    /// End every line of the replacement with `\r\n`.
    CrLf,
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
    use super::{
        DEFAULT_DESCRIPTION, DEFAULT_NEW_TEXT_DESCRIPTION, DEFAULT_OLD_TEXT_DESCRIPTION,
        DEFAULT_PATH_DESCRIPTION, DEFAULT_REPLACE_ALL_DESCRIPTION, EditCeilings, EditConfig,
        MatchMode, NewlinePolicy,
    };
    use crate::ToolError;

    fn assert_spec_invalid(error: &ToolError) {
        assert_eq!(error.code().as_str(), "AGT_SPEC_INVALID");
        assert!(
            error.to_string().starts_with("AGT_SPEC_INVALID:"),
            "unexpected display: {error}"
        );
    }

    #[test]
    fn new_uses_every_documented_default() {
        let config = EditConfig::new();
        assert_eq!(config.max_file_bytes(), 2 * 1024 * 1024);
        assert_eq!(config.max_old_text_bytes(), 256 * 1024);
        assert_eq!(config.max_occurrences(), 10_000);
        assert_eq!(config.match_mode(), MatchMode::Exact);
        assert!(!config.allow_nul());
        assert_eq!(config.newline(), NewlinePolicy::Preserve);
        assert_eq!(config.fuzzy_threshold(), 0.95);
        assert_eq!(config.name(), "edit");
        assert_eq!(config.description(), DEFAULT_DESCRIPTION);
        assert_eq!(config.path_description(), DEFAULT_PATH_DESCRIPTION);
        assert_eq!(config.old_text_description(), DEFAULT_OLD_TEXT_DESCRIPTION);
        assert_eq!(config.new_text_description(), DEFAULT_NEW_TEXT_DESCRIPTION);
        assert_eq!(
            config.replace_all_description(),
            DEFAULT_REPLACE_ALL_DESCRIPTION
        );
        assert_eq!(config.ceilings(), EditCeilings::default());
        assert_eq!(EditConfig::default(), config);
    }

    #[test]
    fn the_default_ceilings_are_the_documented_maximums() {
        let ceilings = EditCeilings::default();
        assert_eq!(ceilings.max_file_bytes, 32 * 1024 * 1024);
        assert_eq!(ceilings.max_old_text_bytes, 4 * 1024 * 1024);
        assert_eq!(ceilings.max_occurrences, 1_000_000);
    }

    #[test]
    fn every_numeric_setter_rejects_zero_and_leaves_the_clone_unchanged() {
        let config = EditConfig::new();
        let before = config.clone();

        let errors = [
            config
                .clone()
                .with_max_file_bytes(0)
                .expect_err("zero is below the floor"),
            config
                .clone()
                .with_max_old_text_bytes(0)
                .expect_err("zero is below the floor"),
            config
                .clone()
                .with_max_occurrences(0)
                .expect_err("zero is below the floor"),
        ];
        for error in &errors {
            assert_spec_invalid(error);
        }

        assert_eq!(before, EditConfig::new());
    }

    #[test]
    fn every_numeric_setter_rejects_a_value_above_the_active_ceiling() {
        let config = EditConfig::new();
        let before = config.clone();

        let errors = [
            config
                .clone()
                .with_max_file_bytes(32 * 1024 * 1024 + 1)
                .expect_err("above the default ceiling"),
            config
                .clone()
                .with_max_old_text_bytes(4 * 1024 * 1024 + 1)
                .expect_err("above the default ceiling"),
            config
                .clone()
                .with_max_occurrences(1_000_000 + 1)
                .expect_err("above the default ceiling"),
        ];
        for error in &errors {
            assert_spec_invalid(error);
        }

        assert_eq!(before, EditConfig::new());
    }

    #[test]
    fn numeric_setters_accept_their_floor_and_their_ceiling() {
        let config = EditConfig::new()
            .with_max_file_bytes(1)
            .expect("the floor is accepted")
            .with_max_old_text_bytes(1)
            .expect("the floor is accepted")
            .with_max_occurrences(1)
            .expect("the floor is accepted");
        assert_eq!(config.max_file_bytes(), 1);
        assert_eq!(config.max_old_text_bytes(), 1);
        assert_eq!(config.max_occurrences(), 1);

        let ceilings = EditCeilings::default();
        let config = EditConfig::new()
            .with_max_file_bytes(ceilings.max_file_bytes)
            .expect("the ceiling is accepted")
            .with_max_old_text_bytes(ceilings.max_old_text_bytes)
            .expect("the ceiling is accepted")
            .with_max_occurrences(ceilings.max_occurrences)
            .expect("the ceiling is accepted");
        assert_eq!(config.max_file_bytes(), ceilings.max_file_bytes);
        assert_eq!(config.max_old_text_bytes(), ceilings.max_old_text_bytes);
        assert_eq!(config.max_occurrences(), ceilings.max_occurrences);
    }

    #[test]
    fn the_fuzzy_threshold_rejects_scores_outside_its_fixed_bounds() {
        let config = EditConfig::new();
        let before = config.clone();

        let rejected = [0.0, 0.4999, 1.0 + f32::EPSILON, f32::NAN, f32::INFINITY];
        for threshold in rejected {
            let error = config
                .clone()
                .with_fuzzy_threshold(threshold)
                .expect_err("outside the fixed bounds");
            assert_spec_invalid(&error);
        }
        assert_eq!(before.fuzzy_threshold(), 0.95);

        for threshold in [0.5, 0.75, 1.0] {
            let config = EditConfig::new()
                .with_fuzzy_threshold(threshold)
                .expect("inside the fixed bounds");
            assert_eq!(config.fuzzy_threshold(), threshold);
        }
    }

    #[test]
    fn with_ceilings_rejects_a_ceiling_below_a_stored_knob() {
        let config = EditConfig::new()
            .with_max_file_bytes(2 * 1024 * 1024)
            .expect("below the default ceiling");
        let before = config.clone();
        let error = config
            .with_ceilings(EditCeilings {
                max_file_bytes: 1024 * 1024,
                ..EditCeilings::default()
            })
            .expect_err("the stored budget is above the new ceiling");
        assert_spec_invalid(&error);
        assert_eq!(before.max_file_bytes(), 2 * 1024 * 1024);
    }

    #[test]
    fn with_ceilings_rejects_a_ceiling_below_its_floor() {
        let config = EditConfig::new();
        let before = config.clone();

        let errors = [
            config
                .clone()
                .with_ceilings(EditCeilings {
                    max_file_bytes: 0,
                    ..EditCeilings::default()
                })
                .expect_err("the ceiling floor is 1"),
            config
                .clone()
                .with_ceilings(EditCeilings {
                    max_old_text_bytes: 0,
                    ..EditCeilings::default()
                })
                .expect_err("the ceiling floor is 1"),
            config
                .clone()
                .with_ceilings(EditCeilings {
                    max_occurrences: 0,
                    ..EditCeilings::default()
                })
                .expect_err("the ceiling floor is 1"),
        ];
        for error in &errors {
            assert_spec_invalid(error);
        }

        assert_eq!(before.ceilings(), EditCeilings::default());
    }

    #[test]
    fn raising_a_ceiling_does_not_raise_the_stored_budget() {
        let config = EditConfig::new()
            .with_ceilings(EditCeilings {
                max_file_bytes: 16 * 1024 * 1024,
                ..EditCeilings::default()
            })
            .expect("the stored budget fits the new ceiling");
        assert_eq!(config.max_file_bytes(), 2 * 1024 * 1024);

        let config = config
            .with_max_file_bytes(16 * 1024 * 1024)
            .expect("the raised ceiling accepts the new budget");
        assert_eq!(config.max_file_bytes(), 16 * 1024 * 1024);

        let error = config
            .clone()
            .with_max_file_bytes(16 * 1024 * 1024 + 1)
            .expect_err("above the raised ceiling");
        assert_spec_invalid(&error);
        assert_eq!(config.max_file_bytes(), 16 * 1024 * 1024);
    }

    #[test]
    fn with_parameter_description_rejects_an_unknown_parameter() {
        let config = EditConfig::new();
        let before = config.clone();
        for parameter in ["content", "mode", ""] {
            let error = config
                .clone()
                .with_parameter_description(parameter, "Text.")
                .expect_err("only the four edit fields are known parameters");
            assert_spec_invalid(&error);
        }
        assert_eq!(before.path_description(), DEFAULT_PATH_DESCRIPTION);
        assert_eq!(before.old_text_description(), DEFAULT_OLD_TEXT_DESCRIPTION);
    }

    #[test]
    fn with_parameter_description_stores_every_edit_field() {
        let config = EditConfig::new()
            .with_parameter_description("path", " Target file. ")
            .expect("path is a known parameter")
            .with_parameter_description("old_text", "Text to find.")
            .expect("old_text is a known parameter")
            .with_parameter_description("new_text", "Text to write.")
            .expect("new_text is a known parameter")
            .with_parameter_description("replace_all", "Every occurrence.")
            .expect("replace_all is a known parameter");

        assert_eq!(config.path_description(), "Target file.");
        assert_eq!(config.old_text_description(), "Text to find.");
        assert_eq!(config.new_text_description(), "Text to write.");
        assert_eq!(config.replace_all_description(), "Every occurrence.");
        assert_eq!(config.description(), DEFAULT_DESCRIPTION);
    }

    #[test]
    fn an_empty_parameter_description_is_rejected() {
        for parameter in ["path", "old_text", "new_text", "replace_all"] {
            let error = EditConfig::new()
                .with_parameter_description(parameter, "   ")
                .expect_err("an empty parameter description is unusable");
            assert_spec_invalid(&error);
        }
    }

    #[test]
    fn an_empty_description_is_rejected() {
        let config = EditConfig::new();
        let before = config.clone();
        for description in ["", "   "] {
            let error = config
                .clone()
                .with_description(description)
                .expect_err("an empty description is unusable");
            assert_spec_invalid(&error);
        }
        assert_eq!(before.description(), DEFAULT_DESCRIPTION);
    }

    #[test]
    fn with_name_uses_the_spec_name_rules() {
        assert_spec_invalid(
            &EditConfig::new()
                .with_name("edit file")
                .expect_err("whitespace is not allowed in a tool name"),
        );
        assert_spec_invalid(
            &EditConfig::new()
                .with_name("")
                .expect_err("an empty name is unusable"),
        );
        let config = EditConfig::new()
            .with_name("edit_file")
            .expect("a valid name");
        assert_eq!(config.name(), "edit_file");
    }

    #[test]
    fn boolean_and_policy_setters_flip_only_their_knob() {
        let defaults = EditConfig::new();
        let config = defaults
            .clone()
            .with_allow_nul(true)
            .with_match_mode(MatchMode::Fuzzy)
            .with_newline(NewlinePolicy::CrLf);

        assert!(config.allow_nul());
        assert_eq!(config.match_mode(), MatchMode::Fuzzy);
        assert_eq!(config.newline(), NewlinePolicy::CrLf);
        assert_eq!(config.max_file_bytes(), defaults.max_file_bytes());
        assert_eq!(config.max_old_text_bytes(), defaults.max_old_text_bytes());
        assert_eq!(config.max_occurrences(), defaults.max_occurrences());
        assert_eq!(config.fuzzy_threshold(), defaults.fuzzy_threshold());
        assert_eq!(config.name(), defaults.name());
        assert_eq!(config.ceilings(), defaults.ceilings());
    }
}
