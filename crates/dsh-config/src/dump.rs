//! Config dump composition: apply layers, attribute rows to their sources, render.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::value::Value;
use crate::{Patch, PatchWarning, apply_entry_patches};

/// One overlay layer with the source label printed in dump comments.
#[derive(Debug, Clone, PartialEq)]
pub struct Layer {
    /// Source name shown in the dump comment.
    pub label: String,
    /// The layer's patches, in application order.
    pub patches: Vec<Patch>,
}

/// A skipped-patch diagnostic attributed to the layer that supplied the patch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DumpWarning {
    /// The layer's source label.
    pub label: String,
    /// What the patch skipped.
    pub warning: PatchWarning,
}

/// One composed row with its source and the layers that changed it.
#[derive(Debug, Clone)]
struct Origin {
    source: String,
    patched_by: Vec<String>,
}

impl Origin {
    fn label(&self) -> String {
        if self.patched_by.is_empty() {
            self.source.clone()
        } else {
            format!("{}, patched by {}", self.source, self.patched_by.join(", "))
        }
    }
}

/// Compose the configured entry list the way the boot include does, then
/// render it with a source comment before each run of rows.
///
/// Every layer is applied as one flattened list over the same base, so a later
/// layer that targets a group child an earlier layer introduced composes
/// identically to a boot. Warnings are reported once, attributed to the layer
/// that first introduced them, and only for layers whose patches added them.
#[must_use]
pub fn render_config_dump(
    base_label: &str,
    base: &[Value],
    layers: &[Layer],
) -> (String, Vec<DumpWarning>) {
    let mut origins: Vec<Origin> = base
        .iter()
        .map(|_| Origin {
            source: base_label.to_string(),
            patched_by: Vec::new(),
        })
        .collect();
    let mut previous_json: Vec<String> = base.iter().map(Value::to_json).collect();
    let mut previous_warning_count = 0;
    let mut warnings = Vec::new();
    let mut composed = base.to_vec();

    for count in 1..=layers.len() {
        let layer = &layers[count - 1];
        let flattened: Vec<Patch> = layers[..count]
            .iter()
            .flat_map(|layer| layer.patches.iter().cloned())
            .collect();
        let (next, layer_warnings) = apply_entry_patches(base, &flattened);
        let new_warnings = layer_warnings.get(previous_warning_count..).unwrap_or(&[]);
        for warning in new_warnings {
            warnings.push(DumpWarning {
                label: layer.label.clone(),
                warning: warning.clone(),
            });
        }
        for (index, row) in next.iter().enumerate() {
            if index >= previous_json.len() {
                origins.push(Origin {
                    source: layer.label.clone(),
                    patched_by: Vec::new(),
                });
            } else if row.to_json() != previous_json[index]
                && let Some(origin) = origins.get_mut(index)
            {
                origin.patched_by.push(layer.label.clone());
            }
        }
        previous_json = next.iter().map(Value::to_json).collect();
        previous_warning_count = layer_warnings.len();
        composed = next;
    }

    (grouped_dump(&composed, &origins), warnings)
}

/// Render rows grouped under one source-and-patches comment per contiguous run.
fn grouped_dump(composed: &[Value], origins: &[Origin]) -> String {
    let mut blocks: Vec<String> = Vec::new();
    let mut current: Option<String> = None;
    let mut group: Vec<Value> = Vec::new();
    for (index, row) in composed.iter().enumerate() {
        let Some(origin) = origins.get(index) else {
            continue;
        };
        let label = origin.label();
        if current.as_deref() != Some(label.as_str()) {
            flush(&mut blocks, current.take(), &mut group);
            current = Some(label);
        }
        group.push(row.clone());
    }
    flush(&mut blocks, current, &mut group);
    let mut rendered = blocks.join("\n");
    rendered.push('\n');
    rendered
}

fn flush(blocks: &mut Vec<String>, label: Option<String>, group: &mut Vec<Value>) {
    let Some(label) = label else {
        return;
    };
    if group.is_empty() {
        return;
    }
    let mut block = format!("# == {label}");
    for line in block_lines(group, 0) {
        block.push('\n');
        block.push_str(&line);
    }
    blocks.push(block);
    group.clear();
}

