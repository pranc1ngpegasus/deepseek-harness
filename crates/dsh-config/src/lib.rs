#![cfg_attr(
    test,
    allow(
        clippy::expect_used,
        clippy::unwrap_used,
        clippy::panic,
        clippy::assert_is_empty,
        clippy::bool_assert_comparison,
        clippy::float_cmp
    )
)]
//! Cordis entry-list patch composition and config dump rendering.
//!
//! The include plugin's patch algorithm is the one semantic shared by mounting
//! and offline tooling: patches target rows by `id`, an `insert` appends rows
//! at the root or into a group's `config`, and every other field replaces the
//! target's field. This crate implements that algorithm over a JSON-safe value
//! model, plus the dump view that names each contributing layer. Keeping both
//! in one place makes a dump that drifts from a boot impossible to express.
#![no_std]
extern crate alloc;

#[cfg(test)]
extern crate std;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

mod dump;
mod value;

pub use dump::{DumpWarning, Layer, render_config_dump};
pub use value::{Mapping, Number, Value};

/// A runtime patch applied to an entry list.
#[derive(Debug, Clone, PartialEq)]
pub struct Patch {
    /// The row to patch, or the group to insert into. Empty means absent.
    pub id: Option<String>,
    /// Rows appended at the root or into the targeted group.
    pub insert: Option<Vec<Value>>,
    /// An assertion of the target's plugin name; never assigned.
    pub name: Option<String>,
    /// Every other field, assigned in source order.
    pub overrides: Vec<(String, Value)>,
}

/// A patch entry that cannot be applied at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatchEntryError {
    /// Human-readable diagnostic, without a trailing newline.
    pub message: String,
}

impl PatchEntryError {
    const fn new(message: String) -> Self {
        Self { message }
    }
}

impl core::fmt::Display for PatchEntryError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str(&self.message)
    }
}

/// A patch that matched nothing, reported and skipped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PatchWarning {
    /// An `insert` named an id that no row carries.
    InsertTargetMissing {
        /// The requested id.
        id: String,
    },
    /// An `insert` named a row that is not a group.
    InsertTargetNotGroup {
        /// The requested id.
        id: String,
    },
    /// A non-insert patch carried no id.
    MissingId,
    /// A non-insert patch named an id that no row carries.
    TargetMissing {
        /// The requested id.
        id: String,
    },
    /// The target exists but names a different plugin.
    NameMismatch {
        /// The requested id.
        id: String,
        /// The plugin name the patch asserted, rendered as JSON.
        expected: String,
        /// The plugin name the target carries, rendered as JSON.
        got: String,
    },
}

impl core::fmt::Display for PatchWarning {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::InsertTargetMissing { id } => {
                write!(
                    formatter,
                    "patch insert: entry {} not found",
                    value::json_string(id)
                )
            }
            Self::InsertTargetNotGroup { id } => write!(
                formatter,
                "patch insert: entry {} is not a group",
                value::json_string(id)
            ),
            Self::MissingId => formatter.write_str("patch: id is required for non-insert patches"),
            Self::TargetMissing { id } => {
                write!(
                    formatter,
                    "patch: entry {} not found",
                    value::json_string(id)
                )
            }
            Self::NameMismatch { id, expected, got } => write!(
                formatter,
                "patch: name mismatch for {} (expected {expected}, got {got}), skipping",
                value::json_string(id)
            ),
        }
    }
}

