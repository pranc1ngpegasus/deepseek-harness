//! YAML and JSON reading for entry lists and patch lists.
//!
//! The entry-list dialect is the include plugin's: a top-level array whose
//! `!!js` scalars stay inert until the loader evaluates them. Saphyr resolves
//! core tags, so `!!js` is rewritten to the equivalent local tag before
//! parsing and mapped back into the value model.

use std::path::Path;

use dsh_config::{Mapping, Number, Patch, Value, parse_patch_list};
use saphyr::{LoadableYamlNode, Scalar, Yaml};

/// Read and parse one YAML or JSON file into a value.
///
/// # Errors
/// Returns a diagnostic naming the path when the file cannot be read, parsed,
/// or represented as JSON-safe data.
pub fn read_value(path: &Path) -> Result<Value, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("dsh: failed to read config {}: {error}", path.display()))?;
    parse_value(path, &text)
}

fn parse_value(path: &Path, text: &str) -> Result<Value, String> {
    if path
        .extension()
        .is_some_and(|extension| extension == "json")
    {
        let parsed: serde_json::Value = serde_json::from_str(text)
            .map_err(|error| format!("dsh: failed to parse config {}: {error}", path.display()))?;
        json_to_value(&parsed).ok_or_else(|| {
            format!(
                "dsh: config {} holds an unsupported JSON value",
                path.display()
            )
        })
    } else {
        parse_yaml(text)
            .map_err(|error| format!("dsh: failed to parse config {}: {error}", path.display()))
    }
}

fn json_to_value(value: &serde_json::Value) -> Option<Value> {
    Some(match value {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(value) => Value::Bool(*value),
        serde_json::Value::Number(number) => {
            if let Some(value) = number.as_i64() {
                Value::Number(Number::Int(value))
            } else {
                Value::Number(Number::Float(number.as_f64()?))
            }
        }
        serde_json::Value::String(value) => Value::String(value.clone()),
        serde_json::Value::Array(values) => {
            Value::Sequence(values.iter().map(json_to_value).collect::<Option<_>>()?)
        }
        serde_json::Value::Object(object) => Value::Mapping(Mapping(
            object
                .iter()
                .map(|(key, value)| Some((key.clone(), json_to_value(value)?)))
                .collect::<Option<_>>()?,
        )),
    })
}

/// Parse one YAML document into the value model.
///
/// # Errors
/// Returns the parser's message, a multi-document error, or the node that has
/// no JSON-safe representation.
pub fn parse_yaml(text: &str) -> Result<Value, String> {
    let rewritten = rewrite_js_tags(text);
    let documents = Yaml::load_from_str(&rewritten).map_err(|error| error.to_string())?;
    let mut documents = documents.into_iter();
    let document = documents
        .next()
        .ok_or_else(|| "document is empty".to_string())?;
    if documents.next().is_some() {
        return Err("expected a single YAML document".to_string());
    }
    from_yaml(&document)
}

/// Read a top-level entry list.
///
/// # Errors
/// Returns a diagnostic when the document is not an array.
pub fn read_entry_list(path: &Path) -> Result<Vec<Value>, String> {
    let value = read_value(path)?;
    match value {
        Value::Sequence(entries) => Ok(entries),
        _ => Err(format!(
            "dsh: config {} must be a top-level YAML array of entries",
            path.display()
        )),
    }
}

/// Read and parse one patch list, labeled for diagnostics.
///
/// # Errors
/// Returns a diagnostic when the document is not an array or an entry cannot
/// be applied.
pub fn read_patch_list(path: &Path, label: &str) -> Result<Vec<Patch>, String> {
    let entries = read_entry_list(path)?;
    parse_patch_list("dsh", &path.display().to_string(), label, &entries)
        .map_err(|error| error.message)
}

