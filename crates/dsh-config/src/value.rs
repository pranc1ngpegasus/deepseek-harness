//! JSON-safe value model shared by patch composition and dump rendering.
//!
//! Entry and patch data reach this crate from YAML, so the model keeps mapping
//! order: JSON equality that decides whether a layer changed a row depends on
//! field order, and a dump that rearranged fields would hide a patch.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

/// A YAML number, keeping the integer spelling JSON needs.
#[derive(Debug, Clone, Copy)]
pub enum Number {
    /// An integer value.
    Int(i64),
    /// A floating-point value.
    Float(f64),
}

impl PartialEq for Number {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Int(left), Self::Int(right)) => left == right,
            (Self::Float(left), Self::Float(right)) => left == right,
            (Self::Int(left), Self::Float(right)) | (Self::Float(right), Self::Int(left)) => {
                #[allow(clippy::cast_precision_loss)]
                let left = *left as f64;
                left == *right
            }
        }
    }
}

/// A JSON-safe YAML value.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// YAML null.
    Null,
    /// A boolean.
    Bool(bool),
    /// A number.
    Number(Number),
    /// A string.
    String(String),
    /// An ordered sequence.
    Sequence(Vec<Self>),
    /// An ordered mapping.
    Mapping(Mapping),
    /// A tagged node; `js` marks an unevaluated `!!js` expression.
    Tagged(String, Box<Self>),
}

/// A mapping that keeps insertion order.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Mapping(pub Vec<(String, Value)>);

impl Mapping {
    /// An empty mapping.
    #[must_use]
    pub const fn new() -> Self {
        Self(Vec::new())
    }

    /// The value under `key`, using the last occurrence when a source repeated it.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.0
            .iter()
            .rev()
            .find(|(candidate, _)| candidate == key)
            .map(|(_, value)| value)
    }

    /// The mutable value under `key`, using the last occurrence.
    pub fn get_mut(&mut self, key: &str) -> Option<&mut Value> {
        self.0
            .iter_mut()
            .rev()
            .find(|(candidate, _)| candidate == key)
            .map(|(_, value)| value)
    }

    /// Assign `key`, replacing in place and appending a new key.
    pub fn set(&mut self, key: &str, value: Value) {
        if let Some(slot) = self.get_mut(key) {
            *slot = value;
            return;
        }
        self.0.push((key.to_string(), value));
    }

    /// Iterate over the entries in order.
    pub fn iter(&self) -> impl Iterator<Item = &(String, Value)> {
        self.0.iter()
    }
}

impl Value {
    /// The string value, when this node is a string.
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(value) => Some(value),
            _ => None,
        }
    }

    /// The sequence, when this node is a non-empty list.
    #[must_use]
    pub const fn as_sequence(&self) -> Option<&Vec<Self>> {
        match self {
            Self::Sequence(values) => Some(values),
            _ => None,
        }
    }

    /// The mutable sequence, when this node is a list.
    pub const fn as_sequence_mut(&mut self) -> Option<&mut Vec<Self>> {
        match self {
            Self::Sequence(values) => Some(values),
            _ => None,
        }
    }

    /// The mapping, when this node is one.
    #[must_use]
    pub const fn as_mapping(&self) -> Option<&Mapping> {
        match self {
            Self::Mapping(mapping) => Some(mapping),
            _ => None,
        }
    }

    /// The mutable mapping, when this node is one.
    pub const fn as_mapping_mut(&mut self) -> Option<&mut Mapping> {
        match self {
            Self::Mapping(mapping) => Some(mapping),
            _ => None,
        }
    }

    /// JavaScript truthiness, which the patch algorithm uses for `insert`,
    /// `group`, `id`, and `name`.
    #[must_use]
    pub fn is_truthy(&self) -> bool {
        match self {
            Self::Null => false,
            Self::Bool(value) => *value,
            Self::Number(Number::Int(value)) => *value != 0,
            Self::Number(Number::Float(value)) => *value != 0.0 && !value.is_nan(),
            Self::String(value) => !value.is_empty(),
            Self::Sequence(_) | Self::Mapping(_) | Self::Tagged(_, _) => true,
        }
    }

    /// The compact JSON spelling, matching the equality `JSON.stringify` gives.
    #[must_use]
    pub fn to_json(&self) -> String {
        match self {
            Self::Null => "null".to_string(),
            Self::Bool(true) => "true".to_string(),
            Self::Bool(false) => "false".to_string(),
            Self::Number(number) => number_to_json(*number),
            Self::String(value) => json_string(value),
            Self::Sequence(values) => {
                let rendered: Vec<String> = values.iter().map(Self::to_json).collect();
                format!("[{}]", rendered.join(","))
            }
            Self::Mapping(mapping) => {
                let rendered: Vec<String> = mapping
                    .iter()
                    .map(|(key, value)| format!("{}:{}", json_string(key), value.to_json()))
                    .collect();
                format!("{{{}}}", rendered.join(","))
            }
            Self::Tagged(tag, value) => {
                if tag == "js" {
                    format!(
                        "{{\"__jsExpr\":{}}}",
                        json_string(value.as_str().unwrap_or_default())
                    )
                } else {
                    value.to_json()
                }
            }
        }
    }
}

