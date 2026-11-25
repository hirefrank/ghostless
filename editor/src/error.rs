use std::fmt;

/// Typed failure for editor I/O, the CLI, and the voice bridge.
///
/// `From<EditorError> for String` exists so Rune callbacks and status text can
/// still render a message. Match on the variant instead of searching that text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum EditorError {
    Io { context: String },
    Json { context: String },
    InvalidSessionId(String),
    ProtectedPath(String),
    BaselineMismatch(String),
    MirrorConflict,
    AlreadyFinished(String),
    CliFailed(String),
    Bridge(String),
    Message(String),
}

impl EditorError {
    pub(crate) fn message(text: impl Into<String>) -> Self {
        Self::Message(text.into())
    }
}

impl fmt::Display for EditorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EditorError::Io { context }
            | EditorError::Json { context }
            | EditorError::ProtectedPath(context)
            | EditorError::BaselineMismatch(context)
            | EditorError::AlreadyFinished(context)
            | EditorError::CliFailed(context)
            | EditorError::Bridge(context)
            | EditorError::Message(context) => f.write_str(context),
            EditorError::InvalidSessionId(id) => write!(f, "Invalid session ID: {id}"),
            EditorError::MirrorConflict => f.write_str("Linked draft changed outside the editor"),
        }
    }
}

impl std::error::Error for EditorError {}

impl From<EditorError> for String {
    fn from(error: EditorError) -> Self {
        error.to_string()
    }
}

impl From<String> for EditorError {
    fn from(message: String) -> Self {
        EditorError::Message(message)
    }
}

impl From<&str> for EditorError {
    fn from(message: &str) -> Self {
        EditorError::Message(message.to_owned())
    }
}
