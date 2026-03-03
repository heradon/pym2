# pym2

`pym2` is a Linux-only process manager for Python projects (PM2-like), focused on small binary size, performance, and robustness.

## Features

- Agent/daemon mode with Unix Domain Socket IPC
- CLI control (`start`, `stop`, `restart`, `status`, `logs`, `events`)
- TUI mode (`pym2 tui`) with keybindings
- Optional built-in Web UI (`agent.web` config)
- TOML config, JSON IPC
- Generic process supervision via `command[]` (with legacy `venv + uvicorn` fallback)
- Restart policies with backoff and crash-loop protection
- System packaging support for `.deb` and `.rpm`

## Runtime paths (system defaults)

- Config: `/etc/pym2/config.toml`
- Socket: `/run/pym2/pym2.sock`
- State + logs: `/var/lib/pym2` and `/var/lib/pym2/logs`

## Optional Web UI

The web UI is built into the agent and disabled by default.

```toml
[agent.web]
enabled = false
host = "127.0.0.1"
port = 17877
# password = "change-me"
```

- `enabled`: turns web UI on/off
- `host`: bind address (`127.0.0.1` or `0.0.0.0`)
- `port`: HTTP port
- `password`: optional on loopback; required when `enabled=true` and host is non-loopback

## Build

```bash
cargo build --release

# Minimal binary (without TUI and Web UI)
cargo build --release --no-default-features
```

## Run

```bash
# Agent
pym2 agent

# CLI
pym2 start <name|all>
pym2 stop <name|all>
pym2 restart <name|all>
pym2 status [--json]
pym2 inspect <name> [--json]
pym2 logs <name> [--tail 200] [--follow]
pym2 events --follow

# TUI
pym2 tui

# Add apps (writes /etc/pym2/config.toml)
pym2 add-fastapi --name api --cwd /srv/api --entry app.main:app --host 0.0.0.0 --port 8000 --restart-schedule "daily@03:00"
pym2 add-cmd --name worker --cwd /srv/worker --command "python worker.py --queue default" --restart-schedule "weekly@sun 03:00"
```

Note: `add-fastapi` and `add-cmd` write to `/etc/pym2/config.toml`, so run them with enough permissions.

## Example config

```toml
[agent]
socket = "/run/pym2/pym2.sock"
state_dir = "/var/lib/pym2"

[[apps]]
name = "api"
cwd = "/srv/api"
command = ["python", "-m", "uvicorn", "app.main:app", "--host", "0.0.0.0", "--port", "8000"]
env_file = "/srv/api/.env"
autostart = true
restart = "on-failure"
stop_signal = "SIGTERM"
kill_timeout_ms = 8000
restart_schedule = "daily@03:00"
env = { PYTHONUNBUFFERED = "1" }
```

Legacy mode is still supported if `command` is empty:
- set `venv`, `entry`, and optional `args`

## Migration (legacy -> command)

Old (legacy):

```toml
[[apps]]
name = "api"
cwd = "/srv/api"
venv = ".venv"
entry = "app.main:app"
args = ["--host", "0.0.0.0", "--port", "8000"]
```

New (recommended):

```toml
[[apps]]
name = "api"
cwd = "/srv/api"
command = ["python", "-m", "uvicorn", "app.main:app", "--host", "0.0.0.0", "--port", "8000"]
```

`restart_schedule` supports:
- `daily@HH:MM` (example: `daily@03:00`)
- `weekly@sun HH:MM` (example: `weekly@sun 03:00`)

## Packaging

Shared metadata:

- `packaging/build-metadata.env`
- `systemd/pym2.service` (installed by deb/rpm packages)

Debian package:

```bash
./scripts/build-deb.sh --arch amd64
./scripts/build-deb.sh --arch arm64
```

RPM package:

```bash
./scripts/build-rpm.sh --arch x86_64
./scripts/build-rpm.sh --arch aarch64
```

Useful flags for both scripts:

- `--no-enable-service`
- `--no-systemd`
- `--no-default-features`
- `--features tui,webui` (or any subset)

## License and attribution

This project is licensed under **AGPL-3.0-or-later**.

If you use `pym2` in production, a simple technical attribution like the
following is appreciated and fully fine:

- `curl`
- `python`
- `pym2`
