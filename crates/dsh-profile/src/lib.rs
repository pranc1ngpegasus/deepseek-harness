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
//! Profile names, shipped templates, and composition rules for `dsh`.
//!
//! A profile is a directory under `$DSH_HOME/profiles/<name>` holding a
//! `package.json` whose `dsh.profile.bundles` list names the bundle layers in
//! application order, a `cordis.patch.yml` user layer, and the pnpm settings
//! out-of-tree plugins need. This crate owns the rules that decide what a
//! profile directory may be, which bundles a new profile starts with, and how
//! a stored bundle list is normalized before it is read. Filesystem access
//! stays with the caller.
#![no_std]
extern crate alloc;

#[cfg(test)]
extern crate std;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::fmt::Write as _;

/// Directory under the Harness home holding every profile.
pub const PROFILES_DIR: &str = "profiles";

/// The user patch layer inside a profile directory.
pub const PROFILE_PATCH_FILENAME: &str = "cordis.patch.yml";

/// Root config filename inside a profile directory.
pub const PROFILE_ROOT_FILENAME: &str = "cordis.yml";

/// The bundle list a `dsh plugin` init uses for a name with no shipped template.
pub const DEFAULT_PROFILE_BUNDLES: &[&str] = &["@deepseek-ai/dsh-base"];

/// Package manifest field holding the profile's ordered bundle selection.
pub const PROFILE_MANIFEST_KEY: &str = "dsh.profile.bundles";

/// The telemetry row a launch-time opt-out disables.
pub const TELEMETRY_ROW_ID: &str = "session-telemetry-otel";

/// Bundles an earlier release shipped and the installation no longer carries.
pub const RETIRED_BUNDLES: &[&str] = &["@deepseek-ai/dsh-experimental-schedule-bundle"];

/// The empty root entry list every profile tree patches over.
pub const PROFILE_ROOT_CONFIG: &str = "\
# dsh profile root — an empty entry list. The tree is composed as patches:
# each bundle in package.json's dsh.profile.bundles, then cordis.patch.yml, then any
# --patch overlays. Edit cordis.patch.yml, not this file.
[]
";

const PROFILE_PATCH_TEMPLATE: &str = "\
# Your patch layer for this dsh profile, applied after every bundle layer:
# a top-level YAML array of loader patch entries (id-targeted config
# overrides, disables, and insert lists; `!!js` expressions allowed).
[]
";

const PROFILE_PNPM_WORKSPACE: &str = "\
packages:
  - .

nodeLinker: hoisted
autoInstallPeers: false
";

/// Why a profile name cannot name a profile directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileNameError {
    /// The empty name would resolve to the profiles directory itself.
    Empty,
    /// A path separator would escape the profiles directory.
    Separator,
    /// `.` names the profiles directory itself.
    CurrentDirectory,
    /// `..` names the Harness home above the profiles directory.
    ParentDirectory,
    /// Node reserves this name for dependency lookup.
    NodeModules,
}

impl ProfileNameError {
    /// The diagnostic text, without the `dsh:` prefix.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::Empty => "profile name is empty",
            Self::Separator => "profile name contains a path separator",
            Self::CurrentDirectory => "profile name cannot be '.'",
            Self::ParentDirectory => "profile name cannot be '..'",
            Self::NodeModules => "profile name cannot be 'node_modules'",
        }
    }
}

/// Validate a name for use as one directory under the profiles directory.
///
/// # Errors
/// Returns the rule the name violates.
pub fn validate_profile_name(name: &str) -> Result<(), ProfileNameError> {
    if name.is_empty() {
        return Err(ProfileNameError::Empty);
    }
    if name.contains('/') || name.contains('\\') {
        return Err(ProfileNameError::Separator);
    }
    match name {
        "." => Err(ProfileNameError::CurrentDirectory),
        ".." => Err(ProfileNameError::ParentDirectory),
        "node_modules" => Err(ProfileNameError::NodeModules),
        _ => Ok(()),
    }
}

/// The profile directory relative to the Harness home, as `profiles/<name>`.
///
/// # Errors
/// Returns the rule the name violates.
pub fn profile_relative_path(name: &str) -> Result<String, ProfileNameError> {
    validate_profile_name(name)?;
    Ok(format!("{PROFILES_DIR}/{name}"))
}

/// The shipped bundles a template name starts a profile with.
///
/// Template lookup is exact-case; [`shipped_profile_name`] handles the
/// case-insensitive reservation checks.
#[must_use]
pub fn template_bundles(name: &str) -> Option<&'static [&'static str]> {
    match name {
        "tui" => Some(&[]),
        "acp" => Some(&["@deepseek-ai/dsh-base", "@deepseek-ai/dsh-acp-app"]),
        "web" => Some(&["@deepseek-ai/dsh-base", "@deepseek-ai/dsh-web-app"]),
        "headless" => Some(&["@deepseek-ai/dsh-base", "@deepseek-ai/dsh-headless"]),
        "sdk" => Some(&["@deepseek-ai/dsh-base", "@deepseek-ai/dsh-sdk-app"]),
        "sdk-minimal" => Some(&["@deepseek-ai/dsh-sdk-minimal"]),
        _ => None,
    }
}

