//! The host-facing tool contract.
//!
//! [`Tool::call`] receives the call context and the model input. It does not
//! receive a filesystem or a process runner.

use async_trait::async_trait;
use serde_json::Value;

use crate::error::ToolError;

/// Description a host registers with an agent.
///
/// Fields stay private so a later optional field does not break constructors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolSpec {
    name: String,
    description: String,
    input_schema: Value,
}

impl ToolSpec {
    /// Builds a spec the host can register.
    ///
    /// # Errors
    ///
    /// Returns [`crate::ErrorCode::SpecInvalid`] when the name is empty or contains
    /// whitespace, the description is empty, or `input_schema` is not a JSON
    /// object.
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        input_schema: Value,
    ) -> Result<Self, ToolError> {
        let name = name.into();
        if name.is_empty() || name.chars().any(char::is_whitespace) {
            return Err(ToolError::spec_invalid(
                "tool name must be non-empty and must not contain whitespace",
            ));
        }

        let description = description.into();
        let description = description.trim();
        if description.is_empty() {
            return Err(ToolError::spec_invalid(
                "tool description must not be empty",
            ));
        }

        if !input_schema.is_object() {
            return Err(ToolError::spec_invalid(
                "tool input schema must be a JSON object",
            ));
        }

        Ok(Self {
            name,
            description: description.to_string(),
            input_schema,
        })
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

    /// JSON Schema object for the call input.
    #[must_use]
    pub fn input_schema(&self) -> &Value {
        &self.input_schema
    }
}

/// Data the host already has for one tool call.
///
/// This is not an I/O port. Host capabilities stay on the tool value.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CallContext {
    call_id: Option<String>,
}

impl CallContext {
    /// A call with no id.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A call identified by `call_id`. A blank id is stored as absent.
    #[must_use]
    pub fn with_call_id(call_id: impl Into<String>) -> Self {
        let call_id = call_id.into();
        let call_id = call_id.trim();
        Self {
            call_id: if call_id.is_empty() {
                None
            } else {
                Some(call_id.to_string())
            },
        }
    }

    /// Host call id, when the agent loop has one.
    #[must_use]
    pub fn call_id(&self) -> Option<&str> {
        self.call_id.as_deref()
    }
}

/// Bytes of one image returned beside the text a model reads.
///
/// The host encodes or resizes these bytes. This crate does not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImagePart {
    media_type: &'static str,
    bytes: Vec<u8>,
}

impl ImagePart {
    /// Builds one image part from a static media type label and raw file bytes.
    ///
    /// `media_type` is a label such as `image/png`. `bytes` are the raw file
    /// bytes. The label is not validated and the bytes are not encoded.
    #[must_use]
    pub fn new(media_type: &'static str, bytes: impl Into<Vec<u8>>) -> Self {
        Self {
            media_type,
            bytes: bytes.into(),
        }
    }

    /// Label such as `image/png`.
    #[must_use]
    pub fn media_type(&self) -> &'static str {
        self.media_type
    }

    /// Raw image bytes.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

/// Text the model sees after a tool runs, plus any images returned beside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolOutput {
    content: String,
    is_error: bool,
    images: Vec<ImagePart>,
}

impl ToolOutput {
    /// A successful result. `content` may be empty.
    #[must_use]
    pub fn text(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: false,
            images: Vec::new(),
        }
    }

    /// An operational failure the model should see and recover from.
    #[must_use]
    pub fn error_text(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: true,
            images: Vec::new(),
        }
    }

    /// Text returned to the model.
    #[must_use]
    pub fn content(&self) -> &str {
        &self.content
    }

    /// Whether the model should treat this result as a failed operation.
    #[must_use]
    pub const fn is_error(&self) -> bool {
        self.is_error
    }

    /// Images returned beside [`ToolOutput::content`], in call order.
    #[must_use]
    pub fn images(&self) -> &[ImagePart] {
        &self.images
    }

    /// Appends one image without changing the text or the error flag.
    #[must_use]
    pub fn with_image(mut self, image: ImagePart) -> Self {
        self.images.push(image);
        self
    }
}

/// One tool the host can put in `Box<dyn Tool>`.
///
/// Implementors bring their own dependencies. Those dependencies are stored on
/// the value and are not arguments of [`Tool::call`].
#[async_trait]
pub trait Tool: Send + Sync {
    /// Specification registered with the agent.
    fn spec(&self) -> &ToolSpec;

    /// Runs the tool for one model call.
    async fn call(&self, ctx: &CallContext, input: Value) -> Result<ToolOutput, ToolError>;

    /// Erases the concrete tool so a host can store mixed tools.
    fn boxed(self) -> Box<dyn Tool>
    where
        Self: Sized + 'static,
    {
        Box::new(self)
    }
}

#[cfg(test)]
mod tests {
    use super::{CallContext, ImagePart, Tool, ToolOutput, ToolSpec};
    use crate::async_trait;
    use crate::error::{ErrorCode, ToolError};
    use serde_json::{Value, json};
    use std::future::Future;
    use std::task::{Context, Poll, Waker};

