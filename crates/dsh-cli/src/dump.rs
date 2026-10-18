//! Offline config composition: collect layers, render the dump, and report.

use std::path::Path;

use dsh_config::{Layer, Patch, render_config_dump};

use crate::home::home_patch_path;
use crate::profile_store::{Profile, prepare_profile};
use crate::yaml::{read_entry_list, read_patch_list};

/// The rendered dump and the diagnostics printed beside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DumpOutput {
    /// The composed YAML document.
    pub document: String,
    /// Stderr lines for skipped patches and skipped bundles.
    pub diagnostics: Vec<String>,
}

/// Read the dump layers: bundle layers, then the profile's user layer, the
/// home-level user layer, and the argv overlays.
///
/// `default_only` omits every layer above the bundles and never reads those
/// files, so it stays usable as the recovery diagnostic for a broken user
/// layer.
///
/// # Errors
/// Returns when a selected layer file cannot be read or parsed.
pub fn collect_dump_layers(
    profile: &Profile,
    default_only: bool,
    patch_files: &[String],
    home: &Path,
) -> Result<Vec<Layer>, String> {
    let mut layers: Vec<Layer> = profile
        .layers
        .iter()
        .map(|layer| Layer {
            label: layer.package_name.clone(),
            patches: layer.patches.clone(),
        })
        .collect();
    if !default_only {
        if profile.patch_path.exists() {
            layers.push(Layer {
                label: profile.patch_path.display().to_string(),
                patches: profile.patches.clone(),
            });
        }
        let home_patch = home_patch_path(home);
        if home_patch.exists() {
            layers.push(Layer {
                label: home_patch.display().to_string(),
                patches: read_patch_list(&home_patch, "patches")?,
            });
        }
        for file in patch_files {
            let absolute = absolute_path(file)?;
            layers.push(Layer {
                label: absolute.display().to_string(),
                patches: read_patch_list(&absolute, "overlay")?,
            });
        }
    }
    Ok(layers)
}

fn absolute_path(file: &str) -> Result<std::path::PathBuf, String> {
    let path = Path::new(file);
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    std::env::current_dir()
        .map(|directory| directory.join(path))
        .map_err(|error| format!("dsh: cannot resolve {file}: {error}"))
}

/// Compose a profile's layers and render the dump.
///
/// # Errors
/// Returns when the profile cannot be prepared or its root config cannot be
/// read.
pub fn dump_config(
    profile_name: &str,
    default_only: bool,
    patch_files: &[String],
    from_default_profile: Option<&str>,
    install_anchor: &Path,
    home: &Path,
) -> Result<DumpOutput, String> {
    let (profile, _) = prepare_profile(
        profile_name,
        !default_only,
        from_default_profile,
        install_anchor,
        home,
    )?;
    let mut diagnostics: Vec<String> = crate::profile_store::skipped_bundle_lines(&profile)
        .into_iter()
        .map(|line| line.trim_end().to_string())
        .collect();
    let layers = collect_dump_layers(&profile, default_only, patch_files, home)?;
    let root = profile.dir.join(dsh_profile::PROFILE_ROOT_FILENAME);
    let base = read_entry_list(&root)?;
    let label = root
        .file_name()
        .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
    let (document, warnings) = render_config_dump(&label, &base, &layers);
    for warning in warnings {
        diagnostics.push(format!("dsh: [{}] {}", warning.label, warning.warning));
    }
    Ok(DumpOutput {
        document,
        diagnostics,
    })
}

/// The JSON representation of one patch, for the boot seam's environment.
#[must_use]
pub fn patches_to_json(patches: &[Patch]) -> serde_json::Value {
    serde_json::Value::Array(patches.iter().map(patch_to_json).collect())
}

fn patch_to_json(patch: &Patch) -> serde_json::Value {
    let mut object = serde_json::Map::new();
    if let Some(id) = &patch.id {
        object.insert("id".to_string(), serde_json::Value::String(id.clone()));
    }
    if let Some(insert) = &patch.insert {
        object.insert(
            "insert".to_string(),
            serde_json::Value::Array(insert.iter().map(value_to_json).collect()),
        );
    }
    if let Some(name) = &patch.name {
        object.insert("name".to_string(), serde_json::Value::String(name.clone()));
    }
    for (key, value) in &patch.overrides {
        object.insert(key.clone(), value_to_json(value));
    }
    serde_json::Value::Object(object)
}

fn value_to_json(value: &dsh_config::Value) -> serde_json::Value {
    use dsh_config::{Number, Value};
    match value {
        Value::Null => serde_json::Value::Null,
        Value::Bool(value) => serde_json::Value::Bool(*value),
        Value::Number(Number::Int(value)) => serde_json::Value::from(*value),
        Value::Number(Number::Float(value)) => serde_json::Value::from(*value),
        Value::String(value) => serde_json::Value::String(value.clone()),
        Value::Sequence(values) => {
            serde_json::Value::Array(values.iter().map(value_to_json).collect())
        }
        Value::Mapping(mapping) => {
            let mut object = serde_json::Map::new();
            for (key, value) in mapping.iter() {
                object.insert(key.clone(), value_to_json(value));
            }
            serde_json::Value::Object(object)
        }
        Value::Tagged(tag, inner) if tag == "js" => {
            let mut object = serde_json::Map::new();
            object.insert(
                "__jsExpr".to_string(),
                serde_json::Value::String(inner.as_str().unwrap_or_default().to_string()),
            );
            serde_json::Value::Object(object)
        }
        Value::Tagged(_, inner) => value_to_json(inner),
    }
}

#[cfg(test)]
mod tests {
    use super::patches_to_json;
    use dsh_config::{Mapping, Number, Patch, Value};

    #[test]
    fn patches_keep_their_fields_and_inert_expressions() {
        let patch = Patch {
            id: Some("row".to_string()),
            insert: Some(vec![Value::Mapping(Mapping(vec![(
                "id".to_string(),
                Value::String("child".to_string()),
            )]))]),
            name: Some("plugin".to_string()),
            overrides: vec![
                (
                    "config".to_string(),
                    Value::Mapping(Mapping(vec![
                        ("plain".to_string(), Value::Number(Number::Int(1))),
                        (
                            "expr".to_string(),
                            Value::Tagged(
                                "js".to_string(),
                                Box::new(Value::String("process.env.X".to_string())),
                            ),
                        ),
                        (
                            "custom".to_string(),
                            Value::Tagged(
                                "!custom".to_string(),
                                Box::new(Value::String("value".to_string())),
                            ),
                        ),
                    ])),
                ),
                ("disabled".to_string(), Value::Bool(true)),
            ],
        };
        let rendered = patches_to_json(&[patch]);
        assert_eq!(rendered[0]["id"], "row");
        assert_eq!(rendered[0]["insert"][0]["id"], "child");
        assert_eq!(rendered[0]["name"], "plugin");
        assert_eq!(rendered[0]["disabled"], true);
        assert_eq!(rendered[0]["config"]["plain"], 1);
        assert_eq!(rendered[0]["config"]["expr"]["__jsExpr"], "process.env.X");
        assert_eq!(rendered[0]["config"]["custom"], "value");
    }
}
