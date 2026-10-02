use atomvar_core::Error as CoreError;
use serde::Serialize;

/// Stable, transport-independent error codes (HTTP body `error.code`, gRPC
/// message prefix, MCP error text).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    InvalidArgument,
    InvalidOrdering,
    InvalidName,
    NotFound,
    TypeMismatch,
    ArenaFull,
    LayoutMismatch,
    IdempotencyConflict,
    /// The operation may or may not have been applied. Never auto-retried.
    OutcomeUnknown,
    ResourceExhausted,
    Unauthenticated,
    Internal,
}

impl ErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InvalidArgument => "INVALID_ARGUMENT",
            Self::InvalidOrdering => "INVALID_ORDERING",
            Self::InvalidName => "INVALID_NAME",
            Self::NotFound => "NOT_FOUND",
            Self::TypeMismatch => "TYPE_MISMATCH",
            Self::ArenaFull => "ARENA_FULL",
            Self::LayoutMismatch => "LAYOUT_MISMATCH",
            Self::IdempotencyConflict => "IDEMPOTENCY_CONFLICT",
            Self::OutcomeUnknown => "OUTCOME_UNKNOWN",
            Self::ResourceExhausted => "RESOURCE_EXHAUSTED",
            Self::Unauthenticated => "UNAUTHENTICATED",
            Self::Internal => "INTERNAL",
        }
    }

    pub fn http_status(self) -> u16 {
        match self {
            Self::InvalidArgument | Self::InvalidOrdering | Self::InvalidName => 400,
            Self::Unauthenticated => 401,
            Self::NotFound => 404,
            Self::TypeMismatch | Self::LayoutMismatch | Self::IdempotencyConflict => 409,
            Self::ResourceExhausted => 429,
            Self::ArenaFull => 507,
            Self::OutcomeUnknown | Self::Internal => 500,
        }
    }

    pub fn grpc_code(self) -> tonic::Code {
        use tonic::Code;
        match self {
            Self::InvalidArgument | Self::InvalidOrdering | Self::InvalidName => {
                Code::InvalidArgument
            }
            Self::Unauthenticated => Code::Unauthenticated,
            Self::NotFound => Code::NotFound,
            Self::TypeMismatch | Self::LayoutMismatch | Self::IdempotencyConflict => {
                Code::FailedPrecondition
            }
            Self::ResourceExhausted | Self::ArenaFull => Code::ResourceExhausted,
            Self::OutcomeUnknown => Code::Unknown,
            Self::Internal => Code::Internal,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ApiError {
    pub code: ErrorCode,
    pub message: String,
}

impl ApiError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        ApiError {
            code,
            message: message.into(),
        }
    }

    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::InvalidArgument, message)
    }

    pub fn outcome_unknown(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::OutcomeUnknown, message)
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code.as_str(), self.message)
    }
}

impl std::error::Error for ApiError {}

impl From<CoreError> for ApiError {
    fn from(e: CoreError) -> Self {
        let code = match &e {
            CoreError::InvalidOrdering(_) => ErrorCode::InvalidOrdering,
            CoreError::InvalidName(_) => ErrorCode::InvalidName,
            CoreError::InvalidArgument(_) => ErrorCode::InvalidArgument,
            CoreError::ArenaFull { .. } => ErrorCode::ArenaFull,
            CoreError::TypeMismatch { .. } => ErrorCode::TypeMismatch,
            CoreError::NotFound { .. } => ErrorCode::NotFound,
            CoreError::LayoutMismatch(_) => ErrorCode::LayoutMismatch,
            CoreError::ForkedProcess | CoreError::UnsupportedCpu | CoreError::Io(_) => {
                ErrorCode::Internal
            }
        };
        ApiError::new(code, e.to_string())
    }
}

impl From<ApiError> for tonic::Status {
    fn from(e: ApiError) -> Self {
        tonic::Status::new(e.code.grpc_code(), e.to_string())
    }
}
