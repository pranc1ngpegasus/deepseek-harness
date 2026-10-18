//! Profile boot: compose the tree and hand it to the harness host.
//!
//! The plugin mount is the host's responsibility. The launcher passes the
//! root config path, the inner application arguments, the frozen profile
//! context, and the ordered patch list through the environment, and owns the
//! bounded shutdown of the host process.

use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use dsh_config::Patch;
use dsh_shutdown::{Action, PROCESS_SHUTDOWN_TIMEOUT_MS, ShutdownMachine};

use crate::dump::patches_to_json;
use crate::profile_store::{Profile, read_profile_patches};

/// Environment variable naming the harness host executable.
pub const HOST_ENV: &str = "DSH_HOST";

/// Environment variable carrying the profile context as JSON.
pub const PROFILE_CONTEXT_ENV: &str = "DSH_PROFILE_CONTEXT";

/// Environment variable carrying the ordered patch list as JSON.
pub const PROFILE_PATCHES_ENV: &str = "DSH_PROFILE_PATCHES";

/// The default harness host executable name.
pub const DEFAULT_HOST: &str = "dsh-host";

/// Everything one profile boot needs.
pub struct RunOptions<'a> {
    /// The prepared profile.
    pub profile: &'a Profile,
    /// Parsed `--patch` overlays in argv order.
    pub overlays: &'a [Patch],
    /// The invocation's inner arguments.
    pub args: &'a [String],
    /// Harness home.
    pub home: &'a Path,
    /// Installation package.json.
    pub install_anchor: &'a Path,
    /// Launch-time `DSH_TELEMETRY_DISABLED`, as the launcher observed it.
    pub telemetry_disabled: Option<&'a str>,
}

/// Compose and boot one profile, returning the process exit status.
///
/// # Errors
/// Returns a diagnostic when the patch layers cannot be read or the host
/// cannot be started.
pub fn run_profile(options: &RunOptions<'_>) -> Result<i32, String> {
    let patches = read_profile_patches(
        options.profile,
        options.overlays,
        options.home,
        options.telemetry_disabled,
    )?;
    let root = options.profile.dir.join(dsh_profile::PROFILE_ROOT_FILENAME);
    let context = profile_context(options);
    let host = std::env::var(HOST_ENV).unwrap_or_else(|_| DEFAULT_HOST.to_string());
    let mut command = Command::new(&host);
    command
        .arg(&root)
        .args(options.args)
        .current_dir(std::env::current_dir().map_err(|error| error.to_string())?)
        .env(
            PROFILE_CONTEXT_ENV,
            serde_json::to_string(&context)
                .map_err(|error| format!("dsh: cannot render profile context: {error}"))?,
        )
        .env(
            PROFILE_PATCHES_ENV,
            serde_json::to_string(&patches_to_json(&patches))
                .map_err(|error| format!("dsh: cannot render profile patches: {error}"))?,
        )
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    let child = command.spawn().map_err(|error| {
        format!(
            "dsh: cannot start the harness host {host:?} for profile {:?}: {error}",
            options.profile.name
        )
    })?;
    Ok(supervise(
        child,
        &signal_receiver(),
        Duration::from_millis(PROCESS_SHUTDOWN_TIMEOUT_MS),
    ))
}

fn profile_context(options: &RunOptions<'_>) -> serde_json::Value {
    let mut context = serde_json::Map::new();
    context.insert(
        "name".to_string(),
        serde_json::Value::String(options.profile.name.clone()),
    );
    context.insert(
        "dir".to_string(),
        serde_json::Value::String(options.profile.dir.display().to_string()),
    );
    context.insert(
        "patchPath".to_string(),
        serde_json::Value::String(options.profile.patch_path.display().to_string()),
    );
    context.insert(
        "installAnchor".to_string(),
        serde_json::Value::String(options.install_anchor.display().to_string()),
    );
    context.insert(
        "cwd".to_string(),
        serde_json::Value::String(std::env::current_dir().map_or_else(
            |_| String::new(),
            |directory| directory.display().to_string(),
        )),
    );
    context.insert(
        "home".to_string(),
        serde_json::Value::String(options.home.display().to_string()),
    );
    context.insert(
        "startedBundles".to_string(),
        serde_json::Value::Array(
            options
                .profile
                .layers
                .iter()
                .map(|layer| serde_json::Value::String(layer.package_name.clone()))
                .collect(),
        ),
    );
    context.insert("overlays".to_string(), patches_to_json(options.overlays));
    context.insert(
        "telemetryDisabledEnv".to_string(),
        options
            .telemetry_disabled
            .map_or(serde_json::Value::Null, |value| {
                serde_json::Value::String(value.to_string())
            }),
    );
    serde_json::Value::Object(context)
}

/// The signal codes a supervisor forwards: `0` for SIGTERM, `130` for SIGINT.
fn signal_receiver() -> Receiver<i32> {
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let Ok(mut signals) = signal_hook::iterator::Signals::new([
            signal_hook::consts::SIGTERM,
            signal_hook::consts::SIGINT,
        ]) else {
            return;
        };
        for signal in signals.forever() {
            let code = signal_code(signal);
            if sender.send(code).is_err() {
                return;
            }
        }
    });
    receiver
}

/// The exit code a forwarded signal reports.
const fn signal_code(signal: i32) -> i32 {
    if signal == signal_hook::consts::SIGINT {
        130
    } else {
        0
    }
}

