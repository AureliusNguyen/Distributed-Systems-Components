//! JSON wire format (HTTP and MCP).
//!
//! - i64 / u64 / u128 are emitted as decimal STRINGS (JavaScript and many JSON
//!   parsers lose precision above 2^53, for signed values too). On input, a
//!   JSON integer that fits or a decimal string is accepted ("0x" hex too for
//!   unsigned types).
//! - bool is a JSON bool.
//! - f64 is emitted as {"value": "<decimal | NaN | Infinity | -Infinity>",
//!   "bits": "0x<16 hex digits>"}. On input, "bits" (if present) is
//!   authoritative, which is the only way to pass an exact NaN payload; a plain
//!   JSON number or decimal/special string is also accepted.

use crate::error::ApiError;
use atomvar_core::{Value, ValueType};
use serde_json::{json, Value as Json};

pub fn fmt_f64(v: f64) -> String {
    if v.is_nan() {
        "NaN".into()
    } else if v == f64::INFINITY {
        "Infinity".into()
    } else if v == f64::NEG_INFINITY {
        "-Infinity".into()
    } else {
        format!("{v:?}") // shortest round-trip representation
    }
}

pub fn encode(v: &Value) -> Json {
    match *v {
        Value::I64(x) => Json::String(x.to_string()),
        Value::U64(x) => Json::String(x.to_string()),
        Value::U128(x) => Json::String(x.to_string()),
        Value::Bool(b) => Json::Bool(b),
        Value::F64(f) => json!({ "value": fmt_f64(f), "bits": format!("0x{:016x}", f.to_bits()) }),
    }
}

fn parse_unsigned<T: TryFrom<u128>>(s: &str) -> Option<T> {
    let s = s.trim();
    let v = if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u128::from_str_radix(hex, 16).ok()?
    } else {
        s.parse::<u128>().ok()?
    };
    T::try_from(v).ok()
}

fn parse_f64_str(s: &str) -> Option<f64> {
    match s.trim() {
        "NaN" | "nan" => Some(f64::NAN),
        "Infinity" | "inf" | "+Infinity" => Some(f64::INFINITY),
        "-Infinity" | "-inf" => Some(f64::NEG_INFINITY),
        other => other.parse::<f64>().ok(),
    }
}

/// Decodes a JSON value into a typed value for a variable of type `ty`.
pub fn decode(j: &Json, ty: ValueType, field: &str) -> Result<Value, ApiError> {
    let bad = || {
        ApiError::invalid(format!(
            "field '{field}': {j} is not a valid {ty} (integers may be JSON numbers or decimal strings)"
        ))
    };
    Ok(match ty {
        ValueType::I64 => Value::I64(match j {
            Json::Number(n) => n.as_i64().ok_or_else(bad)?,
            Json::String(s) => s.trim().parse::<i64>().map_err(|_| bad())?,
            _ => return Err(bad()),
        }),
        ValueType::U64 => Value::U64(match j {
            Json::Number(n) => n.as_u64().ok_or_else(bad)?,
            Json::String(s) => parse_unsigned::<u64>(s).ok_or_else(bad)?,
            _ => return Err(bad()),
        }),
        ValueType::U128 => Value::U128(match j {
            Json::Number(n) => n.as_u64().ok_or_else(bad)? as u128,
            Json::String(s) => parse_unsigned::<u128>(s).ok_or_else(bad)?,
            _ => return Err(bad()),
        }),
        ValueType::Bool => Value::Bool(match j {
            Json::Bool(b) => *b,
            Json::String(s) if s == "true" => true,
            Json::String(s) if s == "false" => false,
            _ => return Err(bad()),
        }),
        ValueType::F64 => Value::F64(match j {
            Json::Number(n) => n.as_f64().ok_or_else(bad)?,
            Json::String(s) => parse_f64_str(s).ok_or_else(bad)?,
            Json::Object(o) => {
                if let Some(bits) = o.get("bits") {
                    let bits = match bits {
                        Json::String(s) => parse_unsigned::<u64>(s),
                        Json::Number(n) => n.as_u64(),
                        _ => None,
                    }
                    .ok_or_else(bad)?;
                    f64::from_bits(bits)
                } else if let Some(v) = o.get("value") {
                    match v {
                        Json::Number(n) => n.as_f64().ok_or_else(bad)?,
                        Json::String(s) => parse_f64_str(s).ok_or_else(bad)?,
                        _ => return Err(bad()),
                    }
                } else {
                    return Err(bad());
                }
            }
            _ => return Err(bad()),
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rt(v: Value) {
        let back = decode(&encode(&v), v.value_type(), "x").unwrap();
        assert_eq!(back, v, "{v:?}"); // Value equality is bitwise for f64
    }

    #[test]
    fn round_trips_are_exact() {
        rt(Value::I64(i64::MIN));
        rt(Value::I64(i64::MAX));
        rt(Value::U64(u64::MAX));
        rt(Value::U128(u128::MAX));
        rt(Value::Bool(true));
        rt(Value::F64(-0.0));
        rt(Value::F64(f64::from_bits(0x7ff8_0000_0000_0001)));
        rt(Value::F64(f64::from_bits(0xfff8_dead_beef_0001)));
        rt(Value::F64(f64::INFINITY));
        rt(Value::F64(f64::NEG_INFINITY));
        rt(Value::F64(1e-310)); // subnormal
        rt(Value::F64(0.1));
    }

    #[test]
    fn encodes_big_ints_as_strings() {
        assert_eq!(encode(&Value::I64(i64::MIN)), json!("-9223372036854775808"));
        assert_eq!(encode(&Value::U128(u128::MAX)), json!(u128::MAX.to_string()));
        assert_eq!(encode(&Value::F64(-0.0)), json!({"value": "-0.0", "bits": "0x8000000000000000"}));
    }

    #[test]
    fn decode_accepts_numbers_and_rejects_bad_input() {
        assert_eq!(decode(&json!(5), ValueType::I64, "v").unwrap(), Value::I64(5));
        assert_eq!(decode(&json!("0xff"), ValueType::U64, "v").unwrap(), Value::U64(255));
        assert_eq!(decode(&json!(1.5), ValueType::F64, "v").unwrap(), Value::F64(1.5));
        assert!(decode(&json!(1.5), ValueType::I64, "v").is_err());
        assert!(decode(&json!(-1), ValueType::U64, "v").is_err());
        assert!(decode(&json!("18446744073709551616"), ValueType::U64, "v").is_err());
        assert!(decode(&json!(9223372036854775808u64), ValueType::I64, "v").is_err());
        assert!(decode(&json!("x"), ValueType::Bool, "v").is_err());
        assert!(decode(&json!({}), ValueType::F64, "v").is_err());
    }
}
