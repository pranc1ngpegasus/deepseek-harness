//! The `dsh` binary.

use std::io::Write;

use dsh_args::{Invocation, Outcome};
use dsh_diagnostics::{DiagnosticContext, StartupFailure};

/// Run the public command line and return its exit status.
fn main() -> std::process::ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let version = env!("CARGO_PKG_VERSION");
    match dsh_args::parse(&argv, version, false) {
        Outcome::Exit(exit) => {
            let _ = std::io::stdout().write_all(exit.stdout.as_bytes());
            let _ = std::io::stderr().write_all(exit.stderr.as_bytes());
            std::process::ExitCode::from(u8::try_from(exit.code).unwrap_or(1))
        }
        Outcome::Run(invocation) => {
            std::process::ExitCode::from(u8::try_from(run(&invocation)).unwrap_or(1))
        }
    }
}

fn run(invocation: &Invocation) -> i32 {
    match invocation {
        Invocation::Profile(profile) if profile.profile == "tui" => run_tui(profile),
        Invocation::Profile(profile) => run_profile(profile),
        Invocation::DumpConfig(dump) => run_dump_config(dump),
        Invocation::DumpConfigSchema(dump) => run_dump_config_schema(dump),
        Invocation::Plugin(plugin) => {
            let home = dsh_cli::home::resolve_dsh_home();
            dsh_cli::plugin::run_plugin(&plugin.profile, &plugin.args, false, &home)
        }
    }
}

fn run_tui(invocation: &dsh_args::ProfileInvocation) -> i32 {
    let home = home();
    if let Some(from_default) = invocation.from_default_profile.as_deref() {
        match dsh_cli::profile_store::initialize_profile_from_default("tui", from_default, &home) {
            Ok(warnings) => {
                for warning in warnings {
                    eprintln!("{warning}");
                }
            }
            Err(error) => {
                eprintln!("{error}");
                return 1;
            }
        }
    }
    let directory = match dsh_cli::home::profile_dir("tui", &home) {
        Ok(directory) => directory,
        Err(error) => {
            eprintln!("{error}");
            return 1;
        }
    };
    if let Err(error) = dsh_cli::profile_store::init_profile(&directory, &[]) {
        eprintln!("{error}");
        return 1;
    }
    let workspace = match std::env::current_dir() {
        Ok(workspace) => workspace,
        Err(error) => {
            eprintln!("dsh: cannot read the current directory: {error}");
            return 1;
        }
    };
    match dsh_tui::parse_args(&invocation.args, home, workspace) {
        Ok(dsh_tui::Parsed::Help) => {
            print!("{}", dsh_tui::HELP);
            0
        }
        Ok(dsh_tui::Parsed::Run(options)) => dsh_tui::run(&options),
        Err(error) => {
            eprintln!("dsh: {error}");
            1
        }
    }
}

fn home() -> std::path::PathBuf {
    dsh_cli::home::resolve_dsh_home()
}

fn run_profile(invocation: &dsh_args::ProfileInvocation) -> i32 {
    let home = home();
    let install_anchor = match dsh_cli::home::install_anchor() {
        Ok(anchor) => anchor,
        Err(error) => {
            eprintln!("{error}");
            return 1;
        }
    };
    let (profile, warnings) = match dsh_cli::profile_store::prepare_profile(
        &invocation.profile,
        true,
        invocation.from_default_profile.as_deref(),
        &install_anchor,
        &home,
    ) {
        Ok(prepared) => prepared,
        Err(error) => {
            eprintln!("{error}");
            return 1;
        }
    };
    for warning in warnings {
        eprintln!("{warning}");
    }
    for line in dsh_cli::profile_store::skipped_bundle_lines(&profile) {
        eprint!("{line}");
    }
    let overlays = match load_overlays(&invocation.patches) {
        Ok(overlays) => overlays,
        Err(error) => {
            eprintln!("{error}");
            return 1;
        }
    };
    let telemetry = std::env::var("DSH_TELEMETRY_DISABLED").ok();
    let options = dsh_cli::runtime::RunOptions {
        profile: &profile,
        overlays: &overlays,
        args: &invocation.args,
        home: &home,
        install_anchor: &install_anchor,
        telemetry_disabled: telemetry.as_deref(),
    };
    match dsh_cli::runtime::run_profile(&options) {
        Ok(code) => code,
        Err(error) => report_failure(&error, &home, &invocation.profile),
    }
}

