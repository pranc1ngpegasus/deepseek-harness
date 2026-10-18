#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::assert_is_empty,
    clippy::bool_assert_comparison,
    clippy::float_cmp
)]
//! End-to-end acceptance for the `dsh` binary.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

const fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_dsh")
}

struct Fixture {
    home: PathBuf,
    install: PathBuf,
    temp: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().expect("tempdir");
        let home = temp.path().join("home");
        let install = temp.path().join("install");
        fs::create_dir_all(&home).expect("home");
        fs::create_dir_all(&install).expect("install");
        fs::write(install.join("package.json"), "{\"name\":\"dsh\"}").expect("manifest");
        Self {
            home,
            install,
            temp,
        }
    }

    fn bundle(&self, name: &str, patch: &str) {
        let dir = self.install.join("node_modules").join(name);
        fs::create_dir_all(&dir).expect("bundle dir");
        fs::write(
            dir.join("package.json"),
            format!(
                "{{\"name\":{name:?},\"dsh\":{{\"bundle\":{{\"patch\":\"cordis.patch.yml\"}}}}}}"
            ),
        )
        .expect("bundle manifest");
        fs::write(dir.join("cordis.patch.yml"), patch).expect("bundle patch");
    }

    fn base_bundles(&self) {
        self.bundle(
            "@deepseek-ai/dsh-base",
            "- insert:\n    - id: base-a\n      name: plugin-a\n",
        );
        self.bundle(
            "@deepseek-ai/dsh-web-app",
            "- insert:\n    - id: web-b\n      name: plugin-b\n",
        );
    }

    fn profile_dir(&self, name: &str) -> PathBuf {
        self.home.join("profiles").join(name)
    }

    fn write_profile_patch(&self, name: &str, patch: &str) {
        let dir = self.profile_dir(name);
        fs::create_dir_all(&dir).expect("profile dir");
        fs::write(dir.join("cordis.patch.yml"), patch).expect("profile patch");
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(binary());
        command
            .args(args)
            .env("DSH_HOME", &self.home)
            .env("DSH_INSTALL_ANCHOR", self.install.join("package.json"))
            .env_remove("DSH_TELEMETRY_DISABLED");
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command(args).output().expect("run dsh")
    }
}