/// The shipped template name that `name` matches without regard to case.
#[must_use]
pub fn shipped_profile_name(name: &str) -> Option<&'static str> {
    ["tui", "acp", "web", "headless", "sdk", "sdk-minimal"]
        .into_iter()
        .find(|shipped| shipped.eq_ignore_ascii_case(name))
}

/// An installation-owned bundle tuple and the shipped template it normalizes to.
fn installation_owned_tuple(name: &str) -> Option<&'static [&'static str]> {
    match name {
        "headless" => Some(&[
            "@deepseek-ai/dsh-base",
            "@deepseek-ai/dsh-web-app",
            "@deepseek-ai/dsh-headless",
        ]),
        _ => None,
    }
}

/// The bundle list a profile is created with: its template, or the default.
#[must_use]
pub fn initial_bundles(name: &str) -> &'static [&'static str] {
    template_bundles(name).unwrap_or(DEFAULT_PROFILE_BUNDLES)
}

fn same_bundles(left: &[String], right: &[&str]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(value, expected)| value == expected)
}

/// The stored bundle list without bundles the installation no longer ships.
///
/// Returns `None` when nothing changed, so a caller writes the manifest back
/// only when the stored list actually listed a retired bundle.
#[must_use]
pub fn drop_retired_bundles(bundles: &[String]) -> Option<Vec<String>> {
    let kept: Vec<String> = bundles
        .iter()
        .filter(|bundle| !RETIRED_BUNDLES.contains(&bundle.as_str()))
        .cloned()
        .collect();
    if kept.len() == bundles.len() {
        None
    } else {
        Some(kept)
    }
}

/// The shipped template that replaces an exact installation-owned tuple.
///
/// Only the exact tuple an earlier release wrote is normalized, so a user
/// who added or removed a bundle keeps their selection.
#[must_use]
pub fn normalize_installation_owned(name: &str, bundles: &[String]) -> Option<Vec<String>> {
    let owned = installation_owned_tuple(name)?;
    if !same_bundles(bundles, owned) {
        return None;
    }
    template_bundles(name).map(|template| template.iter().map(ToString::to_string).collect())
}

/// Whether a launch-time opt-out turns telemetry off.
///
/// Any non-empty value disables, including `0` and `false`; a privacy switch
/// prefers off-by-mistake over on-by-mistake. A composition without the
/// telemetry row exports nothing, so no patch is required there.
#[must_use]
pub fn telemetry_is_disabled(disabled_env: Option<&str>, has_row: bool) -> bool {
    !disabled_env.is_none_or(str::is_empty) && has_row
}

/// The empty root entry list every profile tree patches over.
#[must_use]
pub const fn root_config() -> &'static str {
    PROFILE_ROOT_CONFIG
}

/// The user patch layer template written for a new profile.
#[must_use]
pub const fn patch_template() -> &'static str {
    PROFILE_PATCH_TEMPLATE
}

/// The pnpm settings file template written for a new profile.
#[must_use]
pub const fn pnpm_workspace() -> &'static str {
    PROFILE_PNPM_WORKSPACE
}

/// Escape a string for a JSON double-quoted value.
fn json_string(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len() + 2);
    escaped.push('"');
    for character in value.chars() {
        match character {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            control if control < ' ' => {
                let _ = write!(escaped, "\\u{:04x}", u32::from(control));
            }
            other => escaped.push(other),
        }
    }
    escaped.push('"');
    escaped
}

/// The two-space JSON manifest a new profile directory receives.
///
/// The package name derives from the profile directory name, dependencies
/// start empty, and the bundle selection is copied in order.
#[must_use]
pub fn profile_manifest_json(name: &str, bundles: &[&str]) -> String {
    let mut bundles_json = String::new();
    for (index, bundle) in bundles.iter().enumerate() {
        if index > 0 {
            bundles_json.push(',');
        }
        let _ = write!(bundles_json, "\n        {}", json_string(bundle));
    }
    if !bundles_json.is_empty() {
        bundles_json.push('\n');
        bundles_json.push_str("      ");
    }
    format!(
        "{{\n  \"name\": {},\n  \"private\": true,\n  \"dependencies\": {{}},\n  \"dsh\": {{\n    \"profile\": {{\n      \"bundles\": [{bundles_json}]\n    }}\n  }}\n}}\n",
        json_string(&format!("dsh-profile-{name}")),
    )
}

