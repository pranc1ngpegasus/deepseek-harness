//! Profile plugin management: explicit version exemptions and pnpm forwarding.
//!
//! DSH-owned commands never reach pnpm. Every other argument list is forwarded
//! verbatim inside the profile directory, with pnpm's output both streamed and
//! saved to a diagnostics log.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use dsh_profile::template_bundles;
use serde_json::Value as Json;

use crate::home::profile_dir;
use crate::profile_store::init_profile;

/// The profile compatibility file.
pub const COMPATIBILITY_FILENAME: &str = "compatibility.json";

/// How long a profile lock waits before failing.
///
/// The lock covers one small read/modify/write of the compatibility file, so a
/// short bound separates a slow peer from a crashed one without stalling a
/// package operation.
const LOCK_WAIT_MS: u64 = 10_000;

/// The version this launcher reports as the running DSH.
#[must_use]
pub fn runtime_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// Run a `dsh plugin` invocation and return its exit status.
#[must_use]
pub fn run_plugin(
    profile: &str,
    args: &[String],
    manage_desktop_profile: bool,
    home: &Path,
) -> i32 {
    let dir = match profile_dir(profile, home) {
        Ok(dir) => dir,
        Err(error) => {
            eprintln!("{error}");
            return 1;
        }
    };
    if profile == "desktop"
        && let Err(error) = require_desktop_profile(&dir)
    {
        eprintln!("dsh: {error}");
        return 1;
    }
    let _ = manage_desktop_profile;
    if let Some(code) = version_command(profile, &dir, args) {
        return code;
    }
    if !dir.join("package.json").exists() {
        let bundles = template_bundles(profile).unwrap_or(dsh_profile::DEFAULT_PROFILE_BUNDLES);
        if let Err(error) = init_profile(&dir, bundles) {
            eprintln!("{error}");
            return 1;
        }
    }
    forward_to_pnpm(&dir, args, home)
}

fn require_desktop_profile(dir: &Path) -> Result<(), String> {
    if dir.join("package.json").exists() {
        Ok(())
    } else {
        Err("Open DeepSeek Harness Desktop once to initialize its profile, then fully quit it before running dsh plugin --profile desktop.".to_string())
    }
}

/// Parse and run a DSH-owned exemption command.
///
/// Returns `None` when the arguments belong to pnpm.
fn version_command(profile: &str, dir: &Path, args: &[String]) -> Option<i32> {
    let command = args.first()?.as_str();
    if !matches!(
        command,
        "allow-version" | "revoke-version" | "version-exemptions"
    ) {
        return None;
    }
    let rest = &args[1..];
    match execute_version_command(profile, dir, command, rest) {
        Ok(()) => Some(0),
        Err(error) => {
            eprintln!("dsh: {error}");
            Some(1)
        }
    }
}

