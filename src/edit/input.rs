//! Model input for the edit tool.
//!
//! [`parse_input`] turns the JSON a model sends into an [`EditInput`], and
//! [`input_schema`] builds the JSON Schema a host registers for the same tool.
//! The shape is locked: three required strings, `path`, `old_text`, and
//! `new_text`, plus an optional boolean `replace_all`. Unknown fields are
//! rejected instead of ignored, so a model that invents a field hears which
//! field it invented.
//!
//! The parser is stricter than the schema in one place: a `path` that is blank,
//! padded with whitespace, contains a NUL byte, or is longer than the byte cap
//! is rejected by [`parse_input`]. JSON Schema cannot express all of those
//! rules, so they stay parser-only.
//!
//! `old_text` and `new_text` are never trimmed: leading and trailing whitespace
//! is significant, and removing it is the lenient tier's job, not the parser's.

use serde_json::{Value, json};

use crate::ToolError;
use crate::path::{MAX_MODEL_PATH_BYTES, ModelPathError, check_model_path};

use super::config::EditConfig;

/// Builds the JSON Schema for the edit tool input.
///
/// The shape is locked: `type` is `object`, `additionalProperties` is `false`,
/// `path`, `old_text`, and `new_text` are required strings, and `replace_all`
/// is an optional boolean that defaults to `false`. Each description comes from
/// `config`, so a host that re-describes a field sees its own text here.
#[must_use]
pub(crate) fn input_schema(config: &EditConfig) -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["path", "old_text", "new_text"],
        "properties": {
            "path": {
                "type": "string",
                "description": config.path_description()
            },
            "old_text": {
                "type": "string",
                "description": config.old_text_description()
            },
            "new_text": {
                "type": "string",
                "description": config.new_text_description()
            },
            "replace_all": {
                "type": "boolean",
                "default": false,
                "description": config.replace_all_description()
            }
        }
    })
}

/// Input for one edit call, after the model JSON has been checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EditInput {
    /// File path exactly as the model sent it.
    ///
    /// The path is not trimmed and is not joined to a root. The host decides
    /// what it means.
    pub(crate) path: String,
    /// Text to find, exactly as the model sent it.
    pub(crate) old_text: String,
    /// Text that replaces the match, exactly as the model sent it.
    ///
    /// It may be empty, which deletes the region.
    pub(crate) new_text: String,
    /// Whether every occurrence is replaced instead of exactly one.
    ///
    /// Occurrence counting is left-to-right and non-overlapping, so two adjacent
    /// hits of one `old_text` count as two. A fuzzy match still replaces one
    /// window, whatever this says.
    pub(crate) replace_all: bool,
}

/// Parses one model input value.
///
/// The checks run in a fixed order, and the first match wins:
///
/// 1. The value must be a JSON object, must not carry an unknown field, and
///    must hold `path`, `old_text`, and `new_text` as strings.
/// 2. `replace_all` is optional. When present it must be a boolean: `null` and
///    every non-boolean value are rejected. When absent it is `false`.
/// 3. `path` must not be empty or whitespace only, must not have leading or
///    trailing whitespace, must not contain a NUL byte, and must be at most
///    [`MAX_MODEL_PATH_BYTES`] UTF-8 bytes.
///
/// `old_text` and `new_text` are stored unchanged, including any leading or
/// trailing whitespace.
///
/// # Errors
///
/// Returns [`crate::ErrorCode::InputInvalid`] when any rule fails. The message
/// names the field and, where it helps, the reason that failed.
pub(crate) fn parse_input(value: &Value) -> Result<EditInput, ToolError> {
    let object = value.as_object().ok_or_else(|| {
        ToolError::input_invalid(format!(
            "edit input must be a JSON object, got {}",
            json_type_name(value)
        ))
    })?;

    if let Some(field) = object.keys().find(|key| {
        !matches!(
            key.as_str(),
            "path" | "old_text" | "new_text" | "replace_all"
        )
    }) {
        return Err(ToolError::input_invalid(format!(
            "unknown field \"{field}\" for the edit tool; accepted fields are \
             \"path\", \"old_text\", \"new_text\", and \"replace_all\""
        )));
    }

    let path = required_str(object, "path")?;
    let old_text = required_str(object, "old_text")?;
    let new_text = required_str(object, "new_text")?;

    let replace_all = match object.get("replace_all") {
        None => false,
        Some(value) => value.as_bool().ok_or_else(|| {
            ToolError::input_invalid(format!(
                "field \"replace_all\" must be a boolean, got {}",
                json_type_name(value)
            ))
        })?,
    };

    check_model_path(path).map_err(path_error)?;

    Ok(EditInput {
        path: path.to_string(),
        old_text: old_text.to_string(),
        new_text: new_text.to_string(),
        replace_all,
    })
}

