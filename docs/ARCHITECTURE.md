# Project Architecture

`pym2` is a Linux-first process supervisor for long-running app processes (especially Python services), with a small single-binary design.

The runtime is split into:
- an **agent daemon** (`pym2 agent`) that owns process lifecycle
- a **CLI client** (`pym2 <command>`) that only talks to the agent
- an optional **web interface** (feature `webui`, config controlled)
- **IPC over Unix domain socket** using JSON request/response messages

The core design goal is: one process manager authority (agent), multiple clients (CLI now, TUI/web later) using the same API.

# High Level Architecture

Main components:
- **Agent daemon**: listens on Unix socket, dispatches requests, runs supervisor tick loop
- **CLI**: parses user commands, sends IPC requests, formats output
- **Supervisor**: starts/stops/restarts child processes, applies restart/backoff/crash protection
- **IPC layer**: line-based JSON over Unix socket (`IpcRequest` / `IpcResponse`)
- **Config system**: loads and validates TOML config
- **Runtime persistence**: writes/reads `runtime_state.json` for restart metadata

The supervisor runs a periodic tick loop (currently once per second) that:

- checks for exited child processes
- evaluates restart policy
- applies exponential backoff timers
- executes scheduled restarts

ASCII flow:

```text
CLI
  |
  | IPC (Unix Socket)
  v
Agent
  |
  v
Supervisor
  |
  v
Managed Processes
```

# Source Code Structure

## `src/model/`
Defines shared data models:
- config structs (`ConfigFile`, `AppSpec`, `AgentConfig`, `WebConfig`)
- runtime structs (`AppRuntimeState`, `AppStatus`)
- IPC protocol types (`IpcRequest`, `IpcResponse`, log/event payloads)
- `effective_command()` helper for command resolution (modern + legacy)

## `src/config/`
Configuration loading/saving/validation:
- default config path: `/etc/pym2/config.toml` (override via `PYM2_CONFIG`)
- path expansion (`~`)
- strict validation (required fields, schedule format, web safety)
- atomic config writes (`*.tmp` then rename)

## `src/supervisor/`
Process lifecycle engine:
- start/stop/restart operations
- tick loop checks child states and scheduled restarts
- restart policy + exponential backoff + crash-loop guard
- env file loading and env merge
- stdout/stderr file routing
- runtime state persistence (`runtime_state.json` with schema version)

## `src/agent/`
Daemon runtime:
- socket bind, permissions, signal handling, client connection limits
- IPC request routing to supervisor
- log/event streaming endpoints
- optional web server startup (`src/agent/web.rs`, behind feature `webui`)

## `src/cli/`
User-facing command layer:
- commands: `start/stop/restart/status/inspect/logs/events/ping/doctor/config lint`
- app creation helpers: `add-fastapi`, `add-cmd`
- text + JSON output paths
- no direct process management (always via IPC)

# Process Lifecycle

Typical flow (start):
1. CLI sends `IpcRequest::Start { name }`.
2. Agent receives request on Unix socket and dispatches to supervisor.
3. Supervisor resolves executable/args, prepares env/log files, spawns child in new session (`setsid`).
4. Runtime state is updated (`Running`, `pid`, timestamps, reason).
5. Logs are appended to per-app stdout/stderr files.
6. Tick loop monitors exits and applies restart policy/backoff.

Stop flow:
- send configured stop signal (default `SIGTERM`) to process group
- wait `kill_timeout_ms`
- force `SIGKILL` for process group if still alive

Restart policy (`RestartPolicy`):
- `never`
- `on-failure`
- `always`

# Command Execution Model

Primary execution path is `command[]`:

```toml
[[apps]]
name = "api"
cwd = "/srv/api"
command = ["python", "-m", "uvicorn", "app:app"]
```

Resolution logic (`effective_command()` / supervisor `build_command()`):
- if `command` is non-empty: `command[0]` is executable, rest are args
- else legacy fallback:
  - resolve `venv` path (absolute or relative to `cwd`)
  - prefer `<venv>/bin/uvicorn <entry> ...args`
  - fallback `<venv>/bin/python -m uvicorn <entry> ...args`

