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
//! Launcher argument resolution for the `dsh` command line.
//!
//! The launcher owns only the profile selection, the patch overlays, the
//! configuration dumps, and `plugin` forwarding. The first token it does not
//! recognize starts the inner arguments, which reach the booted application
//! verbatim; every app flag, including its own `-h`, belongs to the app. The
//! resolver is pure: it returns the invocation or the exact text and exit code
//! a caller writes to its streams, so the same behavior is testable without a
//! process.
#![no_std]
extern crate alloc;

#[cfg(test)]
extern crate std;

use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

/// Program name printed in usage text.
pub const BIN_NAME: &str = "dsh";

/// The launcher's own help, printed when no application can receive `-h`.
pub const HELP: &str = "\
Usage: dsh [--profile] <name> [options] [app-args...]
       dsh plugin --profile <name> <pnpm-args...>

dsh: boot a DeepSeek Harness profile — an ordered stack of plugin-bundle patch
layers under your own overrides.

Arguments:
  args                           arguments for the booted profile's app (see:
                                 dsh --profile <name> --help)

Options:
  -V, --version                  output the version number
  --profile <name>               the profile under $DSH_HOME/profiles to boot
  --from-default-profile <name>  replace a custom profile with a fresh copy of a
                                 shipped profile template
  --patch <path>                 extra patch-list overlay applied after the
                                 profile layer (repeatable)
  --dump-config                  print the composed profile tree and exit
  --dump-config-schema           print JSON Schema for profile entries and
                                 patches without mounting
  --dump-default-config          print the profile tree without its user layer
                                 or --patch overlays and exit

Examples:
  dsh web                                   boot the web profile (same as: dsh --profile web)
  dsh rescue --from-default-profile web
                                            recreate rescue from the shipped web template, then boot it
  dsh headless \"run the tests\"              answer one task, print the result, and exit
  dsh tui --patch ./extra.yml               boot a custom profile with one extra overlay
  dsh tui --resume <session>                arguments after the launcher flags reach the app
  dsh web --help                            the web app's own flags and help
  dsh plugin --profile tui add <package>    install a plugin into the tui profile
";

/// Help for the `plugin` forwarder.
pub const PLUGIN_HELP: &str = "\
Usage: dsh plugin [options] [args...]

manage a profile's plugins by forwarding the remaining arguments to pnpm in the
profile directory

Arguments:
  args              pnpm arguments, forwarded verbatim (add <pkg>, remove <pkg>,
                    why <pkg>, ...)

Options:
  -h, --help        show this help
  --profile <name>  the profile whose plugins to manage (initialized on first
                    use)
";

/// Boot a named profile and hand it the invocation's inner arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileInvocation {
    /// The profile under `$DSH_HOME/profiles` to boot.
    pub profile: String,
    /// Shipped template that replaces the named profile.
    pub from_default_profile: Option<String>,
    /// Extra patch-list overlays applied after the profile's own layer, in argv order.
    pub patches: Vec<String>,
    /// Everything after the launcher's own flags, verbatim, for injected app plugins.
    pub args: Vec<String>,
}

/// Print a composed profile tree and exit without booting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DumpConfigInvocation {
    /// The profile to compose.
    pub profile: String,
    /// Shipped template that replaces the named profile.
    pub from_default_profile: Option<String>,
    /// Omit the profile's user layer and `--patch` overlays; print bundle layers only.
    pub default_only: bool,
    /// Extra patch-list overlays in argv order.
    pub patches: Vec<String>,
}

/// Print declared plugin schemas without mounting the profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DumpConfigSchemaInvocation {
    /// The profile whose composition to inspect.
    pub profile: String,
    /// Shipped template that replaces the named profile.
    pub from_default_profile: Option<String>,
    /// Extra patch-list overlays in argv order.
    pub patches: Vec<String>,
}

/// Manage a profile's plugins by forwarding `args` to pnpm.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginInvocation {
    /// The profile whose plugins to manage.
    pub profile: String,
    /// Raw pnpm arguments, verbatim.
    pub args: Vec<String>,
}

/// The resolved `dsh` invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Invocation {
    /// Boot a profile.
    Profile(ProfileInvocation),
    /// Print the composed profile tree.
    DumpConfig(DumpConfigInvocation),
    /// Print the profile's configuration schema.
    DumpConfigSchema(DumpConfigSchemaInvocation),
    /// Forward package operations to pnpm.
    Plugin(PluginInvocation),
}

