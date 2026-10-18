//! Profile discovery, initialization, and patch-layer composition.
//!
//! A profile is a directory holding a manifest whose `dsh.profile.bundles`
//! names the bundle layers in application order, the user's patch layer, and
//! pnpm settings. Loading resolves each bundle from the installation first and
//! from the profile second, so an in-box bundle always comes from the running
//! installation. Unreadable bundles are skipped with a reason instead of
//! failing the whole profile.

use std::path::{Path, PathBuf};

use dsh_config::{Patch, Value, compose_entries};
use dsh_profile::{
    drop_retired_bundles, missing_profile_message, normalize_installation_owned, patch_template,
    pnpm_workspace, profile_manifest_json, root_config, shipped_profile_name, skipped_bundle_line,
    template_bundles,
};
use serde_json::Value as Json;

use crate::home::{home_patch_path, profile_dir};
use crate::yaml::read_patch_list;

/// A selected bundle that could not be loaded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedBundle {
    /// The bundle's package name from `dsh.profile.bundles`.
    pub package_name: String,
    /// The resolution, manifest, or patch-loading failure.
    pub reason: String,
}

/// One resolved bundle layer of a profile.
#[derive(Debug, Clone, PartialEq)]
pub struct ProfileLayer {
    /// The bundle's package name.
    pub package_name: String,
    /// The bundle's absolute package directory.
    pub package_dir: PathBuf,
    /// The bundle's absolute patch files, in application order.
    pub patch_paths: Vec<PathBuf>,
    /// The parsed patches of every file, concatenated in application order.
    pub patches: Vec<Patch>,
}

/// A loaded profile: resolved bundle layers plus the user's own patch layer.
#[derive(Debug, Clone, PartialEq)]
pub struct Profile {
    /// The profile's directory basename.
    pub name: String,
    /// The absolute profile directory.
    pub dir: PathBuf,
    /// Bundle layers in `dsh.profile.bundles` order.
    pub layers: Vec<ProfileLayer>,
    /// The absolute profile patch path.
    pub patch_path: PathBuf,
    /// The profile's own patches; empty when the file is absent or skipped.
    pub patches: Vec<Patch>,
    /// Selected bundles that contributed no layer, with why.
    pub skipped_bundles: Vec<SkippedBundle>,
}

/// Initialize a profile directory: manifest, empty user patch layer, and the
/// pnpm settings out-of-tree plugins need.
///
/// Existing files are never touched, so re-running is a no-op.
///
/// # Errors
/// Returns a diagnostic when the directory or one of its files cannot be
/// written.
pub fn init_profile(dir: &Path, bundles: &[&str]) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|error| {
        format!(
            "dsh: cannot create profile directory {}: {error}",
            dir.display()
        )
    })?;
    let manifest_path = dir.join("package.json");
    if !manifest_path.exists() {
        let name = dir
            .file_name()
            .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
        std::fs::write(&manifest_path, profile_manifest_json(&name, bundles)).map_err(|error| {
            format!(
                "dsh: cannot write profile manifest {}: {error}",
                manifest_path.display()
            )
        })?;
    }
    let patch_path = dir.join(dsh_profile::PROFILE_PATCH_FILENAME);
    if !patch_path.exists() {
        std::fs::write(&patch_path, patch_template()).map_err(|error| {
            format!(
                "dsh: cannot write profile patch {}: {error}",
                patch_path.display()
            )
        })?;
    }
    let workspace_path = dir.join("pnpm-workspace.yaml");
    if !workspace_path.exists() {
        std::fs::write(&workspace_path, pnpm_workspace()).map_err(|error| {
            format!(
                "dsh: cannot write profile pnpm settings {}: {error}",
                workspace_path.display()
            )
        })?;
    }
    Ok(())
}

/// Read a profile manifest as JSON.
///
/// # Errors
/// Returns when the file is missing, unreadable, not JSON, or not an object.
pub fn read_manifest(path: &Path) -> Result<Json, String> {
    let raw = std::fs::read_to_string(path).map_err(|error| {
        format!(
            "dsh: failed to read profile manifest {}: {error}",
            path.display()
        )
    })?;
    let parsed: Json = serde_json::from_str(&raw).map_err(|error| {
        format!(
            "dsh: profile manifest {} is not valid JSON: {error}",
            path.display()
        )
    })?;
    if !parsed.is_object() {
        return Err(format!(
            "dsh: profile manifest {} must hold a JSON object",
            path.display()
        ));
    }
    Ok(parsed)
}

