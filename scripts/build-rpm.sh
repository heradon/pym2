#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

if printf '%s\n' "$@" | grep -qx -- '--help' || printf '%s\n' "$@" | grep -qx -- '-h'; then
  cat << 'USAGE'
Usage: ./scripts/build-rpm.sh [options]

Options:
  --arch <arch>                RPM architecture (x86_64|aarch64)
  --release <n>                RPM release (default: 1)
  --metadata-file <path>       Path to shared metadata env file
  --packager <value>           Spec Packager field
  --summary <text>             Spec Summary
  --description-long <text>    Spec %description text
  --no-enable-service          Do not enable/start service in scriptlets
  --no-systemd                 Do not package systemd unit and skip systemd hooks
  -h, --help                   Show this help

Environment alternatives:
  PACKAGER, SUMMARY, DESCRIPTION_LONG, RELEASE, METADATA_FILE
USAGE
  exit 0
fi

if ! command -v cargo >/dev/null 2>&1; then
  echo "error: cargo not found" >&2
  exit 1
fi
if ! command -v rpmbuild >/dev/null 2>&1; then
  echo "error: rpmbuild not found (install rpm-build)" >&2
  exit 1
fi

VERSION="$(grep -E '^version\s*=\s*"' Cargo.toml | head -n1 | sed -E 's/version\s*=\s*"([^"]+)"/\1/')"
RELEASE="${RELEASE:-1}"
ARCH="$(rpm --eval '%{_arch}' | tr -d '\n')"
METADATA_FILE="${METADATA_FILE:-$ROOT_DIR/packaging/build-metadata.env}"
DEFAULT_PACKAGER="pym2 maintainer <maintainer@example.com>"
DEFAULT_SUMMARY="Linux process manager for Python projects (PM2-like)"
DEFAULT_DESCRIPTION_LONG="Single-binary process supervisor for Python venv + uvicorn apps."
DEFAULT_PROJECT_URL="https://github.com/example/pym2"
PACKAGER="${PACKAGER-}"
SUMMARY="${SUMMARY-}"
DESCRIPTION_LONG="${DESCRIPTION_LONG-}"
PROJECT_URL="${PROJECT_URL-}"
ENABLE_SERVICE=1
USE_SYSTEMD=1

while [[ $# -gt 0 ]]; do
  case "$1" in
    --arch)
      ARCH="$2"
      shift 2
      ;;
    --release)
      RELEASE="$2"
      shift 2
      ;;
    --metadata-file)
      METADATA_FILE="$2"
      shift 2
      ;;
    --packager)
      PACKAGER="$2"
      shift 2
      ;;
    --summary)
      SUMMARY="$2"
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

PACKAGER="${PACKAGER:-${PYM2_PACKAGER:-${DEFAULT_PACKAGER}}}"
SUMMARY="${SUMMARY:-${PYM2_SUMMARY:-${DEFAULT_SUMMARY}}}"
DESCRIPTION_LONG="${DESCRIPTION_LONG:-${PYM2_DESCRIPTION_LONG:-${DEFAULT_DESCRIPTION_LONG}}}"
PROJECT_URL="${PROJECT_URL:-${PYM2_PROJECT_URL:-${DEFAULT_PROJECT_URL}}}"

case "$ARCH" in
  x86_64) RUST_TARGET="x86_64-unknown-linux-gnu" ;;
  aarch64) RUST_TARGET="aarch64-unknown-linux-gnu" ;;
  *)
    echo "error: unsupported RPM arch '$ARCH' (supported: x86_64, aarch64)" >&2
    exit 1
    ;;
esac

BUILD_ROOT="$ROOT_DIR/target/rpm"
TOPDIR="$BUILD_ROOT/rpmbuild"
OUTDIR="$BUILD_ROOT"

rm -rf "$TOPDIR"
mkdir -p "$TOPDIR"/{BUILD,BUILDROOT,RPMS,SOURCES,SPECS,SRPMS,tmp}

