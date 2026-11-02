//! Model input for the write tool.
//!
//! [`parse_input`] turns the JSON a model sends into a [`WriteInput`], and
//! [`input_schema`] builds the JSON Schema a host registers for the same tool.
//! The shape is locked: two required strings, `path` and `content`, and no
//! other field. Unknown fields are rejected instead of ignored, so a model
//! that invents a field hears which field it invented.
//!
//! The parser is stricter than the schema in one place: a `path` that is blank,
//! padded with whitespace, contains a NUL byte, or is longer than the byte cap
//! is rejected by [`parse_input`]. JSON Schema cannot express all of those
//! rules, so they stay parser-only.
//!
//! `content` is never trimmed: a file may start or end with spaces or newlines.
//! Empty content is valid input here; whether the tool accepts it is a content
//! policy check, not a parse rule.

use serde_json::{Value, json};

use crate::ToolError;
use crate::path::{MAX_MODEL_PATH_BYTES, ModelPathError, check_model_path};

/// Builds the JSON Schema for the write tool input.
///
/// The shape is locked: `type` is `object`, `additionalProperties` is `false`,
/// both `path` and `content` are required strings, and no other field exists.
/// Each argument replaces the `description` string of the field with the same
/// name. Nothing else in the schema changes.
#[must_use]
pub(crate) fn input_schema(path_description: &str, content_description: &str) -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["path", "content"],
        "properties": {
            "path": {
                "type": "string",
                "description": path_description
            },
            "content": {
                "type": "string",
                "description": content_description
            }
        }
    })
}

/// Input for one write call, after the model JSON has been checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WriteInput {
    /// File path exactly as the model sent it.
    ///
    /// The path is not trimmed and is not joined to a root. The host decides
    /// what it means.
    pub(crate) path: String,
    /// Full new file content exactly as the model sent it.
    ///
    /// The content is not trimmed. It may be empty; the content policy decides
    /// whether that is allowed.
    pub(crate) content: String,
}