/// Stream contents and exit status a caller writes instead of running an invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exit {
    /// Text for standard output.
    pub stdout: String,
    /// Text for standard error.
    pub stderr: String,
    /// Process exit status.
    pub code: i32,
}

impl Exit {
    fn error(message: impl Into<String>) -> Self {
        Self {
            stdout: String::new(),
            stderr: format!("error: {}\n", message.into()),
            code: 1,
        }
    }

    fn help() -> Self {
        Self {
            stdout: HELP.to_string(),
            stderr: String::new(),
            code: 0,
        }
    }

    fn plugin_help() -> Self {
        Self {
            stdout: PLUGIN_HELP.to_string(),
            stderr: String::new(),
            code: 0,
        }
    }

    fn version(version: &str) -> Self {
        Self {
            stdout: format!("{version}\n"),
            stderr: String::new(),
            code: 0,
        }
    }
}

/// What the launcher should do with one argv.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Run the invocation.
    Run(Invocation),
    /// Write the streams and exit without running.
    Exit(Exit),
}

/// A config dump kind; at most one may be selected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DumpKind {
    /// Compose every layer and print it.
    Config,
    /// Print only the bundle layers.
    DefaultOnly,
    /// Print the schema document.
    Schema,
}

/// Collected launcher flags, before validation.
#[derive(Default)]
struct BootFlags {
    profile: Option<String>,
    from_default_profile: Option<String>,
    patches: Vec<String>,
    dumps: Vec<DumpKind>,
}

fn select_profile(slot: &mut Option<String>, value: String) -> Result<(), Exit> {
    if slot.is_some() {
        return Err(Exit::error("select a profile only once"));
    }
    *slot = Some(value);
    Ok(())
}

fn missing_argument(option: &str) -> Exit {
    Exit::error(format!("option '{option}' argument missing"))
}

/// The value of a `--name=value` token, when it matches.
fn inline_value<'a>(token: &'a str, name: &str) -> Option<&'a str> {
    token.strip_prefix(name)?.strip_prefix('=')
}

fn reject_electron_profile(profile: &str) -> Result<(), Exit> {
    if profile.to_lowercase() == "desktop" {
        return Err(Exit::error(
            "profile \"desktop\" is managed exclusively by the Electron application",
        ));
    }
    Ok(())
}

/// Resolve the boot or dump invocation from parsed flags and leftover inner arguments.
fn resolve_boot(flags: BootFlags, args: Vec<String>) -> Result<Invocation, Exit> {
    let Some(profile) = flags.profile else {
        if args
            .iter()
            .any(|argument| argument == "-h" || argument == "--help")
        {
            return Err(Exit::help());
        }
        return Err(Exit::error("--profile <name> is required"));
    };
    if profile.is_empty() {
        return Err(Exit::error("--profile needs a name"));
    }
    reject_electron_profile(&profile)?;
    if flags.patches.iter().any(String::is_empty) {
        return Err(Exit::error("--patch needs a path"));
    }
    if flags.from_default_profile.as_deref() == Some("") {
        return Err(Exit::error("--from-default-profile needs a name"));
    }
    let dump_count = flags.dumps.len();
    if dump_count == 0 {
        return Ok(Invocation::Profile(ProfileInvocation {
            profile,
            from_default_profile: flags.from_default_profile,
            patches: flags.patches,
            args,
        }));
    }
    if dump_count > 1 {
        return Err(Exit::error(
            "--dump-config, --dump-default-config, and --dump-config-schema are mutually exclusive",
        ));
    }
    if !args.is_empty() {
        let rendered = args
            .iter()
            .map(|argument| format!("{argument:?}"))
            .collect::<Vec<_>>()
            .join(" ");
        return Err(Exit::error(format!(
            "config dumps take no app arguments, got {rendered}"
        )));
    }
    let Some(kind) = flags.dumps.first() else {
        return Err(Exit::error("config dump kind is missing"));
    };
    if *kind == DumpKind::Schema {
        return Ok(Invocation::DumpConfigSchema(DumpConfigSchemaInvocation {
            profile,
            from_default_profile: flags.from_default_profile,
            patches: flags.patches,
        }));
    }
    let default_only = *kind == DumpKind::DefaultOnly;
    if default_only && !flags.patches.is_empty() {
        return Err(Exit::error(
            "--dump-default-config prints the bundle layers and takes no --patch",
        ));
    }
    Ok(Invocation::DumpConfig(DumpConfigInvocation {
        profile,
        from_default_profile: flags.from_default_profile,
        default_only,
        patches: flags.patches,
    }))
}