fn execute_version_command(
    profile: &str,
    dir: &Path,
    command: &str,
    rest: &[String],
) -> Result<(), String> {
    let mut package_version: Option<String> = None;
    let mut runtime_version_argument: Option<String> = None;
    let mut accept_risk = false;
    let mut index = 0;
    while index < rest.len() {
        let argument = &rest[index];
        if argument == "--accept-risk" && command == "allow-version" && !accept_risk {
            accept_risk = true;
        } else if argument == "--dsh-version" && runtime_version_argument.is_none() {
            index += 1;
            let Some(value) = rest.get(index) else {
                return Err("unexpected argument \"--dsh-version\"".to_string());
            };
            runtime_version_argument = Some(value.clone());
        } else if let Some(value) = argument.strip_prefix("--dsh-version=")
            && runtime_version_argument.is_none()
        {
            runtime_version_argument = Some(value.to_string());
        } else if !argument.starts_with('-') && package_version.is_none() {
            package_version = Some(argument.clone());
        } else {
            return Err(format!("unexpected argument {argument:?}"));
        }
        index += 1;
    }
    if command == "version-exemptions" && !rest.is_empty() {
        return Err("usage: dsh plugin version-exemptions".to_string());
    }
    let request = if command == "version-exemptions" {
        None
    } else {
        let Some(package_version) = package_version else {
            return Err(usage(command));
        };
        let Some(runtime_version) = runtime_version_argument else {
            return Err(usage(command));
        };
        Some((package_version, runtime_version))
    };

    if command == "allow-version" {
        eprintln!(
            "dsh: warning: allowing incompatible plugin versions can break the application or corrupt data. Approval applies only to the exact package and DSH versions."
        );
    }
    std::fs::create_dir_all(dir)
        .map_err(|error| format!("cannot create profile directory {}: {error}", dir.display()))?;
    let lock = ProfileLock::acquire(&dir.join("package.json"), LOCK_WAIT_MS)?;
    let result = (|| {
        if profile == "desktop" {
            require_desktop_profile(dir)?;
        } else if !dir.join("package.json").exists() {
            let bundles = template_bundles(profile).unwrap_or(dsh_profile::DEFAULT_PROFILE_BUNDLES);
            init_profile(dir, bundles)?;
        }
        match request {
            None => {
                let compatibility = read_compatibility(dir);
                for warning in &compatibility.warnings {
                    eprintln!("dsh: warning: {warning}");
                }
                let mut rendered = serde_json::to_string_pretty(&compatibility.exemptions)
                    .map_err(|error| format!("cannot render exemptions: {error}"))?;
                rendered.push('\n');
                print!("{rendered}");
                Ok(())
            }
            Some((package_version, runtime_version)) => {
                set_exemption(
                    dir,
                    &package_version,
                    &runtime_version,
                    command == "allow-version",
                    accept_risk,
                )?;
                println!(
                    "dsh: {} {package_version} for DSH {runtime_version}",
                    if command == "allow-version" {
                        "allowed"
                    } else {
                        "revoked"
                    }
                );
                Ok(())
            }
        }
    })();
    drop(lock);
    result
}

fn usage(command: &str) -> String {
    let risk = if command == "allow-version" {
        " --accept-risk"
    } else {
        ""
    };
    format!("usage: dsh plugin {command} <package@version> --dsh-version <exact>{risk}")
}

#[derive(Debug, Clone, PartialEq)]
struct Compatibility {
    exemptions: serde_json::Map<String, Json>,
    warnings: Vec<String>,
    rewritable: bool,
}

fn read_compatibility(dir: &Path) -> Compatibility {
    let filename = dir.join(COMPATIBILITY_FILENAME);
    let text = match std::fs::read_to_string(&filename) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Compatibility {
                exemptions: serde_json::Map::new(),
                warnings: Vec::new(),
                rewritable: true,
            };
        }
        Err(error) => {
            return unreadable(&filename, &format!("cannot be read ({error})"));
        }
    };
    let parsed: Json = match serde_json::from_str(&text) {
        Ok(parsed) => parsed,
        Err(error) => return unreadable(&filename, &format!("is not valid JSON ({error})")),
    };
    let Some(object) = parsed.as_object() else {
        return unreadable(
            &filename,
            "must map exact package@version keys to DSH version lists",
        );
    };
    let mut exemptions = serde_json::Map::new();
    let mut warnings = Vec::new();
    for (key, versions) in object {
        if !is_exact_package_version(key) {
            warnings.push(format!(
                "{}: {key:?} is not an exact package-name@version key; the record is ignored",
                filename.display()
            ));
            continue;
        }
        let Some(list) = versions.as_array() else {
            warnings.push(format!(
                "{}: {key} must contain a list of exact DSH versions; the record is ignored",
                filename.display()
            ));
            continue;
        };
        if !list
            .iter()
            .all(|version| version.as_str().is_some_and(is_exact_version))
        {
            warnings.push(format!(
                "{}: {key} must contain a list of exact DSH versions; the record is ignored",
                filename.display()
            ));
            continue;
        }
        exemptions.insert(key.clone(), versions.clone());
    }
    Compatibility {
        exemptions,
        rewritable: warnings.is_empty(),
        warnings,
    }
}

fn unreadable(filename: &Path, reason: &str) -> Compatibility {
    Compatibility {
        exemptions: serde_json::Map::new(),
        warnings: vec![format!(
            "{} {reason}; treating the profile as having no exemptions",
            filename.display()
        )],
        rewritable: false,
    }
}