    fn ready<F>(future: F) -> F::Output
    where
        F: Future,
    {
        let mut future = std::pin::pin!(future);
        let waker = Waker::noop();
        let mut context = Context::from_waker(waker);
        match future.as_mut().poll(&mut context) {
            Poll::Ready(value) => value,
            Poll::Pending => panic!("test future should not wait"),
        }
    }

    struct Echo {
        spec: ToolSpec,
    }

    #[async_trait]
    impl Tool for Echo {
        fn spec(&self) -> &ToolSpec {
            &self.spec
        }

        async fn call(&self, ctx: &CallContext, input: Value) -> Result<ToolOutput, ToolError> {
            let text = input
                .get("text")
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
                .ok_or_else(|| {
                    ToolError::input_invalid("field \"text\" must be a non-empty string")
                })?;
            Ok(ToolOutput::text(format!(
                "{}:{text}",
                ctx.call_id().unwrap_or("none")
            )))
        }
    }

    fn echo() -> Echo {
        Echo {
            spec: ToolSpec::new(
                "echo",
                " Repeat the text field. ",
                json!({"type": "object"}),
            )
            .expect("valid spec"),
        }
    }

    #[test]
    fn spec_rejects_unusable_values() {
        let schema = json!({"type": "object"});
        let cases = [
            ToolSpec::new("", "read a file", schema.clone()),
            ToolSpec::new("read file", "read a file", schema.clone()),
            ToolSpec::new("read", "   ", schema.clone()),
            ToolSpec::new("read", "read a file", json!(null)),
            ToolSpec::new("read", "read a file", json!("object")),
        ];
        for case in cases {
            let error = case.expect_err("spec should be rejected");
            assert_eq!(error.code(), ErrorCode::SpecInvalid);
            assert!(error.to_string().starts_with("AGT_SPEC_INVALID: "));
        }
    }

    #[test]
    fn spec_stores_the_trimmed_description() {
        let spec = echo().spec;
        assert_eq!(spec.name(), "echo");
        assert_eq!(spec.description(), "Repeat the text field.");
        assert!(spec.input_schema().is_object());
    }

    #[test]
    fn blank_call_id_is_absent() {
        assert_eq!(CallContext::new().call_id(), None);
        assert_eq!(CallContext::with_call_id("   ").call_id(), None);
        assert_eq!(
            CallContext::with_call_id(" call-1 ").call_id(),
            Some("call-1")
        );
    }

    #[test]
    fn dyn_tool_returns_model_text_or_a_contract_error() {
        let tool = echo().boxed();
        let ok = ready(tool.call(&CallContext::with_call_id("call-1"), json!({"text": "hi"})))
            .expect("call");
        assert_eq!(ok.content(), "call-1:hi");
        assert!(!ok.is_error());
        assert!(ok.images().is_empty());

        let error = ready(tool.call(&CallContext::new(), json!({}))).expect_err("missing text");
        assert_eq!(
            error.to_string(),
            "AGT_INPUT_INVALID: field \"text\" must be a non-empty string"
        );

        let seen = ToolOutput::error_text("file not found");
        assert!(seen.is_error());
        assert_eq!(seen.content(), "file not found");
        assert!(seen.images().is_empty());
    }

    #[test]
    fn text_output_has_no_images_and_is_not_an_error() {
        let output = ToolOutput::text("hello");
        assert!(output.images().is_empty());
        assert_eq!(output.content(), "hello");
        assert!(!output.is_error());
    }

    #[test]
    fn error_text_output_has_no_images_and_is_an_error() {
        let output = ToolOutput::error_text("nope");
        assert!(output.images().is_empty());
        assert!(output.is_error());
    }

    #[test]
    fn with_image_appends_one_part_and_keeps_the_text() {
        let output =
            ToolOutput::text("notice").with_image(ImagePart::new("image/png", vec![1, 2, 3]));

        assert_eq!(output.images().len(), 1);
        let image = &output.images()[0];
        assert_eq!(image.media_type(), "image/png");
        assert_eq!(image.bytes(), &[1, 2, 3]);
        assert_eq!(output.content(), "notice");
    }

    #[test]
    fn output_equality_covers_image_bytes() {
        let different_bytes =
            ToolOutput::text("notice").with_image(ImagePart::new("image/png", vec![4, 5, 6]));
        let base =
            ToolOutput::text("notice").with_image(ImagePart::new("image/png", vec![1, 2, 3]));
        assert_ne!(base, different_bytes);

        let same_bytes =
            ToolOutput::text("notice").with_image(ImagePart::new("image/png", vec![1, 2, 3]));
        assert_eq!(base, same_bytes);
    }

    #[test]
    fn with_image_keeps_call_order() {
        let first = ImagePart::new("image/png", vec![1]);
        let second = ImagePart::new("image/jpeg", vec![2]);
        let output = ToolOutput::text("two").with_image(first).with_image(second);

        let images = output.images();
        assert_eq!(images.len(), 2);
        assert_eq!(images[0].media_type(), "image/png");
        assert_eq!(images[0].bytes(), &[1]);
        assert_eq!(images[1].media_type(), "image/jpeg");
        assert_eq!(images[1].bytes(), &[2]);
    }
}
