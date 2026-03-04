# pym2 1.2.0 Notes

## Highlights

- Generic process mode with `command = [..]` per app.
- Backward-compatible legacy mode (`venv` + `entry` + `args`) remains supported.
- Optional `env_file` loading (`KEY=VALUE`, simple format).
- Improved restart behavior:
  - success grace reset after 10s runtime
  - crash-loop guard at `>5 restarts in 60s`
  - clear `errored` state with `last_reason`
- Runtime persistence now writes atomically and includes `schema_version`.
- New CLI helper commands:
  - `pym2 add-fastapi ...`
  - `pym2 add-cmd ...`
  - both support `--restart-schedule`
- New diagnostics command:
  - `pym2 inspect <name> [--json]`
- Feature-gated builds:
  - `tui`
  - `webui`
- Packaging scripts support cargo feature flags:
  - `--no-default-features`
  - `--features ...`
- CI now tests both default and minimal feature builds.

## Migration Summary

Recommended app definition style:

```toml
[[apps]]
name = "api"
cwd = "/srv/api"
command = ["python", "-m", "uvicorn", "app.main:app", "--host", "0.0.0.0", "--port", "8000"]
```

Legacy definitions still work if `command` is empty.

## Notes

- No multi-server orchestration was added in this cycle.
- Existing IPC model remains the central integration point for future hub/TUI work.
