# Development Rules

This file defines mandatory development rules for contributors and AI agents.

These rules must be followed when modifying the codebase.

# Project Principles

- Keep the system small and practical: no framework-heavy redesigns.
- Linux-first behavior is intentional (`pym2` runtime commands are Linux-only; `pym2 doctor` is the explicit cross-platform diagnostic exception).
- Keep dependencies minimal; prefer standard library when possible.
- Preserve the agent + client split:
  - agent owns runtime/process state
  - CLI is a client over IPC
- Keep optional surfaces optional:
  - `webui` and `tui` are feature-gated in build configuration.

# Architecture Rules

- CLI must never directly start/stop OS processes.
- Process lifecycle logic must stay in `src/supervisor/`.
- IPC request handling must go through `src/agent/` dispatch.
- Any new runtime state fields must be persisted/restored via supervisor runtime state.
- Runtime state writes must remain atomic (`tmp` + rename pattern).
- Web UI must not bypass supervisor logic; it should call supervisor operations equivalent to IPC actions.

# Code Structure Rules

- Module responsibilities are fixed:
  - `src/model/` -> shared data models, runtime state structs, IPC types
  - `src/config/` -> config loading, defaults, validation, config writes
  - `src/supervisor/` -> process lifecycle, restart/backoff, schedules, logs, runtime persistence
  - `src/agent/` -> daemon loop, socket server, IPC dispatch, event/log streaming
  - `src/cli/` -> argument parsing, IPC client calls, output formatting
  - `src/ipc/` -> wire helpers and client abstraction
- Do not move business logic into `main.rs`; keep it as startup/error-exit glue.
- Avoid circular module dependencies; use `model` for shared types.
- Keep parsing/validation code in `config` or dedicated helpers, not scattered across command handlers.
- Exception: `pym2 config lint` may parse raw TOML in CLI for reporting-focused diagnostics without enforcing full runtime validation flow.

# Process Management Rules

- All managed processes must be created via supervisor `start_one` path.
- Processes must run in a new process group (`setsid`) as currently implemented.
- Stop behavior must follow current semantics:
  - send configured stop signal to process group
  - wait `kill_timeout_ms`
  - escalate to `SIGKILL` for process group
- Restart behavior must continue to use existing policy + backoff functions.
- Crash-loop protection (`MAX_RESTARTS_IN_WINDOW`) must not be bypassed.
- `last_reason` / `last_error` fields must be updated on lifecycle transitions.

# Supervisor Isolation Rules

- The supervisor must remain the single authority over process lifecycle.
- No other component (CLI, Web UI, or future remote APIs) may directly manage processes or runtime state.
- All lifecycle operations must go through supervisor APIs.

# Configuration Rules

- Always validate config before use (`validate_config`).
- Preferred execution model is `command[]`.
- Legacy `venv` + `entry` + `args` remains fallback only.
- `cwd` is required for apps.
- `restart_schedule` must use existing parser formats (`daily@HH:MM`, `weekly@day HH:MM`).
- `env_file` loading format stays simple `KEY=VALUE` (no complex shell parsing).
- Web safety rule must stay enforced:
  - non-loopback web bind requires auth token/password.

# IPC Rules

- CLI interactions with runtime state must use IPC (`IpcClient`).
- Do not mutate supervisor state from CLI directly.
- IPC messages are JSON, line-delimited over Unix socket.
- Keep request/response schema compatible when extending protocol:
  - add fields/variants carefully
  - preserve existing commands used by CLI/web.
- Healthcheck behavior (`Ping`) must continue to return version and agent pid.

# Logging Rules

- stdout and stderr must be captured separately.
- Log files must remain in `<state_dir>/logs` with naming:
  - `<app>.out.log`
  - `<app>.err.log`
- `logs` and streaming endpoints must read from these files, not from in-memory buffers.

# Testing Rules

- Keep policy logic testable without requiring real child processes when possible.
- Unit tests should cover:
  - command resolution
  - restart limiter / grace reset behavior
  - config validation constraints
- `scripts/smoke.sh` is the integration smoke test and must stay Linux-only.
- Smoke scenarios must continue to verify:
  - basic command lifecycle
  - crash-loop protection
  - grace reset behavior
  - `env_file` handling.

# Dependency Rules

- Avoid heavy dependencies and async frameworks unless clearly required.
- Prefer existing crates already used in the project (`clap`, `serde`, `toml`, `thiserror`, `libc`, `shell-words`).
- New dependency additions require a concrete need and small surface impact.
- Prefer simple helper functions over introducing abstraction-heavy libraries.

# Documentation Rules

- Architecture-impacting changes must update `docs/ARCHITECTURE.md`.
- Release process changes must update `RELEASE.md` (and `docs/releasing.md` if applicable).
- User-visible CLI/config behavior changes must update `README.md` examples.
- New config keys or IPC requests must be documented in developer docs.
- Keep docs aligned with actual code behavior; do not document planned features as implemented.
