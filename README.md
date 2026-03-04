
# pym2

[![CI](https://github.com/USER/pym2/actions/workflows/ci.yml/badge.svg)](https://github.com/USER/pym2/actions)
[![License](https://img.shields.io/badge/license-AGPL--3.0-blue.svg)](LICENSE)

**pym2** is a lightweight Linux-first process manager for Python services.

It is similar to **PM2**, but designed as a **small single binary** with minimal dependencies and a clean agent-based architecture.

Ideal for:

- FastAPI
- Django
- background workers
- Python APIs
- small self-hosted services

---

# Why pym2?

Many Python services end up managed by either:

- raw `systemd` units
- heavy orchestrators
- Node-based process managers

`pym2` focuses on a simpler model.

| Feature | pym2 | systemd | PM2 |
|-------|------|------|------|
| Single binary | ✅ | ❌ | ❌ |
| Python-focused | ✅ | ❌ | ❌ |
| CLI management | ✅ | ⚠️ | ✅ |
| Crash protection | ✅ | ⚠️ | ✅ |
| TUI interface | ✅ | ❌ | ⚠️ |
| Web UI | ✅ | ❌ | ✅ |
| Small footprint | ✅ | ⚠️ | ❌ |

---

# Quickstart

Start the agent:

```bash
pym2 agent
```

Add an application:

```bash
pym2 add-fastapi --name api --cwd /srv/api --entry app:app
```

Start it:

```bash
pym2 start api
pym2 status
```

---

# Example

```toml
[[apps]]
name = "api"
cwd = "/srv/api"

command = [
  "python",
  "-m",
  "uvicorn",
  "app.main:app",
  "--host","0.0.0.0",
  "--port","8000"
]

env_file = "/srv/api/.env"
restart = "on-failure"
autostart = true
```

---

# CLI

Lifecycle:

```bash
pym2 start <app>
pym2 stop <app>
pym2 restart <app>
pym2 status
pym2 inspect <app>
```

Logs:

```bash
pym2 logs <app> -f
pym2 events --follow
```

Diagnostics:

```bash
pym2 ping
pym2 doctor
pym2 config lint
```

Interactive mode:

```bash
pym2 tui
```

---

# Web UI (optional)

Enable in config:

```toml
[agent.web]
enabled = true
host = "127.0.0.1"
port = 17877
password = "change-me"
```

Security rules:

- localhost by default
- public bind requires password
- recommended behind reverse proxy + TLS

---

# Crash Protection

pym2 protects services from restart storms.

Defaults:

| Setting | Value |
|------|------|
Restart window | 60 seconds |
Max restarts | 5 |
Grace reset | 10 seconds |

If exceeded:

status: Errored
reason: max_restarts_exceeded

---

# Runtime Paths

| Path | Purpose |
|-----|-----|
`/etc/pym2/config.toml` | configuration |
`/run/pym2/pym2.sock` | agent socket |
`/var/lib/pym2` | runtime state |
`/var/lib/pym2/logs` | logs |

---

# Build

Minimal build:

cargo build --release

With TUI:

cargo build --release --features tui

With Web UI:

cargo build --release --features webui

---

# Installation

Manual:

sudo install -m755 pym2 /usr/bin/pym2

Systemd:

sudo cp systemd/pym2.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now pym2

---

# Packaging

Debian:

./scripts/build-deb.sh --arch amd64

RPM:

./scripts/build-rpm.sh --arch x86_64

---

# Architecture

High level:

CLI
 │
 │ IPC (Unix socket)
 ▼
Agent
 │
 ▼
Supervisor
 │
 ▼
Managed Processes

See:

docs/ARCHITECTURE.md
docs/DEV_RULES.md

---

# License

AGPL-3.0-or-later
