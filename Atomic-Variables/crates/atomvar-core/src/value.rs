use crate::error::{Error, Result};
use std::fmt;

/// Type tag stored in each arena slot. Values are part of the on-disk layout.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ValueType {
    I64 = 1,
    U64 = 2,
    Bool = 3,
    F64 = 4,
    U128 = 5,
}

impl ValueType {
    pub fn from_tag(tag: u8) -> Option<Self> {
        Some(match tag {
            1 => Self::I64,
            2 => Self::U64,
            3 => Self::Bool,
            4 => Self::F64,
            5 => Self::U128,
            _ => return None,
        })
    }

    pub fn parse(s: &str) -> Result<Self> {
        Ok(match s.to_ascii_lowercase().as_str() {
            "i64" | "int" | "int64" => Self::I64,
            "u64" | "uint" | "uint64" => Self::U64,
            "bool" | "boolean" => Self::Bool,
            "f64" | "float" | "double" | "float64" => Self::F64,
            "u128" | "uint128" => Self::U128,
            _ => return Err(Error::InvalidArgument(format!("unknown type '{s}'"))),
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::I64 => "i64",
            Self::U64 => "u64",
            Self::Bool => "bool",
            Self::F64 => "f64",
            Self::U128 => "u128",
        }
    }

    pub fn zero(self) -> Value {
        match self {
            Self::I64 => Value::I64(0),
            Self::U64 => Value::U64(0),
            Self::Bool => Value::Bool(false),
            Self::F64 => Value::F64(0.0),
            Self::U128 => Value::U128(0),
        }
    }
}

impl fmt::Display for ValueType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A dynamically typed value, used by the daemon and other type-erased callers.
#[derive(Clone, Copy, Debug)]
pub enum Value {
    I64(i64),
    U64(u64),
    Bool(bool),
    F64(f64),
    U128(u128),
}

impl Value {
    pub fn value_type(&self) -> ValueType {
        match self {
            Self::I64(_) => ValueType::I64,
            Self::U64(_) => ValueType::U64,
            Self::Bool(_) => ValueType::Bool,
            Self::F64(_) => ValueType::F64,
            Self::U128(_) => ValueType::U128,
        }
    }

    /// Raw 128-bit image used to initialize a slot (f64 as its exact bits).
    pub(crate) fn raw(&self) -> u128 {
        match *self {
            Self::I64(v) => v as u64 as u128,
            Self::U64(v) => v as u128,
            Self::Bool(v) => v as u128,
            Self::F64(v) => v.to_bits() as u128,
            Self::U128(v) => v,
        }
    }
}

/// Bitwise equality (f64 compared by bits, so NaN == NaN with the same payload
/// and +0.0 != -0.0), matching compare_exchange semantics.
impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        self.value_type() == other.value_type() && self.raw() == other.raw()
    }
}
impl Eq for Value {}

impl std::hash::Hash for Value {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        (self.value_type() as u8).hash(state);
        self.raw().hash(state);
    }
}
