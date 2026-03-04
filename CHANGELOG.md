# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [1.2.0] - 2026-03-04

### Added
- Generic process runner via `command[]` as the primary app execution model.
- CLI presets: `pym2 add-fastapi` and `pym2 add-cmd`.
- Optional `env_file` loading per app.
- Linux smoke test script at `scripts/smoke.sh` covering command start/stop, crash-loop protection, grace reset, and env file injection.
- Unit tests for command parsing and restart policy behavior.

### Changed
- Effective legacy command construction now aligns with runtime behavior (`venv/bin/uvicorn` preferred, fallback to `venv/bin/python -m uvicorn`).
- Status/inspect output shows reason and effective command for easier diagnostics.

### Fixed
- Inspect output fields now include start/exit metadata consistently.
- IPC client error messages now include clearer agent socket/connect guidance.