fn text(output: &Output) -> (String, String) {
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

#[test]
fn dumps_the_composed_profile_with_layer_comments() {
    let fixture = Fixture::new();
    fixture.base_bundles();
    fixture.write_profile_patch("web", "- id: base-a\n  config:\n    value: 2\n");
    fs::write(
        fixture.home.join("cordis.patch.yml"),
        "- id: web-b\n  disabled: true\n",
    )
    .expect("home patch");
    let output = fixture.run(&["web", "--dump-config"]);
    let (stdout, stderr) = text(&output);
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert!(stdout.starts_with("# == @deepseek-ai/dsh-base, patched by "));
    assert!(stdout.contains("- id: base-a"));
    assert!(stdout.contains("    value: 2"));
    assert!(stdout.contains("- id: web-b"));
    assert!(stdout.contains("  disabled: true"));
    assert!(stdout.contains("cordis.patch.yml"));
    assert!(stderr.is_empty(), "{stderr}");
}

#[test]
fn default_only_omits_user_and_overlay_layers() {
    let fixture = Fixture::new();
    fixture.base_bundles();
    fixture.write_profile_patch("web", "- id: base-a\n  disabled: true\n");
    let overlay = fixture.temp.path().join("overlay.yml");
    fs::write(&overlay, "- id: web-b\n  disabled: true\n").expect("overlay");
    let default = fixture.run(&["web", "--dump-default-config"]);
    let (stdout, _) = text(&default);
    assert_eq!(default.status.code(), Some(0));
    assert!(!stdout.contains("disabled"));
    assert!(stdout.contains("- id: base-a"));
    let with_overlay = fixture.run(&[
        "web",
        "--dump-config",
        "--patch",
        overlay.to_str().expect("path"),
    ]);
    let (stdout, _) = text(&with_overlay);
    assert!(stdout.contains("disabled: true"));

    let invalid = fixture.run(&["web", "--dump-default-config", "--patch", "x.yml"]);
    assert_eq!(invalid.status.code(), Some(1));
    let (_, stderr) = text(&invalid);
    assert!(stderr.contains("takes no --patch"));
}

#[test]
fn a_skipped_bundle_is_reported_and_makes_the_schema_incomplete() {
    let fixture = Fixture::new();
    // Initializing with no installation bundle leaves the base selection unloadable.
    let init = fixture.run(&["plugin", "--profile", "custom", "version-exemptions"]);
    assert_eq!(init.status.code(), Some(0), "{}", text(&init).1);
    let output = fixture.run(&["custom", "--dump-config"]);
    let (_, stderr) = text(&output);
    assert_eq!(output.status.code(), Some(0));
    assert!(stderr.contains("skipping profile bundle"));

    let schema = fixture.run(&["custom", "--dump-config-schema"]);
    let (stdout, stderr) = text(&schema);
    assert_eq!(schema.status.code(), Some(1));
    let document: serde_json::Value = serde_json::from_str(&stdout).expect("json");
    assert_eq!(document["x-cordis"]["complete"], false);
    assert!(stderr.contains("could not be loaded"));

    fixture.base_bundles();
    let complete = fixture.run(&["web", "--dump-config-schema"]);
    let (stdout, _) = text(&complete);
    assert_eq!(complete.status.code(), Some(0));
    let document: serde_json::Value = serde_json::from_str(&stdout).expect("json");
    assert_eq!(document["type"], "array");
    assert_eq!(document["x-cordis"]["complete"], true);
}

#[test]
fn rejects_dump_arguments_and_conflicting_flags() {
    let fixture = Fixture::new();
    let output = fixture.run(&["web", "--dump-config", "task"]);
    assert_eq!(output.status.code(), Some(1));
    let (_, stderr) = text(&output);
    assert!(stderr.contains("config dumps take no app arguments"));
}

#[test]
fn grants_lists_and_revokes_exact_version_exemptions() {
    let fixture = Fixture::new();
    let runtime = env!("CARGO_PKG_VERSION");
    let allow = fixture.run(&[
        "plugin",
        "--profile",
        "test",
        "allow-version",
        "plugin@1.2.3",
        "--dsh-version",
        runtime,
        "--accept-risk",
    ]);
    assert_eq!(allow.status.code(), Some(0), "{}", text(&allow).1);
    assert!(
        text(&allow)
            .1
            .contains("allowing incompatible plugin versions")
    );

    let repeated = fixture.run(&[
        "plugin",
        "--profile",
        "test",
        "allow-version",
        "plugin@1.2.3",
        "--dsh-version",
        runtime,
        "--accept-risk",
    ]);
    assert_eq!(repeated.status.code(), Some(0), "{}", text(&repeated).1);

    let listed = fixture.run(&["plugin", "--profile", "test", "version-exemptions"]);
    assert_eq!(listed.status.code(), Some(0));
    let document: serde_json::Value =
        serde_json::from_str(&text(&listed).0).expect("exemption json");
    assert_eq!(document["plugin@1.2.3"][0], runtime);
    assert_eq!(document["plugin@1.2.3"].as_array().expect("list").len(), 1);

    let revoke = fixture.run(&[
        "plugin",
        "--profile",
        "test",
        "revoke-version",
        "plugin@1.2.3",
        "--dsh-version",
        runtime,
    ]);
    assert_eq!(revoke.status.code(), Some(0));
    assert!(text(&revoke).0.contains("revoked"));
    let listed = fixture.run(&["plugin", "--profile", "test", "version-exemptions"]);
    assert_eq!(text(&listed).0.trim(), "{}");

    for arguments in [
        vec![
            "allow-version",
            "plugin@^1.2.3",
            "--dsh-version",
            runtime,
            "--accept-risk",
        ],
        vec![
            "allow-version",
            "plugin@1.2.3",
            "--dsh-version",
            "999.0.0",
            "--accept-risk",
        ],
        vec!["allow-version", "plugin@1.2.3", "--dsh-version", runtime],
        vec!["revoke-version", "plugin@1.2.3"],
        vec!["version-exemptions", "extra"],
    ] {
        let mut full = vec!["plugin", "--profile", "test"];
        full.extend(arguments);
        let output = fixture.run(&full);
        assert_eq!(output.status.code(), Some(1), "{full:?}");
    }
    assert!(
        fixture
            .profile_dir("test")
            .join("compatibility.json")
            .exists()
    );
}

#[test]
fn forwards_other_plugin_commands_to_pnpm_and_keeps_a_log() {
    let fixture = Fixture::new();
    let bin = fixture.temp.path().join("bin");
    fs::create_dir_all(&bin).expect("bin");
    let pnpm = bin.join("pnpm");
    fs::write(&pnpm, "#!/bin/sh\necho \"pnpm $@\"\nexit 3\n").expect("pnpm script");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&pnpm, fs::Permissions::from_mode(0o755)).expect("chmod");
    }
    fs::create_dir_all(fixture.profile_dir("test")).expect("profile");
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let output = fixture
        .command(&["plugin", "--profile", "test", "list"])
        .env("PATH", path)
        .output()
        .expect("run");
    let (stdout, stderr) = text(&output);
    assert_eq!(output.status.code(), Some(3));
    assert!(stdout.contains("pnpm list"), "{stdout}");
    assert!(stderr.contains("plugin command failed; diagnostics:"));
    let logs = fixture.home.join("logs");
    assert_eq!(fs::read_dir(&logs).expect("logs").count(), 1);
}

