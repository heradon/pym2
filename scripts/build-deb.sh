#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

if ! command -v cargo >/dev/null 2>&1; then
  echo "error: cargo not found" >&2
  exit 1
fi
if ! command -v dpkg-deb >/dev/null 2>&1; then
  echo "error: dpkg-deb not found (install dpkg-dev)" >&2
  exit 1
fi

VERSION="$(grep -E '^version\s*=\s*"' Cargo.toml | head -n1 | sed -E 's/version\s*=\s*"([^"]+)"/\1/')"
ARCH="${1:-$(dpkg --print-architecture)}"
PKG_NAME="pym2"
BUILD_ROOT="$ROOT_DIR/target/deb"
STAGE_DIR="$BUILD_ROOT/${PKG_NAME}_${VERSION}_${ARCH}"

rm -rf "$STAGE_DIR"
mkdir -p "$STAGE_DIR/DEBIAN" \
         "$STAGE_DIR/usr/bin" \
         "$STAGE_DIR/etc/pym2" \
         "$STAGE_DIR/lib/systemd/system"

cargo build --release
install -m 0755 "$ROOT_DIR/target/release/pym2" "$STAGE_DIR/usr/bin/pym2"

cat > "$STAGE_DIR/etc/pym2/config.toml" << 'CONF'
[agent]
socket = "/run/pym2/pym2.sock"
state_dir = "/var/lib/pym2"

# Add managed apps below.
# [[apps]]
# name = "api"
# cwd = "/srv/api"
# venv = ".venv"
# entry = "app.main:app"
# args = ["--host", "0.0.0.0", "--port", "8000"]
# autostart = true
# restart = "on-failure"
# stop_signal = "SIGTERM"
# kill_timeout_ms = 8000
# env = { PYTHONUNBUFFERED = "1" }
CONF

cat > "$STAGE_DIR/lib/systemd/system/pym2.service" << 'SERVICE'
[Unit]
Description=Pym2 Python Process Manager Agent
After=network.target

[Service]
Type=simple
User=pym2
Group=pym2
ExecStart=/usr/bin/pym2 agent
Restart=always
RestartSec=2
RuntimeDirectory=pym2
RuntimeDirectoryMode=0755
StateDirectory=pym2
StateDirectoryMode=0755
NoNewPrivileges=true

[Install]
WantedBy=multi-user.target
SERVICE

cat > "$STAGE_DIR/DEBIAN/control" << CONTROL
Package: $PKG_NAME
Version: $VERSION
Section: admin
Priority: optional
Architecture: $ARCH
Maintainer: pym2 maintainer <maintainer@example.com>
Depends: libc6
Description: Linux process manager for Python projects (PM2-like)
 Single-binary process supervisor for Python venv + uvicorn apps.
CONTROL

cat > "$STAGE_DIR/DEBIAN/conffiles" << 'CONFFILES'
/etc/pym2/config.toml
CONFFILES

cat > "$STAGE_DIR/DEBIAN/postinst" << 'POSTINST'
#!/bin/sh
set -e

if ! id -u pym2 >/dev/null 2>&1; then
  adduser --system --group --no-create-home --home /var/lib/pym2 pym2
fi

mkdir -p /var/lib/pym2 /var/lib/pym2/logs
chown -R pym2:pym2 /var/lib/pym2

if command -v systemctl >/dev/null 2>&1; then
  systemctl daemon-reload || true
  systemctl enable pym2.service || true
fi
POSTINST

cat > "$STAGE_DIR/DEBIAN/prerm" << 'PRERM'
#!/bin/sh
set -e

if command -v systemctl >/dev/null 2>&1; then
  systemctl stop pym2.service || true
  systemctl disable pym2.service || true
fi
PRERM

cat > "$STAGE_DIR/DEBIAN/postrm" << 'POSTRM'
#!/bin/sh
set -e

if command -v systemctl >/dev/null 2>&1; then
  systemctl daemon-reload || true
fi
POSTRM

chmod 0755 "$STAGE_DIR/DEBIAN/postinst" "$STAGE_DIR/DEBIAN/prerm" "$STAGE_DIR/DEBIAN/postrm"

OUT_DEB="$BUILD_ROOT/${PKG_NAME}_${VERSION}_${ARCH}.deb"
mkdir -p "$BUILD_ROOT"
dpkg-deb --root-owner-group --build "$STAGE_DIR" "$OUT_DEB"

echo "Built: $OUT_DEB"
