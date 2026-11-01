//! Model input for the read tool.
//!
//! [`parse_input`] turns the JSON a model sends into a [`ReadInput`], and
//! [`input_schema`] builds the JSON Schema a host registers for the same tool.
//! The shape is locked: a required `path` string, an optional 1-based `offset`,
//! and an optional `limit`. Unknown fields are rejected instead of ignored, so
//! a model that invents a field hears which field it invented.
//!
//! The parser is stricter than the schema in one place: a `path` that is blank
//! or padded with whitespace is rejected by [`parse_input`]. `minLength` cannot
//! express that rule, so it stays parser-only.

use serde_json::{Value, json};

use super::config::ReadConfig;
use crate::ToolError;

/// Default tool description for the read tool.
pub const DEFAULT_TOOL_DESCRIPTION: &str = "Read a file the host allows. `path` is the file path. `offset` is the first 1-based line and defaults to 1. `limit` is how many lines to return and defaults to the configured maximum. Directories and binary files fail. Images are returned as image bytes, not as text. A footer that says more lines exist means the rest of the file was not returned.";

/// Default description of the `path` field.
pub const DEFAULT_PATH_DESCRIPTION: &str =
    "File path to read. The host decides which paths are allowed.";

/// Default description of the `offset` field.
pub const DEFAULT_OFFSET_DESCRIPTION: &str =
    "First line to return. Lines are numbered from 1. Omit to start at line 1.";

/// Default description of the `limit` field.
pub const DEFAULT_LIMIT_DESCRIPTION: &str =
    "Maximum number of lines to return. Omit to use the configured maximum.";

/// Builds the JSON Schema for the read tool input.
///
/// The shape is locked: `type` is `object`, `additionalProperties` is `false`,
/// `path` is the only required field, and `offset` and `limit` are integers
/// with a minimum of 1. Each argument replaces the `description` string of the
/// field with the same name. Nothing else in the schema changes.
#[must_use]
pub(crate) fn input_schema(
    path_description: &str,
    offset_description: &str,
    limit_description: &str,
) -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["path"],
        "properties": {
            "path": {
                "type": "string",
                "minLength": 1,
                "description": path_description
            },
            "offset": {
                "type": "integer",
                "minimum": 1,
                "description": offset_description
            },
            "limit": {
                "type": "integer",
                "minimum": 1,
                "description": limit_description
            }
        }
    })
}

/// Input for one read call, after the model JSON has been checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReadInput {
    /// File path exactly as the model sent it.
    ///
    /// The path is not trimmed and is not joined to a root. The host decides
    /// what it means.
    pub(crate) path: String,
    /// First line to return, numbered from 1.
    ///
    /// This is not clamped to the file length.
    pub(crate) offset: u64,
    /// Lines to return, already clamped to [`ReadConfig::max_lines`].
    pub(crate) limit: u32,
    /// The integer the model sent for `limit`, before the clamp.
    ///
    /// A missing `limit` stores the applied maximum here, so this value is
    /// greater than [`ReadInput::limit`] only when the model asked for more
    /// lines than the configuration allows. A model integer that is larger
    /// than `u32::MAX` is stored as `u32::MAX`.
    pub(crate) requested_limit: u32,
}

