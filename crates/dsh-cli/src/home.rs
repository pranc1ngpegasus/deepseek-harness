//! Harness-home and installation-anchor resolution.

use std::path::{Path, PathBuf};

/// Directory name for the default Harness home under the OS home.
pub const DSH_HOME_DIR_NAME: &str = ".dsh";

/// Environment variable that overrides the default Harness home.
pub const DSH_HOME_ENV: &str = "DSH_HOME";

/// Environment variable that names the installation package manifest.
pub const INSTALL_ANCHOR_ENV: &str = "DSH_INSTALL_ANCHOR";

/// Expand a leading `~` to the OS home.
#[must_use]
pub fn expand_home(path: &str, os_home: &Path) -> PathBuf {
    if path == "~" {
        return os_home.to_path_buf();
    }
    if let Some(rest) = path.strip_prefix("~/").or_else(|| path.strip_prefix("~\\")) {
        return os_home.join(rest);
    }
    PathBuf::from(path)
}

/// Resolve the Harness home.
///
/// Precedence, highest first: an explicit path, `$DSH_HOME`, then `~/.dsh`.
/// An empty or whitespace-only `$DSH_HOME` is unset, so a blank override never
/// resolves to the current directory.
#[must_use]
pub fn resolve_home(configured: Option<&str>, from_env: Option<&str>, os_home: &Path) -> PathBuf {
    let selected = configured
        .map(str::to_string)
        .or_else(|| {
            from_env
                .filter(|value| !value.trim().is_empty())
                .map(str::to_string)
        })
        .unwrap_or_else(|| format!("{}/{DSH_HOME_DIR_NAME}", os_home.display()));
    expand_home(&selected, os_home)
}

/// Resolve the Harness home from the process environment.
#[must_use]
pub fn resolve_dsh_home() -> PathBuf {
    let os_home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map_or_else(|_| PathBuf::from("."), PathBuf::from);
    resolve_home(None, std::env::var(DSH_HOME_ENV).ok().as_deref(), &os_home)
}

/// The Harness-home patch layer applied over every profile's own layer.
#[must_use]
pub fn home_patch_path(home: &Path) -> PathBuf {
    home.join(dsh_profile::PROFILE_PATCH_FILENAME)
}

/// The directory holding every profile.
#[must_use]
pub fn profiles_dir(home: &Path) -> PathBuf {
    home.join(dsh_profile::PROFILES_DIR)
}

/// Resolve one profile's directory under the Harness home.
///
/// # Errors
/// Returns the rule the name violates.
pub fn profile_dir(name: &str, home: &Path) -> Result<PathBuf, String> {
    dsh_profile::validate_profile_name(name)
        .map_err(|error| format!("dsh: invalid profile name {name:?}: {}", error.message()))?;
    Ok(profiles_dir(home).join(name))
}

/// Resolve the installation's package.json, the first bundle-resolution anchor.
///
/// `$DSH_INSTALL_ANCHOR` names it for packaged launchers. Otherwise the
/// launcher looks beside its executable, then at the workspace root recorded
/// when it was built.
///
/// # Errors
/// Returns when no candidate exists, because bundle resolution would silently
/// fall back to the profile and pick a different copy of `dsh-base`.
pub fn install_anchor() -> Result<PathBuf, String> {
    if let Some(configured) = std::env::var_os(INSTALL_ANCHOR_ENV) {
        return Ok(PathBuf::from(configured));
    }
    if let Ok(executable) = std::env::current_exe()
        && let Some(directory) = executable.parent()
    {
        let beside = directory.join("package.json");
        if beside.is_file() {
            return Ok(beside);
        }
    }
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../package.json");
    if workspace.is_file() {
        return Ok(workspace);
    }
    Err(format!(
        "dsh: cannot locate the installation package.json; set {INSTALL_ANCHOR_ENV}"
    ))
}

#[cfg(test)]
mod tests {
    use super::{expand_home, home_patch_path, profile_dir, profiles_dir, resolve_home};
    use std::path::{Path, PathBuf};

    #[test]
    fn home_precedence_is_configured_then_environment_then_default() {
        let os_home = Path::new("/home/person");
        assert_eq!(
            resolve_home(Some("/custom"), Some("/env"), os_home),
            PathBuf::from("/custom")
        );
        assert_eq!(
            resolve_home(None, Some("/env"), os_home),
            PathBuf::from("/env")
        );
        assert_eq!(
            resolve_home(None, Some("   "), os_home),
            PathBuf::from("/home/person/.dsh")
        );
        assert_eq!(
            resolve_home(None, None, os_home),
            PathBuf::from("/home/person/.dsh")
        );
    }

    #[test]
    fn a_leading_tilde_expands() {
        let os_home = Path::new("/home/person");
        assert_eq!(expand_home("~", os_home), PathBuf::from("/home/person"));
        assert_eq!(
            expand_home("~/a/b", os_home),
            PathBuf::from("/home/person/a/b")
        );
        assert_eq!(
            expand_home("~\\a", os_home),
            PathBuf::from("/home/person/a")
        );
        assert_eq!(expand_home("relative", os_home), PathBuf::from("relative"));
    }

    #[test]
    fn profile_paths_join_the_resolved_home() {
        let home = Path::new("/home/person/.dsh");
        assert_eq!(
            profiles_dir(home),
            PathBuf::from("/home/person/.dsh/profiles")
        );
        assert_eq!(
            home_patch_path(home),
            PathBuf::from("/home/person/.dsh/cordis.patch.yml")
        );
        assert_eq!(
            profile_dir("web", home).expect("valid"),
            PathBuf::from("/home/person/.dsh/profiles/web")
        );
        assert!(profile_dir("../escape", home).is_err());
    }
}
