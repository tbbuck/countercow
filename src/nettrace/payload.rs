//! Decoding an event payload against its metadata.
//!
//! The encoding is as simple as it gets: fields concatenated in metadata order, natural width,
//! little-endian, no alignment and no length prefixes. `Object` contributes no bytes of its own —
//! its children are inlined — which is why a flattened view is the natural output.

use std::collections::HashMap;

use super::metadata::{Field, FieldType, TypeCode};
use super::reader::{ParseError, Reader, Result};

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Bool(bool),
    Char(u16),
    Int(i64),
    UInt(u64),
    Float(f64),
    String(String),
    Guid([u8; 16]),
    Decimal([u8; 16]),
    /// Year, month, day-of-week, day, hour, minute, second, millisecond.
    DateTime([i16; 8]),
    Array(Vec<Value>),
}

impl Value {
    /// Numeric value as f64, whatever integer or float width it arrived as.
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Float(v) => Some(*v),
            Value::Int(v) => Some(*v as f64),
            Value::UInt(v) => Some(*v as f64),
            Value::Bool(v) => Some(f64::from(u8::from(*v))),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Value::Int(v) => Some(*v),
            Value::UInt(v) => Some(*v as i64),
            _ => None,
        }
    }
}

/// Decode a payload into a flat map keyed by leaf field name.
///
/// Nested `Object` levels are flattened away. For the payloads countercow reads this is exactly
/// what is wanted — EventCounters wraps its fields in two anonymous struct levels, and the leaf
/// names are unique.
pub fn decode_flat(fields: &[Field], payload: &[u8]) -> Result<HashMap<String, Value>> {
    let mut out = HashMap::new();
    let mut r = Reader::new(payload);
    decode_into(fields, &mut r, &mut out)?;
    Ok(out)
}

fn decode_into(
    fields: &[Field],
    r: &mut Reader<'_>,
    out: &mut HashMap<String, Value>,
) -> Result<()> {
    for field in fields {
        match &field.ty {
            // A struct level: no bytes of its own, just keep going.
            FieldType::Object(children) => decode_into(children, r, out)?,
            ty => {
                let value = decode_value(ty, r)?;
                out.insert(field.name.clone(), value);
            }
        }
    }
    Ok(())
}

fn decode_value(ty: &FieldType, r: &mut Reader<'_>) -> Result<Value> {
    match ty {
        FieldType::Scalar(code) => decode_scalar(*code, r),
        FieldType::Array(element) => {
            // Arrays are length-prefixed with a u16 count.
            let count = r.u16()?;
            let mut values = Vec::with_capacity(count as usize);
            for _ in 0..count {
                values.push(decode_value(element, r)?);
            }
            Ok(Value::Array(values))
        }
        FieldType::Object(children) => {
            // Only reached for an object nested inside an array element.
            let mut nested = HashMap::new();
            decode_into(children, r, &mut nested)?;
            // Represent it as an array of its values; countercow never needs this shape, but
            // silently dropping bytes would be worse.
            Ok(Value::Array(nested.into_values().collect()))
        }
        // We cannot know how many bytes an unmodelled type occupies, so everything after it
        // would be misaligned. Fail rather than return plausible nonsense.
        FieldType::Unknown(code) => Err(ParseError::Unexpected {
            what: "payload field type",
            got: code.to_string(),
        }),
    }
}