impl Patch {
    /// Build one patch from an entry mapping.
    ///
    /// `id` and `name` must be strings when present. A truthy `insert` must be
    /// a list. Empty `id` and `name` values behave as absent, matching the
    /// falsiness tests the patch algorithm applies.
    ///
    /// # Errors
    /// Returns the field that cannot be applied.
    pub fn from_mapping(mapping: &Mapping) -> Result<Self, PatchEntryError> {
        let id = match mapping.get("id") {
            None | Some(Value::Null) => None,
            Some(Value::String(value)) if value.is_empty() => None,
            Some(Value::String(value)) => Some(value.clone()),
            Some(_) => {
                return Err(PatchEntryError::new(
                    "patch id must be a string".to_string(),
                ));
            }
        };
        let name = match mapping.get("name") {
            None | Some(Value::Null) => None,
            Some(Value::String(value)) if value.is_empty() => None,
            Some(Value::String(value)) => Some(value.clone()),
            Some(_) => {
                return Err(PatchEntryError::new(
                    "patch name must be a string".to_string(),
                ));
            }
        };
        let insert = match mapping.get("insert") {
            Some(value) if value.is_truthy() => match value {
                Value::Sequence(rows) => Some(rows.clone()),
                _ => {
                    return Err(PatchEntryError::new(
                        "patch insert must be a list of entries".to_string(),
                    ));
                }
            },
            _ => None,
        };
        let overrides = mapping
            .iter()
            .filter(|(key, _)| {
                key.as_str() != "id" && key.as_str() != "insert" && key.as_str() != "name"
            })
            .cloned()
            .collect();
        Ok(Self {
            id,
            insert,
            name,
            overrides,
        })
    }
}

/// Validate one patch list and build its patches.
///
/// Every patch is checked before any is applied, because a patch file that
/// cannot be applied at all is a misconfiguration. A patch whose target row is
/// absent stays a per-entry warning instead.
///
/// # Errors
/// Returns the first entry that is not a mapping or whose fields cannot be
/// applied, labeled with `bin_name`, `label`, and `file`.
pub fn parse_patch_list(
    bin_name: &str,
    file: &str,
    label: &str,
    entries: &[Value],
) -> Result<Vec<Patch>, PatchEntryError> {
    let mut patches = Vec::with_capacity(entries.len());
    for (index, entry) in entries.iter().enumerate() {
        let Value::Mapping(mapping) = entry else {
            return Err(PatchEntryError::new(format!(
                "{bin_name}: {label} entry {} in {file} must be a mapping (a loader patch entry)",
                index + 1
            )));
        };
        let patch = Patch::from_mapping(mapping).map_err(|error| {
            PatchEntryError::new(format!(
                "{bin_name}: {label} entry {} in {file}: {}",
                index + 1,
                error.message
            ))
        })?;
        patches.push(patch);
    }
    Ok(patches)
}

/// One row's position: a root index followed by the indexes inside each
/// enclosing group's entry list.
type EntryPath = Vec<usize>;

fn entry_at_mut<'a>(entries: &'a mut [Value], path: &[usize]) -> Option<&'a mut Value> {
    let (first, rest) = path.split_first()?;
    let entry = entries.get_mut(*first)?;
    if rest.is_empty() {
        return Some(entry);
    }
    let children = entry
        .as_mapping_mut()?
        .get_mut("config")?
        .as_sequence_mut()?;
    entry_at_mut(children, rest)
}

fn index_entries(entries: &[Value], base: &[usize], map: &mut Vec<(String, EntryPath)>) {
    for (index, entry) in entries.iter().enumerate() {
        let mut path = Vec::with_capacity(base.len() + 1);
        path.extend_from_slice(base);
        path.push(index);
        let Some(mapping) = entry.as_mapping() else {
            continue;
        };
        if let Some(Value::String(id)) = mapping.get("id") {
            map.push((id.clone(), path.clone()));
        }
        let is_group = mapping.get("group").is_some_and(Value::is_truthy);
        if is_group && let Some(Value::Sequence(children)) = mapping.get("config") {
            index_entries(children, &path, map);
        }
    }
}

fn lookup<'a>(map: &'a [(String, EntryPath)], id: &str) -> Option<&'a EntryPath> {
    map.iter()
        .rev()
        .find(|(candidate, _)| candidate == id)
        .map(|(_, path)| path)
}

fn render_undefined_or_json(value: Option<&Value>) -> String {
    value.map_or_else(|| "undefined".to_string(), Value::to_json)
}