This preserves old uvicorn behavior while making generic process execution default.

# Configuration System

Config file: `/etc/pym2/config.toml` (or `PYM2_CONFIG`).

Important app fields:
- `name` (required)
- `cwd` (required)
- `command` (preferred)
- legacy fallback fields: `venv`, `entry`, `args`
- `env` map
- `env_file` (optional)
- `restart`: `never|on-failure|always`
- `restart_schedule`: `daily@HH:MM` or `weekly@sun HH:MM`
- `stop_signal`, `kill_timeout_ms`, `autostart`

Validation highlights (actual behavior):
- app name must not be empty
- `cwd` is required
- either `command` must exist, or legacy `venv+entry` must exist
- `command[0]` must not be empty
- `env_file` if set must be non-empty
- restart schedule must parse
- web UI safety: if enabled on non-loopback host, auth token is required (`password`)

# Runtime State

File: `<state_dir>/runtime_state.json` (default `/var/lib/pym2/runtime_state.json`).

Stored per app:
- status (`running|stopped|errored`)
- `pid`, `started_at`, `last_start_attempt_at`
- `restart_count`
- `last_exit_code`, `last_exit_signal`
- `last_error`, `last_reason`
- `backoff_until`, `next_scheduled_restart_at`

Persistence details:
- includes `schema_version` (currently `1`)
- writes are atomic via temp file + rename
- on restore, unknown schema version is rejected
- stale running PIDs are sanitized to stopped state

# IPC Protocol

Transport:
- Unix domain socket (default `/run/pym2/pym2.sock`)
- line-delimited JSON messages

Core request types:
- `Ping`
- `Start { name }`
- `Stop { name }`
- `Restart { name }`
- `ListApps`
- `GetApp { name }`
- `TailLogs { name, tail, source }`
- `StreamLogs { ... }`
- `WatchEvents`

Flow:
1. client connects to socket
2. sends one JSON request line
3. agent returns `IpcResponse { ok, data?, error? }`
4. streaming calls keep connection open and emit repeated `IpcResponse` lines

`Ping` response data includes:
- `version` (`env!("CARGO_PKG_VERSION")`)
- `pid` (agent PID)

# Logging

For each app, stdout/stderr are redirected to:
- `<logs_dir>/<name>.out.log`
- `<logs_dir>/<name>.err.log`

Defaults:
- logs dir is `<state_dir>/logs` (default `/var/lib/pym2/logs`)

CLI log access:
- `pym2 logs <name> --tail N`
- `pym2 logs <name> --follow`

IPC log APIs provide both tail and streaming modes.

# Crash Protection

Implemented in supervisor restart logic:
- restart window: `60s`
- max restarts in window: `5`
- grace period for stability reset: `10s` runtime
- exponential backoff: `1s -> 2s -> 4s ... max 30s`

Behavior:
- if process ran >= 10s before exit, consecutive restart counters are reset
- if restarts exceed limit in window, app enters `Errored` with reason `max_restarts_exceeded`

# Testing

Implemented test layers:
- `cargo test` unit tests (config validation, command resolution, restart policy behavior)
- `scripts/smoke.sh` integration smoke test

Smoke scenarios include:
- basic command app lifecycle
- crash-loop protection trigger
- grace reset behavior
- `env_file` propagation into process environment

Note: `smoke.sh` is Linux-only and skips on non-Linux hosts.

# Release Process

Current release flow (documented in `RELEASE.md`):
1. bump `Cargo.toml` version
2. update `CHANGELOG.md`
3. run validation (`cargo test`, `cargo test --all-features`, `bash scripts/smoke.sh`)
4. tag release (`git tag vX.Y.Z`, `git push origin vX.Y.Z`)
5. build release binary (`cargo build --release`)
6. build/package artifacts (`.deb` / `.rpm` if packaging scripts are used)