fn from_yaml(node: &Yaml<'_>) -> Result<Value, String> {
    match node {
        Yaml::Value(scalar) => Ok(match scalar {
            Scalar::Null => Value::Null,
            Scalar::Boolean(value) => Value::Bool(*value),
            Scalar::Integer(value) => Value::Number(Number::Int(*value)),
            Scalar::FloatingPoint(value) => Value::Number(Number::Float(**value)),
            Scalar::String(value) => Value::String(value.to_string()),
        }),
        Yaml::Sequence(values) => Ok(Value::Sequence(
            values
                .iter()
                .map(from_yaml)
                .collect::<Result<Vec<_>, _>>()?,
        )),
        Yaml::Mapping(mapping) => {
            let mut pairs = Vec::with_capacity(mapping.len());
            for (key, value) in mapping {
                pairs.push((key_string(key)?, from_yaml(value)?));
            }
            Ok(Value::Mapping(Mapping(pairs)))
        }
        Yaml::Tagged(tag, value) => {
            let inner = from_yaml(value)?;
            let name = if tag.suffix == "js" {
                "js".to_string()
            } else {
                format!("{}{}", tag.handle, tag.suffix)
            };
            Ok(Value::Tagged(name, Box::new(inner)))
        }
        Yaml::Representation(raw, _, _) => Ok(Value::String(raw.to_string())),
        Yaml::Alias(_) => Err("YAML aliases are not supported in config files".to_string()),
        Yaml::BadValue => Err("invalid YAML value".to_string()),
    }
}

fn key_string(node: &Yaml<'_>) -> Result<String, String> {
    match node {
        Yaml::Value(Scalar::String(value)) => Ok(value.to_string()),
        Yaml::Value(Scalar::Integer(value)) => Ok(value.to_string()),
        Yaml::Value(Scalar::Boolean(value)) => Ok(value.to_string()),
        Yaml::Value(Scalar::Null) => Ok("null".to_string()),
        _ => Err("mapping keys must be scalars".to_string()),
    }
}