/// Apply patch lists to an entry list.
///
/// The input is not mutated. Inserted rows are indexed as they are added, so a
/// later patch in the same list can target a row an earlier one inserted. A
/// patch that matches nothing is reported and skipped.
#[must_use]
pub fn apply_entry_patches(data: &[Value], patches: &[Patch]) -> (Vec<Value>, Vec<PatchWarning>) {
    let mut entries = data.to_vec();
    let mut warnings = Vec::new();
    if patches.is_empty() {
        return (entries, warnings);
    }
    for patch in patches {
        if let Some(insert) = &patch.insert {
            let mut index = Vec::new();
            index_entries(&entries, &[], &mut index);
            if let Some(id) = &patch.id {
                let Some(path) = lookup(&index, id).cloned() else {
                    warnings.push(PatchWarning::InsertTargetMissing { id: id.clone() });
                    continue;
                };
                let Some(target) = entry_at_mut(&mut entries, &path) else {
                    warnings.push(PatchWarning::InsertTargetMissing { id: id.clone() });
                    continue;
                };
                let Some(mapping) = target.as_mapping_mut() else {
                    warnings.push(PatchWarning::InsertTargetMissing { id: id.clone() });
                    continue;
                };
                if !mapping.get("group").is_some_and(Value::is_truthy) {
                    warnings.push(PatchWarning::InsertTargetNotGroup { id: id.clone() });
                    continue;
                }
                if !matches!(mapping.get("config"), Some(Value::Sequence(_))) {
                    mapping.set("config", Value::Sequence(Vec::new()));
                }
                if let Some(Value::Sequence(children)) = mapping.get_mut("config") {
                    children.extend(insert.iter().cloned());
                }
            } else {
                entries.extend(insert.iter().cloned());
            }
            continue;
        }
        let Some(id) = &patch.id else {
            warnings.push(PatchWarning::MissingId);
            continue;
        };
        let mut index = Vec::new();
        index_entries(&entries, &[], &mut index);
        let Some(path) = lookup(&index, id).cloned() else {
            warnings.push(PatchWarning::TargetMissing { id: id.clone() });
            continue;
        };
        let Some(target) = entry_at_mut(&mut entries, &path) else {
            warnings.push(PatchWarning::TargetMissing { id: id.clone() });
            continue;
        };
        let Some(mapping) = target.as_mapping_mut() else {
            warnings.push(PatchWarning::TargetMissing { id: id.clone() });
            continue;
        };
        if let Some(expected) = &patch.name {
            let got = mapping.get("name");
            let matches =
                matches!(got, Some(Value::String(actual)) if actual.as_str() == expected.as_str());
            if !matches {
                warnings.push(PatchWarning::NameMismatch {
                    id: id.clone(),
                    expected: value::json_string(expected),
                    got: render_undefined_or_json(got),
                });
                continue;
            }
        }
        for (key, value) in &patch.overrides {
            mapping.set(key, value.clone());
        }
    }
    (entries, warnings)
}

/// Compose patch layers into the effective entry list over an empty root.
#[must_use]
pub fn compose_entries(layers: &[Vec<Patch>]) -> (Vec<Value>, Vec<PatchWarning>) {
    let flattened: Vec<Patch> = layers.iter().flatten().cloned().collect();
    apply_entry_patches(&[], &flattened)
}

#[cfg(test)]
mod tests {
    use super::{
        Layer, Patch, PatchWarning, apply_entry_patches, compose_entries, parse_patch_list,
        render_config_dump,
    };
    use alloc::string::ToString;
    use alloc::vec;
    use alloc::vec::Vec;

    fn mapping(pairs: &[(&str, super::Value)]) -> super::Value {
        super::Value::Mapping(super::Mapping(
            pairs
                .iter()
                .map(|(key, value)| ((*key).to_string(), value.clone()))
                .collect(),
        ))
    }