fn parse_boot(tokens: &[String], version: &str) -> Outcome {
    let mut flags = BootFlags::default();
    let mut args: Vec<String> = Vec::new();
    let mut remaining: VecDeque<&String> = tokens.iter().collect();
    while let Some(token) = remaining.pop_front() {
        let token = token.as_str();
        match token {
            "-V" | "--version" => return Outcome::Exit(Exit::version(version)),
            "--dump-config" => flags.dumps.push(DumpKind::Config),
            "--dump-config-schema" => flags.dumps.push(DumpKind::Schema),
            "--dump-default-config" => flags.dumps.push(DumpKind::DefaultOnly),
            "--profile" => {
                let Some(value) = remaining.pop_front() else {
                    return Outcome::Exit(missing_argument("--profile <name>"));
                };
                if let Err(exit) = select_profile(&mut flags.profile, value.clone()) {
                    return Outcome::Exit(exit);
                }
            }
            "--from-default-profile" => {
                let Some(value) = remaining.pop_front() else {
                    return Outcome::Exit(missing_argument("--from-default-profile <name>"));
                };
                flags.from_default_profile = Some(value.clone());
            }
            "--patch" => {
                let Some(value) = remaining.pop_front() else {
                    return Outcome::Exit(missing_argument("--patch <path>"));
                };
                flags.patches.push(value.clone());
            }
            "--" => {
                args.extend(remaining.drain(..).cloned());
                break;
            }
            _ => {
                if let Some(value) = inline_value(token, "--profile") {
                    if let Err(exit) = select_profile(&mut flags.profile, value.to_string()) {
                        return Outcome::Exit(exit);
                    }
                } else if let Some(value) = inline_value(token, "--from-default-profile") {
                    flags.from_default_profile = Some(value.to_string());
                } else if let Some(value) = inline_value(token, "--patch") {
                    flags.patches.push(value.to_string());
                } else {
                    args.push(token.to_string());
                    args.extend(remaining.drain(..).cloned());
                    break;
                }
            }
        }
    }
    match resolve_boot(flags, args) {
        Ok(invocation) => Outcome::Run(invocation),
        Err(exit) => Outcome::Exit(exit),
    }
}

fn parse_plugin(tokens: &[String], manage_desktop_profile: bool) -> Outcome {
    let mut profile: Option<String> = None;
    let mut forwarded: Vec<String> = Vec::new();
    let mut stopped_on_unknown_option = false;
    let mut remaining: VecDeque<&String> = tokens.iter().collect();
    while let Some(token) = remaining.pop_front() {
        let token = token.as_str();
        match token {
            "--profile" => {
                let Some(value) = remaining.pop_front() else {
                    return Outcome::Exit(missing_argument("--profile <name>"));
                };
                if let Err(exit) = select_profile(&mut profile, value.clone()) {
                    return Outcome::Exit(exit);
                }
            }
            "--" => {
                forwarded.extend(remaining.drain(..).cloned());
                break;
            }
            _ => {
                if let Some(value) = inline_value(token, "--profile") {
                    if let Err(exit) = select_profile(&mut profile, value.to_string()) {
                        return Outcome::Exit(exit);
                    }
                } else {
                    // Commander scans only the unknown run for a help flag. A
                    // stop on an option leaves the rest unknown; a stop on a
                    // positional operand forwards its help verbatim.
                    stopped_on_unknown_option = token.starts_with('-');
                    forwarded.push(token.to_string());
                    forwarded.extend(remaining.drain(..).cloned());
                    break;
                }
            }
        }
    }
    if stopped_on_unknown_option
        && forwarded
            .iter()
            .any(|argument| argument == "-h" || argument == "--help")
    {
        return Outcome::Exit(Exit::plugin_help());
    }
    let Some(profile) = profile else {
        return Outcome::Exit(Exit::error(
            "required option '--profile <name>' not specified",
        ));
    };
    if profile.is_empty() {
        return Outcome::Exit(Exit::error("--profile needs a name"));
    }
    if !manage_desktop_profile && let Err(exit) = reject_electron_profile(&profile) {
        return Outcome::Exit(exit);
    }
    if forwarded.is_empty() {
        return Outcome::Exit(Exit::error(
            "plugin needs pnpm arguments to forward (e.g. add <package>)",
        ));
    }
    let profile = if profile.to_lowercase() == "desktop" {
        "desktop".to_string()
    } else {
        profile
    };
    Outcome::Run(Invocation::Plugin(PluginInvocation {
        profile,
        args: forwarded,
    }))
}

