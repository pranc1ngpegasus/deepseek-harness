//! Schema dump: describe the composed entry list and patch lists as JSON Schema.
//!
//! Plugin Config schemas come from importing plugin modules, which this
//! launcher does not do. The document therefore describes the entry dialect
//! itself and reports `complete: false` with a diagnostic, instead of claiming
//! a completeness it cannot observe.

use dsh_config::{Patch, compose_entries};
use serde_json::{Value as Json, json};

use crate::profile_store::Profile;

/// One schema-collection diagnostic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    /// `warning` or `error`.
    pub level: &'static str,
    /// The diagnostic text.
    pub message: String,
    /// The entry path, when the diagnostic names one.
    pub path: Option<String>,
}

/// A schema document plus everything collection could not resolve.
#[derive(Debug, Clone, PartialEq)]
pub struct SchemaDump {
    /// The JSON Schema document.
    pub document: Json,
    /// Diagnostics reported beside the document.
    pub diagnostics: Vec<Diagnostic>,
    /// Whether every selected bundle and patch was resolved.
    pub complete: bool,
}

/// Generate the schema document for a prepared profile.
///
/// # Errors
/// Returns when the composed layers cannot be applied.
pub fn generate_config_schema(
    profile: &Profile,
    layers: &[Vec<Patch>],
) -> Result<SchemaDump, String> {
    let mut diagnostics = Vec::new();
    for skipped in &profile.skipped_bundles {
        diagnostics.push(Diagnostic {
            level: "error",
            message: format!(
                "Selected profile bundle {:?} could not be loaded; repair or remove its bundle selection.",
                skipped.package_name
            ),
            path: None,
        });
    }
    let (_, warnings) = compose_entries(layers);
    for warning in warnings {
        diagnostics.push(Diagnostic {
            level: "warning",
            message: warning.to_string(),
            path: None,
        });
    }
    let complete = diagnostics
        .iter()
        .all(|diagnostic| diagnostic.level != "error");
    let diagnostics_json: Vec<Json> = diagnostics
        .iter()
        .map(|diagnostic| {
            let mut object = serde_json::Map::new();
            object.insert(
                "level".to_string(),
                Json::String(diagnostic.level.to_string()),
            );
            object.insert(
                "message".to_string(),
                Json::String(diagnostic.message.clone()),
            );
            if let Some(path) = &diagnostic.path {
                object.insert("path".to_string(), Json::String(path.clone()));
            }
            Json::Object(object)
        })
        .collect();
    let title = format!("Cordis configuration for profile {}", profile.name);
    let document = schema_document(&title, complete, &diagnostics_json);
    Ok(SchemaDump {
        document,
        diagnostics,
        complete,
    })
}

fn schema_document(title: &str, complete: bool, diagnostics: &[Json]) -> Json {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": title,
        "description": "Describes the parsed entry-list YAML printed by --dump-config. Use $defs.patchList for a profile/home/CLI overlay. Parse !!js with the Cordis entry-list YAML dialect. Expression results, service dependencies, plugin startup checks, and sequence-dependent patch targets still require runtime validation.",
        "$comment": "Bundle, profile, home, and CLI layers apply in that order. A patch config replaces the whole config. JSON Schema defaults are annotations. Root patch id constraints describe the current index, not ids introduced or changed by earlier patches.",
        "type": "array",
        "items": { "$ref": "#/$defs/entry" },
        "$defs": {
            "loaderExpression": {
                "type": "object",
                "properties": { "__jsExpr": { "type": "string" } },
                "required": ["__jsExpr"],
                "description": "Inert representation of a YAML !!js scalar. Its result is evaluated and validated only at runtime."
            },
            "entryMetadata": {
                "type": "object",
                "properties": {
                    "id": { "type": "string" },
                    "name": { "type": "string" },
                    "config": {},
                    "group": { "type": ["boolean", "null"] },
                    "disabled": {
                        "anyOf": [
                            { "type": ["boolean", "null"] },
                            { "$ref": "#/$defs/loaderExpression" }
                        ]
                    }
                }
            },
            "entry": {
                "type": "object",
                "allOf": [{ "$ref": "#/$defs/entryMetadata" }],
                "required": ["name"]
            },
            "patch": {
                "type": "object",
                "allOf": [{ "$ref": "#/$defs/entryMetadata" }],
                "properties": {
                    "insert": { "$ref": "#/$defs/entryList" }
                },
                "description": "An insert appends entries, optionally inside the group identified by id. Other patches replace supplied fields; config is replaced wholesale, not deep-merged."
            },
            "entryList": {
                "type": "array",
                "items": { "$ref": "#/$defs/entry" }
            },
            "patchList": {
                "type": "array",
                "items": { "$ref": "#/$defs/patch" }
            }
        },
        "x-cordis": {
            "complete": complete,
            "diagnostics": diagnostics
        }
    })
}

/// The stderr line for one diagnostic.
#[must_use]
pub fn diagnostic_line(diagnostic: &Diagnostic) -> String {
    let location = diagnostic
        .path
        .as_ref()
        .map_or_else(String::new, |path| format!(" [{path}]"));
    format!(
        "dsh: {}:{location} {}\n",
        diagnostic.level, diagnostic.message
    )
}