    fn text(value: &str) -> super::Value {
        super::Value::String(value.to_string())
    }

    fn int(value: i64) -> super::Value {
        super::Value::Number(super::Number::Int(value))
    }

    fn patch(pairs: &[(&str, super::Value)]) -> Patch {
        match mapping(pairs) {
            super::Value::Mapping(map) => Patch::from_mapping(&map).expect("patch"),
            _ => unreachable!(),
        }
    }

    #[test]
    fn inserts_at_root_into_groups_and_indexes_new_rows_for_later_patches() {
        let base = vec![mapping(&[("id", text("a")), ("name", text("plugin-a"))])];
        let group = mapping(&[("id", text("g")), ("group", super::Value::Bool(true))]);
        let patches = vec![
            patch(&[("insert", super::Value::Sequence(vec![group]))]),
            patch(&[
                ("id", text("g")),
                (
                    "insert",
                    super::Value::Sequence(vec![mapping(&[("id", text("child"))])]),
                ),
            ]),
            patch(&[
                ("id", text("child")),
                ("disabled", super::Value::Bool(true)),
            ]),
        ];
        let (composed, warnings) = apply_entry_patches(&base, &patches);
        assert!(warnings.is_empty());
        assert_eq!(composed.len(), 2);
        let child = composed[1]
            .as_mapping()
            .and_then(|group| group.get("config"))
            .and_then(super::Value::as_sequence)
            .and_then(|children| children.first())
            .and_then(super::Value::as_mapping)
            .expect("child");
        assert_eq!(child.get("disabled"), Some(&super::Value::Bool(true)));
    }

    #[test]
    fn a_non_group_target_rejects_insertion() {
        let base = vec![mapping(&[("id", text("a"))])];
        let patches = vec![patch(&[
            ("id", text("a")),
            ("insert", super::Value::Sequence(vec![mapping(&[])])),
        ])];
        let (composed, warnings) = apply_entry_patches(&base, &patches);
        assert_eq!(
            warnings,
            vec![PatchWarning::InsertTargetNotGroup {
                id: "a".to_string()
            }]
        );
        assert_eq!(composed, base);
    }

    #[test]
    fn unknown_targets_are_warned_and_skipped() {
        let base = vec![mapping(&[("id", text("a"))])];
        let patches = vec![
            patch(&[
                ("id", text("absent")),
                ("disabled", super::Value::Bool(true)),
            ]),
            patch(&[("disabled", super::Value::Bool(true))]),
            patch(&[
                ("id", text("absent")),
                ("insert", super::Value::Sequence(vec![mapping(&[])])),
            ]),
        ];
        let (composed, warnings) = apply_entry_patches(&base, &patches);
        assert_eq!(composed, base);
        assert_eq!(
            warnings,
            vec![
                PatchWarning::TargetMissing {
                    id: "absent".to_string()
                },
                PatchWarning::MissingId,
                PatchWarning::InsertTargetMissing {
                    id: "absent".to_string()
                },
            ]
        );
    }

    #[test]
    fn a_name_mismatch_skips_the_patch_and_reports_both_names() {
        let base = vec![mapping(&[("id", text("a")), ("name", text("expected"))])];
        let patches = vec![patch(&[
            ("id", text("a")),
            ("name", text("other")),
            ("disabled", super::Value::Bool(true)),
        ])];
        let (composed, warnings) = apply_entry_patches(&base, &patches);
        assert_eq!(composed, base);
        assert_eq!(
            warnings,
            vec![PatchWarning::NameMismatch {
                id: "a".to_string(),
                expected: "\"other\"".to_string(),
                got: "\"expected\"".to_string(),
            }]
        );
    }

    #[test]
    fn a_matching_name_asserts_without_renaming() {
        let base = vec![mapping(&[("id", text("a")), ("name", text("plugin"))])];
        let patches = vec![patch(&[
            ("id", text("a")),
            ("name", text("plugin")),
            ("disabled", super::Value::Bool(true)),
        ])];
        let (composed, warnings) = apply_entry_patches(&base, &patches);
        assert!(warnings.is_empty());
        let row = composed[0].as_mapping().expect("row");
        assert_eq!(row.get("name"), Some(&text("plugin")));
        assert_eq!(row.get("disabled"), Some(&super::Value::Bool(true)));
    }