/// Parses one model input value against `config`.
///
/// The checks run in a fixed order: the value must be a JSON object, every key
/// must be a known field, `path` must be a clean string, and `offset` and
/// `limit` must be integers of at least 1 when they are present.
///
/// A `limit` above [`ReadConfig::max_lines`] is not rejected. It is clamped for
/// the read, and the requested value is kept in [`ReadInput::requested_limit`]
/// so the footer can report that the limit was capped.
///
/// # Errors
///
/// Returns [`crate::ErrorCode::InputInvalid`] when any rule fails. The message
/// names the field and, where it helps, the value that failed.
pub(crate) fn parse_input(value: &Value, config: &ReadConfig) -> Result<ReadInput, ToolError> {
    let object = value.as_object().ok_or_else(|| {
        ToolError::input_invalid(format!(
            "read input must be a JSON object, got {}",
            json_type_name(value)
        ))
    })?;

    if let Some(field) = object
        .keys()
        .find(|key| !matches!(key.as_str(), "path" | "offset" | "limit"))
    {
        return Err(ToolError::input_invalid(format!(
            "unknown field \"{field}\" for the read tool; accepted fields are \"path\", \"offset\", and \"limit\""
        )));
    }

    let path_value = object
        .get("path")
        .ok_or_else(|| ToolError::input_invalid("field \"path\" is required"))?;
    let path = path_value.as_str().ok_or_else(|| {
        ToolError::input_invalid(format!("field \"path\" must be a string, got {path_value}"))
    })?;
    if path.trim().is_empty() {
        return Err(ToolError::input_invalid(
            "field \"path\" must not be empty or whitespace only",
        ));
    }
    if path != path.trim() {
        return Err(ToolError::input_invalid(
            "field \"path\" must not have leading or trailing whitespace",
        ));
    }

    let offset = match object.get("offset") {
        Some(value) => positive_integer("offset", value)?,
        None => 1,
    };

    let max_lines = config.max_lines();
    let (limit, requested_limit) = match object.get("limit") {
        Some(value) => {
            let requested = positive_integer("limit", value)?;
            match u32::try_from(requested) {
                Ok(requested) => (requested.min(max_lines), requested),
                // The request is above `u32::MAX`, so it is also above
                // `max_lines`; the applied limit is the configured maximum.
                // When `max_lines` itself is `u32::MAX` the two values are
                // equal, so the footer does not claim a cap that did not
                // happen.
                Err(_) => (max_lines, u32::MAX),
            }
        }
        None => (max_lines, max_lines),
    };

    Ok(ReadInput {
        path: path.to_string(),
        offset,
        limit,
        requested_limit,
    })
}

/// Reads one JSON number that must be an integer of at least 1.
///
/// `1.0` and `1e3` are integers. A fractional part such as `1.5`, a negative
/// value, a string, and null are rejected. A number too large to be an exact
/// `u64` is rejected too.
fn positive_integer(field: &str, value: &Value) -> Result<u64, ToolError> {
    let invalid = || {
        ToolError::input_invalid(format!(
            "field \"{field}\" must be an integer of at least 1, got {value}"
        ))
    };
    if let Some(number) = value.as_u64() {
        return if number >= 1 {
            Ok(number)
        } else {
            Err(invalid())
        };
    }
    let Some(number) = value.as_f64() else {
        return Err(invalid());
    };
    const EXACT_INT_LIMIT: f64 = (1_u64 << 53) as f64;
    if (1.0..EXACT_INT_LIMIT).contains(&number) && number.fract() == 0.0 {
        return Ok(number as u64);
    }
    Err(invalid())
}

