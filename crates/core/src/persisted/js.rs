//! JavaScript value semantics the stored JSON was written and validated under.

use serde::{Deserialize, Deserializer, Serializer};
use serde_json::{Map, Value};

/// Largest integer a JS number holds exactly (`Number.MAX_SAFE_INTEGER`).
pub const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

/// A JS number as `JSON.stringify` writes it: integral values without a fraction.
pub fn num<S: Serializer>(value: &f64, serializer: S) -> Result<S::Ok, S::Error> {
    if !value.is_finite() {
        return serializer.serialize_unit();
    }
    if value.fract() == 0.0 {
        if value.abs() < 9.0e18 {
            return serializer.serialize_i64(*value as i64);
        }
        if *value > 0.0 && *value < 1.8e19 {
            return serializer.serialize_u64(*value as u64);
        }
    }
    serializer.serialize_f64(*value)
}

/// [`num`] for optional fields; pair with `skip_serializing_if = "Option::is_none"`.
pub fn opt_num<S: Serializer>(value: &Option<f64>, serializer: S) -> Result<S::Ok, S::Error> {
    match value {
        Some(value) => num(value, serializer),
        None => serializer.serialize_none(),
    }
}

/// Serializes a map of JS numbers.
pub fn num_map<S: Serializer>(
    value: &std::collections::BTreeMap<String, f64>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    use serde::ser::SerializeMap;
    let mut map = serializer.serialize_map(Some(value.len()))?;
    for (key, number) in value {
        map.serialize_entry(key, &Num(*number))?;
    }
    map.end()
}

/// Optional [`num_map`].
pub fn opt_num_map<S: Serializer>(
    value: &Option<std::collections::BTreeMap<String, f64>>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    match value {
        Some(value) => num_map(value, serializer),
        None => serializer.serialize_none(),
    }
}

/// A JS number that serializes as `JSON.stringify` would.
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd, Default, Deserialize)]
#[serde(transparent)]
pub struct Num(pub f64);

impl serde::Serialize for Num {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        num(&self.0, serializer)
    }
}

/// `true` flags (`field?: true`): present only when set.
pub fn is_false(value: &bool) -> bool {
    !*value
}

/// Accepts `true` for a `field?: true` flag; anything else reads as unset.
pub fn flag<'de, D: Deserializer<'de>>(deserializer: D) -> Result<bool, D::Error> {
    Ok(matches!(Value::deserialize(deserializer)?, Value::Bool(true)))
}

/// A string field the stored record does not check: anything other than a string reads as unset.
pub fn opt_string<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<String>, D::Error> {
    Ok(match Value::deserialize(deserializer)? {
        Value::String(text) => Some(text),
        _ => None,
    })
}

/// `ToNumber` on a JSON value.
pub fn to_number(value: &Value) -> f64 {
    match value {
        Value::Null => 0.0,
        Value::Bool(flag) => f64::from(u8::from(*flag)),
        Value::Number(number) => number.as_f64().unwrap_or(f64::NAN),
        Value::String(text) => string_to_number(text),
        Value::Array(items) => match items.as_slice() {
            [] => 0.0,
            [Value::Null] => 0.0,
            [Value::String(text)] => string_to_number(text),
            [item @ (Value::Number(_) | Value::Bool(_))] => string_to_number(&item.to_string()),
            [Value::Array(_)] => to_number(&items[0]),
            _ => f64::NAN,
        },
        Value::Object(_) => f64::NAN,
    }
}

fn string_to_number(text: &str) -> f64 {
    let text = trim(text);
    if text.is_empty() {
        return 0.0;
    }
    let radix = |prefix: [&str; 2], radix: u32| {
        prefix.iter().find_map(|prefix| text.strip_prefix(prefix)).map(|digits| {
            u64::from_str_radix(digits, radix).map(|value| value as f64).unwrap_or(f64::NAN)
        })
    };
    if let Some(value) = radix(["0x", "0X"], 16).or_else(|| radix(["0o", "0O"], 8)).or_else(|| radix(["0b", "0B"], 2)) {
        return value;
    }
    match text {
        "Infinity" | "+Infinity" => f64::INFINITY,
        "-Infinity" => f64::NEG_INFINITY,
        _ if text.bytes().all(|byte| byte.is_ascii_digit() || b"+-.eE".contains(&byte)) => text.parse().unwrap_or(f64::NAN),
        _ => f64::NAN,
    }
}