fn decode_scalar(code: TypeCode, r: &mut Reader<'_>) -> Result<Value> {
    Ok(match code {
        // A .NET Boolean on the wire is a 4-byte int, not a single byte.
        TypeCode::Boolean => Value::Bool(r.i32()? != 0),
        TypeCode::Char => Value::Char(r.u16()?),
        TypeCode::SByte => Value::Int(i64::from(r.u8()? as i8)),
        TypeCode::Byte => Value::UInt(u64::from(r.u8()?)),
        TypeCode::Int16 => Value::Int(i64::from(r.i16()?)),
        TypeCode::UInt16 => Value::UInt(u64::from(r.u16()?)),
        TypeCode::Int32 => Value::Int(i64::from(r.i32()?)),
        TypeCode::UInt32 => Value::UInt(u64::from(r.u32()?)),
        TypeCode::Int64 => Value::Int(r.i64()?),
        TypeCode::UInt64 => Value::UInt(r.u64()?),
        TypeCode::Single => Value::Float(f64::from(r.f32()?)),
        TypeCode::Double => Value::Float(r.f64()?),
        TypeCode::Decimal => {
            let mut b = [0u8; 16];
            b.copy_from_slice(r.bytes(16, "decimal")?);
            Value::Decimal(b)
        }
        TypeCode::DateTime => {
            let mut parts = [0i16; 8];
            for part in &mut parts {
                *part = r.i16()?;
            }
            Value::DateTime(parts)
        }
        TypeCode::Guid => {
            let mut b = [0u8; 16];
            b.copy_from_slice(r.bytes(16, "guid")?);
            Value::Guid(b)
        }
        TypeCode::String => Value::String(r.utf16_nul_string()?),
        // Handled by the caller, which has the nested field list.
        TypeCode::Object | TypeCode::Array => {
            return Err(ParseError::Unexpected {
                what: "scalar field type",
                got: format!("{code:?}"),
            })
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scalar(name: &str, code: TypeCode) -> Field {
        Field { name: name.into(), ty: FieldType::Scalar(code) }
    }

    fn wstr(s: &str) -> Vec<u8> {
        let mut out = Vec::new();
        for unit in s.encode_utf16() {
            out.extend_from_slice(&unit.to_le_bytes());
        }
        out.extend_from_slice(&[0, 0]);
        out
    }

    #[test]
    fn decodes_scalars_at_natural_width() {
        let fields = vec![
            scalar("i", TypeCode::Int32),
            scalar("f", TypeCode::Single),
            scalar("d", TypeCode::Double),
            scalar("b", TypeCode::Boolean),
        ];
        let mut payload = Vec::new();
        payload.extend_from_slice(&(-7i32).to_le_bytes());
        payload.extend_from_slice(&1.5f32.to_le_bytes());
        payload.extend_from_slice(&2.25f64.to_le_bytes());
        payload.extend_from_slice(&1i32.to_le_bytes());

        let map = decode_flat(&fields, &payload).unwrap();
        assert_eq!(map["i"], Value::Int(-7));
        assert_eq!(map["f"].as_f64().unwrap(), 1.5);
        assert_eq!(map["d"], Value::Float(2.25));
        assert_eq!(map["b"], Value::Bool(true));
    }

    #[test]
    fn strings_are_nul_terminated_and_consume_exactly_their_bytes() {
        let fields = vec![
            scalar("name", TypeCode::String),
            scalar("after", TypeCode::Int32),
        ];
        let mut payload = wstr("cpu-usage");
        payload.extend_from_slice(&99i32.to_le_bytes());

        let map = decode_flat(&fields, &payload).unwrap();
        assert_eq!(map["name"].as_str().unwrap(), "cpu-usage");
        assert_eq!(map["after"], Value::Int(99));
    }

    #[test]
    fn nested_objects_are_flattened_and_contribute_no_bytes() {
        // The real EventCounters shape: outer anonymous struct -> "Payload" -> leaves.
        let inner = vec![
            scalar("Name", TypeCode::String),
            scalar("Mean", TypeCode::Double),
        ];
        let outer = vec![Field { name: "Payload".into(), ty: FieldType::Object(inner) }];
        let fields = vec![Field { name: String::new(), ty: FieldType::Object(outer) }];

        let mut payload = wstr("gc-heap-size");
        payload.extend_from_slice(&42.5f64.to_le_bytes());

        let map = decode_flat(&fields, &payload).unwrap();
        assert_eq!(map.len(), 2, "only leaves appear");
        assert_eq!(map["Name"].as_str().unwrap(), "gc-heap-size");
        assert_eq!(map["Mean"].as_f64().unwrap(), 42.5);
    }

    #[test]
    fn arrays_are_length_prefixed() {
        let fields = vec![Field {
            name: "values".into(),
            ty: FieldType::Array(Box::new(FieldType::Scalar(TypeCode::Int32))),
        }];
        let mut payload = 3u16.to_le_bytes().to_vec();
        for v in [1i32, 2, 3] {
            payload.extend_from_slice(&v.to_le_bytes());
        }

        let map = decode_flat(&fields, &payload).unwrap();
        assert_eq!(
            map["values"],
            Value::Array(vec![Value::Int(1), Value::Int(2), Value::Int(3)])
        );
    }

    #[test]
    fn unknown_field_type_errors_rather_than_misaligning() {
        let fields = vec![
            Field { name: "odd".into(), ty: FieldType::Unknown(77) },
            scalar("after", TypeCode::Int32),
        ];
        assert!(decode_flat(&fields, &[0u8; 8]).is_err());
    }

    #[test]
    fn truncated_payload_errors() {
        let fields = vec![scalar("d", TypeCode::Double)];
        assert!(decode_flat(&fields, &[0u8; 4]).is_err());
    }

    #[test]
    fn incrementing_counter_payload_shape_round_trips() {
        // IncrementingCounterPayload: 9 fields, Increment rather than Mean.
        let inner = vec![
            scalar("Name", TypeCode::String),
            scalar("DisplayName", TypeCode::String),
            scalar("DisplayRateTimeScale", TypeCode::String),
            scalar("Increment", TypeCode::Double),
            scalar("IntervalSec", TypeCode::Single),
            scalar("Metadata", TypeCode::String),
            scalar("Series", TypeCode::String),
            scalar("CounterType", TypeCode::String),
            scalar("DisplayUnits", TypeCode::String),
        ];
        let outer = vec![Field { name: "Payload".into(), ty: FieldType::Object(inner) }];
        let fields = vec![Field { name: String::new(), ty: FieldType::Object(outer) }];

        let mut payload = Vec::new();
        payload.extend(wstr("gen-0-gc-count"));
        payload.extend(wstr("Gen 0 GC Count"));
        payload.extend(wstr("00:01:00"));
        payload.extend_from_slice(&3.0f64.to_le_bytes());
        payload.extend_from_slice(&1.0f32.to_le_bytes());
        payload.extend(wstr(""));
        payload.extend(wstr("Interval=1000"));
        payload.extend(wstr("Sum"));
        payload.extend(wstr(""));

        let map = decode_flat(&fields, &payload).unwrap();
        assert_eq!(map["Name"].as_str().unwrap(), "gen-0-gc-count");
        assert_eq!(map["CounterType"].as_str().unwrap(), "Sum");
        assert_eq!(map["Increment"].as_f64().unwrap(), 3.0);
        assert_eq!(map["DisplayRateTimeScale"].as_str().unwrap(), "00:01:00");
        assert_eq!(map["IntervalSec"].as_f64().unwrap(), 1.0);
    }
}