/// Returns a readable JSON type name for one value, used in error messages.
fn json_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DEFAULT_LIMIT_DESCRIPTION, DEFAULT_OFFSET_DESCRIPTION, DEFAULT_PATH_DESCRIPTION, ReadInput,
        input_schema, parse_input,
    };
    use crate::read::config::{ReadCeilings, ReadConfig};
    use crate::{ErrorCode, ToolError};
    use serde_json::{Value, json};

    fn default_config() -> ReadConfig {
        ReadConfig::new()
    }

    fn parse(value: &Value) -> ReadInput {
        parse_input(value, &default_config()).expect("input should be accepted")
    }

    fn parse_error(value: &Value) -> ToolError {
        parse_input(value, &default_config()).expect_err("input should be rejected")
    }

    fn assert_input_invalid(value: &Value) {
        let error = parse_error(value);
        assert_eq!(error.code(), ErrorCode::InputInvalid);
        assert!(
            error.to_string().starts_with("AGT_INPUT_INVALID:"),
            "unexpected display: {error}"
        );
    }

    /// Builds an input object with `path` set and one extra field.
    fn with_field(field: &str, value: Value) -> Value {
        let mut object = serde_json::Map::new();
        object.insert("path".to_string(), json!("src/lib.rs"));
        object.insert(field.to_string(), value);
        Value::Object(object)
    }

    fn schema() -> Value {
        input_schema(
            DEFAULT_PATH_DESCRIPTION,
            DEFAULT_OFFSET_DESCRIPTION,
            DEFAULT_LIMIT_DESCRIPTION,
        )
    }

    #[test]
    fn a_non_object_input_is_rejected() {
        for case in [Value::Null, json!([]), json!("read"), json!(3)] {
            assert_input_invalid(&case);
        }
    }

    #[test]
    fn unknown_fields_are_rejected_and_named() {
        let error = parse_error(&json!({"path": "src/lib.rs", "pages": "1-5"}));
        assert_eq!(error.code(), ErrorCode::InputInvalid);
        let message = error.message();
        assert!(message.contains("pages"), "message: {message}");

        assert_input_invalid(&json!({"path": "src/lib.rs", "start_line": 2}));
        assert_input_invalid(&json!({"path": "src/lib.rs", "limit": 2, "max": 4}));
    }

    #[test]
    fn path_is_required_and_must_be_clean() {
        let cases = [
            json!({}),
            json!({"path": ""}),
            json!({"path": "   "}),
            json!({"path": " file.txt"}),
            json!({"path": "file.txt "}),
            json!({"path": "\tfile.txt"}),
            json!({"path": null}),
            json!({"path": 7}),
            json!({"path": ["file.txt"]}),
        ];
        for case in cases {
            assert_input_invalid(&case);
        }

        let input = parse(&json!({"path": "my notes.txt"}));
        assert_eq!(input.path, "my notes.txt");
    }

    #[test]
    fn offset_and_limit_must_be_integers_of_at_least_one() {
        let invalid = [
            json!(0),
            json!(-1),
            json!(1.5),
            json!("2"),
            Value::Null,
            json!(true),
        ];
        for field in ["offset", "limit"] {
            for value in &invalid {
                assert_input_invalid(&with_field(field, value.clone()));
            }
        }

        let error = parse_error(&with_field("offset", json!(0)));
        let message = error.message();
        assert!(message.contains("offset"), "message: {message}");
        assert!(message.contains("at least 1"), "message: {message}");

        let error = parse_error(&with_field("limit", json!(null)));
        let message = error.message();
        assert!(message.contains("limit"), "message: {message}");

        let input = parse(&with_field("offset", json!(1.0)));
        assert_eq!(input.offset, 1);
        let input = parse(&with_field("limit", json!(1e3)));
        assert_eq!(input.requested_limit, 1000);
    }

    #[test]
    fn missing_offset_and_limit_use_their_defaults() {
        let config = default_config();
        let input = parse_input(&json!({"path": "src/lib.rs"}), &config).expect("input");

        assert_eq!(input.offset, 1);
        assert_eq!(input.limit, config.max_lines());
        assert_eq!(input.requested_limit, input.limit);
        assert_eq!(input.limit, 2_000);
    }

    #[test]
    fn a_limit_above_max_lines_is_clamped_and_reported() {
        let input = parse(&json!({"path": "src/lib.rs", "limit": 1_000_000}));

        assert_eq!(input.limit, 2_000);
        assert_eq!(input.requested_limit, 1_000_000);
        assert!(input.requested_limit > input.limit);
    }

    #[test]
    fn a_limit_equal_to_max_lines_is_not_capped() {
        let input = parse(&json!({"path": "src/lib.rs", "limit": 2_000}));

        assert_eq!(input.limit, 2_000);
        assert_eq!(input.requested_limit, 2_000);
    }

    #[test]
    fn a_limit_that_does_not_fit_u32_becomes_max_lines() {
        let too_big = u64::from(u32::MAX) + 1;
        let input = parse(&json!({"path": "src/lib.rs", "limit": too_big}));

        assert_eq!(input.limit, 2_000);
        assert_eq!(input.requested_limit, u32::MAX);
        assert!(input.requested_limit > input.limit);
    }

    #[test]
    fn a_huge_limit_at_a_u32_max_max_lines_is_not_capped() {
        let ceilings = ReadCeilings {
            max_lines: u32::MAX,
            ..ReadCeilings::default()
        };
        let config = ReadConfig::new()
            .with_ceilings(ceilings)
            .expect("raise the ceiling")
            .with_max_lines(u32::MAX)
            .expect("store the value at the ceiling");
        let too_big = u64::from(u32::MAX) + 1;

        let input =
            parse_input(&json!({"path": "src/lib.rs", "limit": too_big}), &config).expect("input");
        assert_eq!(input.limit, u32::MAX);
        assert_eq!(input.requested_limit, u32::MAX);
        assert!(input.requested_limit <= input.limit);

        let input = parse_input(&json!({"path": "src/lib.rs"}), &config).expect("input");
        assert_eq!(input.limit, u32::MAX);
        assert_eq!(input.requested_limit, u32::MAX);
    }

    #[test]
    fn offset_is_stored_as_given_and_never_clamped() {
        let input = parse(&json!({"path": "src/lib.rs", "offset": 12}));
        assert_eq!(input.offset, 12);

        let input = parse(&json!({"path": "src/lib.rs", "offset": u64::MAX}));
        assert_eq!(input.offset, u64::MAX);
    }

    #[test]
    fn schema_locks_the_input_shape() {
        let schema = schema();

        assert_eq!(schema["type"], json!("object"));
        assert_eq!(schema["additionalProperties"], json!(false));
        assert_eq!(schema["required"], json!(["path"]));

        let properties = schema["properties"]
            .as_object()
            .expect("properties should be an object");
        assert_eq!(properties.len(), 3);
        assert_eq!(schema["properties"]["path"]["type"], json!("string"));
        assert_eq!(schema["properties"]["path"]["minLength"], json!(1));
        assert_eq!(schema["properties"]["offset"]["type"], json!("integer"));
        assert_eq!(schema["properties"]["offset"]["minimum"], json!(1));
        assert_eq!(schema["properties"]["limit"]["type"], json!("integer"));
        assert_eq!(schema["properties"]["limit"]["minimum"], json!(1));
    }

    #[test]
    fn schema_uses_the_descriptions_it_is_given() {
        let schema = input_schema("p", "o", "l");

        assert_eq!(schema["properties"]["path"]["description"], json!("p"));
        assert_eq!(schema["properties"]["offset"]["description"], json!("o"));
        assert_eq!(schema["properties"]["limit"]["description"], json!("l"));
    }

    #[test]
    fn schema_defaults_are_the_public_constants() {
        let schema = schema();

        assert_eq!(
            schema["properties"]["path"]["description"],
            json!(DEFAULT_PATH_DESCRIPTION)
        );
        assert_eq!(
            schema["properties"]["offset"]["description"],
            json!(DEFAULT_OFFSET_DESCRIPTION)
        );
        assert_eq!(
            schema["properties"]["limit"]["description"],
            json!(DEFAULT_LIMIT_DESCRIPTION)
        );
    }

    #[test]
    fn schema_min_length_is_one_and_does_not_reject_spaces() {
        let schema = schema();
        let path = &schema["properties"]["path"];

        assert_eq!(path["minLength"], json!(1));
        assert!(
            path.get("pattern").is_none(),
            "the schema must not try to reject padded paths"
        );
        // The whitespace rule lives in the parser, not in the schema.
        assert_input_invalid(&json!({"path": "   "}));
    }
}