/// Reads one required string field, naming the field and the type it got.
fn required_str<'a>(
    object: &'a serde_json::Map<String, Value>,
    field: &str,
) -> Result<&'a str, ToolError> {
    let value = object
        .get(field)
        .ok_or_else(|| ToolError::input_invalid(format!("field \"{field}\" is required")))?;
    value.as_str().ok_or_else(|| {
        ToolError::input_invalid(format!(
            "field \"{field}\" must be a string, got {}",
            json_type_name(value)
        ))
    })
}

/// Turns one [`ModelPathError`] into a model-facing contract error.
fn path_error(error: ModelPathError) -> ToolError {
    match error {
        ModelPathError::Blank => {
            ToolError::input_invalid("field \"path\" must not be empty or whitespace only")
        }
        ModelPathError::Padded => {
            ToolError::input_invalid("field \"path\" must not have leading or trailing whitespace")
        }
        ModelPathError::Nul => {
            ToolError::input_invalid("field \"path\" must not contain a NUL byte")
        }
        ModelPathError::TooLong { bytes } => {
            let limit = MAX_MODEL_PATH_BYTES;
            ToolError::input_invalid(format!(
                "field \"path\" must be at most {limit} UTF-8 bytes, got {bytes}"
            ))
        }
    }
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
    use super::{EditInput, input_schema, parse_input};
    use crate::ToolError;
    use crate::edit::config::EditConfig;
    use crate::path::MAX_MODEL_PATH_BYTES;
    use serde_json::{Value, json};

    fn parse(value: &Value) -> EditInput {
        parse_input(value).expect("input should be accepted")
    }

    fn parse_error(value: &Value) -> ToolError {
        parse_input(value).expect_err("input should be rejected")
    }

    fn assert_input_invalid(value: &Value) {
        let error = parse_error(value);
        assert_eq!(error.code().as_str(), "AGT_INPUT_INVALID");
        assert!(
            error.to_string().starts_with("AGT_INPUT_INVALID:"),
            "unexpected display: {error}"
        );
    }

    fn schema() -> Value {
        input_schema(&EditConfig::new())
    }

    /// Evaluates the fixed shape [`input_schema`] builds for one candidate.
    ///
    /// This is deliberately small: it understands the `required`,
    /// `additionalProperties`, `type`, and `default` keywords this schema uses,
    /// for strings and booleans, so a parser test can also prove the published
    /// schema agrees with the parser on the same example.
    fn schema_accepts(schema: &Value, value: &Value) -> bool {
        let Some(object) = value.as_object() else {
            return false;
        };
        let required = schema["required"]
            .as_array()
            .expect("required should be an array");
        if !required
            .iter()
            .all(|key| key.as_str().is_some_and(|key| object.contains_key(key)))
        {
            return false;
        }
        let properties = schema["properties"]
            .as_object()
            .expect("properties should be an object");
        if schema["additionalProperties"] == json!(false)
            && object.keys().any(|key| !properties.contains_key(key))
        {
            return false;
        }
        properties
            .iter()
            .all(|(name, property)| match object.get(name) {
                None => true,
                Some(field) => match property["type"].as_str() {
                    Some("string") => field.is_string(),
                    Some("boolean") => field.is_boolean(),
                    _ => true,
                },
            })
    }

    #[test]
    fn a_non_object_input_is_rejected() {
        for case in [Value::Null, json!([]), json!("edit"), json!(3), json!(true)] {
            assert_input_invalid(&case);
        }
    }

    #[test]
    fn path_old_text_and_new_text_are_required() {
        assert_input_invalid(&json!({}));
        assert_input_invalid(&json!({"old_text": "a", "new_text": "b"}));
        assert_input_invalid(&json!({"path": "a.txt", "new_text": "b"}));
        assert_input_invalid(&json!({"path": "a.txt", "old_text": "a"}));

        let error = parse_error(&json!({"path": "a.txt", "new_text": "b"}));
        assert!(error.message().contains("old_text"), "message: {error}");
        let error = parse_error(&json!({"path": "a.txt", "old_text": "a"}));
        assert!(error.message().contains("new_text"), "message: {error}");
    }

    #[test]
    fn non_string_text_fields_are_rejected() {
        let invalid = [
            Value::Null,
            json!(1),
            json!(1.5),
            json!(true),
            json!(["a"]),
            json!({"value": "a"}),
        ];
        for value in &invalid {
            assert_input_invalid(&json!({"path": value, "old_text": "a", "new_text": "b"}));
            assert_input_invalid(&json!({"path": "a.txt", "old_text": value, "new_text": "b"}));
            assert_input_invalid(&json!({"path": "a.txt", "old_text": "a", "new_text": value}));
        }

        let error = parse_error(&json!({"path": "a.txt", "old_text": true, "new_text": "b"}));
        assert_eq!(error.code().as_str(), "AGT_INPUT_INVALID");
        assert!(error.message().contains("old_text"), "message: {error}");
        assert!(error.message().contains("boolean"), "message: {error}");
    }

    #[test]
    fn a_null_or_non_boolean_replace_all_is_rejected() {
        for value in [Value::Null, json!("true"), json!(1), json!([])] {
            assert_input_invalid(&json!({
                "path": "a.txt",
                "old_text": "a",
                "new_text": "b",
                "replace_all": value
            }));
        }

        let error = parse_error(&json!({
            "path": "a.txt",
            "old_text": "a",
            "new_text": "b",
            "replace_all": "yes"
        }));
        assert!(error.message().contains("replace_all"), "message: {error}");
        assert!(error.message().contains("boolean"), "message: {error}");
    }

    #[test]
    fn a_missing_replace_all_defaults_to_false() {
        assert!(!parse(&json!({"path": "a.txt", "old_text": "a", "new_text": "b"})).replace_all);
        assert!(
            parse(&json!({
                "path": "a.txt",
                "old_text": "a",
                "new_text": "b",
                "replace_all": true
            }))
            .replace_all
        );
    }

    #[test]
    fn an_unknown_field_is_rejected_and_named() {
        let error = parse_error(&json!({
            "path": "a.txt",
            "old_text": "a",
            "new_text": "b",
            "occurrence": 2
        }));
        assert_eq!(error.code().as_str(), "AGT_INPUT_INVALID");
        assert!(error.message().contains("occurrence"), "message: {error}");

        assert_input_invalid(&json!({
            "path": "a.txt",
            "old_text": "a",
            "new_text": "b",
            "content": "text"
        }));
    }

    #[test]
    fn a_blank_path_is_rejected() {
        for path in ["", " ", "   ", "\t", "\n", " \t\n "] {
            assert_input_invalid(&json!({"path": path, "old_text": "a", "new_text": "b"}));
        }
    }

    #[test]
    fn a_padded_path_is_rejected_and_is_not_trimmed_into_a_valid_path() {
        for path in [" foo", "foo ", " foo ", "\tfoo", "foo\n"] {
            assert_input_invalid(&json!({"path": path, "old_text": "a", "new_text": "b"}));
        }

        let error = parse_error(&json!({"path": " foo ", "old_text": "a", "new_text": "b"}));
        assert!(error.message().contains("whitespace"), "message: {error}");
    }

    #[test]
    fn a_path_containing_nul_is_rejected() {
        for path in ["\0", "a\0b", "src/lib\0.rs"] {
            assert_input_invalid(&json!({"path": path, "old_text": "a", "new_text": "b"}));
        }

        let error = parse_error(&json!({"path": "a\0b", "old_text": "a", "new_text": "b"}));
        assert!(error.message().contains("NUL"), "message: {error}");
    }

    #[test]
    fn a_path_over_the_4096_byte_cap_is_rejected() {
        let path = "a".repeat(MAX_MODEL_PATH_BYTES + 1);
        let error = parse_error(&json!({"path": path, "old_text": "a", "new_text": "b"}));
        assert_eq!(error.code().as_str(), "AGT_INPUT_INVALID");
        assert!(error.message().contains("4096"), "message: {error}");
    }

    #[test]
    fn the_text_fields_are_stored_exactly_as_sent() {
        let input = parse(&json!({
            "path": "my notes.txt",
            "old_text": "  first line\n\tsecond line\n\n",
            "new_text": "  replacement  \n"
        }));
        assert_eq!(input.path, "my notes.txt");
        assert_eq!(input.old_text, "  first line\n\tsecond line\n\n");
        assert_eq!(input.new_text, "  replacement  \n");
    }

    #[test]
    fn an_empty_new_text_deletes_and_is_valid_input() {
        let input = parse(&json!({"path": "a.txt", "old_text": "drop me", "new_text": ""}));
        assert_eq!(input.new_text, "");
    }

    #[test]
    fn an_empty_old_text_parses_and_is_left_to_the_tool_checks() {
        let input = parse(&json!({"path": "a.txt", "old_text": "", "new_text": "b"}));
        assert_eq!(input.old_text, "");
    }

    #[test]
    fn schema_locks_the_input_shape() {
        let schema = schema();

        assert_eq!(schema["type"], json!("object"));
        assert_eq!(schema["additionalProperties"], json!(false));
        assert_eq!(schema["required"], json!(["path", "old_text", "new_text"]));

        let properties = schema["properties"]
            .as_object()
            .expect("properties should be an object");
        assert_eq!(properties.len(), 4);
        assert_eq!(schema["properties"]["path"]["type"], json!("string"));
        assert_eq!(schema["properties"]["old_text"]["type"], json!("string"));
        assert_eq!(schema["properties"]["new_text"]["type"], json!("string"));
        assert_eq!(
            schema["properties"]["replace_all"]["type"],
            json!("boolean")
        );
        assert_eq!(schema["properties"]["replace_all"]["default"], json!(false));
    }

    #[test]
    fn schema_uses_the_config_descriptions() {
        let config = EditConfig::new()
            .with_parameter_description("path", "p")
            .expect("path is a known parameter")
            .with_parameter_description("old_text", "o")
            .expect("old_text is a known parameter")
            .with_parameter_description("new_text", "n")
            .expect("new_text is a known parameter")
            .with_parameter_description("replace_all", "r")
            .expect("replace_all is a known parameter");
        let schema = input_schema(&config);

        assert_eq!(schema["properties"]["path"]["description"], json!("p"));
        assert_eq!(schema["properties"]["old_text"]["description"], json!("o"));
        assert_eq!(schema["properties"]["new_text"]["description"], json!("n"));
        assert_eq!(
            schema["properties"]["replace_all"]["description"],
            json!("r")
        );
    }

    #[test]
    fn schema_and_parser_agree_on_the_input_shape() {
        let schema = schema();

        let accepted = [
            json!({"path": "src/lib.rs", "old_text": "a", "new_text": "b"}),
            json!({"path": "a", "old_text": "a", "new_text": ""}),
            json!({"path": "a", "old_text": "a", "new_text": "b", "replace_all": true}),
            json!({"path": "a", "old_text": "a", "new_text": "b", "replace_all": false}),
        ];
        for value in accepted {
            assert!(schema_accepts(&schema, &value), "schema rejected {value}");
            assert!(parse_input(&value).is_ok(), "parser rejected {value}");
        }

        let rejected = [
            Value::Null,
            json!("edit"),
            json!([]),
            json!({}),
            json!({"path": "a", "new_text": "b"}),
            json!({"path": "a", "old_text": "a"}),
            json!({"old_text": "a", "new_text": "b"}),
            json!({"path": 1, "old_text": "a", "new_text": "b"}),
            json!({"path": "a", "old_text": 1, "new_text": "b"}),
            json!({"path": "a", "old_text": "a", "new_text": 1}),
            json!({"path": "a", "old_text": "a", "new_text": "b", "replace_all": null}),
            json!({"path": "a", "old_text": "a", "new_text": "b", "replace_all": "yes"}),
            json!({"path": "a", "old_text": "a", "new_text": "b", "occurrence": 2}),
        ];
        for value in rejected {
            assert!(!schema_accepts(&schema, &value), "schema accepted {value}");
            assert!(parse_input(&value).is_err(), "parser accepted {value}");
        }
    }

    #[test]
    fn the_parser_is_stricter_than_the_schema_for_path_policy() {
        let schema = schema();
        for path in ["   ", " foo ", "a\0b"] {
            let value = json!({"path": path, "old_text": "a", "new_text": "b"});
            assert!(schema_accepts(&schema, &value), "schema rejected {value}");
            assert_input_invalid(&value);
        }
    }
}