    #[test]
    fn overrides_replace_whole_values_and_keep_field_order() {
        let base = vec![mapping(&[
            ("id", text("a")),
            ("config", mapping(&[("n", int(1))])),
        ])];
        let patches = vec![patch(&[
            ("id", text("a")),
            ("config", mapping(&[("m", int(2))])),
        ])];
        let (composed, _) = apply_entry_patches(&base, &patches);
        let row = composed[0].as_mapping().expect("row");
        assert_eq!(row.get("config"), Some(&mapping(&[("m", int(2))])));
        let keys: Vec<&str> = row.iter().map(|(key, _)| key.as_str()).collect();
        assert_eq!(keys, vec!["id", "config"]);
    }

    #[test]
    fn compose_entries_flattens_layers_over_an_empty_root() {
        let (composed, warnings) = compose_entries(&[vec![patch(&[(
            "insert",
            super::Value::Sequence(vec![mapping(&[("id", text("a"))])]),
        )])]]);
        assert!(warnings.is_empty());
        assert_eq!(composed.len(), 1);
    }

    #[test]
    fn patch_lists_validate_every_entry_before_application() {
        assert!(parse_patch_list("dsh", "f.yml", "patches", &[super::Value::Null]).is_err());
        let bad_id = mapping(&[("id", int(3))]);
        assert!(
            parse_patch_list("dsh", "f.yml", "patches", &[bad_id])
                .expect_err("bad id")
                .message
                .contains("patch id must be a string")
        );
        let bad_insert = mapping(&[("insert", int(3))]);
        assert!(parse_patch_list("dsh", "f.yml", "patches", &[bad_insert]).is_err());
        let good = mapping(&[("id", text("a"))]);
        assert_eq!(
            parse_patch_list("dsh", "f.yml", "patches", &[good])
                .expect("good")
                .len(),
            1
        );
    }

    #[test]
    fn an_empty_name_behaves_as_absent() {
        let parsed = patch(&[
            ("id", text("a")),
            ("name", text("")),
            ("disabled", super::Value::Bool(true)),
        ]);
        assert_eq!(parsed.name, None);
        assert_eq!(parsed.overrides.len(), 1);
    }

    #[test]
    fn falsy_insert_and_empty_id_behave_as_absent() {
        let parsed = patch(&[
            ("id", text("")),
            ("insert", super::Value::Null),
            ("disabled", super::Value::Bool(true)),
        ]);
        assert_eq!(parsed.id, None);
        assert_eq!(parsed.insert, None);
        assert_eq!(parsed.overrides.len(), 1);
    }

    #[test]
    fn dump_groups_rows_by_origin_and_lists_patching_layers() {
        let base = vec![mapping(&[("id", text("a")), ("name", text("a"))])];
        let layers = vec![
            Layer {
                label: "bundle".to_string(),
                patches: vec![patch(&[(
                    "insert",
                    super::Value::Sequence(vec![mapping(&[
                        ("id", text("b")),
                        ("name", text("b")),
                    ])]),
                )])],
            },
            Layer {
                label: "overlay".to_string(),
                patches: vec![patch(&[
                    ("id", text("a")),
                    ("disabled", super::Value::Bool(true)),
                ])],
            },
        ];
        let (rendered, warnings) = render_config_dump("cordis.yml", &base, &layers);
        assert!(warnings.is_empty());
        assert_eq!(
            rendered,
            "# == cordis.yml, patched by overlay\n- id: a\n  name: a\n  disabled: true\n# == bundle\n- id: b\n  name: b\n"
        );
    }