/// The number a JSON value holds, as JS reads it.
pub fn number(value: &Value) -> Option<f64> {
    value.as_f64()
}

/// The JSON `JSON.stringify` writes for a number: integral values without a fraction.
pub fn number_value(value: f64) -> Value {
    if value.is_finite() && value.fract() == 0.0 && value.abs() < 9.0e18 {
        Value::from(value as i64)
    } else if value.is_finite() && value.fract() == 0.0 && value > 0.0 && value < 1.8e19 {
        Value::from(value as u64)
    } else {
        serde_json::Number::from_f64(value).map(Value::Number).unwrap_or(Value::Null)
    }
}

/// `typeof value === "number" && Number.isFinite(value)`. JSON numbers are always finite.
pub fn finite(value: Option<&Value>) -> bool {
    matches!(value, Some(Value::Number(_)))
}

/// `Number.isSafeInteger(value)`.
pub fn safe_integer(value: Option<&Value>) -> bool {
    match value {
        Some(Value::Number(number)) => {
            if number.is_i64() || number.is_u64() {
                number.as_f64().is_some_and(|value| value.abs() <= MAX_SAFE_INTEGER)
            } else {
                number.as_f64().is_some_and(|value| value.fract() == 0.0 && value.abs() <= MAX_SAFE_INTEGER)
            }
        }
        _ => false,
    }
}

/// `Number.isInteger(value)`.
pub fn integer(value: Option<&Value>) -> bool {
    match value {
        Some(Value::Number(number)) => number.is_i64() || number.is_u64() || number.as_f64().is_some_and(|value| value.fract() == 0.0),
        _ => false,
    }
}

/// Whitespace as `String.prototype.trim` and the regex `\s` class read it.
pub fn is_js_whitespace(character: char) -> bool {
    character.is_whitespace() || character == '\u{FEFF}'
}

/// `String.prototype.trim`.
pub fn trim(text: &str) -> &str {
    text.trim_matches(is_js_whitespace)
}

/// A string's `length` in JS: UTF-16 code units.
pub fn length(text: &str) -> usize {
    text.encode_utf16().count()
}

/// `typeof value === "string" && value.trim().length > 0`.
pub fn non_empty_string(value: Option<&Value>) -> bool {
    matches!(value, Some(Value::String(text)) if !trim(text).is_empty())
}

/// `typeof value === "string"`.
pub fn string(value: Option<&Value>) -> bool {
    matches!(value, Some(Value::String(_)))
}

/// A plain object, as `isRecord` checks it.
pub fn record(value: Option<&Value>) -> Option<&Map<String, Value>> {
    match value {
        Some(Value::Object(map)) => Some(map),
        _ => None,
    }
}

/// `value === true`.
pub fn is_true(value: Option<&Value>) -> bool {
    matches!(value, Some(Value::Bool(true)))
}

/// `value === undefined`: the key is absent. A stored `null` is a value.
pub fn absent(value: Option<&Value>) -> bool {
    value.is_none()
}

/// JS truthiness.
pub fn truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(flag)) => *flag,
        Some(Value::Number(number)) => number.as_f64().is_some_and(|value| value != 0.0 && !value.is_nan()),
        Some(Value::String(text)) => !text.is_empty(),
        Some(_) => true,
    }
}

/// Names every JS object inherits, which `Object.hasOwn(Object.prototype, name)` is true for.
pub const OBJECT_PROTOTYPE_KEYS: [&str; 12] = [
    "constructor",
    "__defineGetter__",
    "__defineSetter__",
    "hasOwnProperty",
    "__lookupGetter__",
    "__lookupSetter__",
    "isPrototypeOf",
    "propertyIsEnumerable",
    "toString",
    "valueOf",
    "__proto__",
    "toLocaleString",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_serialize_like_json_stringify() {
        assert_eq!(serde_json::to_string(&Num(1789345723590.0)).unwrap(), "1789345723590");
        assert_eq!(serde_json::to_string(&Num(-0.0)).unwrap(), "0");
        assert_eq!(serde_json::to_string(&Num(1.5)).unwrap(), "1.5");
        assert_eq!(serde_json::to_string(&Num(9007199254740992.0)).unwrap(), "9007199254740992");
    }

    #[test]
    fn trim_matches_js() {
        assert_eq!(trim("\u{FEFF} a \u{2028}"), "a");
        assert_eq!(length("😀"), 2);
    }
}