/// Resolve argv into one invocation, or the help, version, or error output that ends the run.
///
/// `argv` holds the arguments after the program name. `version` is printed by
/// `--version`. `manage_desktop_profile` permits Desktop's installed carrier to
/// manage its reserved profile's plugins.
#[must_use]
pub fn parse(argv: &[String], version: &str, manage_desktop_profile: bool) -> Outcome {
    let Some(first) = argv.first() else {
        return parse_boot(argv, version);
    };
    if first == "plugin" {
        return parse_plugin(&argv[1..], manage_desktop_profile);
    }
    if first.starts_with('-') {
        return parse_boot(argv, version);
    }
    let mut expanded = Vec::with_capacity(argv.len() + 2);
    expanded.push("--profile".to_string());
    expanded.extend(argv.iter().cloned());
    parse_boot(&expanded, version)
}

#[cfg(test)]
mod tests {
    use super::{Exit, Invocation, Outcome, parse};
    use alloc::string::{String, ToString};
    use alloc::vec;
    use alloc::vec::Vec;

    fn argv(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    fn parse_args(values: &[&str]) -> Outcome {
        parse(&argv(values), "1.2.3", false)
    }

    fn run(values: &[&str]) -> Invocation {
        match parse_args(values) {
            Outcome::Run(invocation) => invocation,
            Outcome::Exit(exit) => panic!("expected a run, got {exit:?}"),
        }
    }

    fn exit(values: &[&str]) -> Exit {
        match parse_args(values) {
            Outcome::Exit(exit) => exit,
            Outcome::Run(invocation) => panic!("expected an exit, got {invocation:?}"),
        }
    }

    fn exit_code(values: &[&str]) -> i32 {
        exit(values).code
    }

    fn profile(
        name: &str,
        from_default_profile: Option<&str>,
        patches: &[&str],
        args: &[&str],
    ) -> Invocation {
        Invocation::Profile(super::ProfileInvocation {
            profile: name.to_string(),
            from_default_profile: from_default_profile.map(ToString::to_string),
            patches: argv(patches),
            args: argv(args),
        })
    }

    #[test]
    fn routes_profile_boots_and_shorthand() {
        assert_eq!(run(&["--profile", "tui"]), profile("tui", None, &[], &[]));
        assert_eq!(
            run(&["--profile", "tui", "--patch", "a.yml", "--patch", "b.yml"]),
            profile("tui", None, &["a.yml", "b.yml"], &[])
        );
        assert_eq!(
            run(&["--profile", "rescue", "--from-default-profile", "web"]),
            profile("rescue", Some("web"), &[], &[])
        );
        assert_eq!(run(&["web"]), profile("web", None, &[], &[]));
        assert_eq!(
            run(&["web", "--patch", "web.yml"]),
            profile("web", None, &["web.yml"], &[])
        );
    }

    #[test]
    fn ends_launcher_flags_at_the_first_token_it_does_not_own() {
        assert_eq!(
            run(&["--profile", "tui", "--resume", "abc"]),
            profile("tui", None, &[], &["--resume", "abc"])
        );
        assert_eq!(
            run(&["--profile", "web", "-h"]),
            profile("web", None, &[], &["-h"])
        );
        assert_eq!(
            run(&[
                "web",
                "--host",
                "127.0.0.1",
                "--port",
                "8080",
                "--no-open",
                "--future-web-flag",
            ]),
            profile(
                "web",
                None,
                &[],
                &[
                    "--host",
                    "127.0.0.1",
                    "--port",
                    "8080",
                    "--no-open",
                    "--future-web-flag",
                ]
            )
        );
        assert_eq!(
            run(&["--profile", "headless", "run", "the", "tests"]),
            profile("headless", None, &[], &["run", "the", "tests"])
        );
        assert_eq!(
            run(&[
                "--profile",
                "tui",
                "--patch",
                "a.yml",
                "--resume",
                "b",
                "--patch",
                "late.yml",
            ]),
            profile(
                "tui",
                None,
                &["a.yml"],
                &["--resume", "b", "--patch", "late.yml"]
            )
        );
        assert_eq!(
            run(&[
                "--profile",
                "rescue",
                "--resume",
                "abc",
                "--from-default-profile",
                "web",
            ]),
            profile(
                "rescue",
                None,
                &[],
                &["--resume", "abc", "--from-default-profile", "web"]
            )
        );
        assert_eq!(
            run(&["web", "--from-default-profile", "web"]),
            profile("web", Some("web"), &[], &[])
        );
    }

    #[test]
    fn expands_shorthand_without_looking_up_profiles() {
        let profiles = [
            "web",
            "headless",
            "sdk",
            "sdk-minimal",
            "acp",
            "tui",
            "custom",
            "run",
            "help",
        ];
        let tails: [&[&str]; 13] = [
            &[],
            &["task", "words"],
            &["--help"],
            &["-h"],
            &["web"],
            &["--patch", "a.yml", "--patch", "b.yml"],
            &["--from-default-profile", "web", "--help"],
            &["--dump-config"],
            &["--dump-default-config"],
            &["--dump-config-schema"],
            &["--patch", "a.yml", "--resume", "id", "--patch", "late.yml"],
            &["--", "--help"],
            &["--", "--", "task"],
        ];
        for name in profiles {
            for tail in tails {
                let mut shorthand = vec![name];
                shorthand.extend_from_slice(tail);
                let mut explicit = vec!["--profile", name];
                explicit.extend_from_slice(tail);
                assert_eq!(parse_args(&shorthand), parse_args(&explicit));
            }
        }
        for tail in [&["plugin"][..], &["--", "plugin"][..]] {
            let mut shorthand = vec!["custom"];
            shorthand.extend_from_slice(tail);
            let mut explicit = vec!["--profile", "custom"];
            explicit.extend_from_slice(tail);
            assert_eq!(parse_args(&shorthand), parse_args(&explicit));
        }
    }

    #[test]
    fn reserves_leading_plugin_and_forwards_later_command_names() {
        assert_eq!(
            run(&["--profile", "plugin"]),
            profile("plugin", None, &[], &[])
        );
        assert_eq!(
            run(&["--profile", "x", "plugin", "add", "y"]),
            profile("x", None, &[], &["plugin", "add", "y"])
        );
        match run(&["headless", "web"]) {
            Invocation::Profile(invocation) => {
                assert_eq!(invocation.profile, "headless");
                assert_eq!(invocation.args, argv(&["web"]));
            }
            other => panic!("expected a profile, got {other:?}"),
        }
        assert_eq!(exit_code(&["--patch", "a.yml", "tui"]), 1);
        assert_eq!(exit_code(&["--", "tui"]), 1);
    }

    #[test]
    fn rejects_invalid_shorthand() {
        for values in [
            &[""][..],
            &["desktop"],
            &["Desktop"],
            &["DESKTOP"],
            &["custom", "--patch="],
            &["custom", "--from-default-profile="],
            &["custom", "--dump-config", "--dump-default-config"],
            &["custom", "--dump-default-config", "--patch", "a.yml"],
            &["custom", "--dump-config", "task"],
        ] {
            assert_eq!(exit_code(values), 1, "{values:?}");
        }
    }

    #[test]
    fn prints_the_launcher_version_for_shorthand() {
        for flag in ["-V", "--version"] {
            let printed = exit(&["custom", flag]);
            assert_eq!(printed.code, 0);
            assert_eq!(printed.stdout, "1.2.3\n");
            match run(&["custom", "task", flag]) {
                Invocation::Profile(invocation) => {
                    assert_eq!(invocation.args, argv(&["task", flag]));
                }
                other => panic!("expected a profile, got {other:?}"),
            }
        }
    }

    #[test]
    fn rejects_repeated_profile_selection() {
        for values in [
            &["web", "--profile", "tui"][..],
            &["--profile", "web", "--profile", "tui"],
            &["--profile=web", "--profile=web"],
            &["plugin", "--profile", "web", "--profile", "tui", "add", "x"],
        ] {
            let printed = exit(values);
            assert_eq!(printed.code, 1);
            assert!(printed.stderr.contains("select a profile only once"));
        }
    }

    #[test]
    fn forwards_late_profile_options_to_the_application() {
        assert_eq!(
            run(&["web", "task", "--profile", "tui"]),
            profile("web", None, &[], &["task", "--profile", "tui"])
        );
    }

    #[test]
    fn routes_the_plugin_forwarder() {
        let plugin = |name: &str, args: &[&str]| {
            Invocation::Plugin(super::PluginInvocation {
                profile: name.to_string(),
                args: argv(args),
            })
        };
        assert_eq!(
            run(&["plugin", "--profile", "tui", "add", "turtle-ui"]),
            plugin("tui", &["add", "turtle-ui"])
        );
        assert_eq!(
            run(&["plugin", "--profile", "tui", "remove", "turtle-ui"]),
            plugin("tui", &["remove", "turtle-ui"])
        );
        assert_eq!(
            run(&["plugin", "--profile", "tui", "why", "@deepseek-ai/cordis"]),
            plugin("tui", &["why", "@deepseek-ai/cordis"])
        );
        assert_eq!(
            run(&["plugin", "--profile", "tui", "add", "--save-dev", "x"]),
            plugin("tui", &["add", "--save-dev", "x"])
        );
        assert_eq!(
            run(&["plugin", "--profile", "tui", "add", "--help"]),
            plugin("tui", &["add", "--help"])
        );
        assert_eq!(
            run(&["plugin", "--profile", "tui", "add", "-h", "--profile", "x"]),
            plugin("tui", &["add", "-h", "--profile", "x"])
        );
        assert_eq!(
            run(&[
                "plugin",
                "--profile",
                "tui",
                "--filter",
                "x",
                "--profile",
                "y",
                "add",
                "z",
            ]),
            plugin("tui", &["--filter", "x", "--profile", "y", "add", "z"])
        );
        assert_eq!(
            run(&["plugin", "--profile", "tui", "--", "add", "x"]),
            plugin("tui", &["add", "x"])
        );
        assert_eq!(
            run(&["plugin", "--profile", "tui", "--", "--help"]),
            plugin("tui", &["--help"])
        );
        assert_eq!(
            run(&["plugin", "--profile", "tui", "add", "x"]),
            plugin("tui", &["add", "x"])
        );
        assert_eq!(exit_code(&["plugin", "add", "x", "--profile", "tui"]), 1);
        assert_eq!(
            exit_code(&["plugin", "--filter", "x", "add", "--profile", "tui"]),
            1
        );
    }

    #[test]
    fn prints_the_plugin_forwarder_help() {
        for values in [
            &["plugin", "--help"][..],
            &["plugin", "-h"],
            &["plugin", "--profile", "tui", "--help"],
            &[
                "plugin",
                "--profile",
                "tui",
                "--filter",
                "x",
                "add",
                "--help",
            ],
        ] {
            let printed = exit(values);
            assert_eq!(printed.code, 0);
            assert!(
                printed
                    .stdout
                    .contains("Usage: dsh plugin [options] [args...]")
            );
        }
    }

    #[test]
    fn permits_installed_desktop_plugin_commands() {
        for name in ["desktop", "Desktop", "DESKTOP"] {
            let parsed = parse(
                &argv(&["plugin", "--profile", name, "add", "example-plugin"]),
                "1.2.3",
                true,
            );
            let expected = Invocation::Plugin(super::PluginInvocation {
                profile: "desktop".to_string(),
                args: argv(&["add", "example-plugin"]),
            });
            assert_eq!(parsed, Outcome::Run(expected));
            assert_eq!(
                exit_code(&["plugin", "--profile", name, "add", "example-plugin"]),
                1
            );
        }
    }

    #[test]
    fn keeps_desktop_boot_and_inspection_reserved() {
        for values in [
            &["desktop"][..],
            &["--profile", "Desktop"],
            &["desktop", "--dump-config"],
            &["DESKTOP", "--dump-default-config"],
            &["desktop", "--dump-config-schema"],
        ] {
            let parsed = parse(&argv(values), "1.2.3", true);
            assert!(
                matches!(parsed, Outcome::Exit(Exit { code: 1, .. })),
                "{values:?}"
            );
        }
    }

    #[test]
    fn routes_profile_and_web_config_dumps() {
        assert_eq!(
            run(&["--profile", "web", "--dump-config"]),
            Invocation::DumpConfig(super::DumpConfigInvocation {
                profile: "web".to_string(),
                from_default_profile: None,
                default_only: false,
                patches: vec![],
            })
        );
        assert_eq!(
            run(&["--profile", "web", "--dump-default-config"]),
            Invocation::DumpConfig(super::DumpConfigInvocation {
                profile: "web".to_string(),
                from_default_profile: None,
                default_only: true,
                patches: vec![],
            })
        );
        assert_eq!(
            run(&[
                "--profile",
                "rescue",
                "--from-default-profile",
                "web",
                "--dump-config",
            ]),
            Invocation::DumpConfig(super::DumpConfigInvocation {
                profile: "rescue".to_string(),
                from_default_profile: Some("web".to_string()),
                default_only: false,
                patches: vec![],
            })
        );
        assert_eq!(
            run(&["--profile", "tui", "--dump-config", "--patch", "x.yml"]),
            Invocation::DumpConfig(super::DumpConfigInvocation {
                profile: "tui".to_string(),
                from_default_profile: None,
                default_only: false,
                patches: argv(&["x.yml"]),
            })
        );
        assert_eq!(
            run(&["web", "--dump-config"]),
            Invocation::DumpConfig(super::DumpConfigInvocation {
                profile: "web".to_string(),
                from_default_profile: None,
                default_only: false,
                patches: vec![],
            })
        );
        assert_eq!(
            run(&["web", "--dump-default-config"]),
            Invocation::DumpConfig(super::DumpConfigInvocation {
                profile: "web".to_string(),
                from_default_profile: None,
                default_only: true,
                patches: vec![],
            })
        );
    }

    #[test]
    fn routes_schema_dumps_with_ordered_overlays() {
        assert_eq!(
            run(&["--profile", "web", "--dump-config-schema"]),
            Invocation::DumpConfigSchema(super::DumpConfigSchemaInvocation {
                profile: "web".to_string(),
                from_default_profile: None,
                patches: vec![],
            })
        );
        assert_eq!(
            run(&[
                "rescue",
                "--from-default-profile",
                "web",
                "--patch",
                "first.yml",
                "--dump-config-schema",
                "--patch",
                "second.json",
            ]),
            Invocation::DumpConfigSchema(super::DumpConfigSchemaInvocation {
                profile: "rescue".to_string(),
                from_default_profile: Some("web".to_string()),
                patches: argv(&["first.yml", "second.json"]),
            })
        );
        assert_eq!(
            run(&["web", "--port", "8080", "--dump-config-schema"]),
            profile(
                "web",
                None,
                &[],
                &["--port", "8080", "--dump-config-schema"]
            )
        );
    }

    #[test]
    fn rejects_competing_dump_flags() {
        for flags in [
            &["--dump-config", "--dump-default-config"][..],
            &["--dump-config", "--dump-config-schema"],
            &["--dump-default-config", "--dump-config-schema"],
            &[
                "--dump-config",
                "--dump-default-config",
                "--dump-config-schema",
            ],
        ] {
            let mut forward = vec!["web"];
            forward.extend_from_slice(flags);
            assert_eq!(exit_code(&forward), 1);
            let mut reversed: Vec<&str> = flags.to_vec();
            reversed.reverse();
            let mut explicit = vec!["--profile", "web"];
            explicit.extend_from_slice(&reversed);
            assert_eq!(exit_code(&explicit), 1);
        }
    }

    #[test]
    fn rejects_invalid_schema_dump_inputs() {
        for values in [
            &["--dump-config-schema"][..],
            &["desktop", "--dump-config-schema"],
            &["--profile", "Desktop", "--dump-config-schema"],
            &["web", "--dump-config-schema", "task"],
            &["web", "--dump-config-schema", "--port", "8080"],
            &["web", "--dump-config-schema", "--help"],
            &["web", "--dump-config-schema", "--", "--patch", "late.yml"],
            &["web", "--dump-config-schema", "--patch="],
            &["rescue", "--dump-config-schema", "--from-default-profile="],
        ] {
            assert_eq!(exit_code(values), 1, "{values:?}");
        }
    }

    #[test]
    fn rejects_missing_profile_removed_flags_and_contradictions() {
        let cases: [&[&str]; 25] = [
            &[],
            &["--config", "c.yml"],
            &["-p", "task"],
            &["--profile", ""],
            &["--profile", "x", "--from-default-profile="],
            &["--profile", "x", "--from-default-profile"],
            &["--profile", "x", "--patch="],
            &["--dump-config"],
            &["--profile", "x", "--dump-config", "--dump-default-config"],
            &[
                "--profile",
                "x",
                "--dump-default-config",
                "--patch",
                "p.yml",
            ],
            &["--profile", "x", "--dump-config", "task"],
            &["--bogus"],
            &["web", "--dump-config", "--dump-default-config"],
            &["web", "--dump-default-config", "--patch", "w.yml"],
            &["web", "--patch="],
            &["web", "--dump-config", "--port", "8080"],
            &["--profile", "web", "--dump-config", "-h"],
            &["plugin", "add", "x"],
            &["plugin", "--profile", "tui"],
            &["plugin", "--profile", ""],
            &["--profile", "desktop"],
            &["--profile", "Desktop"],
            &["--profile", "DESKTOP"],
            &["--profile", "desktop", "--dump-config"],
            &["plugin", "--profile", "desktop", "add", "x"],
        ];
        for values in cases {
            assert_eq!(exit_code(values), 1, "{values:?}");
        }
        assert_eq!(
            exit_code(&["plugin", "--profile", "Desktop", "add", "x"]),
            1
        );
        assert_eq!(
            exit_code(&[
                "--from-default-profile",
                "web",
                "plugin",
                "--profile",
                "x",
                "add",
                "y",
            ]),
            1
        );
    }

    #[test]
    fn keeps_its_own_help_with_no_app_to_hand_it_to() {
        for flag in ["--help", "-h", "--version"] {
            let printed = exit(&[flag]);
            assert_eq!(printed.code, 0);
        }
        let help = exit(&["--help"]);
        assert!(!help.stdout.contains("help [command]"));
        assert!(help.stdout.starts_with("Usage: dsh [--profile] <name>"));
    }

    #[test]
    fn reports_missing_option_arguments() {
        assert!(exit(&["--profile"]).stderr.contains("argument missing"));
        assert!(
            exit(&["--profile", "x", "--patch"])
                .stderr
                .contains("argument missing")
        );
        assert!(
            exit(&["--profile", "x", "--from-default-profile"])
                .stderr
                .contains("argument missing")
        );
        assert!(
            exit(&["plugin", "--profile"])
                .stderr
                .contains("argument missing")
        );
    }

    #[test]
    fn treats_a_bare_separator_as_the_end_of_launcher_flags() {
        assert_eq!(
            run(&["--profile", "x", "--", "--patch", "late.yml"]),
            profile("x", None, &[], &["--patch", "late.yml"])
        );
        assert_eq!(run(&["--profile", "x", "--"]), profile("x", None, &[], &[]));
    }

    #[test]
    fn quotes_dump_arguments_like_json() {
        let printed = exit(&["web", "--dump-config", "a b", "q\"x"]);
        assert_eq!(printed.code, 1);
        assert!(printed.stderr.contains(r#""a b" "q\"x""#));
    }

    proptest::proptest! {
        #[test]
        fn shorthand_and_explicit_forms_agree(
            name in "[A-Za-z0-9_][A-Za-z0-9_-]{0,11}",
            extra in proptest::collection::vec("[A-Za-z0-9_.:=/ -]{0,8}", 0..5),
        ) {
            let mut shorthand = vec![name.clone()];
            shorthand.extend(extra.iter().cloned());
            let mut explicit = vec!["--profile".to_string(), name];
            explicit.extend(extra);
            proptest::prop_assert_eq!(
                parse(&shorthand, "1.0.0", false),
                parse(&explicit, "1.0.0", false)
            );
        }

        #[test]
        fn every_resolved_profile_is_non_empty_and_desktop_free(
            name in "[A-Za-z0-9_-]{1,12}",
            extra in proptest::collection::vec("[A-Za-z0-9_.:=/ -]{0,8}", 0..5),
        ) {
            let mut values = vec!["--profile".to_string(), name];
            values.extend(extra);
            if let Outcome::Run(Invocation::Profile(invocation)) = parse(&values, "1", false) {
                proptest::prop_assert!(!invocation.profile.is_empty());
                proptest::prop_assert_ne!(invocation.profile.to_lowercase(), "desktop");
            }
        }
    }
}