/// The one-line stderr diagnostic a launcher prints for a skipped bundle.
#[must_use]
pub fn skipped_bundle_line(bin_name: &str, package_name: &str, reason: &str) -> String {
    format!(
        "{bin_name}: skipping profile bundle {}: {reason}\n",
        json_string(package_name)
    )
}

/// The error for a profile directory that does not exist and has no template.
#[must_use]
pub fn missing_profile_message(bin_name: &str, name: &str) -> String {
    format!(
        "{bin_name}: profile {} does not exist; create it with 'dsh plugin --profile {name} add <package>'",
        json_string(name)
    )
}

#[cfg(test)]
mod tests {
    use super::{
        PROFILE_PATCH_FILENAME, PROFILE_ROOT_CONFIG, ProfileNameError, drop_retired_bundles,
        initial_bundles, missing_profile_message, normalize_installation_owned, patch_template,
        pnpm_workspace, profile_manifest_json, profile_relative_path, root_config,
        shipped_profile_name, skipped_bundle_line, telemetry_is_disabled, template_bundles,
        validate_profile_name,
    };
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    #[test]
    fn every_name_error_has_its_own_message() {
        assert_eq!(ProfileNameError::Empty.message(), "profile name is empty");
        assert_eq!(
            ProfileNameError::Separator.message(),
            "profile name contains a path separator"
        );
        assert_eq!(
            ProfileNameError::CurrentDirectory.message(),
            "profile name cannot be '.'"
        );
        assert_eq!(
            ProfileNameError::ParentDirectory.message(),
            "profile name cannot be '..'"
        );
        assert_eq!(
            ProfileNameError::NodeModules.message(),
            "profile name cannot be 'node_modules'"
        );
    }

    #[test]
    fn json_escaping_handles_control_characters() {
        let rendered = profile_manifest_json("a\u{1f}b", &[]);
        assert!(rendered.contains("dsh-profile-a\\u001fb"));
        assert!(!rendered.contains('\u{1f}'));
        assert!(profile_manifest_json("a b", &[]).contains("dsh-profile-a b"));
    }

    #[test]
    fn profile_names_that_escape_the_profiles_directory_are_rejected() {
        for (name, expected) in [
            ("", ProfileNameError::Empty),
            ("a/b", ProfileNameError::Separator),
            ("a\\b", ProfileNameError::Separator),
            (".", ProfileNameError::CurrentDirectory),
            ("..", ProfileNameError::ParentDirectory),
            ("node_modules", ProfileNameError::NodeModules),
        ] {
            assert_eq!(validate_profile_name(name), Err(expected));
        }
        for name in ["web", "sdk-minimal", "a.b", "two words", "_x"] {
            assert_eq!(validate_profile_name(name), Ok(()));
        }
    }

    #[test]
    fn profile_path_joins_a_validated_name() {
        assert_eq!(profile_relative_path("web").as_deref(), Ok("profiles/web"));
        assert!(profile_relative_path("../x").is_err());
    }

    #[test]
    fn shipped_templates_keep_their_bundle_order() {
        assert_eq!(template_bundles("tui"), Some(&[][..]));
        assert_eq!(
            template_bundles("acp"),
            Some(&["@deepseek-ai/dsh-base", "@deepseek-ai/dsh-acp-app"][..])
        );
        assert_eq!(
            template_bundles("web"),
            Some(&["@deepseek-ai/dsh-base", "@deepseek-ai/dsh-web-app"][..])
        );
        assert_eq!(
            template_bundles("headless"),
            Some(&["@deepseek-ai/dsh-base", "@deepseek-ai/dsh-headless"][..])
        );
        assert_eq!(
            template_bundles("sdk"),
            Some(&["@deepseek-ai/dsh-base", "@deepseek-ai/dsh-sdk-app"][..])
        );
        assert_eq!(
            template_bundles("sdk-minimal"),
            Some(&["@deepseek-ai/dsh-sdk-minimal"][..])
        );
        assert_eq!(template_bundles("tui"), Some(&[][..]));
        assert_eq!(initial_bundles("tui"), &[] as &[&str]);
        assert_eq!(initial_bundles("web"), template_bundles("web").unwrap());
    }

    #[test]
    fn shipped_names_are_reserved_without_regard_to_case() {
        assert_eq!(shipped_profile_name("web"), Some("web"));
        assert_eq!(shipped_profile_name("Web"), Some("web"));
        assert_eq!(shipped_profile_name("SDK-MINIMAL"), Some("sdk-minimal"));
        assert_eq!(shipped_profile_name("tui"), Some("tui"));
    }