/// Parses one model input value.
///
/// The checks run in a fixed order, and the first match wins:
///
/// 1. The value must be a JSON object, must not carry an unknown field, and
///    must hold a `path` and a `content` that are both strings.
/// 2. `path` must not be empty or whitespace only, and must not have leading
///    or trailing whitespace. It is never trimmed and then accepted.
/// 3. `path` must not contain a NUL byte.
/// 4. `path` must not be longer than [`MAX_MODEL_PATH_BYTES`] UTF-8 bytes.
///
/// `content` is stored unchanged, including any leading or trailing
/// whitespace.
///
/// # Errors
///
/// Returns [`crate::ErrorCode::InputInvalid`] when any rule fails. The message
/// names the field and, where it helps, the reason that failed.
pub(crate) fn parse_input(value: &Value) -> Result<WriteInput, ToolError> {
    let object = value.as_object().ok_or_else(|| {
        ToolError::input_invalid(format!(
            "write input must be a JSON object, got {}",
            json_type_name(value)
        ))
    })?;

    if let Some(field) = object
        .keys()
        .find(|key| !matches!(key.as_str(), "path" | "content"))
    {
        return Err(ToolError::input_invalid(format!(
            "unknown field \"{field}\" for the write tool; accepted fields are \"path\" and \"content\""
        )));
    }

    let path_value = object
        .get("path")
        .ok_or_else(|| ToolError::input_invalid("field \"path\" is required"))?;
    let path = path_value.as_str().ok_or_else(|| {
        ToolError::input_invalid(format!(
            "field \"path\" must be a string, got {}",
            json_type_name(path_value)
        ))
    })?;

    let content_value = object
        .get("content")
        .ok_or_else(|| ToolError::input_invalid("field \"content\" is required"))?;
    let content = content_value.as_str().ok_or_else(|| {
        ToolError::input_invalid(format!(
            "field \"content\" must be a string, got {}",
            json_type_name(content_value)
        ))
    })?;

    check_model_path(path).map_err(path_error)?;

    Ok(WriteInput {
        path: path.to_string(),
        content: content.to_string(),
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
    use super::{WriteInput, input_schema, parse_input};
    use crate::ToolError;
    use crate::path::MAX_MODEL_PATH_BYTES;
    use crate::write::config::WriteConfig;
    use serde_json::{Value, json};

    fn parse(value: &Value) -> WriteInput {
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

    fn default_descriptions() -> (String, String) {
        let config = WriteConfig::new();
        (
            config.path_description().to_string(),
            config.content_description().to_string(),
        )
    }

    fn schema() -> Value {
        let (path, content) = default_descriptions();
        input_schema(&path, &content)
    }

    /// Evaluates the fixed shape [`input_schema`] builds for one candidate.
    ///
    /// This is deliberately small: it understands the `required`,
    /// `additionalProperties`, and string `type` keywords this schema uses, so
    /// a parser test can also prove the published schema agrees with the
    /// parser on the same example.
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
                Some(field) => property["type"] != json!("string") || field.is_string(),
            })
    }

    #[test]
    fn a_non_object_input_is_rejected() {
        for case in [
            Value::Null,
            json!([]),
            json!("write"),
            json!(3),
            json!(true),
        ] {
            assert_input_invalid(&case);
        }
    }

    #[test]
    fn path_and_content_are_required() {
        assert_input_invalid(&json!({}));
        assert_input_invalid(&json!({"content": "text"}));
        assert_input_invalid(&json!({"path": "file.txt"}));

        let error = parse_error(&json!({"content": "text"}));
        assert!(error.message().contains("path"), "message: {error}");
        let error = parse_error(&json!({"path": "file.txt"}));
        assert!(error.message().contains("content"), "message: {error}");
    }

    #[test]
    fn non_string_path_and_content_are_rejected() {
        let invalid = [
            Value::Null,
            json!(1),
            json!(1.5),
            json!(true),
            json!(["a"]),
            json!({"value": "a"}),
        ];
        for value in &invalid {
            assert_input_invalid(&json!({"path": value, "content": "text"}));
            assert_input_invalid(&json!({"path": "file.txt", "content": value}));
        }

        let error = parse_error(&json!({"path": 1, "content": "text"}));
        assert!(error.message().contains("path"), "message: {error}");
        assert!(error.message().contains("number"), "message: {error}");
        let error = parse_error(&json!({"path": "file.txt", "content": true}));
        assert!(error.message().contains("content"), "message: {error}");
        assert!(error.message().contains("boolean"), "message: {error}");
    }

    #[test]
    fn an_unknown_field_is_rejected_and_named() {
        let error = parse_error(&json!({
            "path": "file.txt",
            "content": "text",
            "append": true
        }));
        assert_eq!(error.code().as_str(), "AGT_INPUT_INVALID");
        assert!(error.message().contains("append"), "message: {error}");

        assert_input_invalid(&json!({"path": "file.txt", "content": "", "offset": 4}));
        assert_input_invalid(&json!({"path": "file.txt", "content": "", "overwrite": true}));
    }

    #[test]
    fn a_non_string_content_wins_over_a_blank_path() {
        let error = parse_error(&json!({"path": "   ", "content": 5}));
        assert_eq!(error.code().as_str(), "AGT_INPUT_INVALID");
        assert!(error.message().contains("content"), "message: {error}");
    }

    #[test]
    fn a_blank_path_is_rejected() {
        for path in ["", " ", "   ", "\t", "\n", " \t\n "] {
            assert_input_invalid(&json!({"path": path, "content": "text"}));
        }
    }

    #[test]
    fn a_padded_path_is_rejected_and_is_not_trimmed_into_a_valid_path() {
        for path in [" foo", "foo ", " foo ", "\tfoo", "foo\n"] {
            assert_input_invalid(&json!({"path": path, "content": "text"}));
        }

        let error = parse_error(&json!({"path": " foo ", "content": "text"}));
        assert!(error.message().contains("whitespace"), "message: {error}");
    }

    #[test]
    fn a_path_containing_nul_is_rejected() {
        for path in ["\0", "a\0b", "src/lib\0.rs"] {
            assert_input_invalid(&json!({"path": path, "content": "text"}));
        }

        let error = parse_error(&json!({"path": "a\0b", "content": "text"}));
        assert!(error.message().contains("NUL"), "message: {error}");
    }

    #[test]
    fn a_path_at_the_byte_cap_is_accepted() {
        let path = "a".repeat(MAX_MODEL_PATH_BYTES);
        let input = parse(&json!({"path": path, "content": "text"}));
        assert_eq!(input.path.len(), MAX_MODEL_PATH_BYTES);
    }

    #[test]
    fn a_path_over_the_byte_cap_is_rejected() {
        let path = "a".repeat(MAX_MODEL_PATH_BYTES + 1);
        let error = parse_error(&json!({"path": path, "content": "text"}));
        assert_eq!(error.code().as_str(), "AGT_INPUT_INVALID");
        assert!(error.message().contains("4096"), "message: {error}");
        assert!(
            error
                .message()
                .contains(&(MAX_MODEL_PATH_BYTES + 1).to_string()),
            "message: {error}"
        );
    }

    #[test]
    fn whitespace_padding_is_rejected_before_the_byte_cap() {
        let path = format!(" {}", "a".repeat(MAX_MODEL_PATH_BYTES));
        assert!(path.len() > MAX_MODEL_PATH_BYTES);

        let error = parse_error(&json!({"path": path, "content": "text"}));
        assert_eq!(error.code().as_str(), "AGT_INPUT_INVALID");
        assert!(error.message().contains("whitespace"), "message: {error}");
        assert!(!error.message().contains("4096"), "message: {error}");
    }

    #[test]
    fn nul_is_rejected_before_the_byte_cap() {
        let path = format!("a\0{}", "a".repeat(MAX_MODEL_PATH_BYTES));
        assert!(path.len() > MAX_MODEL_PATH_BYTES);

        let error = parse_error(&json!({"path": path, "content": "text"}));
        assert_eq!(error.code().as_str(), "AGT_INPUT_INVALID");
        assert!(error.message().contains("NUL"), "message: {error}");
        assert!(!error.message().contains("4096"), "message: {error}");
    }

    #[test]
    fn the_byte_cap_counts_utf8_bytes_not_scalars() {
        let path = "é".repeat(MAX_MODEL_PATH_BYTES);
        assert_eq!(path.chars().count(), MAX_MODEL_PATH_BYTES);
        assert_eq!(path.len(), MAX_MODEL_PATH_BYTES * 2);

        let error = parse_error(&json!({"path": path, "content": "text"}));
        assert_eq!(error.code().as_str(), "AGT_INPUT_INVALID");
        assert!(
            error
                .message()
                .contains(&(MAX_MODEL_PATH_BYTES * 2).to_string()),
            "message: {error}"
        );

        let at_cap = "é".repeat(MAX_MODEL_PATH_BYTES / 2);
        assert_eq!(at_cap.len(), MAX_MODEL_PATH_BYTES);
        let input = parse(&json!({"path": at_cap, "content": "text"}));
        assert_eq!(input.path.len(), MAX_MODEL_PATH_BYTES);
    }

    #[test]
    fn path_and_content_are_stored_exactly_as_sent() {
        let input = parse(&json!({
            "path": "my notes.txt",
            "content": "  first line\nsecond line\n\n"
        }));
        assert_eq!(input.path, "my notes.txt");
        assert_eq!(input.content, "  first line\nsecond line\n\n");
    }

    #[test]
    fn empty_content_is_valid_input() {
        let input = parse(&json!({"path": "empty.txt", "content": ""}));
        assert_eq!(input.content, "");
    }

    #[test]
    fn schema_locks_the_input_shape() {
        let schema = schema();

        assert_eq!(schema["type"], json!("object"));
        assert_eq!(schema["additionalProperties"], json!(false));
        assert_eq!(schema["required"], json!(["path", "content"]));

        let properties = schema["properties"]
            .as_object()
            .expect("properties should be an object");
        assert_eq!(properties.len(), 2);
        assert_eq!(schema["properties"]["path"]["type"], json!("string"));
        assert_eq!(schema["properties"]["content"]["type"], json!("string"));
        assert!(schema["properties"].get("offset").is_none());
        assert!(schema["properties"].get("append").is_none());
        assert!(schema["properties"].get("overwrite").is_none());
    }

    #[test]
    fn schema_uses_the_descriptions_it_is_given() {
        let schema = input_schema("p", "c");

        assert_eq!(schema["properties"]["path"]["description"], json!("p"));
        assert_eq!(schema["properties"]["content"]["description"], json!("c"));
    }

    #[test]
    fn schema_defaults_are_the_config_getters() {
        let schema = schema();
        let (path, content) = default_descriptions();

        assert_eq!(schema["properties"]["path"]["description"], json!(path));
        assert_eq!(
            schema["properties"]["content"]["description"],
            json!(content)
        );
    }

    #[test]
    fn schema_and_parser_agree_on_the_input_shape() {
        let schema = schema();

        let accepted = [
            json!({"path": "src/lib.rs", "content": "fn main() {}"}),
            json!({"path": "a", "content": ""}),
        ];
        for value in accepted {
            assert!(schema_accepts(&schema, &value), "schema rejected {value}");
            assert!(parse_input(&value).is_ok(), "parser rejected {value}");
        }

        let rejected = [
            Value::Null,
            json!("write"),
            json!([]),
            json!({}),
            json!({"content": "text"}),
            json!({"path": "a"}),
            json!({"path": 1, "content": "text"}),
            json!({"path": "a", "content": 1}),
            json!({"path": "a", "content": "text", "append": true}),
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
            let value = json!({"path": path, "content": "text"});
            assert!(schema_accepts(&schema, &value), "schema rejected {value}");
            assert_input_invalid(&value);
        }
    }
}