/// The bundle list a manifest selects.
#[must_use]
pub fn manifest_bundles(manifest: &Json) -> Vec<String> {
    manifest
        .pointer("/dsh/profile/bundles")
        .and_then(Json::as_array)
        .map(|bundles| {
            bundles
                .iter()
                .filter_map(Json::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn set_manifest_bundles(manifest: &mut Json, bundles: Vec<String>) {
    let Some(object) = manifest.as_object_mut() else {
        return;
    };
    let dsh = object
        .entry("dsh")
        .or_insert_with(|| Json::Object(serde_json::Map::new()));
    if !dsh.is_object() {
        *dsh = Json::Object(serde_json::Map::new());
    }
    let Some(dsh_object) = dsh.as_object_mut() else {
        return;
    };
    let profile = dsh_object
        .entry("profile")
        .or_insert_with(|| Json::Object(serde_json::Map::new()));
    if !profile.is_object() {
        *profile = Json::Object(serde_json::Map::new());
    }
    if let Some(profile_object) = profile.as_object_mut() {
        profile_object.insert(
            "bundles".to_string(),
            Json::Array(bundles.into_iter().map(Json::String).collect()),
        );
    }
}

/// Write a manifest back with two-space JSON and a trailing newline.
///
/// # Errors
/// Returns when the file cannot be serialized or written.
pub fn write_manifest(path: &Path, manifest: &Json) -> Result<(), String> {
    let mut rendered = serde_json::to_string_pretty(manifest).map_err(|error| {
        format!(
            "dsh: cannot render profile manifest {}: {error}",
            path.display()
        )
    })?;
    rendered.push('\n');
    std::fs::write(path, rendered).map_err(|error| {
        format!(
            "dsh: cannot write profile manifest {}: {error}",
            path.display()
        )
    })
}

/// Drop bundles the installation no longer ships, writing only when changed.
fn drop_retired(manifest: &mut Json, dir: &Path) -> Result<(), String> {
    let bundles = manifest_bundles(manifest);
    let Some(kept) = drop_retired_bundles(&bundles) else {
        return Ok(());
    };
    set_manifest_bundles(manifest, kept);
    write_manifest(&dir.join("package.json"), manifest)
}

/// Normalize an exact installation-owned tuple, writing only when it matched.
fn normalize_installation(name: &str, manifest: &mut Json, dir: &Path) -> Result<(), String> {
    let bundles = manifest_bundles(manifest);
    let Some(normalized) = normalize_installation_owned(name, &bundles) else {
        return Ok(());
    };
    set_manifest_bundles(manifest, normalized);
    write_manifest(&dir.join("package.json"), manifest)
}

/// Resolve a package directory by walking the anchor's `node_modules` chain.
#[must_use]
pub fn package_dir_from_anchor(anchor: &Path, package_name: &str) -> Option<PathBuf> {
    let start = anchor.parent()?;
    for directory in start.ancestors() {
        let candidate = directory.join("node_modules").join(package_name);
        if candidate.join("package.json").is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Resolve one bundle package from the installation, then the profile.
///
/// # Errors
/// Returns the diagnostic the launcher prints when neither anchor carries the
/// package.
pub fn resolve_bundle_dir(
    bin_name: &str,
    package_name: &str,
    install_anchor: &Path,
    profile_dir: &Path,
) -> Result<PathBuf, String> {
    for anchor in [
        install_anchor.to_path_buf(),
        profile_dir.join("package.json"),
    ] {
        if let Some(directory) = package_dir_from_anchor(&anchor, package_name) {
            return Ok(directory);
        }
    }
    let profile_name = profile_dir
        .file_name()
        .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
    Err(format!(
        "{bin_name}: cannot resolve profile bundle {package_name:?} from the dsh installation or {}; run 'dsh plugin --profile {profile_name} install' if its dependency is not installed",
        profile_dir.display()
    ))
}

/// The patch files a bundle declares, in application order.
fn bundle_patch_files(manifest: &Json) -> Result<Vec<String>, String> {
    let bundle = manifest
        .pointer("/dsh/bundle")
        .ok_or_else(|| "package.json declares no dsh.bundle".to_string())?;
    let patch = bundle.get("patch");
    match patch {
        Some(Json::String(file)) => Ok(vec![file.clone()]),
        Some(Json::Array(files)) if files.iter().all(Json::is_string) => Ok(files
            .iter()
            .filter_map(Json::as_str)
            .map(str::to_string)
            .collect()),
        _ => Err("dsh.bundle.patch must be a file path or a list of file paths".to_string()),
    }
}

/// Load an initialized profile directory.
///
/// # Errors
/// Returns when the manifest cannot be read or a non-bundle file is
/// malformed. Unreadable bundles are skipped instead.
pub fn load_profile_directory(
    bin_name: &str,
    dir: &Path,
    install_anchor: &Path,
    user_layer: bool,
) -> Result<Profile, String> {
    let mut manifest = read_manifest(&dir.join("package.json"))?;
    let name = dir
        .file_name()
        .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
    drop_retired(&mut manifest, dir)?;
    normalize_installation(&name, &mut manifest, dir)?;
    let bundles = manifest_bundles(&manifest);
    let mut layers = Vec::new();
    let mut skipped_bundles = Vec::new();
    for package_name in bundles {
        match load_bundle(bin_name, &package_name, install_anchor, dir) {
            Ok(layer) => layers.push(layer),
            Err(reason) => skipped_bundles.push(SkippedBundle {
                package_name,
                reason,
            }),
        }
    }
    let patch_path = dir.join(dsh_profile::PROFILE_PATCH_FILENAME);
    let patches = if user_layer && patch_path.exists() {
        read_patch_list(&patch_path, "patches")?
    } else {
        Vec::new()
    };
    Ok(Profile {
        name,
        dir: dir.to_path_buf(),
        layers,
        patch_path,
        patches,
        skipped_bundles,
    })
}

fn load_bundle(
    bin_name: &str,
    package_name: &str,
    install_anchor: &Path,
    profile_dir: &Path,
) -> Result<ProfileLayer, String> {
    let package_dir = resolve_bundle_dir(bin_name, package_name, install_anchor, profile_dir)?;
    let manifest = read_manifest(&package_dir.join("package.json"))?;
    let files = bundle_patch_files(&manifest)
        .map_err(|error| format!("{bin_name}: profile bundle {package_name:?} {error}"))?;
    let patch_paths: Vec<PathBuf> = files.iter().map(|file| package_dir.join(file)).collect();
    let mut patches = Vec::new();
    for path in &patch_paths {
        patches.extend(read_patch_list(path, "patches")?);
    }
    Ok(ProfileLayer {
        package_name: package_name.to_string(),
        package_dir,
        patch_paths,
        patches,
    })
}

/// Load a profile by name, initializing a shipped template on first use.
///
/// # Errors
/// Returns when the name is invalid, the profile does not exist and has no
/// template, or a profile file is malformed.
pub fn load_profile(
    bin_name: &str,
    name: &str,
    install_anchor: &Path,
    home: &Path,
    user_layer: bool,
) -> Result<Profile, String> {
    let dir = profile_dir(name, home)?;
    if !dir.join("package.json").exists() {
        let Some(template) = template_bundles(name) else {
            return Err(missing_profile_message(bin_name, name));
        };
        init_profile(&dir, template)?;
    }
    load_profile_directory(bin_name, &dir, install_anchor, user_layer)
}

/// Replace a custom profile with a fresh copy of one shipped template.
///
/// The new profile is assembled beside the target and moved into place; only
/// then is the previous directory deleted. A failed move restores the previous
/// directory, and a restore failure names the staging path holding it.
///
/// # Errors
/// Returns when the template is unknown, the target name is shipped, a
/// concurrent recreation wins, or a file operation fails.
pub fn initialize_profile_from_default(
    name: &str,
    from_default_profile: &str,
    home: &Path,
) -> Result<Vec<String>, String> {
    let dir = profile_dir(name, home)?;
    let Some(template) = template_bundles(from_default_profile) else {
        let mut expected: Vec<String> = ["acp", "web", "headless", "sdk", "sdk-minimal"]
            .into_iter()
            .map(|value| format!("{value:?}"))
            .collect();
        expected.sort();
        return Err(format!(
            "dsh: unknown default profile {from_default_profile:?}; expected one of {}",
            expected.join(", ")
        ));
    };
    if let Some(shipped) = shipped_profile_name(name) {
        return Err(format!(
            "dsh: profile {name:?} names shipped profile {shipped:?} and cannot be a custom profile target; omit --from-default-profile to use it"
        ));
    }
    let parent = dir
        .parent()
        .ok_or_else(|| format!("dsh: profile directory {} has no parent", dir.display()))?;
    std::fs::create_dir_all(parent).map_err(|error| {
        format!(
            "dsh: cannot create profiles directory {}: {error}",
            parent.display()
        )
    })?;
    let staging = parent.join(format!(".{name}.staging-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&staging).map_err(|error| {
        format!(
            "dsh: cannot create staging directory {}: {error}",
            staging.display()
        )
    })?;
    let previous = staging.join("previous");
    let result = replace_from_template(&dir, &staging, &previous, name, template);
    let mut warnings = Vec::new();
    match result {
        Ok(()) => {
            if let Err(error) = std::fs::remove_dir_all(&staging) {
                warnings.push(format!(
                    "dsh: replaced profile {name:?} but could not remove {}: {error}",
                    staging.display()
                ));
            }
        }
        Err(error) => {
            discard_staging_if_unused(&previous, &staging);
            return Err(error);
        }
    }
    Ok(warnings)
}

/// Remove the staging directory when it holds no previous profile to recover.
fn discard_staging_if_unused(previous: &std::path::Path, staging: &std::path::Path) {
    if !previous.exists() {
        let _ = std::fs::remove_dir_all(staging);
    }
}

fn replace_from_template(
    dir: &Path,
    staging: &Path,
    previous: &Path,
    name: &str,
    template: &[&str],
) -> Result<(), String> {
    let fresh = staging.join("new").join(name);
    init_profile(&fresh, template)?;
    let mut moved = false;
    match std::fs::rename(dir, previous) {
        Ok(()) => moved = true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(format!(
                "dsh: cannot move the previous profile {} aside: {error}",
                dir.display()
            ));
        }
    }
    if let Err(error) = std::fs::rename(&fresh, dir) {
        if dir.exists() {
            let _ = std::fs::remove_dir_all(previous);
            return Err(format!(
                "dsh: profile {name:?} was recreated concurrently at {}",
                dir.display()
            ));
        }
        if moved && let Err(restore) = std::fs::rename(previous, dir) {
            return Err(format!(
                "dsh: profile replacement failed and {} could not be restored from {}: {error}; {restore}",
                dir.display(),
                previous.display()
            ));
        }
        return Err(format!(
            "dsh: cannot move the fresh profile into {}: {error}",
            dir.display()
        ));
    }
    Ok(())
}

/// Prepare a profile: optionally recreate it from a template, load it, and
/// rewrite the empty root config.
///
/// # Errors
/// Returns the initialization or load diagnostic.
pub fn prepare_profile(
    name: &str,
    user_layer: bool,
    from_default_profile: Option<&str>,
    install_anchor: &Path,
    home: &Path,
) -> Result<(Profile, Vec<String>), String> {
    let mut warnings = Vec::new();
    if let Some(from_default) = from_default_profile {
        warnings = initialize_profile_from_default(name, from_default, home)?;
    }
    let profile = load_profile("dsh", name, install_anchor, home, user_layer)?;
    std::fs::write(
        profile.dir.join(dsh_profile::PROFILE_ROOT_FILENAME),
        root_config(),
    )
    .map_err(|error| {
        format!(
            "dsh: cannot write profile root {}: {error}",
            profile
                .dir
                .join(dsh_profile::PROFILE_ROOT_FILENAME)
                .display()
        )
    })?;
    Ok((profile, warnings))
}

/// The stderr lines a launcher prints once for each skipped bundle.
#[must_use]
pub fn skipped_bundle_lines(profile: &Profile) -> Vec<String> {
    profile
        .skipped_bundles
        .iter()
        .map(|skipped| skipped_bundle_line("dsh", &skipped.package_name, &skipped.reason))
        .collect()
}

/// The telemetry disable patch for a launch-time opt-out.
///
/// `patches` is the complete ordered layer list the boot receives, so a home
/// layer that inserts the telemetry row makes the switch effective. Any
/// non-empty opt-out value disables when the row is present.
#[must_use]
pub fn telemetry_patch(patches: &[Patch], disabled_env: Option<&str>) -> Option<Patch> {
    let (composed, _) = compose_entries(&[patches.to_vec()]);
    let has_row = composed.iter().any(|row| {
        row.as_mapping()
            .and_then(|mapping| mapping.get("id"))
            .and_then(Value::as_str)
            == Some(dsh_profile::TELEMETRY_ROW_ID)
    });
    if dsh_profile::telemetry_is_disabled(disabled_env, has_row) {
        Some(Patch {
            id: Some(dsh_profile::TELEMETRY_ROW_ID.to_string()),
            insert: None,
            name: None,
            overrides: vec![("disabled".to_string(), Value::Bool(true))],
        })
    } else {
        None
    }
}

/// Read the patch list a boot hands the loader: bundle layers, the profile's
/// layer, the home layer, the launch overlays, then the telemetry switch.
///
/// # Errors
/// Returns when the home patch layer cannot be read.
pub fn read_profile_patches(
    profile: &Profile,
    overlays: &[Patch],
    home: &Path,
    telemetry_disabled_env: Option<&str>,
) -> Result<Vec<Patch>, String> {
    let mut patches: Vec<Patch> = profile
        .layers
        .iter()
        .flat_map(|layer| layer.patches.iter().cloned())
        .collect();
    patches.extend(profile.patches.iter().cloned());
    let home_patch = home_patch_path(home);
    if home_patch.exists() {
        patches.extend(read_patch_list(&home_patch, "patches")?);
    }
    patches.extend(overlays.iter().cloned());
    if let Some(telemetry) = telemetry_patch(&patches, telemetry_disabled_env) {
        patches.push(telemetry);
    }
    Ok(patches)
}

#[cfg(test)]
mod tests {
    use super::{
        init_profile, initialize_profile_from_default, load_profile, load_profile_directory,
        manifest_bundles, read_manifest, resolve_bundle_dir, skipped_bundle_lines, telemetry_patch,
    };
    use serde_json::json;
    use std::fs;
    use tempfile::TempDir;

    fn fixture_home() -> TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    fn write_bundle(install: &std::path::Path, name: &str, patch: &str) {
        let dir = install.join("node_modules").join(name);
        fs::create_dir_all(&dir).expect("bundle dir");
        fs::write(
            dir.join("package.json"),
            format!(
                "{{ \"name\": {name:?}, \"dsh\": {{ \"bundle\": {{ \"patch\": \"cordis.patch.yml\" }} }} }}"
            ),
        )
        .expect("bundle manifest");
        fs::write(dir.join("cordis.patch.yml"), patch).expect("bundle patch");
    }

    fn write_install(root: &std::path::Path) {
        fs::write(root.join("package.json"), "{\"name\":\"dsh\"}").expect("install manifest");
    }

    #[test]
    fn bundle_patch_declarations_accept_one_file_or_an_ordered_list() {
        let single = serde_json::json!({"dsh": {"bundle": {"patch": "a.yml"}}});
        assert_eq!(
            super::bundle_patch_files(&single).expect("single"),
            vec!["a.yml"]
        );
        let listed = serde_json::json!({"dsh": {"bundle": {"patch": ["a.yml", "b.yml"]}}});
        assert_eq!(
            super::bundle_patch_files(&listed).expect("listed"),
            vec!["a.yml", "b.yml"]
        );
        let missing = serde_json::json!({"name": "x"});
        assert!(super::bundle_patch_files(&missing).is_err());
        let invalid = serde_json::json!({"dsh": {"bundle": {"patch": ["a.yml", 3]}}});
        assert!(super::bundle_patch_files(&invalid).is_err());
        let empty = serde_json::json!({"dsh": {"bundle": {"patch": []}}});
        assert!(super::bundle_patch_files(&empty).expect("empty").is_empty());
    }

    #[test]
    fn manifest_bundle_writes_create_and_repair_the_nested_objects() {
        let mut manifest = serde_json::json!({"name": "p", "dsh": "broken"});
        super::set_manifest_bundles(&mut manifest, vec!["a".to_string()]);
        assert_eq!(manifest_bundles(&manifest), vec!["a"]);
        let mut manifest = serde_json::json!({"name": "p", "dsh": {"profile": "broken"}});
        super::set_manifest_bundles(&mut manifest, vec!["b".to_string()]);
        assert_eq!(manifest_bundles(&manifest), vec!["b"]);
        let mut manifest = serde_json::json!({"name": "p"});
        super::set_manifest_bundles(&mut manifest, vec!["c".to_string()]);
        assert_eq!(manifest_bundles(&manifest), vec!["c"]);
    }

    #[test]
    fn a_staging_directory_is_discarded_only_without_a_previous_profile() {
        let home = fixture_home();
        let staging = home.path().join("staging");
        std::fs::create_dir_all(&staging).expect("staging");
        let previous = staging.join("previous");
        super::discard_staging_if_unused(&previous, &staging);
        assert!(!staging.exists());

        std::fs::create_dir_all(&previous).expect("previous");
        std::fs::write(previous.join("keep"), "x").expect("keep");
        super::discard_staging_if_unused(&previous, &staging);
        assert!(previous.join("keep").is_file());
    }

    #[test]
    fn a_failed_move_reports_the_previous_profile_error() {
        let home = fixture_home();
        let dir = home.path().join("profiles/rescue");
        std::fs::create_dir_all(&dir).expect("profile");
        std::fs::write(dir.join("marker"), "old").expect("marker");
        let staging = home.path().join("staging");
        std::fs::create_dir_all(&staging).expect("staging");
        let previous = staging.join("previous");
        std::fs::create_dir_all(&previous).expect("previous");
        std::fs::write(previous.join("occupied"), "x").expect("occupied");
        let error = super::replace_from_template(
            &dir,
            &staging,
            &previous,
            "rescue",
            &["@deepseek-ai/dsh-base"],
        )
        .expect_err("the move must fail");
        assert!(
            error.contains("cannot move the previous profile"),
            "{error}"
        );
    }

    #[test]
    fn init_creates_manifest_patch_and_workspace_without_touching_existing_files() {
        let home = fixture_home();
        let dir = home.path().join("profiles/web");
        init_profile(&dir, &["@deepseek-ai/dsh-base"]).expect("init");
        let manifest = read_manifest(&dir.join("package.json")).expect("manifest");
        assert_eq!(manifest_bundles(&manifest), vec!["@deepseek-ai/dsh-base"]);
        assert!(dir.join("cordis.patch.yml").is_file());
        assert!(dir.join("pnpm-workspace.yaml").is_file());
        fs::write(dir.join("cordis.patch.yml"), "custom\n").expect("customize");
        init_profile(&dir, &["@deepseek-ai/dsh-base"]).expect("reinit");
        assert_eq!(
            fs::read_to_string(dir.join("cordis.patch.yml")).expect("read"),
            "custom\n"
        );
    }

    #[test]
    fn load_initializes_a_shipped_template_and_rejects_an_unknown_name() {
        let home = fixture_home();
        let install = home.path().join("install");
        fs::create_dir_all(&install).expect("install dir");
        write_install(&install);
        write_bundle(
            &install,
            "@deepseek-ai/dsh-base",
            "- id: a\n  name: plugin-a\n",
        );
        write_bundle(
            &install,
            "@deepseek-ai/dsh-web-app",
            "- id: b\n  name: plugin-b\n",
        );
        let profile = load_profile(
            "dsh",
            "web",
            &install.join("package.json"),
            home.path(),
            true,
        )
        .expect("load");
        assert_eq!(profile.layers.len(), 2);
        assert!(!profile.dir.join("cordis.yml").exists());
        assert_eq!(
            load_profile(
                "dsh",
                "missing",
                &install.join("package.json"),
                home.path(),
                true
            )
            .expect_err("missing"),
            "dsh: profile \"missing\" does not exist; create it with 'dsh plugin --profile missing add <package>'"
        );
    }

    #[test]
    fn a_bundle_that_cannot_resolve_is_skipped_with_a_reason() {
        let home = fixture_home();
        let install = home.path().join("install");
        fs::create_dir_all(&install).expect("install dir");
        write_install(&install);
        let dir = home.path().join("profiles/custom");
        init_profile(&dir, &["@deepseek-ai/dsh-base"]).expect("init");
        let profile =
            load_profile_directory("dsh", &dir, &install.join("package.json"), true).expect("load");
        assert_eq!(profile.skipped_bundles.len(), 1);
        assert!(
            profile.skipped_bundles[0]
                .reason
                .contains("cannot resolve profile bundle")
        );
        let lines = skipped_bundle_lines(&profile);
        assert!(lines[0].contains("skipping profile bundle"));
    }

    #[test]
    fn retired_bundles_are_removed_from_the_manifest_and_installation_tuples_normalize() {
        let home = fixture_home();
        let install = home.path().join("install");
        fs::create_dir_all(&install).expect("install dir");
        write_install(&install);
        let dir = home.path().join("profiles/headless");
        fs::create_dir_all(&dir).expect("profile");
        fs::write(
            dir.join("package.json"),
            serde_json::to_string(&json!({
                "name": "p",
                "dsh": { "profile": { "bundles": [
                    "@deepseek-ai/dsh-base",
                    "@deepseek-ai/dsh-web-app",
                    "@deepseek-ai/dsh-headless",
                    "@deepseek-ai/dsh-experimental-schedule-bundle"
                ] } }
            }))
            .expect("json"),
        )
        .expect("write");
        let _ =
            load_profile_directory("dsh", &dir, &install.join("package.json"), true).expect("load");
        let manifest = read_manifest(&dir.join("package.json")).expect("manifest");
        assert_eq!(
            manifest_bundles(&manifest),
            vec!["@deepseek-ai/dsh-base", "@deepseek-ai/dsh-headless"]
        );
    }

    #[test]
    fn bundle_resolution_prefers_the_installation_over_the_profile() {
        let home = fixture_home();
        let install = home.path().join("install");
        let profile = home.path().join("profiles/x");
        fs::create_dir_all(&install).expect("install");
        fs::create_dir_all(&profile).expect("profile");
        write_install(&install);
        write_bundle(&install, "pkg", "- id: install\n");
        write_bundle(&profile, "pkg", "- id: profile\n");
        let resolved = resolve_bundle_dir("dsh", "pkg", &install.join("package.json"), &profile)
            .expect("resolved");
        assert_eq!(resolved, install.join("node_modules").join("pkg"));
    }

    #[test]
    fn replacing_from_a_default_template_swaps_the_directory_and_rejects_reserved_targets() {
        let home = fixture_home();
        let warnings =
            initialize_profile_from_default("rescue", "web", home.path()).expect("replace");
        assert!(warnings.is_empty());
        let manifest =
            read_manifest(&home.path().join("profiles/rescue/package.json")).expect("manifest");
        assert_eq!(
            manifest_bundles(&manifest),
            vec!["@deepseek-ai/dsh-base", "@deepseek-ai/dsh-web-app"]
        );
        assert!(
            initialize_profile_from_default("Web", "web", home.path())
                .expect_err("reserved")
                .contains("names shipped profile")
        );
        assert!(
            initialize_profile_from_default("rescue", "unknown", home.path())
                .expect_err("unknown")
                .contains("unknown default profile")
        );
    }

    #[test]
    fn telemetry_disables_only_for_a_non_empty_switch_and_a_present_row() {
        let home = fixture_home();
        let install = home.path().join("install");
        fs::create_dir_all(&install).expect("install");
        write_install(&install);
        write_bundle(
            &install,
            "base",
            "- insert:\n    - id: session-telemetry-otel\n      name: telemetry\n",
        );
        let dir = home.path().join("profiles/x");
        init_profile(&dir, &["base"]).expect("init");
        let profile =
            load_profile_directory("dsh", &dir, &install.join("package.json"), true).expect("load");
        let patches: Vec<dsh_config::Patch> = profile
            .layers
            .iter()
            .flat_map(|layer| layer.patches.iter().cloned())
            .collect();
        assert!(telemetry_patch(&patches, Some("1")).is_some());
        assert!(telemetry_patch(&patches, Some("")).is_none());
        assert!(telemetry_patch(&patches, None).is_none());
    }
}
