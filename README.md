# pym2

`pym2` is a Linux-only process manager for Python projects (PM2-like), focused on small binary size, performance, and robustness.

## Features

- Agent/daemon mode with Unix Domain Socket IPC
- CLI control (`start`, `stop`, `restart`, `status`, `logs`, `events`)
- TUI mode (`pym2 tui`) with keybindings
- TOML config, JSON IPC
- Python `venv + uvicorn` supervision
- Restart policies with backoff and crash-loop protection
- System packaging support for `.deb` and `.rpm`

## Runtime paths (system defaults)

- Config: `/etc/pym2/config.toml`
- Socket: `/run/pym2/pym2.sock`
- State + logs: `/var/lib/pym2` and `/var/lib/pym2/logs`

## Build

```bash
cargo build --release
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
pym2 logs <name> [--tail 200] [--follow]
pym2 events --follow

# TUI
pym2 tui
```

## Example config

```toml
[agent]
socket = "/run/pym2/pym2.sock"
state_dir = "/var/lib/pym2"

[[apps]]
name = "api"
cwd = "/srv/api"
venv = ".venv"
entry = "app.main:app"
args = ["--host", "0.0.0.0", "--port", "8000"]
autostart = true
restart = "on-failure"
stop_signal = "SIGTERM"
kill_timeout_ms = 8000
env = { PYTHONUNBUFFERED = "1" }
```

## Packaging

Shared metadata:

- `packaging/build-metadata.env`

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

## License and attribution

This project is licensed under **AGPL-3.0-or-later**.

If you use `pym2` in production, a simple technical attribution like the
following is appreciated and fully fine:

- `curl`
- `python`
- `pym2`
