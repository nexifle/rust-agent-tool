//! Configuration for the write tool.
//!
//! [`WriteConfig`] holds the content byte budget, the NUL and empty content
//! policy, the omission-marker list, and the tool name and descriptions. Every
//! fallible setter rejects an unusable value instead of clamping it, so a host
//! learns about a bad setting when it builds the tool, not on the first call.
//!
//! [`WriteCeilings`] bounds what a setter may store. A host that needs a larger
//! content budget must say so with [`WriteConfig::with_ceilings`].

use std::fmt;

use crate::ToolError;

/// Lowest accepted `max_content_bytes` and the lowest accepted ceiling for it.
const MIN_MAX_CONTENT_BYTES: usize = 1;
/// Default `max_content_bytes`.
const DEFAULT_MAX_CONTENT_BYTES: usize = 1024 * 1024;
/// Default ceiling for `max_content_bytes`.
const DEFAULT_CEILING_MAX_CONTENT_BYTES: usize = 32 * 1024 * 1024;

/// Highest number of omission markers one config may hold.
const MAX_OMISSION_MARKERS: usize = 32;
/// Longest accepted omission marker, in UTF-8 bytes.
const MAX_OMISSION_MARKER_BYTES: usize = 128;

/// Default tool name.
const DEFAULT_NAME: &str = "write";

/// Default tool description.
const DEFAULT_DESCRIPTION: &str = "Create a file or replace its entire content. This tool does not append and does not patch: `content` is the full new file. A later write to the same path replaces the file again. Use the edit tool for partial changes.";

/// Default description of the `path` field.
const DEFAULT_PATH_DESCRIPTION: &str =
    "File path to create or overwrite. The host decides which paths are allowed.";

/// Default description of the `content` field.
const DEFAULT_CONTENT_DESCRIPTION: &str = "Full new content of the file. It replaces the existing content; it is not appended and it is not a patch.";

/// Highest value a [`WriteConfig`] setter may store.
///
/// The default matches the ceiling of [`WriteConfig::new`]. A host replaces it
/// with [`WriteConfig::with_ceilings`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WriteCeilings {
    /// Highest value [`WriteConfig::with_max_content_bytes`] may store.
    pub max_content_bytes: usize,
}

impl Default for WriteCeilings {
    fn default() -> Self {
        default_ceilings()
    }
}

/// Upper bounds for the knobs. These are not the default knob values.
const fn default_ceilings() -> WriteCeilings {
    WriteCeilings {
        max_content_bytes: DEFAULT_CEILING_MAX_CONTENT_BYTES,
    }
}

/// Configuration for one write tool.
///
/// Build a value with [`WriteConfig::new`] and change one knob at a time.
/// Setters take `self` by value and return the changed value, so a failing
/// setter drops the value it was called on. A caller that still needs the old
/// config clones before the call. Numeric setters fail when the value is below
/// its floor or above the active ceiling; they never clamp.
///
/// The tool name and descriptions are stored here so the write tool can build
/// its specification from this config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteConfig {
    max_content_bytes: usize,
    allow_nul: bool,
    allow_empty: bool,
    omission_markers: Vec<String>,
    name: String,
    description: String,
    path_description: String,
    content_description: String,
    ceilings: WriteCeilings,
}

impl WriteConfig {
    /// Builds the documented defaults.
    ///
    /// `max_content_bytes` is 1 MiB (`1024 * 1024`), NUL bytes are refused,
    /// empty content is allowed, and the omission-marker list is empty, which
    /// disables the marker check. The name is `write`, and the description
    /// says the tool creates or fully overwrites and is neither append nor
    /// patch. The ceilings are [`WriteCeilings::default`].
    #[must_use]
    pub fn new() -> Self {
        Self {
            max_content_bytes: DEFAULT_MAX_CONTENT_BYTES,
            allow_nul: false,
            allow_empty: true,
            omission_markers: Vec::new(),
            name: DEFAULT_NAME.to_string(),
            description: DEFAULT_DESCRIPTION.to_string(),
            path_description: DEFAULT_PATH_DESCRIPTION.to_string(),
            content_description: DEFAULT_CONTENT_DESCRIPTION.to_string(),
            ceilings: default_ceilings(),
        }
    }