/// Render entries as YAML lines, each carrying its own indentation.
fn block_lines(entries: &[Value], indent: usize) -> Vec<String> {
    let mut lines = Vec::new();
    for entry in entries {
        if let Some(text) = inline(entry) {
            lines.push(format!("{}- {text}", padding(indent)));
        } else {
            let mut nested = value_lines(entry, indent + 2);
            let first = nested.remove(0);
            lines.push(format!("{}- {}", padding(indent), first.trim_start()));
            lines.extend(nested);
        }
    }
    lines
}

fn value_lines(value: &Value, indent: usize) -> Vec<String> {
    let pad = padding(indent);
    match value {
        Value::Mapping(mapping) => {
            let mut lines = Vec::new();
            for (key, value) in mapping.iter() {
                if let Some(text) = inline(value) {
                    lines.push(format!("{pad}{}: {text}", yaml_key(key)));
                } else {
                    lines.push(format!("{pad}{}:", yaml_key(key)));
                    lines.extend(value_lines(value, indent + 2));
                }
            }
            lines
        }
        Value::Sequence(values) => {
            let mut lines = Vec::new();
            for value in values {
                if let Some(text) = inline(value) {
                    lines.push(format!("{pad}- {text}"));
                } else {
                    let mut nested = value_lines(value, indent + 2);
                    let first = nested.remove(0);
                    lines.push(format!("{pad}- {}", first.trim_start()));
                    lines.extend(nested);
                }
            }
            lines
        }
        other => alloc::vec![format!("{pad}{}", inline(other).unwrap_or_default())],
    }
}

fn padding(indent: usize) -> String {
    " ".repeat(indent)
}

/// The single-line spelling of a value, or `None` for a non-empty collection.
fn inline(value: &Value) -> Option<String> {
    match value {
        Value::Null => Some("null".to_string()),
        Value::Bool(true) => Some("true".to_string()),
        Value::Bool(false) => Some("false".to_string()),
        Value::Number(_) => Some(value.to_json()),
        Value::String(text) => Some(yaml_scalar(text)),
        Value::Sequence(values) if values.is_empty() => Some("[]".to_string()),
        Value::Mapping(mapping) if mapping.0.is_empty() => Some("{}".to_string()),
        Value::Sequence(_) | Value::Mapping(_) => None,
        Value::Tagged(tag, inner) => Some(format!(
            "!!{tag} {}",
            inline(inner).unwrap_or_else(|| "null".to_string())
        )),
    }
}

/// Quote a string unless a plain scalar round-trips unambiguously.
fn yaml_scalar(text: &str) -> String {
    if is_plain_safe(text) {
        return text.to_string();
    }
    let mut quoted = String::with_capacity(text.len() + 2);
    quoted.push('"');
    for character in text.chars() {
        match character {
            '"' => quoted.push_str("\\\""),
            '\\' => quoted.push_str("\\\\"),
            '\n' => quoted.push_str("\\n"),
            '\r' => quoted.push_str("\\r"),
            '\t' => quoted.push_str("\\t"),
            control if control < ' ' => {
                let _ = core::fmt::Write::write_fmt(
                    &mut quoted,
                    format_args!("\\u{:04x}", u32::from(control)),
                );
            }
            other => quoted.push(other),
        }
    }
    quoted.push('"');
    quoted
}

fn yaml_key(text: &str) -> String {
    yaml_scalar(text)
}

fn is_plain_safe(text: &str) -> bool {
    if text.is_empty() || text.trim() != text {
        return false;
    }
    if !text
        .chars()
        .next()
        .is_some_and(|first| first.is_ascii_alphanumeric() || first == '_')
    {
        return false;
    }
    if text
        .chars()
        .any(|character| !(character.is_ascii_alphanumeric() || "_.-/@+ ".contains(character)))
    {
        return false;
    }
    if ["true", "false", "null", "yes", "no", "on", "off", "~"]
        .iter()
        .any(|word| word.eq_ignore_ascii_case(text))
    {
        return false;
    }
    !looks_numeric(text)
}

fn looks_numeric(text: &str) -> bool {
    // A leading `-` never reaches here, and a non-negative integer parses as
    // both u64 and f64, so the unsigned check subsumes the signed one.
    text.parse::<u64>().is_ok() || text.parse::<f64>().is_ok()
}
