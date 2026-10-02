use crate::value::ValueType;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid memory ordering: {0}")]
    InvalidOrdering(&'static str),

    #[error("invalid name: {0}")]
    InvalidName(String),

    #[error("invalid argument: {0}")]
    InvalidArgument(String),

    #[error("arena '{arena}' is full (capacity {capacity})")]
    ArenaFull { arena: String, capacity: u32 },

    #[error("variable '{name}' exists with type {existing}, requested {requested}")]
    TypeMismatch {
        name: String,
        existing: ValueType,
        requested: ValueType,
    },

    #[error("variable '{name}' not found")]
    NotFound { name: String },

    #[error("arena layout mismatch: {0}")]
    LayoutMismatch(String),

    /// Returned in a child created by plain fork() (no exec). Carries no data so
    /// that producing it never allocates.
    #[error("atomvar used in a forked child process; open arenas in a spawned process instead")]
    ForkedProcess,

    #[error("CPU lacks CMPXCHG16B; this build of atomvar is unsupported on this machine")]
    UnsupportedCpu,

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}