    /// Maximum UTF-8 bytes of `content` one call accepts.
    #[must_use]
    pub const fn max_content_bytes(&self) -> usize {
        self.max_content_bytes
    }

    /// Whether NUL bytes inside `content` are allowed.
    #[must_use]
    pub const fn allow_nul(&self) -> bool {
        self.allow_nul
    }

    /// Whether empty `content` is allowed.
    ///
    /// Whitespace-only content is not empty.
    #[must_use]
    pub const fn allow_empty(&self) -> bool {
        self.allow_empty
    }

    /// Omission markers, in the order they were stored.
    ///
    /// Each marker is a case-sensitive substring. An empty list disables the
    /// marker check.
    #[must_use]
    pub fn omission_markers(&self) -> &[String] {
        &self.omission_markers
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

    /// Description of the `content` input field.
    #[must_use]
    pub fn content_description(&self) -> &str {
        &self.content_description
    }

    /// Copy of the ceilings in force.
    #[must_use]
    pub const fn ceilings(&self) -> WriteCeilings {
        self.ceilings
    }

    /// Replaces the maximum UTF-8 bytes of `content` one call accepts.
    ///
    /// # Errors
    ///
    /// Returns [`crate::ErrorCode::SpecInvalid`] when `max_content_bytes` is
    /// below 1 or above the active [`WriteCeilings::max_content_bytes`]. The
    /// value is not clamped.
    pub fn with_max_content_bytes(mut self, max_content_bytes: usize) -> Result<Self, ToolError> {
        check_knob(
            "max_content_bytes",
            max_content_bytes,
            MIN_MAX_CONTENT_BYTES,
            self.ceilings.max_content_bytes,
        )?;
        self.max_content_bytes = max_content_bytes;
        Ok(self)
    }

    /// Enables or disables NUL bytes inside `content`. Other knobs are
    /// unchanged.
    #[must_use]
    pub fn with_allow_nul(mut self, enabled: bool) -> Self {
        self.allow_nul = enabled;
        self
    }

    /// Enables or disables empty `content`. Whitespace-only content stays
    /// valid. Other knobs are unchanged.
    #[must_use]
    pub fn with_allow_empty(mut self, enabled: bool) -> Self {
        self.allow_empty = enabled;
        self
    }

    /// Replaces the omission-marker list.
    ///
    /// Matching is a case-sensitive substring search. Each marker is stored
    /// exactly as given, in the order given, and a marker that occurs in
    /// `content` rejects the write. An empty list disables the check.
    ///
    /// The list below shows the shape a host might pass. It is not a
    /// recommended set and it is not protection:
    ///
    /// - `rest of the file`
    /// - `rest of the code`
    /// - `remaining code unchanged`
    /// - `... existing code ...`
    ///
    /// Those substrings also match real source, comments, and string
    /// literals, so a host that passes them accepts false positives. The
    /// default remains an empty list, which disables the check.
    ///
    /// # Errors
    ///
    /// Returns [`crate::ErrorCode::SpecInvalid`] when the list holds more than
    /// 32 markers, when a marker is empty, or when a marker is longer than 128
    /// UTF-8 bytes. Nothing is stored on failure.
    pub fn with_omission_markers<I, S>(mut self, markers: I) -> Result<Self, ToolError>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let markers: Vec<String> = markers.into_iter().map(Into::into).collect();
        if markers.len() > MAX_OMISSION_MARKERS {
            return Err(ToolError::spec_invalid(format!(
                "omission markers must contain at most {MAX_OMISSION_MARKERS} entries, got {}",
                markers.len()
            )));
        }
        for (index, marker) in markers.iter().enumerate() {
            if marker.is_empty() {
                return Err(ToolError::spec_invalid(format!(
                    "omission marker at index {index} must not be empty"
                )));
            }
            if marker.len() > MAX_OMISSION_MARKER_BYTES {
                return Err(ToolError::spec_invalid(format!(
                    "omission marker at index {index} must be at most \
                     {MAX_OMISSION_MARKER_BYTES} bytes, got {}",
                    marker.len()
                )));
            }
        }
        self.omission_markers = markers;
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
    /// Only `path` and `content` are accepted. The description is trimmed
    /// before it is stored.
    ///
    /// # Errors
    ///
    /// Returns [`crate::ErrorCode::SpecInvalid`] when the description is empty
    /// after trimming or when `parameter` is not `path` or `content`.
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
            "content" => self.content_description = description,
            other => {
                return Err(ToolError::spec_invalid(format!(
                    "unknown write parameter: {other}"
                )));
            }
        }
        Ok(self)
    }

    /// Replaces the ceilings after proving every stored value still fits.
    ///
    /// Every new ceiling must also be at or above the floor of its knob.
    /// Raising a ceiling does not raise the stored budget: a host that wants a
    /// 16 MiB budget calls this method and then
    /// [`WriteConfig::with_max_content_bytes`].
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
    pub fn with_ceilings(mut self, ceilings: WriteCeilings) -> Result<Self, ToolError> {
        check_ceiling_floor(
            "max_content_bytes",
            ceilings.max_content_bytes,
            MIN_MAX_CONTENT_BYTES,
        )?;
        check_knob(
            "max_content_bytes",
            self.max_content_bytes,
            MIN_MAX_CONTENT_BYTES,
            ceilings.max_content_bytes,
        )?;
        self.ceilings = ceilings;
        Ok(self)
    }
}

