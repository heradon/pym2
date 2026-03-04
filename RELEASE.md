# Release Flow (1.2.0)

## 1) Prepare version and changelog

1. Set `Cargo.toml` version to the target release (for this branch: `1.2.0`).
2. Update `CHANGELOG.md` with release date and key changes.

## 2) Run checks locally

```bash
cargo fmt -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
cargo test --all-features
./scripts/smoke.sh
```

## 3) Run packaging builds

```bash
./scripts/build-deb.sh --arch amd64
./scripts/build-deb.sh --arch arm64
./scripts/build-rpm.sh --arch x86_64
./scripts/build-rpm.sh --arch aarch64
```

## 4) Validate service install on a Linux VM

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now pym2
systemctl status pym2
pym2 status
```

Reboot once and verify `pym2` comes back.

## 5) Cut release tag

```bash
git tag v1.2.0
git push origin v1.2.0
```

Tag workflow can publish release artifacts.