    #[test]
    fn retired_bundles_are_dropped_and_other_selectors_are_kept() {
        assert_eq!(drop_retired_bundles(&strings(&["a", "b"])), None);
        assert_eq!(
            drop_retired_bundles(&strings(&[
                "a",
                "@deepseek-ai/dsh-experimental-schedule-bundle",
                "b",
            ]))
            .expect("changed"),
            strings(&["a", "b"])
        );
        assert_eq!(drop_retired_bundles(&[]), None);
    }

    #[test]
    fn only_the_exact_installation_owned_tuple_is_normalized() {
        let owned = strings(&[
            "@deepseek-ai/dsh-base",
            "@deepseek-ai/dsh-web-app",
            "@deepseek-ai/dsh-headless",
        ]);
        assert_eq!(
            normalize_installation_owned("headless", &owned).expect("normalized"),
            strings(&["@deepseek-ai/dsh-base", "@deepseek-ai/dsh-headless"])
        );
        assert_eq!(normalize_installation_owned("web", &owned), None);
        let edited = strings(&[
            "@deepseek-ai/dsh-base",
            "@deepseek-ai/dsh-web-app",
            "@deepseek-ai/dsh-headless",
            "extra",
        ]);
        assert_eq!(normalize_installation_owned("headless", &edited), None);
    }

    #[test]
    fn telemetry_disables_on_any_non_empty_value_with_the_row() {
        assert!(!telemetry_is_disabled(None, true));
        assert!(!telemetry_is_disabled(Some(""), true));
        assert!(!telemetry_is_disabled(None, false));
        assert!(!telemetry_is_disabled(Some("1"), false));
        assert!(telemetry_is_disabled(Some("1"), true));
        assert!(telemetry_is_disabled(Some("0"), true));
        assert!(telemetry_is_disabled(Some("false"), true));
    }

    #[test]
    fn templates_match_the_launcher_contract() {
        assert_eq!(root_config(), PROFILE_ROOT_CONFIG);
        assert!(patch_template().contains("top-level YAML array"));
        assert!(patch_template().ends_with("[]\n"));
        assert!(pnpm_workspace().contains("nodeLinker: hoisted"));
        assert!(pnpm_workspace().contains("autoInstallPeers: false"));
        assert_eq!(PROFILE_PATCH_FILENAME, "cordis.patch.yml");
    }

    #[test]
    fn manifest_rendering_is_stable_json() {
        assert_eq!(
            profile_manifest_json(
                "web",
                &["@deepseek-ai/dsh-base", "@deepseek-ai/dsh-web-app"]
            ),
            "{\n  \"name\": \"dsh-profile-web\",\n  \"private\": true,\n  \"dependencies\": {},\n  \"dsh\": {\n    \"profile\": {\n      \"bundles\": [\n        \"@deepseek-ai/dsh-base\",\n        \"@deepseek-ai/dsh-web-app\"\n      ]\n    }\n  }\n}\n"
        );
        assert_eq!(
            profile_manifest_json("empty", &[]),
            "{\n  \"name\": \"dsh-profile-empty\",\n  \"private\": true,\n  \"dependencies\": {},\n  \"dsh\": {\n    \"profile\": {\n      \"bundles\": []\n    }\n  }\n}\n"
        );
    }

    #[test]
    fn manifest_rendering_escapes_names_and_bundle_names() {
        let rendered = profile_manifest_json("a\"b", &["x\\y", "line\nbreak"]);
        assert!(rendered.contains("\"name\": \"dsh-profile-a\\\"b\""));
        assert!(rendered.contains("\"x\\\\y\""));
        assert!(rendered.contains("\"line\\nbreak\""));
    }

    #[test]
    fn diagnostics_quote_the_package_name() {
        assert_eq!(
            skipped_bundle_line("dsh", "@scope/pkg", "missing"),
            "dsh: skipping profile bundle \"@scope/pkg\": missing\n"
        );
        assert_eq!(
            missing_profile_message("dsh", "tui"),
            "dsh: profile \"tui\" does not exist; create it with 'dsh plugin --profile tui add <package>'"
        );
    }

    proptest::proptest! {
        #[test]
        fn valid_names_never_escape_the_profiles_directory(name in "[A-Za-z0-9_. -]{1,20}") {
            if let Ok(relative) = profile_relative_path(&name) {
                proptest::prop_assert!(!relative.contains("//"));
                proptest::prop_assert!(relative.starts_with("profiles/"));
                let suffix = &relative["profiles/".len()..];
                proptest::prop_assert_eq!(suffix, name.as_str());
            }
        }

        #[test]
        fn retiring_is_idempotent(bundles in proptest::collection::vec("[@a-z/._-]{0,20}", 0..6)) {
            let first = drop_retired_bundles(&bundles);
            let stabilized = first.clone().unwrap_or_else(|| bundles.clone());
            proptest::prop_assert!(drop_retired_bundles(&stabilized).is_none());
            proptest::prop_assert!(first.is_none() || first.unwrap().len() < bundles.len());
        }
    }
}