impl Default for WriteConfig {
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

    fn assert_spec_invalid(error: &ToolError) {
        assert_eq!(error.code().as_str(), "AGT_SPEC_INVALID");
        assert!(
            error.to_string().starts_with("AGT_SPEC_INVALID:"),
            "unexpected display: {error}"
        );
    }

    #[test]
    fn new_uses_every_documented_default() {
        let config = WriteConfig::new();
        assert_eq!(config.max_content_bytes(), 1024 * 1024);
        assert!(!config.allow_nul());
        assert!(config.allow_empty());
        assert!(config.omission_markers().is_empty());
        assert_eq!(config.name(), "write");
        assert_eq!(config.description(), DEFAULT_DESCRIPTION);
        assert_eq!(config.path_description(), DEFAULT_PATH_DESCRIPTION);
        assert_eq!(config.content_description(), DEFAULT_CONTENT_DESCRIPTION);
        assert_eq!(config.ceilings(), WriteCeilings::default());
        assert_eq!(WriteConfig::default(), config);
    }

    #[test]
    fn the_default_description_names_overwrite_and_the_edit_tool() {
        let description = WriteConfig::new().description().to_string();
        assert!(description.contains("does not append"));
        assert!(description.contains("does not patch"));
        assert!(description.contains("replaces the file again"));
        assert!(description.contains("edit tool"));
    }

    #[test]
    fn default_ceilings_match_the_documented_maximum() {
        assert_eq!(WriteCeilings::default().max_content_bytes, 32 * 1024 * 1024);
    }

    #[test]
    fn max_content_bytes_of_zero_is_rejected_and_leaves_the_clone_unchanged() {
        let config = WriteConfig::new();
        let before = config.clone();
        let error = config
            .with_max_content_bytes(0)
            .expect_err("zero is below the floor");
        assert_spec_invalid(&error);
        assert_eq!(before.max_content_bytes(), 1024 * 1024);
        assert_eq!(before, WriteConfig::new());
    }

    #[test]
    fn max_content_bytes_above_the_active_ceiling_is_rejected() {
        let config = WriteConfig::new();
        let before = config.clone();
        let error = config
            .with_max_content_bytes(32 * 1024 * 1024 + 1)
            .expect_err("above the default ceiling");
        assert_spec_invalid(&error);
        assert_eq!(before.max_content_bytes(), 1024 * 1024);
    }

    #[test]
    fn max_content_bytes_at_the_floor_and_at_the_ceiling_are_accepted() {
        let config = WriteConfig::new()
            .with_max_content_bytes(1)
            .expect("the floor is accepted");
        assert_eq!(config.max_content_bytes(), 1);
        assert_eq!(
            config.max_content_bytes(),
            WriteConfig::new()
                .with_max_content_bytes(1)
                .expect("valid")
                .max_content_bytes()
        );

        let config = WriteConfig::new()
            .with_max_content_bytes(32 * 1024 * 1024)
            .expect("the ceiling is accepted");
        assert_eq!(config.max_content_bytes(), 32 * 1024 * 1024);
    }

