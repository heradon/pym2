#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

if printf '%s\n' "$@" | grep -qx -- '--help' || printf '%s\n' "$@" | grep -qx -- '-h'; then
  cat << 'USAGE'
Usage: ./scripts/build-deb.sh [options]

Options:
  --arch <arch>                Debian architecture (default: dpkg --print-architecture)
  --metadata-file <path>       Path to shared metadata env file
  --maintainer <value>         Maintainer field
  --description-short <text>   Control Description short line
  --description-long <text>    Control Description long text
  --no-enable-service          Do not enable service in postinst
  --no-systemd                 Do not package systemd unit and skip systemd hooks
  -h, --help                   Show this help

Environment alternatives:
  MAINTAINER, DESCRIPTION_SHORT, DESCRIPTION_LONG, METADATA_FILE
USAGE
  exit 0
fi

if ! command -v cargo >/dev/null 2>&1; then
  echo "error: cargo not found" >&2
  exit 1
fi
if ! command -v dpkg-deb >/dev/null 2>&1; then
  echo "error: dpkg-deb not found (install dpkg-dev)" >&2
  exit 1
fi

PKG_NAME="pym2"
VERSION="$(grep -E '^version\s*=\s*"' Cargo.toml | head -n1 | sed -E 's/version\s*=\s*"([^"]+)"/\1/')"
ARCH="$(dpkg --print-architecture)"
METADATA_FILE="${METADATA_FILE:-$ROOT_DIR/packaging/build-metadata.env}"
DEFAULT_MAINTAINER="pym2 maintainer <maintainer@example.com>"
DEFAULT_DESCRIPTION_SHORT="Linux process manager for Python projects (PM2-like)"
DEFAULT_DESCRIPTION_LONG="Single-binary process supervisor for Python venv + uvicorn apps."
MAINTAINER="${MAINTAINER-}"
DESCRIPTION_SHORT="${DESCRIPTION_SHORT-}"
DESCRIPTION_LONG="${DESCRIPTION_LONG-}"
ENABLE_SERVICE=1
USE_SYSTEMD=1

while [[ $# -gt 0 ]]; do
  case "$1" in
    --arch)
      ARCH="$2"
      shift 2
      ;;
    --metadata-file)
      METADATA_FILE="$2"
      shift 2
      ;;
    --maintainer)
      MAINTAINER="$2"
      shift 2
      ;;
    --description-short)
      DESCRIPTION_SHORT="$2"
      shift 2
      ;;
    --description-long)
      DESCRIPTION_LONG="$2"
      shift 2
      ;;
    --no-enable-service)
      ENABLE_SERVICE=0
      shift
      ;;
    --no-systemd)
      USE_SYSTEMD=0
      ENABLE_SERVICE=0
      shift
      ;;
    -h|--help) exit 0 ;;
    *)
      echo "error: unknown option: $1" >&2
      exit 1
      ;;
  esac
done

if [[ -f "$METADATA_FILE" ]]; then
  # shellcheck disable=SC1090
  source "$METADATA_FILE"
fi

MAINTAINER="${MAINTAINER:-${PYM2_MAINTAINER:-${DEFAULT_MAINTAINER}}}"
DESCRIPTION_SHORT="${DESCRIPTION_SHORT:-${PYM2_DESCRIPTION_SHORT:-${DEFAULT_DESCRIPTION_SHORT}}}"
DESCRIPTION_LONG="${DESCRIPTION_LONG:-${PYM2_DESCRIPTION_LONG:-${DEFAULT_DESCRIPTION_LONG}}}"

case "$ARCH" in
  amd64) RUST_TARGET="x86_64-unknown-linux-gnu" ;;
  arm64) RUST_TARGET="aarch64-unknown-linux-gnu" ;;
  *)
    echo "error: unsupported Debian arch '$ARCH' (supported: amd64, arm64)" >&2
    exit 1
    ;;
esac

BUILD_ROOT="$ROOT_DIR/target/deb"
STAGE_DIR="$BUILD_ROOT/${PKG_NAME}_${VERSION}_${ARCH}"

rm -rf "$STAGE_DIR"
mkdir -p "$STAGE_DIR/DEBIAN" \
         "$STAGE_DIR/usr/bin" \
         "$STAGE_DIR/etc/pym2"

if [[ "$USE_SYSTEMD" -eq 1 ]]; then
  mkdir -p "$STAGE_DIR/lib/systemd/system"
fi

cargo build --release --target "$RUST_TARGET"
install -m 0755 "$ROOT_DIR/target/$RUST_TARGET/release/pym2" "$STAGE_DIR/usr/bin/pym2"
install -m 0644 "$ROOT_DIR/packaging/config.toml" "$STAGE_DIR/etc/pym2/config.toml"

if [[ "$USE_SYSTEMD" -eq 1 ]]; then
  install -m 0644 "$ROOT_DIR/packaging/pym2.service" "$STAGE_DIR/lib/systemd/system/pym2.service"
fi

cat > "$STAGE_DIR/DEBIAN/control" << CONTROL
Package: $PKG_NAME
Version: $VERSION
Section: admin
Priority: optional
Architecture: $ARCH
Maintainer: $MAINTAINER
Depends: libc6
Description: $DESCRIPTION_SHORT
 $DESCRIPTION_LONG
CONTROL

cat > "$STAGE_DIR/DEBIAN/conffiles" << 'CONFFILES'
/etc/pym2/config.toml
CONFFILES

cat > "$STAGE_DIR/DEBIAN/postinst" << POSTINST
#!/bin/sh
set -e

if ! id -u pym2 >/dev/null 2>&1; then
  adduser --system --group --no-create-home --home /var/lib/pym2 pym2
fi

mkdir -p /var/lib/pym2 /var/lib/pym2/logs
chown -R pym2:pym2 /var/lib/pym2

if command -v systemctl >/dev/null 2>&1; then
  systemctl daemon-reload || true
$(if [[ "$USE_SYSTEMD" -eq 1 && "$ENABLE_SERVICE" -eq 1 ]]; then echo "  systemctl enable pym2.service || true"; fi)
fi
POSTINST

cat > "$STAGE_DIR/DEBIAN/prerm" << PRERM
#!/bin/sh
set -e

if command -v systemctl >/dev/null 2>&1; then
$(if [[ "$USE_SYSTEMD" -eq 1 ]]; then cat << 'EOP'
  systemctl stop pym2.service || true
  systemctl disable pym2.service || true
EOP
fi)
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
