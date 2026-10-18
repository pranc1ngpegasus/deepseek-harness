# Rust CLI

The Rust workspace replaces the `apps/cli` launcher. It composes profiles,
prints config dumps, forwards plugin commands, and supervises the harness host.
The harness package tree under `packages/` remains the plugin implementation;
the launcher reaches it through one documented process seam.

## Crates

| Crate | Plane | Responsibility |
| --- | --- | --- |
| `dsh-args` | `no_std` | Launcher argv resolution: profile selection, overlays, dump flags, `plugin` forwarding, help and error text. |
| `dsh-profile` | `no_std` | Profile name rules, shipped templates, retired-bundle removal, installation-tuple normalization, telemetry switch, file templates. |
| `dsh-config` | `no_std` | The include plugin's patch algorithm over an ordered JSON-safe value model, plus dump grouping. |
| `dsh-shutdown` | `no_std` | The bounded shutdown state machine: coalesced disposal, escalation, grace timeout. |
| `dsh-diagnostics` | `no_std` | Startup failure report formatting and private report naming. |
| `dsh-cli` | `std` | Filesystem profile store, YAML/JSON reading, dump and schema commands, plugin exemptions and pnpm forwarding, host supervision, `dsh tui` dispatch. |
| `dsh-agent` | `no_std` | Messages, tool contract, streamed tool-call assembly, turn state machine, output truncation. |
| `dsh-llm` | `std` | Streaming chat-completions client over SSE. |
| `dsh-tools` | `std` | Workspace tools: bash, read, write, edit, list, search. |
| `dsh-session` | `std` | Append-only JSONL session log. |
| `dsh-tui` | `std` | Ratatui screen, key handling, worker thread, approvals, `--print` mode. |
| `xtask` | `std` | Repository gates, currently the mutation score gate. |

The pure crates carry no filesystem or process access, so their behavior is
tested directly and their mutants are meaningful.

## Behavior sources

| Rust | TypeScript |
| --- | --- |
| `crates/dsh-args` | `apps/cli/src/args.ts` |
| `crates/dsh-profile` | `packages/boot/app-boot/src/profile.ts` (names, templates, normalization) |
| `crates/dsh-config` | `vendor/include/src/index.ts` (patch algorithm) and `renderConfigDump` |
| `crates/dsh-shutdown` | `apps/cli/src/process-shutdown.ts` |
| `crates/dsh-diagnostics` | `apps/cli/src/startup-diagnostics.ts` |
| `crates/dsh-cli` | `apps/cli/src/bin.ts`, `dump-config.ts`, `plugin.ts`, `profile-boot.ts` |
| `crates/dsh-agent` | the coding-agent contract; new native runtime, no TypeScript source |
| `crates/dsh-llm` | DeepSeek provider and SSE handling; new native runtime |
| `crates/dsh-tools` | bash and filesystem tools; new native runtime |
| `crates/dsh-session` | JSONL session log; new native runtime |
| `crates/dsh-tui` | `apps/cli` profile boot plus a native terminal loop |

## `dsh tui`

`dsh tui` is a self-contained terminal coding agent. It does not mount the
TypeScript plugin tree and needs no `dsh-host`.

```sh
export DEEPSEEK_API_KEY=...
dsh tui                         # interactive
dsh tui "run the tests"         # open with a first request
dsh tui --print "run the tests" # one shot, answer on stdout, tool trace on stderr
```

Flags: `--model`, `--base-url`, `--api-key`, `--max-turns`, `--yes`.
`DEEPSEEK_MODEL` or `DSH_MODEL` selects the model, and `DEEPSEEK_BASE_URL`
selects the endpoint. The default is `deepseek-chat` at
`https://api.deepseek.com`.

The same runtime also installs as a standalone `dsh-tui` binary for direct use
and for its own end-to-end tests.

The model streams assistant text and tool calls. The TUI asks `y` or `n` before
each tool call unless `--yes` was passed. `Esc` or the first `Ctrl+C` cancels a
turn, the second `Ctrl+C` quits. Every request, reply, tool call, and result is
appended to `$DSH_HOME/sessions/<id>.jsonl`.

Tools operate inside the current directory. A path that escapes it is rejected,
and search skips `.git`, `target`, and `node_modules`. Tool output is capped at
30,000 characters with both ends kept.

Behavior that is observable by a person is kept: flag boundaries, profile
validation, layer order, skipped-patch warnings, bundle-skip diagnostics,
exact-version exemptions, private report permissions, and exit codes. The YAML
dump uses its own emitter, so whitespace and quoting can differ from `js-yaml`
while the document parses to the same entries.

## Host seam

`dsh <profile> [app-args...]` prepares the profile, writes the empty root
config, computes the ordered patch list, and starts the harness host named by
`DSH_HOST` (default `dsh-host`) with:

- argv: the profile's `cordis.yml` path, then the invocation's inner arguments;
- environment: `DSH_PROFILE_CONTEXT` (profile facts as JSON) and
  `DSH_PROFILE_PATCHES` (the ordered patch list as JSON).

The launcher owns the host's shutdown: SIGTERM starts a graceful termination
and SIGINT reports 130, a second signal kills the host, and the five second
grace period falls back to SIGKILL.

## Gates

```sh
cargo test --workspace
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo mutants -p dsh-args -p dsh-profile -p dsh-config -p dsh-shutdown -p dsh-diagnostics -j 8
cargo xtask mutation-gate
```

`cargo xtask mutation-gate` reads `mutants.out/outcomes.json`. It fails when a
pure crate reports a surviving mutant or the workspace kill rate is below 98%.
Timeouts count against the kill rate; unviable and skipped mutants do not.

## Known gaps

These launcher and TUI behaviors are not ported yet:

- Session resume. A long session keeps the newest 200 messages and its system
  prompt, and drops an old tool result that no longer has its assistant call.
- MCP servers, web search and fetch, plan mode, subagents, and image input.
- Layered `.env` loading (`process`, project `.env`, `$DSH_HOME/.env`) and the
  proxy installation derived from that snapshot. The TUI reads the process
  environment.
- Plugin requirement checks against the running DSH version when loading a
  bundle. Unresolvable bundles are still skipped with a diagnostic.
- Plugin `Config` schemas in `--dump-config-schema`. The document describes
  the entry dialect and marks `x-cordis.complete` from composition alone, so a
  complete profile still reports plugin schemas as absent.
- `renderConfigDump`'s exact `js-yaml` byte formatting.
- Web, desktop, ACP, and SDK surfaces are intentionally out of scope.