#[test]
fn boots_a_profile_through_the_host_seam() {
    let fixture = Fixture::new();
    fixture.base_bundles();
    let overlay = fixture.temp.path().join("overlay.yml");
    fs::write(
        &overlay,
        "- insert:\n    - id: overlay-row\n      name: extra\n",
    )
    .expect("overlay");
    let host = fixture.temp.path().join("host.sh");
    fs::write(
        &host,
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$HOST_ARGS\"\nprintf '%s' \"$DSH_PROFILE_CONTEXT\" > \"$HOST_CONTEXT\"\nprintf '%s' \"$DSH_PROFILE_PATCHES\" > \"$HOST_PATCHES\"\nexit 7\n",
    )
    .expect("host script");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&host, fs::Permissions::from_mode(0o755)).expect("chmod");
    }
    let args_path = fixture.temp.path().join("args");
    let context_path = fixture.temp.path().join("context");
    let patches_path = fixture.temp.path().join("patches");
    let output = fixture
        .command(&[
            "web",
            "--patch",
            overlay.to_str().expect("path"),
            "--resume",
            "abc",
        ])
        .env("DSH_HOST", &host)
        .env("HOST_ARGS", &args_path)
        .env("HOST_CONTEXT", &context_path)
        .env("HOST_PATCHES", &patches_path)
        .output()
        .expect("run");
    assert_eq!(output.status.code(), Some(7), "{}", text(&output).1);
    let args = fs::read_to_string(&args_path).expect("args");
    assert!(args.contains("cordis.yml"));
    assert!(args.contains("--resume\nabc"));
    let context: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&context_path).expect("context")).expect("json");
    assert_eq!(context["name"], "web");
    assert_eq!(context["startedBundles"][0], "@deepseek-ai/dsh-base");
    assert_eq!(context["overlays"][0]["insert"][0]["id"], "overlay-row");
    let patches: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&patches_path).expect("patches")).expect("json");
    assert_eq!(patches[0]["insert"][0]["id"], "base-a");
    assert!(
        patches
            .as_array()
            .expect("array")
            .iter()
            .any(|patch| patch["insert"][0]["id"] == "overlay-row")
    );
}

#[test]
fn reports_a_missing_host_as_a_startup_failure() {
    let fixture = Fixture::new();
    fixture.base_bundles();
    let output = fixture
        .command(&["web"])
        .env("DSH_HOST", "/nonexistent/dsh-host")
        .output()
        .expect("run");
    assert_eq!(output.status.code(), Some(1));
    let (_, stderr) = text(&output);
    assert!(stderr.contains("cannot start the harness host"));
    assert!(stderr.contains("Full diagnostics: "));
    assert_eq!(
        fs::read_dir(fixture.home.join("logs"))
            .expect("logs")
            .count(),
        1
    );
}

#[test]
fn help_and_version_keep_the_launcher_surface() {
    let fixture = Fixture::new();
    let help = fixture.run(&["--help"]);
    assert_eq!(help.status.code(), Some(0));
    assert!(text(&help).0.starts_with("Usage: dsh [--profile] <name>"));
    let version = fixture.run(&["--version"]);
    assert_eq!(version.status.code(), Some(0));
    assert_eq!(text(&version).0.trim(), env!("CARGO_PKG_VERSION"));
    let missing = fixture.run(&[]);
    assert_eq!(missing.status.code(), Some(1));
    assert!(text(&missing).1.contains("--profile <name> is required"));
}

#[test]
fn a_from_default_template_recreates_the_profile() {
    let fixture = Fixture::new();
    fixture.base_bundles();
    fixture.write_profile_patch("rescue", "- id: old\n  disabled: true\n");
    let output = fixture.run(&["rescue", "--from-default-profile", "web", "--dump-config"]);
    let (stdout, stderr) = text(&output);
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert!(stdout.contains("- id: base-a"));
    assert!(!stdout.contains("old"));
    let reserved = fixture.run(&["Web", "--from-default-profile", "web", "--dump-config"]);
    assert_eq!(reserved.status.code(), Some(1));
    assert!(text(&reserved).1.contains("names shipped profile"));
}

#[test]
fn a_missing_pnpm_is_reported_as_127() {
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.profile_dir("test")).expect("profile");
    let empty = fixture.temp.path().join("empty-path");
    fs::create_dir_all(&empty).expect("empty");
    let output = fixture
        .command(&["plugin", "--profile", "test", "list"])
        .env("PATH", &empty)
        .output()
        .expect("run");
    assert_eq!(output.status.code(), Some(127), "{}", text(&output).1);
    let (_, stderr) = text(&output);
    assert!(stderr.contains("pnpm was not found"));
}

