# Releasing pym2

Use this checklist before creating a new release tag.

## 1) Tests

- `cargo test`
- `cargo test --all-features`

## 2) Smoke test

- `./scripts/smoke.sh`
- Must finish with `smoke OK` and exit code `0`.

## 3) Build packages

- Debian:
  - `./scripts/build-deb.sh --arch amd64`
  - `./scripts/build-deb.sh --arch arm64`
- RPM:
  - `./scripts/build-rpm.sh --arch x86_64`
  - `./scripts/build-rpm.sh --arch aarch64`

## 4) Systemd verification

On a test host/VM:

- Install package
- `sudo systemctl enable --now pym2`
- `pym2 status`
- Reboot host
- Verify service is up after reboot:
  - `systemctl status pym2`
  - `pym2 status`

## 5) Final checks

- Confirm `README.md` reflects current CLI/config behavior
- Confirm changelog/release notes are up to date
- Create and push release tag