fn set_exemption(
    dir: &Path,
    package_version: &str,
    requested_runtime: &str,
    enabled: bool,
    accept_risk: bool,
) -> Result<(), String> {
    if !is_exact_package_version(package_version) || !is_exact_version(requested_runtime) {
        return Err(
            "Version exemptions require an exact npm package-name@version and an exact DSH runtime version"
                .to_string(),
        );
    }
    if enabled && !accept_risk {
        return Err("Incompatible plugins may cause crashes or data loss. To grant this exact-version exemption, explicitly acknowledge the risk with --accept-risk (acceptRisk: true).".to_string());
    }
    let current = runtime_version();
    if enabled && requested_runtime != current {
        return Err(format!(
            "Cannot approve DSH {requested_runtime}: this application runs DSH {current}. Use --dsh-version {current}."
        ));
    }
    let compatibility = read_compatibility(dir);
    if !compatibility.rewritable {
        return Err(format!(
            "{COMPATIBILITY_FILENAME} must be repaired before exemptions change:\n{}",
            compatibility.warnings.join("\n")
        ));
    }
    let mut exemptions = compatibility.exemptions;
    if enabled {
        let mut versions: Vec<Json> = exemptions
            .get(package_version)
            .and_then(Json::as_array)
            .cloned()
            .unwrap_or_default();
        if !versions
            .iter()
            .any(|value| value.as_str() == Some(requested_runtime))
        {
            versions.push(Json::String(requested_runtime.to_string()));
        }
        exemptions.insert(package_version.to_string(), Json::Array(versions));
    } else {
        let retained: Vec<Json> = exemptions
            .get(package_version)
            .and_then(Json::as_array)
            .map(|versions| {
                versions
                    .iter()
                    .filter(|value| value.as_str() != Some(requested_runtime))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        if retained.is_empty() {
            exemptions.remove(package_version);
        } else {
            exemptions.insert(package_version.to_string(), Json::Array(retained));
        }
    }
    let mut rendered = serde_json::to_string_pretty(&Json::Object(exemptions))
        .map_err(|error| format!("cannot render exemptions: {error}"))?;
    rendered.push('\n');
    write_private(&dir.join(COMPATIBILITY_FILENAME), &rendered)
}

fn write_private(path: &Path, contents: &str) -> Result<(), String> {
    let temporary = path.with_extension("json.tmp");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&temporary)
        .map_err(|error| format!("cannot write {}: {error}", temporary.display()))?;
    file.write_all(contents.as_bytes())
        .map_err(|error| format!("cannot write {}: {error}", temporary.display()))?;
    std::fs::rename(&temporary, path)
        .map_err(|error| format!("cannot replace {}: {error}", path.display()))
}

#[derive(Debug)]
struct ProfileLock {
    path: PathBuf,
}

impl ProfileLock {
    fn acquire(target: &Path, wait_ms: u64) -> Result<Self, String> {
        let path = PathBuf::from(format!("{}.lock", target.display()));
        let deadline = Instant::now() + Duration::from_millis(wait_ms);
        loop {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(mut file) => {
                    let _ = writeln!(file, "{}", std::process::id());
                    return Ok(Self { path });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    if Instant::now() >= deadline {
                        return Err(format!(
                            "timed out waiting for profile lock {}",
                            path.display()
                        ));
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => {
                    return Err(format!(
                        "cannot create profile lock {}: {error}",
                        path.display()
                    ));
                }
            }
        }
    }
}

impl Drop for ProfileLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn forward_to_pnpm(dir: &Path, args: &[String], home: &Path) -> i32 {
    let mut command = Command::new("pnpm");
    command
        .args(args)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("dsh: pnpm was not found; install pnpm and make it available on PATH.");
            return 127;
        }
        Err(error) => {
            eprintln!("dsh: cannot run pnpm: {error}");
            return 1;
        }
    };
    let log = start_log(home);
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let stdout_thread = stdout.map(|stream| pump(stream, false, log.clone()));
    let stderr_thread = stderr.map(|stream| pump(stream, true, log.clone()));
    let status = child.wait();
    if let Some(thread) = stdout_thread {
        let _ = thread.join();
    }
    if let Some(thread) = stderr_thread {
        let _ = thread.join();
    }
    let exit_code = match status {
        Ok(status) => status.code().unwrap_or(1),
        Err(error) => {
            eprintln!("dsh: cannot wait for pnpm: {error}");
            1
        }
    };
    if exit_code == 127 {
        eprintln!("dsh: pnpm was not found; install pnpm and make it available on PATH.");
    }
    if exit_code != 0 {
        if let Some(path) = &log {
            eprintln!(
                "dsh: plugin command failed; diagnostics: {}",
                path.display()
            );
        }
        if args.iter().any(|argument| {
            argument.starts_with("git+")
                || argument.starts_with("github:")
                || argument.contains(".git#")
                || argument
                    .rsplit_once('.')
                    .is_some_and(|(_, extension)| extension == "git")
        }) {
            eprintln!(
                "dsh: git-hosted plugins build on install via their prepare script, which pnpm blocks until allowed; add the exact key pnpm printed above under allowBuilds in {}, then re-run",
                dir.join("pnpm-workspace.yaml").display()
            );
        }
    }
    exit_code
}