#[test]
fn a_git_hosted_plugin_failure_names_the_build_allowlist() {
    let fixture = Fixture::new();
    let bin = fixture.temp.path().join("bin");
    fs::create_dir_all(&bin).expect("bin");
    let pnpm = bin.join("pnpm");
    fs::write(&pnpm, "#!/bin/sh\nexit 1\n").expect("pnpm script");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&pnpm, fs::Permissions::from_mode(0o755)).expect("chmod");
    }
    fs::create_dir_all(fixture.profile_dir("test")).expect("profile");
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let output = fixture
        .command(&[
            "plugin",
            "--profile",
            "test",
            "add",
            "github:example/plugin",
        ])
        .env("PATH", path)
        .output()
        .expect("run");
    assert_eq!(output.status.code(), Some(1));
    let (_, stderr) = text(&output);
    assert!(stderr.contains("git-hosted plugins"), "{stderr}");
}

#[test]
fn a_failing_forwarded_command_does_not_claim_pnpm_is_missing() {
    let fixture = Fixture::new();
    let bin = fixture.temp.path().join("bin");
    fs::create_dir_all(&bin).expect("bin");
    let pnpm = bin.join("pnpm");
    fs::write(&pnpm, "#!/bin/sh\nexit 3\n").expect("pnpm script");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&pnpm, fs::Permissions::from_mode(0o755)).expect("chmod");
    }
    fs::create_dir_all(fixture.profile_dir("test")).expect("profile");
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let output = fixture
        .command(&["plugin", "--profile", "test", "list"])
        .env("PATH", path)
        .output()
        .expect("run");
    assert_eq!(output.status.code(), Some(3));
    let (_, stderr) = text(&output);
    assert!(!stderr.contains("pnpm was not found"), "{stderr}");
}

#[test]
fn forwarding_initializes_a_missing_profile_before_running_pnpm() {
    let fixture = Fixture::new();
    let bin = fixture.temp.path().join("bin");
    fs::create_dir_all(&bin).expect("bin");
    let pnpm = bin.join("pnpm");
    fs::write(&pnpm, "#!/bin/sh\npwd\nexit 0\n").expect("pnpm script");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&pnpm, fs::Permissions::from_mode(0o755)).expect("chmod");
    }
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let output = fixture
        .command(&["plugin", "--profile", "fresh", "list"])
        .env("PATH", path)
        .output()
        .expect("run");
    let (stdout, stderr) = text(&output);
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert!(stdout.contains("profiles/fresh"), "{stdout}");
    assert!(fixture.profile_dir("fresh").join("package.json").is_file());
}

#[test]
fn every_git_hosted_argument_form_names_the_build_allowlist() {
    let fixture = Fixture::new();
    let bin = fixture.temp.path().join("bin");
    fs::create_dir_all(&bin).expect("bin");
    let pnpm = bin.join("pnpm");
    fs::write(&pnpm, "#!/bin/sh\nexit 1\n").expect("pnpm script");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&pnpm, fs::Permissions::from_mode(0o755)).expect("chmod");
    }
    fs::create_dir_all(fixture.profile_dir("test")).expect("profile");
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    for argument in [
        "git+https://example/plugin",
        "example.git",
        "example.git#frag",
    ] {
        let output = fixture
            .command(&["plugin", "--profile", "test", "add", argument])
            .env("PATH", &path)
            .output()
            .expect("run");
        assert_eq!(output.status.code(), Some(1));
        assert!(
            text(&output).1.contains("git-hosted plugins"),
            "{argument}: {}",
            text(&output).1
        );
    }
}

#[test]
fn a_pnpm_that_cannot_execute_reports_the_spawn_error() {
    let fixture = Fixture::new();
    let bin = fixture.temp.path().join("bin");
    fs::create_dir_all(&bin).expect("bin");
    let pnpm = bin.join("pnpm");
    fs::write(&pnpm, "#!/bin/sh\nexit 0\n").expect("pnpm script");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&pnpm, fs::Permissions::from_mode(0o644)).expect("chmod");
    }
    fs::create_dir_all(fixture.profile_dir("test")).expect("profile");
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let output = fixture
        .command(&["plugin", "--profile", "test", "list"])
        .env("PATH", path)
        .output()
        .expect("run");
    assert_eq!(output.status.code(), Some(1));
    let (_, stderr) = text(&output);
    assert!(stderr.contains("cannot run pnpm"), "{stderr}");
    assert!(!stderr.contains("pnpm was not found"), "{stderr}");
}