cargo build --release --target "$RUST_TARGET"
install -m 0755 "$ROOT_DIR/target/$RUST_TARGET/release/pym2" "$TOPDIR/SOURCES/pym2"
install -m 0644 "$ROOT_DIR/packaging/config.toml" "$TOPDIR/SOURCES/config.toml"
if [[ "$USE_SYSTEMD" -eq 1 ]]; then
  install -m 0644 "$ROOT_DIR/packaging/pym2.service" "$TOPDIR/SOURCES/pym2.service"
fi

SPEC_PATH="$TOPDIR/SPECS/pym2.spec"
cat > "$SPEC_PATH" << SPEC
Name:           pym2
Version:        $VERSION
Release:        $RELEASE%{?dist}
Summary:        $SUMMARY
License:        AGPL-3.0-or-later
URL:            $PROJECT_URL
Packager:       $PACKAGER
BuildArch:      $ARCH

Source0:        pym2
Source1:        config.toml
$(if [[ "$USE_SYSTEMD" -eq 1 ]]; then echo "Source2:        pym2.service"; fi)

Requires(pre):  shadow-utils
$(if [[ "$USE_SYSTEMD" -eq 1 ]]; then cat << 'EOR'
Requires(post): systemd
Requires(preun): systemd
Requires(postun): systemd
EOR
fi)

%description
$DESCRIPTION_LONG

%prep

%build

%install
mkdir -p %{buildroot}/usr/bin
mkdir -p %{buildroot}/etc/pym2
install -m 0755 %{SOURCE0} %{buildroot}/usr/bin/pym2
install -m 0644 %{SOURCE1} %{buildroot}/etc/pym2/config.toml
$(if [[ "$USE_SYSTEMD" -eq 1 ]]; then cat << 'EOI'
mkdir -p %{buildroot}/lib/systemd/system
install -m 0644 %{SOURCE2} %{buildroot}/lib/systemd/system/pym2.service
EOI
fi)

%pre
getent group pym2 >/dev/null || groupadd -r pym2
getent passwd pym2 >/dev/null || useradd -r -g pym2 -d /var/lib/pym2 -s /sbin/nologin -c "pym2 service user" pym2
exit 0

%post
mkdir -p /var/lib/pym2 /var/lib/pym2/logs
chown -R pym2:pym2 /var/lib/pym2
$(if [[ "$USE_SYSTEMD" -eq 1 ]]; then
  if [[ "$ENABLE_SERVICE" -eq 1 ]]; then
    echo "%systemd_post pym2.service"
  else
    cat << 'EOP'
if [ -x /usr/bin/systemctl ]; then
  /usr/bin/systemctl daemon-reload >/dev/null 2>&1 || true
fi
EOP
  fi
fi)

%preun
$(if [[ "$USE_SYSTEMD" -eq 1 ]]; then echo "%systemd_preun pym2.service"; fi)

%postun
$(if [[ "$USE_SYSTEMD" -eq 1 ]]; then echo "%systemd_postun_with_restart pym2.service"; fi)

%files
%attr(0755,root,root) /usr/bin/pym2
%config(noreplace) /etc/pym2/config.toml
$(if [[ "$USE_SYSTEMD" -eq 1 ]]; then echo "%attr(0644,root,root) /lib/systemd/system/pym2.service"; fi)

%changelog
* Tue Mar 03 2026 $PACKAGER - $VERSION-$RELEASE
- Automated build
SPEC

rpmbuild --define "_topdir $TOPDIR" --define "_tmppath %{_topdir}/tmp" -bb "$SPEC_PATH"

mkdir -p "$OUTDIR"
find "$TOPDIR/RPMS" -type f -name "*.rpm" -exec cp {} "$OUTDIR" \;

echo "Built RPMs:"
ls -1 "$OUTDIR"/*.rpm