    #[test]
    fn warnings_render_the_documented_diagnostics() {
        assert_eq!(
            PatchWarning::InsertTargetMissing {
                id: "a".to_string()
            }
            .to_string(),
            "patch insert: entry \"a\" not found"
        );
        assert_eq!(
            PatchWarning::InsertTargetNotGroup {
                id: "a".to_string()
            }
            .to_string(),
            "patch insert: entry \"a\" is not a group"
        );
        assert_eq!(
            PatchWarning::MissingId.to_string(),
            "patch: id is required for non-insert patches"
        );
        assert_eq!(
            PatchWarning::TargetMissing {
                id: "a".to_string()
            }
            .to_string(),
            "patch: entry \"a\" not found"
        );
        assert_eq!(
            PatchWarning::NameMismatch {
                id: "a".to_string(),
                expected: "\"x\"".to_string(),
                got: "undefined".to_string(),
            }
            .to_string(),
            "patch: name mismatch for \"a\" (expected \"x\", got undefined), skipping"
        );
        assert_eq!(
            super::PatchEntryError {
                message: "boom".to_string()
            }
            .to_string(),
            "boom"
        );
    }

    #[test]
    fn dump_renders_nested_values_and_quotes_ambiguous_scalars() {
        let base = vec![mapping(&[
            ("id", text("a")),
            (
                "config",
                super::Value::Mapping(super::Mapping(vec![
                    (
                        "nested".to_string(),
                        super::Value::Mapping(super::Mapping(vec![
                            ("deep".to_string(), int(1)),
                            (
                                "list".to_string(),
                                super::Value::Sequence(vec![text("x"), text("y")]),
                            ),
                        ])),
                    ),
                    ("word".to_string(), text("true")),
                    ("number".to_string(), text("12")),
                    ("dash".to_string(), text("-x")),
                    ("colon".to_string(), text("a: b")),
                    ("spaced".to_string(), text("a b")),
                    ("trailing".to_string(), text("a ")),
                    ("float".to_string(), text("1.5")),
                    (
                        "objects".to_string(),
                        super::Value::Sequence(vec![super::Value::Mapping(super::Mapping(vec![
                            ("k".to_string(), text("v")),
                            ("k2".to_string(), text("v2")),
                        ]))]),
                    ),
                    ("empty".to_string(), text("")),
                ])),
            ),
            (
                "empty_map",
                super::Value::Mapping(super::Mapping(Vec::new())),
            ),
            ("empty_list", super::Value::Sequence(Vec::new())),
            ("control", text("a\u{1f}b")),
        ])];
        let (rendered, warnings) = render_config_dump("cordis.yml", &base, &[]);
        assert!(warnings.is_empty());
        let expected = "# == cordis.yml\n- id: a\n  config:\n    nested:\n      deep: 1\n      list:\n        - x\n        - y\n    word: \"true\"\n    number: \"12\"\n    dash: \"-x\"\n    colon: \"a: b\"\n    spaced: a b\n    trailing: \"a \"\n    float: \"1.5\"\n    objects:\n      - k: v\n        k2: v2\n    empty: \"\"\n  empty_map: {}\n  empty_list: []\n  control: \"a\\u001fb\"\n";
        assert_eq!(rendered, expected);
    }

    #[test]
    fn dump_keeps_an_empty_composition_on_one_line() {
        let (rendered, warnings) = render_config_dump("cordis.yml", &[], &[]);
        assert!(warnings.is_empty());
        assert_eq!(rendered, "\n");
    }

    #[test]
    fn dump_reports_only_the_warnings_each_new_layer_adds() {
        let base = vec![mapping(&[("id", text("a"))])];
        let layers = vec![
            Layer {
                label: "one".to_string(),
                patches: vec![patch(&[("id", text("missing"))])],
            },
            Layer {
                label: "two".to_string(),
                patches: vec![
                    patch(&[("id", text("missing"))]),
                    patch(&[(
                        "insert",
                        super::Value::Sequence(vec![mapping(&[("id", text("b"))])]),
                    )]),
                ],
            },
        ];
        let (_, warnings) = render_config_dump("cordis.yml", &base, &layers);
        assert_eq!(warnings.len(), 2);
        assert_eq!(warnings[0].label, "one");
        assert_eq!(warnings[1].label, "two");
    }