fn load_overlays(files: &[String]) -> Result<Vec<dsh_config::Patch>, String> {
    let mut overlays = Vec::new();
    for file in files {
        let path = std::path::Path::new(file);
        let absolute = if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()
                .map_err(|error| format!("dsh: cannot resolve {file}: {error}"))?
                .join(path)
        };
        overlays.extend(dsh_cli::yaml::read_patch_list(&absolute, "overlay")?);
    }
    Ok(overlays)
}

fn report_failure(message: &str, home: &std::path::Path, profile: &str) -> i32 {
    let failure = StartupFailure {
        message: message.to_string(),
        configuration_path: None,
        plugins: Vec::new(),
        messages: Vec::new(),
    };
    let context = DiagnosticContext {
        home: home.display().to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        profile: profile.to_string(),
    };
    let output = dsh_cli::diagnostics_io::report_startup_failure(&failure, &context);
    eprint!("{output}");
    1
}

fn run_dump_config(invocation: &dsh_args::DumpConfigInvocation) -> i32 {
    let home = home();
    let install_anchor = match dsh_cli::home::install_anchor() {
        Ok(anchor) => anchor,
        Err(error) => {
            eprintln!("{error}");
            return 1;
        }
    };
    match dsh_cli::dump::dump_config(
        &invocation.profile,
        invocation.default_only,
        &invocation.patches,
        invocation.from_default_profile.as_deref(),
        &install_anchor,
        &home,
    ) {
        Ok(output) => {
            for diagnostic in output.diagnostics {
                eprintln!("{diagnostic}");
            }
            print!("{}", output.document);
            0
        }
        Err(error) => {
            eprintln!("{error}");
            1
        }
    }
}

fn run_dump_config_schema(invocation: &dsh_args::DumpConfigSchemaInvocation) -> i32 {
    let home = home();
    let install_anchor = match dsh_cli::home::install_anchor() {
        Ok(anchor) => anchor,
        Err(error) => {
            eprintln!("{error}");
            return 1;
        }
    };
    let prepared = dsh_cli::profile_store::prepare_profile(
        &invocation.profile,
        true,
        invocation.from_default_profile.as_deref(),
        &install_anchor,
        &home,
    );
    let (profile, warnings) = match prepared {
        Ok(prepared) => prepared,
        Err(error) => {
            eprintln!("{error}");
            return 1;
        }
    };
    for warning in warnings {
        eprintln!("{warning}");
    }
    let layers =
        match dsh_cli::dump::collect_dump_layers(&profile, false, &invocation.patches, &home) {
            Ok(layers) => layers,
            Err(error) => {
                eprintln!("{error}");
                return 1;
            }
        };
    let patch_lists: Vec<Vec<dsh_config::Patch>> =
        layers.into_iter().map(|layer| layer.patches).collect();
    match dsh_cli::schema::generate_config_schema(&profile, &patch_lists) {
        Ok(dump) => {
            match serde_json::to_string_pretty(&dump.document) {
                Ok(rendered) => {
                    println!("{rendered}");
                }
                Err(error) => {
                    eprintln!("dsh: cannot render schema: {error}");
                    return 1;
                }
            }
            for diagnostic in &dump.diagnostics {
                eprint!("{}", dsh_cli::schema::diagnostic_line(diagnostic));
            }
            i32::from(!dump.complete)
        }
        Err(error) => {
            eprintln!("{error}");
            1
        }
    }
}