    #[test]
    fn with_ceilings_rejects_a_ceiling_below_the_stored_budget() {
        let config = WriteConfig::new()
            .with_max_content_bytes(2 * 1024 * 1024)
            .expect("below the default ceiling");
        let before = config.clone();
        let error = config
            .with_ceilings(WriteCeilings {
                max_content_bytes: 1024 * 1024,
            })
            .expect_err("the stored budget is above the new ceiling");
        assert_spec_invalid(&error);
        assert_eq!(before.max_content_bytes(), 2 * 1024 * 1024);
        assert_eq!(
            before,
            WriteConfig::new()
                .with_max_content_bytes(2 * 1024 * 1024)
                .expect("valid")
        );
    }

    #[test]
    fn with_ceilings_rejects_a_ceiling_below_the_floor() {
        let config = WriteConfig::new();
        let before = config.clone();
        let error = config
            .with_ceilings(WriteCeilings {
                max_content_bytes: 0,
            })
            .expect_err("the ceiling floor is 1");
        assert_spec_invalid(&error);
        assert_eq!(before.ceilings(), WriteCeilings::default());
    }

    #[test]
    fn raising_the_ceiling_does_not_raise_the_stored_budget() {
        let config = WriteConfig::new()
            .with_ceilings(WriteCeilings {
                max_content_bytes: 16 * 1024 * 1024,
            })
            .expect("the stored budget fits the new ceiling");
        assert_eq!(config.max_content_bytes(), 1024 * 1024);

        let config = config
            .with_max_content_bytes(16 * 1024 * 1024)
            .expect("the raised ceiling accepts the new budget");
        assert_eq!(config.max_content_bytes(), 16 * 1024 * 1024);

        let error = config
            .with_max_content_bytes(16 * 1024 * 1024 + 1)
            .expect_err("above the raised ceiling");
        assert_spec_invalid(&error);
    }

    #[test]
    fn more_than_32_omission_markers_are_rejected_and_leave_the_clone_unchanged() {
        let config = WriteConfig::new();
        let before = config.clone();
        let markers: Vec<String> = (0..33).map(|index| format!("marker {index}")).collect();
        let error = config
            .with_omission_markers(markers)
            .expect_err("33 markers are above the limit");
        assert_spec_invalid(&error);
        assert!(before.omission_markers().is_empty());
    }

    #[test]
    fn an_empty_omission_marker_is_rejected_and_leaves_the_clone_unchanged() {
        let config = WriteConfig::new();
        let before = config.clone();
        let error = config
            .with_omission_markers(["fine", ""])
            .expect_err("an empty marker is unusable");
        assert_spec_invalid(&error);
        assert!(before.omission_markers().is_empty());
    }

    #[test]
    fn an_omission_marker_longer_than_128_bytes_is_rejected() {
        let config = WriteConfig::new();
        let before = config.clone();
        let long = "a".repeat(129);
        let error = config
            .with_omission_markers([long])
            .expect_err("129 bytes are above the marker limit");
        assert_spec_invalid(&error);
        assert!(before.omission_markers().is_empty());
    }

    #[test]
    fn omission_markers_at_the_limits_are_stored_as_given() {
        let longest = "a".repeat(128);
        let markers: Vec<String> = (0..31)
            .map(|index| format!("marker {index}"))
            .chain(std::iter::once(longest.clone()))
            .collect();
        let config = WriteConfig::new()
            .with_omission_markers(markers)
            .expect("32 markers of at most 128 bytes are accepted");
        assert_eq!(config.omission_markers().len(), 32);
        assert_eq!(config.omission_markers()[0], "marker 0");
        assert_eq!(config.omission_markers()[31], longest);
    }

    #[test]
    fn the_marker_length_limit_counts_utf8_bytes() {
        // Sixty-four characters, 128 UTF-8 bytes: accepted.
        let accepted = "é".repeat(64);
        assert_eq!(accepted.len(), 128);
        let config = WriteConfig::new()
            .with_omission_markers([accepted.clone()])
            .expect("128 bytes of UTF-8 are accepted");
        assert_eq!(config.omission_markers(), [accepted]);

        let rejected = "é".repeat(65);
        assert_eq!(rejected.len(), 130);
        let error = WriteConfig::new()
            .with_omission_markers([rejected])
            .expect_err("130 bytes are above the limit");
        assert_spec_invalid(&error);
    }