    #[test]
    fn number_equality_matches_numeric_values_across_spellings() {
        use super::{Number, Value};
        assert_eq!(Number::Int(1), Number::Float(1.0));
        assert_eq!(Number::Float(1.0), Number::Int(1));
        assert_ne!(Number::Int(1), Number::Float(1.5));
        assert_ne!(Number::Int(1), Number::Int(2));
        assert_ne!(Number::Float(f64::NAN), Number::Float(f64::NAN));
        assert_eq!(
            Value::Number(Number::Int(1)),
            Value::Number(Number::Float(1.0))
        );
    }

    #[test]
    fn float_rendering_matches_javascript_at_the_integer_boundary() {
        use super::{Number, Value};
        assert_eq!(Value::Number(Number::Float(1.0)).to_json(), "1");
        assert_eq!(Value::Number(Number::Float(-0.0)).to_json(), "0");
        assert_eq!(
            Value::Number(Number::Float(9_007_199_254_740_992.0)).to_json(),
            "9007199254740992"
        );
        assert_eq!(
            Value::Number(Number::Float(-9_007_199_254_740_992.0)).to_json(),
            "-9007199254740992"
        );
        assert_eq!(
            Value::Number(Number::Float(9_223_372_036_854_775_808.0)).to_json(),
            "9.223372036854776e+18"
        );
        assert_eq!(Value::Number(Number::Float(1.5e300)).to_json(), "1.5e+300");
        assert_eq!(Value::Number(Number::Float(1e21)).to_json(), "1e+21");
        assert_eq!(Value::Number(Number::Float(-1e21)).to_json(), "-1e+21");
        assert_eq!(Value::Number(Number::Float(1e-7)).to_json(), "1e-7");
    }

    #[test]
    fn control_characters_are_escaped_but_space_is_not() {
        use super::Value;
        use super::value::json_string;
        assert_eq!(json_string("\u{1f}"), "\"\\u001f\"");
        assert_eq!(json_string("\u{8}\u{c}"), "\"\\b\\f\"");
        assert_eq!(json_string(" "), "\" \"");
        assert_eq!(
            Value::String("tab\tnewline\n".to_string()).to_json(),
            "\"tab\\tnewline\\n\""
        );
    }

    proptest::proptest! {
        #[test]
        fn applying_no_patches_returns_the_input(
            ids in proptest::collection::vec("[a-z]{1,6}", 0..5),
        ) {
            let entries: Vec<super::Value> = ids
                .iter()
                .map(|id| mapping(&[("id", text(id))]))
                .collect();
            let (composed, warnings) = apply_entry_patches(&entries, &[]);
            proptest::prop_assert_eq!(composed, entries);
            proptest::prop_assert!(warnings.is_empty());
        }

        #[test]
        fn root_inserts_are_append_only(
            existing in proptest::collection::vec("[a-z]{1,6}", 0..5),
            inserted in proptest::collection::vec("[a-z]{1,6}", 0..5),
        ) {
            let entries: Vec<super::Value> = existing
                .iter()
                .map(|id| mapping(&[("id", text(id))]))
                .collect();
            let rows: Vec<super::Value> = inserted
                .iter()
                .map(|id| mapping(&[("id", text(id))]))
                .collect();
            let patches = vec![patch(&[("insert", super::Value::Sequence(rows))])];
            let (composed, warnings) = apply_entry_patches(&entries, &patches);
            proptest::prop_assert!(warnings.is_empty());
            proptest::prop_assert_eq!(composed.len(), existing.len() + inserted.len());
            proptest::prop_assert_eq!(&composed[..existing.len()], &entries[..]);
        }
    }
}