/// Rewrite `!!js` tags to the local `!js` tag, outside quoted scalars.
fn rewrite_js_tags(input: &str) -> String {
    let mut iterator = input.chars().peekable();
    let mut output = String::with_capacity(input.len());
    let mut in_single = false;
    let mut in_double = false;
    while let Some(character) = iterator.next() {
        if in_single {
            output.push(character);
            if character == '\'' {
                in_single = false;
            }
            continue;
        }
        if in_double {
            output.push(character);
            if character == '\\' {
                if let Some(next) = iterator.next() {
                    output.push(next);
                }
            } else if character == '"' {
                in_double = false;
            }
            continue;
        }
        match character {
            '\'' => {
                in_single = true;
                output.push(character);
            }
            '"' => {
                in_double = true;
                output.push(character);
            }
            '!' => {
                let mut look = iterator.clone();
                let is_js = look.next() == Some('!')
                    && look.next() == Some('j')
                    && look.next() == Some('s');
                let before_ok = output
                    .chars()
                    .last()
                    .is_none_or(|previous| previous.is_whitespace() || ":,-[".contains(previous));
                let after_ok = look
                    .next()
                    .is_none_or(|next| next.is_whitespace() || "]},".contains(next));
                if is_js && before_ok && after_ok {
                    output.push_str("!js");
                    iterator.next();
                    iterator.next();
                    iterator.next();
                } else {
                    output.push(character);
                }
            }
            _ => output.push(character),
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::{parse_yaml, rewrite_js_tags};
    use dsh_config::{Number, Value};

    #[test]
    fn parses_scalars_sequences_and_ordered_mappings() {
        let value = parse_yaml("b: 1\na: [true, null, 1.5, text]\n").expect("parse");
        let mapping = value.as_mapping().expect("mapping");
        let keys: Vec<&str> = mapping.iter().map(|(key, _)| key.as_str()).collect();
        assert_eq!(keys, vec!["b", "a"]);
        assert_eq!(
            mapping.get("a"),
            Some(&Value::Sequence(vec![
                Value::Bool(true),
                Value::Null,
                Value::Number(Number::Float(1.5)),
                Value::String("text".to_string()),
            ]))
        );
    }

    #[test]
    fn keeps_js_expressions_inert() {
        let value = parse_yaml("config:\n  path: !!js process.env.DSH_HOME\n").expect("parse");
        let config = value
            .as_mapping()
            .and_then(|mapping| mapping.get("config"))
            .and_then(Value::as_mapping)
            .expect("config");
        assert_eq!(
            config.get("path"),
            Some(&Value::Tagged(
                "js".to_string(),
                Box::new(Value::String("process.env.DSH_HOME".to_string()))
            ))
        );
    }

    #[test]
    fn the_js_rewrite_respects_quoted_scalars() {
        assert_eq!(rewrite_js_tags("a: !!js x"), "a: !js x");
        assert_eq!(rewrite_js_tags("- !!js x"), "- !js x");
        assert_eq!(rewrite_js_tags("a: \"!!js x\""), "a: \"!!js x\"");
        assert_eq!(rewrite_js_tags("a: '!!js x'"), "a: '!!js x'");
        assert_eq!(rewrite_js_tags("a: x!!js"), "a: x!!js");
        assert_eq!(rewrite_js_tags("a: !!javascript x"), "a: !!javascript x");
    }

    #[test]
    fn rejects_multiple_documents_and_empty_input() {
        assert!(parse_yaml("a: 1\n---\nb: 2\n").is_err());
        assert!(parse_yaml("").is_err());
    }
}

#[cfg(test)]
mod extra_tests {
    use super::{read_value, rewrite_js_tags};
    use dsh_config::{Number, Value};

    #[test]
    fn json_configs_parse_into_the_same_model() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("config.json");
        std::fs::write(&path, r#"{"a":[1,true,null],"b":"x"}"#).expect("write");
        let value = read_value(&path).expect("json");
        let mapping = value.as_mapping().expect("mapping");
        assert_eq!(
            mapping.get("a"),
            Some(&Value::Sequence(vec![
                Value::Number(Number::Int(1)),
                Value::Bool(true),
                Value::Null,
            ]))
        );
        assert_eq!(mapping.get("b"), Some(&Value::String("x".to_string())));
    }

    #[test]
    fn scalar_mapping_keys_become_strings() {
        let value = super::parse_yaml("1: a\ntrue: b\nnull: c\n").expect("parse");
        let mapping = value.as_mapping().expect("mapping");
        let keys: Vec<&str> = mapping.iter().map(|(key, _)| key.as_str()).collect();
        assert_eq!(keys, vec!["1", "true", "null"]);
    }

    #[test]
    fn structured_mapping_keys_are_rejected() {
        assert!(super::parse_yaml("[a]: b\n").is_err());
    }

    #[test]
    fn the_js_rewrite_covers_the_boundaries_and_quoted_forms() {
        assert_eq!(rewrite_js_tags("!!js x"), "!js x");
        assert_eq!(rewrite_js_tags("a: [!!js x]"), "a: [!js x]");
        assert_eq!(rewrite_js_tags("a: !!jsx"), "a: !!jsx");
        assert_eq!(rewrite_js_tags("a: !js x"), "a: !js x");
        assert_eq!(rewrite_js_tags("a: \"x\\\"!!js y\""), "a: \"x\\\"!!js y\"");
        assert_eq!(rewrite_js_tags("a: 'it''s !!js x'"), "a: 'it''s !!js x'");
        assert_eq!(rewrite_js_tags("a: \"x !!js y\""), "a: \"x !!js y\"");
        assert_eq!(rewrite_js_tags("a: 'x !!js y'"), "a: 'x !!js y'");
        assert_eq!(
            rewrite_js_tags("a: '!!js x' && !!js y"),
            "a: '!!js x' && !js y"
        );
        assert_eq!(
            rewrite_js_tags("a: \"!!js x\" !!js y"),
            "a: \"!!js x\" !js y"
        );
    }

    #[test]
    fn js_expressions_inside_sequences_stay_inert() {
        let value = super::parse_yaml("list:\n  - !!js a\n  - plain\n").expect("parse");
        let list = value
            .as_mapping()
            .and_then(|mapping| mapping.get("list"))
            .and_then(Value::as_sequence)
            .expect("list");
        assert_eq!(
            list[0],
            Value::Tagged("js".to_string(), Box::new(Value::String("a".to_string())))
        );
        assert_eq!(list[1], Value::String("plain".to_string()));
    }
}