fn start_log(home: &Path) -> Option<PathBuf> {
    let directory = home.join(dsh_diagnostics::LOGS_DIR);
    std::fs::create_dir_all(&directory).ok()?;
    let stamp = crate::diagnostics_io::iso_timestamp(std::time::SystemTime::now());
    let name = format!(
        "plugin-{}-{}.log",
        stamp.replace(':', "-"),
        uuid::Uuid::new_v4()
    );
    let path = directory.join(name);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(&path).ok()?;
    Some(path)
}

fn pump(
    mut stream: impl Read + Send + 'static,
    is_stderr: bool,
    log: Option<PathBuf>,
) -> std::thread::JoinHandle<()> {
    let log = log.map(|path| Arc::new(Mutex::new(path)));
    std::thread::spawn(move || {
        let mut buffer = [0_u8; 8192];
        loop {
            match stream.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(read) => {
                    let chunk = &buffer[..read];
                    if is_stderr {
                        let _ = std::io::stderr().write_all(chunk);
                    } else {
                        let _ = std::io::stdout().write_all(chunk);
                    }
                    if let Some(path) = &log
                        && let Ok(mut file) =
                            std::fs::OpenOptions::new().append(true).create(true).open(
                                &*path
                                    .lock()
                                    .unwrap_or_else(std::sync::PoisonError::into_inner),
                            )
                    {
                        let _ = file.write_all(chunk);
                    }
                }
            }
        }
    })
}

/// Whether a string is an exact `package@version` identity.
#[must_use]
pub fn is_exact_package_version(value: &str) -> bool {
    let Some(separator) = value.rfind('@') else {
        return false;
    };
    if separator == 0 {
        return false;
    }
    is_package_name(&value[..separator]) && is_exact_version(&value[separator + 1..])
}

fn is_package_name(value: &str) -> bool {
    if let Some(rest) = value.strip_prefix('@') {
        let Some((scope, name)) = rest.split_once('/') else {
            return false;
        };
        !name.contains('/') && valid_name_segment(scope) && valid_name_segment(name)
    } else {
        valid_name_segment(value)
    }
}

fn valid_name_segment(value: &str) -> bool {
    let mut characters = value.chars();
    let Some(first) = characters.next() else {
        return false;
    };
    (first.is_ascii_lowercase() || first.is_ascii_digit())
        && value.chars().all(|character| {
            character.is_ascii_lowercase()
                || character.is_ascii_digit()
                || character == '.'
                || character == '_'
                || character == '-'
        })
}

/// Whether a string is a canonical exact `SemVer` version.
#[must_use]
pub fn is_exact_version(value: &str) -> bool {
    let (without_build, build) = match value.split_once('+') {
        Some((version, build)) => {
            if build.contains('+') {
                return false;
            }
            (version, Some(build))
        }
        None => (value, None),
    };
    let (core, prerelease) = match without_build.split_once('-') {
        Some((core, prerelease)) => (core, Some(prerelease)),
        None => (without_build, None),
    };
    let mut parts = core.split('.');
    let (Some(major), Some(minor), Some(patch), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    if !numeric_identifier(major) || !numeric_identifier(minor) || !numeric_identifier(patch) {
        return false;
    }
    if let Some(prerelease) = prerelease {
        if prerelease.is_empty() {
            return false;
        }
        for identifier in prerelease.split('.') {
            if identifier.is_empty()
                || !identifier
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric() || character == '-')
            {
                return false;
            }
            if identifier
                .chars()
                .all(|character| character.is_ascii_digit())
                && identifier.len() > 1
                && identifier.starts_with('0')
            {
                return false;
            }
        }
    }
    if let Some(build) = build {
        if build.is_empty() {
            return false;
        }
        for identifier in build.split('.') {
            if identifier.is_empty()
                || !identifier
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric() || character == '-')
            {
                return false;
            }
        }
    }
    true
}