    #[test]
    fn the_empty_default_list_disables_the_marker_check() {
        assert!(WriteConfig::new().omission_markers().is_empty());
    }

    #[test]
    fn with_name_rejects_a_name_with_whitespace() {
        let config = WriteConfig::new();
        let before = config.clone();
        let error = config
            .with_name("write file")
            .expect_err("whitespace is not allowed in a tool name");
        assert_spec_invalid(&error);
        assert_eq!(before.name(), "write");
    }

    #[test]
    fn with_name_rejects_an_empty_name() {
        let error = WriteConfig::new()
            .with_name("")
            .expect_err("an empty name is unusable");
        assert_spec_invalid(&error);
    }

    #[test]
    fn with_name_stores_a_valid_name() {
        let config = WriteConfig::new()
            .with_name("write_file")
            .expect("a valid name");
        assert_eq!(config.name(), "write_file");
    }

    #[test]
    fn an_empty_description_is_rejected() {
        let config = WriteConfig::new();
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
    fn with_description_replaces_the_default_text() {
        let config = WriteConfig::new()
            .with_description("Rewrite one file.")
            .expect("a valid description");
        assert_eq!(config.description(), "Rewrite one file.");
        assert_ne!(config.description(), DEFAULT_DESCRIPTION);
        assert_eq!(config.path_description(), DEFAULT_PATH_DESCRIPTION);
        assert_eq!(config.content_description(), DEFAULT_CONTENT_DESCRIPTION);
    }

    #[test]
    fn with_description_trims_the_stored_text() {
        let config = WriteConfig::new()
            .with_description("  Rewrite one file.  ")
            .expect("a valid description");
        assert_eq!(config.description(), "Rewrite one file.");
    }

    #[test]
    fn with_parameter_description_rejects_an_unknown_parameter() {
        let config = WriteConfig::new();
        let before = config.clone();
        let error = config
            .with_parameter_description("mode", "How to write.")
            .expect_err("only path and content are known parameters");
        assert_spec_invalid(&error);
        assert_eq!(before.path_description(), DEFAULT_PATH_DESCRIPTION);
        assert_eq!(before.content_description(), DEFAULT_CONTENT_DESCRIPTION);
    }

    #[test]
    fn with_parameter_description_rejects_an_empty_description() {
        for parameter in ["path", "content"] {
            let error = WriteConfig::new()
                .with_parameter_description(parameter, "   ")
                .expect_err("an empty parameter description is unusable");
            assert_spec_invalid(&error);
        }
    }

    #[test]
    fn with_parameter_description_stores_path_and_content() {
        let config = WriteConfig::new()
            .with_parameter_description("path", " Target file. ")
            .expect("path is a known parameter")
            .with_parameter_description("content", "Full new text.")
            .expect("content is a known parameter");
        assert_eq!(config.path_description(), "Target file.");
        assert_eq!(config.content_description(), "Full new text.");
    }

    #[test]
    fn boolean_setters_flip_only_their_flag() {
        let defaults = WriteConfig::new();
        let config = defaults
            .clone()
            .with_allow_nul(true)
            .with_allow_empty(false);

        assert!(config.allow_nul());
        assert!(!config.allow_empty());
        assert_eq!(config.max_content_bytes(), defaults.max_content_bytes());
        assert_eq!(config.omission_markers(), defaults.omission_markers());
        assert_eq!(config.name(), defaults.name());
        assert_eq!(config.description(), defaults.description());
        assert_eq!(config.ceilings(), defaults.ceilings());
    }

    #[test]
    fn a_failed_setter_leaves_an_earlier_clone_usable() {
        let config = WriteConfig::new()
            .with_allow_nul(true)
            .with_omission_markers(["rest of the file"])
            .expect("valid markers");
        let before = config.clone();

        let error = config
            .with_max_content_bytes(0)
            .expect_err("zero is below the floor");
        assert_spec_invalid(&error);

        assert_eq!(
            before,
            WriteConfig::new()
                .with_allow_nul(true)
                .with_omission_markers(["rest of the file"])
                .expect("valid markers")
        );
    }
}