#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::float_cmp
)]
fn number_to_json(number: Number) -> String {
    match number {
        Number::Int(value) => format!("{value}"),
        Number::Float(value) => {
            if !value.is_finite() {
                return "null".to_string();
            }
            if value >= i64::MIN as f64
                && value <= i64::MAX as f64
                && value == (value as i64) as f64
                && value.abs() < 9_223_372_036_854_775_808.0
            {
                #[allow(clippy::cast_possible_truncation)]
                let integer = value as i64;
                return format!("{integer}");
            }
            let mut buffer = ryu::Buffer::new();
            let mut rendered = buffer.format_finite(value).to_string();
            if let Some((mantissa, exponent)) = rendered.split_once('e')
                && !exponent.starts_with(['+', '-'])
            {
                rendered = format!("{mantissa}e+{exponent}");
            }
            rendered
        }
    }
}

/// Escape a string as a JSON double-quoted value.
#[must_use]
pub fn json_string(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len() + 2);
    escaped.push('"');
    for character in value.chars() {
        match character {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            '\u{0008}' => escaped.push_str("\\b"),
            '\u{000c}' => escaped.push_str("\\f"),
            control if control < ' ' => {
                let _ = core::fmt::Write::write_fmt(
                    &mut escaped,
                    format_args!("\\u{:04x}", u32::from(control)),
                );
            }
            other => escaped.push(other),
        }
    }
    escaped.push('"');
    escaped
}

#[cfg(test)]
mod tests {
    use super::{Mapping, Number, Value, json_string};
    use alloc::boxed::Box;
    use alloc::string::{String, ToString};
    use alloc::vec;
    use alloc::vec::Vec;

    fn mapping(pairs: &[(&str, Value)]) -> Mapping {
        Mapping(
            pairs
                .iter()
                .map(|(key, value)| ((*key).to_string(), value.clone()))
                .collect(),
        )
    }

    #[test]
    fn mapping_set_replaces_in_place_and_appends_new_keys() {
        let mut map = mapping(&[("a", Value::Bool(true)), ("b", Value::Bool(false))]);
        map.set("a", Value::Bool(false));
        map.set("c", Value::Null);
        let keys: Vec<&str> = map.iter().map(|(key, _)| key.as_str()).collect();
        assert_eq!(keys, vec!["a", "b", "c"]);
        assert_eq!(map.get("a"), Some(&Value::Bool(false)));
    }

    #[test]
    fn truthiness_matches_javascript() {
        assert!(!Value::Null.is_truthy());
        assert!(!Value::Bool(false).is_truthy());
        assert!(!Value::Number(Number::Int(0)).is_truthy());
        assert!(!Value::Number(Number::Float(0.0)).is_truthy());
        assert!(!Value::Number(Number::Float(f64::NAN)).is_truthy());
        assert!(!Value::String(String::new()).is_truthy());
        assert!(Value::String("0".to_string()).is_truthy());
        assert!(Value::Sequence(vec![]).is_truthy());
        assert!(Value::Mapping(Mapping::new()).is_truthy());
    }

    #[test]
    fn json_rendering_matches_the_equality_the_dump_relies_on() {
        assert_eq!(Value::Null.to_json(), "null");
        assert_eq!(Value::Bool(true).to_json(), "true");
        assert_eq!(Value::Number(Number::Int(-3)).to_json(), "-3");
        assert_eq!(Value::Number(Number::Float(1.0)).to_json(), "1");
        assert_eq!(Value::Number(Number::Float(1.5)).to_json(), "1.5");
        assert_eq!(Value::Number(Number::Float(f64::NAN)).to_json(), "null");
        assert_eq!(
            Value::String("a\"b\\c\n".to_string()).to_json(),
            "\"a\\\"b\\\\c\\n\""
        );
        assert_eq!(
            Value::Sequence(vec![Value::Null, Value::Bool(false)]).to_json(),
            "[null,false]"
        );
        assert_eq!(
            Value::Mapping(mapping(&[("b", Value::Bool(true)), ("a", Value::Null)])).to_json(),
            "{\"b\":true,\"a\":null}"
        );
    }

    #[test]
    fn js_expressions_keep_their_inert_representation() {
        let tagged = Value::Tagged(
            "js".to_string(),
            Box::new(Value::String("process.env.X".to_string())),
        );
        assert_eq!(tagged.to_json(), "{\"__jsExpr\":\"process.env.X\"}");
        assert_eq!(json_string("x"), "\"x\"");
        assert_eq!(json_string("\u{1}"), "\"\\u0001\"");
    }
}