fn numeric_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.chars().all(|character| character.is_ascii_digit())
        && (value == "0" || !value.starts_with('0'))
}

#[cfg(test)]
mod tests {
    use super::{Compatibility, is_exact_package_version, is_exact_version, read_compatibility};
    use serde_json::json;

    #[test]
    fn exact_versions_reject_ranges_prefixes_and_whitespace() {
        for value in [
            "1.2.3",
            "0.0.0",
            "1.2.3-rc.1",
            "1.2.3+build.5",
            "1.2.3-rc.1+build.5",
        ] {
            assert!(is_exact_version(value), "{value}");
        }
        for value in [
            "^1.2.3",
            "~1.2.3",
            "v1.2.3",
            " 1.2.3",
            "1.2",
            "1.2.3.4",
            "01.2.3",
            "1.2.3-01",
            "1.2.3-",
            "1.2.3+",
            "1.2.3+",
            "1.2.3+a+b",
        ] {
            assert!(!is_exact_version(value), "{value}");
        }
    }

    #[test]
    fn package_versions_require_a_scoped_or_plain_exact_name() {
        assert!(is_exact_package_version("@example/plugin@1.2.3"));
        assert!(is_exact_package_version("plugin@1.2.3"));
        assert!(is_exact_package_version("a.b_c-d@1.2.3"));
        for value in [
            "plugin",
            "@example/plugin",
            "@example/plugin@^1.2.3",
            "@/x@1.2.3",
            "@scope/@1.2.3",
            "Plugin@1.2.3",
            "plugin@1.2.3@2.0.0",
        ] {
            assert!(!is_exact_package_version(value), "{value}");
        }
        assert!(!is_exact_package_version("@example/plugin@1.2.3@0.1.0"));
        assert!(!is_exact_package_version("a@b@1.2.3"));
    }