fn supervise(mut child: std::process::Child, signals: &Receiver<i32>, grace: Duration) -> i32 {
    let mut machine = ShutdownMachine::new();
    let mut deadline: Option<Instant> = None;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if machine.started() {
                    return match machine.dispose_resolved() {
                        Action::Complete(code) | Action::ForceExit(code) => code,
                        _ => status.code().unwrap_or(1),
                    };
                }
                return status.code().unwrap_or(1);
            }
            Ok(None) => {}
            Err(_) => return 1,
        }
        while let Ok(code) = signals.try_recv() {
            let action = if code == 0 {
                machine.shutdown(0)
            } else {
                machine.interrupt(code)
            };
            match action {
                Action::StartDispose => {
                    terminate(&child);
                    deadline = Some(Instant::now() + grace);
                }
                Action::ForceExit(code) => {
                    let _ = child.kill();
                    return code;
                }
                Action::Complete(code) => return code,
                Action::None => {}
            }
        }
        if deadline.is_some_and(|deadline| Instant::now() >= deadline)
            && let Action::ForceExit(code) = machine.timeout()
        {
            let _ = child.kill();
            return code;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn terminate(child: &std::process::Child) {
    #[cfg(unix)]
    {
        let pid = nix::unistd::Pid::from_raw(i32::try_from(child.id()).unwrap_or(i32::MAX));
        let _ = nix::sys::signal::kill(pid, nix::sys::signal::Signal::SIGTERM);
    }
    #[cfg(not(unix))]
    {
        let _ = child;
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::{signal_code, supervise};
    use std::os::unix::fs::PermissionsExt;
    use std::sync::mpsc::channel;
    use std::time::{Duration, Instant};

    fn script(directory: &std::path::Path, body: &str) -> std::path::PathBuf {
        let path = directory.join("child.sh");
        std::fs::write(&path, body).expect("script");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        path
    }

    #[test]
    fn interrupts_report_130_and_terminations_report_zero() {
        assert_eq!(signal_code(signal_hook::consts::SIGINT), 130);
        assert_eq!(signal_code(signal_hook::consts::SIGTERM), 0);
    }

    fn wait_for(path: &std::path::Path) {
        for _ in 0..200 {
            if path.exists() {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("child did not become ready: {}", path.display());
    }

    fn spawn(mut command: std::process::Command) -> std::process::Child {
        let mut last_error = None;
        for _ in 0..20 {
            match command.spawn() {
                Ok(child) => return child,
                Err(error) => {
                    last_error = Some(error);
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
        }
        panic!("spawn: {}", last_error.expect("a spawn error"));
    }

    #[test]
    fn a_child_that_exits_on_its_own_keeps_its_status() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = script(directory.path(), "#!/bin/sh\nexit 7\n");
        let child = std::process::Command::new(path).spawn().expect("spawn");
        let (_sender, receiver) = channel();
        assert_eq!(supervise(child, &receiver, Duration::from_millis(100)), 7);
    }

    #[test]
    fn a_termination_request_reaches_the_child_and_completes_zero() {
        let directory = tempfile::tempdir().expect("tempdir");
        let marker = directory.path().join("marker");
        let ready = directory.path().join("ready");
        let path = script(
            directory.path(),
            "#!/bin/sh\ntrap 'touch \"$MARKER\"; exit 0' TERM\ntouch \"$READY\"\nwhile :; do sleep 1; done\n",
        );
        let mut command = std::process::Command::new(path);
        command.env("MARKER", &marker).env("READY", &ready);
        let child = spawn(command);
        wait_for(&ready);
        let (sender, receiver) = channel();
        sender.send(0).expect("signal");
        let started = Instant::now();
        assert_eq!(supervise(child, &receiver, Duration::from_millis(2_000)), 0);
        assert!(marker.exists(), "the child received SIGTERM");
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn a_sigint_request_forces_the_interrupt_code_after_disposal() {
        let directory = tempfile::tempdir().expect("tempdir");
        let marker = directory.path().join("marker");
        let ready = directory.path().join("ready");
        let path = script(
            directory.path(),
            "#!/bin/sh\ntrap 'touch \"$MARKER\"; exit 0' TERM\ntouch \"$READY\"\nwhile :; do sleep 1; done\n",
        );
        let mut command = std::process::Command::new(path);
        command.env("MARKER", &marker).env("READY", &ready);
        let child = spawn(command);
        wait_for(&ready);
        let (sender, receiver) = channel();
        sender.send(130).expect("signal");
        assert_eq!(
            supervise(child, &receiver, Duration::from_millis(2_000)),
            130
        );
        assert!(marker.exists());
    }

    #[test]
    fn the_grace_period_is_bounded_and_then_kills_the_child() {
        let directory = tempfile::tempdir().expect("tempdir");
        let marker = directory.path().join("marker");
        let ready = directory.path().join("ready");
        let path = script(
            directory.path(),
            "#!/bin/sh\ntrap 'touch \"$MARKER\"' TERM\ntouch \"$READY\"\nwhile :; do sleep 1; done\n",
        );
        let mut command = std::process::Command::new(path);
        command.env("MARKER", &marker).env("READY", &ready);
        let child = spawn(command);
        wait_for(&ready);
        let (sender, receiver) = channel();
        sender.send(0).expect("signal");
        let started = Instant::now();
        assert_eq!(supervise(child, &receiver, Duration::from_millis(100)), 0);
        let elapsed = started.elapsed();
        assert!(elapsed >= Duration::from_millis(90), "{elapsed:?}");
        assert!(elapsed < Duration::from_secs(3), "{elapsed:?}");
    }
}