    #[test]
    fn compatibility_reads_exact_records_and_reports_the_rest() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(
            read_compatibility(dir.path()),
            Compatibility {
                exemptions: serde_json::Map::new(),
                warnings: vec![],
                rewritable: true
            }
        );
        std::fs::write(
            dir.path().join(super::COMPATIBILITY_FILENAME),
            serde_json::to_string(&json!({
                "@example/plugin@1.2.3": ["0.1.0"],
                "bad-key": ["0.1.0"],
                "other@1.2.3": ["^0.1.0"]
            }))
            .expect("json"),
        )
        .expect("write");
        let compatibility = read_compatibility(dir.path());
        assert_eq!(compatibility.exemptions.len(), 1);
        assert_eq!(compatibility.warnings.len(), 2);
        assert!(!compatibility.rewritable);
    }

    #[test]
    fn a_non_object_compatibility_file_is_not_rewritable() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join(super::COMPATIBILITY_FILENAME), "[]").expect("write");
        let compatibility = read_compatibility(dir.path());
        assert!(compatibility.exemptions.is_empty());
        assert!(!compatibility.rewritable);
        std::fs::write(dir.path().join(super::COMPATIBILITY_FILENAME), "not json").expect("write");
        assert!(!read_compatibility(dir.path()).rewritable);
    }

    #[test]
    fn a_missing_compatibility_file_is_rewritable() {
        let dir = tempfile::tempdir().expect("tempdir");
        let compatibility = read_compatibility(dir.path());
        assert!(compatibility.exemptions.is_empty());
        assert!(compatibility.warnings.is_empty());
        assert!(compatibility.rewritable);
    }

    #[cfg(unix)]
    #[test]
    fn an_unreadable_compatibility_file_is_not_rewritable() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(super::COMPATIBILITY_FILENAME);
        std::fs::write(&path, "{}").expect("write");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).expect("chmod");
        if std::fs::read_to_string(&path).is_ok() {
            return;
        }
        let compatibility = read_compatibility(dir.path());
        assert!(!compatibility.rewritable);
        assert!(compatibility.warnings[0].contains("cannot be read"));
    }

    #[test]
    fn the_profile_lock_serializes_holders_and_is_removed_on_drop() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("package.json");
        let lock_path = dir.path().join("package.json.lock");
        {
            let _lock = super::ProfileLock::acquire(&target, 1_000).expect("lock");
            assert!(lock_path.exists());
        }
        assert!(!lock_path.exists());
    }

    #[test]
    fn a_contended_lock_waits_for_the_holder_to_release() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("package.json");
        let holder = super::ProfileLock::acquire(&target, 1_000).expect("holder");
        let waiting = { std::thread::spawn(move || super::ProfileLock::acquire(&target, 1_000)) };
        std::thread::sleep(std::time::Duration::from_millis(30));
        drop(holder);
        assert!(waiting.join().expect("thread").is_ok());
    }

    #[test]
    fn a_held_lock_times_out_after_the_wait() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("package.json");
        let _holder = super::ProfileLock::acquire(&target, 1_000).expect("holder");
        let error = super::ProfileLock::acquire(&target, 20).expect_err("timeout");
        assert!(error.contains("timed out waiting for profile lock"));
    }

    #[test]
    fn a_lock_in_a_missing_directory_reports_the_creation_error() {
        let directory = tempfile::tempdir().expect("tempdir");
        let target = directory.path().join("missing/package.json");
        let error = super::ProfileLock::acquire(&target, 20).expect_err("error");
        assert!(error.contains("cannot create profile lock"), "{error}");
    }

    #[test]
    fn desktop_commands_require_the_application_to_initialize_the_profile() {
        let home = tempfile::tempdir().expect("tempdir");
        assert_eq!(
            super::run_plugin("desktop", &["list".to_string()], false, home.path()),
            1
        );
        assert!(!home.path().join("profiles/desktop/package.json").exists());
    }

    #[test]
    fn exemption_usage_names_the_risk_flag_only_for_grants() {
        let dir = tempfile::tempdir().expect("tempdir");
        let allow = super::execute_version_command("p", dir.path(), "allow-version", &[]);
        assert!(allow.expect_err("usage").contains("--accept-risk"));
        let revoke = super::execute_version_command("p", dir.path(), "revoke-version", &[]);
        let message = revoke.expect_err("usage");
        assert!(message.contains("--dsh-version"));
        assert!(!message.contains("--accept-risk"));
    }

    #[test]
    fn exemption_arguments_are_parsed_strictly() {
        let current = super::runtime_version();
        for (command, arguments) in [
            (
                "allow-version",
                vec!["p@1.2.3", "--dsh-version", "999.0.0", "--accept-risk"],
            ),
            (
                "allow-version",
                vec!["p@1.2.3", "--dsh-version", current.as_str()],
            ),
            (
                "allow-version",
                vec![
                    "p@1.2.3",
                    "--dsh-version",
                    current.as_str(),
                    "--accept-risk",
                    "--accept-risk",
                ],
            ),
            (
                "revoke-version",
                vec![
                    "p@1.2.3",
                    "--dsh-version",
                    current.as_str(),
                    "--accept-risk",
                ],
            ),
            ("version-exemptions", vec!["extra"]),
            (
                "allow-version",
                vec![
                    "p@^1.2.3",
                    "--dsh-version",
                    current.as_str(),
                    "--accept-risk",
                ],
            ),
        ] {
            let directory = tempfile::tempdir().expect("tempdir");
            let owned: Vec<String> = arguments.iter().map(|value| (*value).to_string()).collect();
            assert!(
                super::execute_version_command("p", directory.path(), command, &owned).is_err(),
                "{command} {arguments:?}"
            );
        }
    }

    #[test]
    fn an_unknown_option_is_not_accepted_as_a_package_version() {
        let directory = tempfile::tempdir().expect("tempdir");
        let current = super::runtime_version();
        let arguments = vec![
            "--dsh-version".to_string(),
            current,
            "--accept-risk".to_string(),
        ];
        let error =
            super::execute_version_command("p", directory.path(), "revoke-version", &arguments)
                .expect_err("unexpected");
        assert!(error.contains("unexpected argument"), "{error}");
    }

    #[test]
    fn additional_exact_version_shapes_are_accepted_or_rejected() {
        for value in [
            "1.2.3-0",
            "1.2.3-0a",
            "1.2.3-rc-1",
            "1.2.3+bu-ild",
            "1.2.3+001",
        ] {
            assert!(super::is_exact_version(value), "{value}");
        }
        for value in ["1.2.3-a..b", "1.2.3+", "1.2.3+a..b", "1.2.3-", "1.2.01"] {
            assert!(!super::is_exact_version(value), "{value}");
        }
    }
}
