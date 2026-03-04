#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="${PYM2_BIN:-$ROOT_DIR/target/debug/pym2}"
SMOKE_ROOT="/tmp/pym2-smoke-$$"
CFG="$SMOKE_ROOT/config.toml"
SOCK="$SMOKE_ROOT/pym2.sock"
STATE_DIR="$SMOKE_ROOT/state"
AGENT_LOG="$SMOKE_ROOT/agent.log"
AGENT_PID=""
SMOKE_OK=0

log() { printf '\n[%s] %s\n' "$(date +%H:%M:%S)" "$*"; }
fail() { echo "ERROR: $*" >&2; exit 1; }

if [[ "$(uname -s)" != "Linux" ]]; then
  echo "SKIP: smoke.sh requires Linux" >&2
  exit 0
fi

pym2_cmd() {
  PYM2_CONFIG="$CFG" "$BIN" "$@"
}

cleanup() {
  if [[ -n "$AGENT_PID" ]] && kill -0 "$AGENT_PID" 2>/dev/null; then
    kill "$AGENT_PID" 2>/dev/null || true
    wait "$AGENT_PID" 2>/dev/null || true
  fi
  if [[ "$SMOKE_OK" -eq 1 ]]; then
    rm -rf "$SMOKE_ROOT"
  else
    echo "smoke artifacts kept at: $SMOKE_ROOT" >&2
  fi
}
trap cleanup EXIT

wait_for() {
  local timeout="$1"
  local interval="$2"
  shift 2
  local start
  start="$(date +%s)"
  while true; do
    if "$@"; then
      return 0
    fi
    if (( $(date +%s) - start >= timeout )); then
      return 1
    fi
    sleep "$interval"
  done
}

start_agent() {
  PYM2_CONFIG="$CFG" "$BIN" agent >"$AGENT_LOG" 2>&1 &
  AGENT_PID="$!"
  wait_for 20 0.2 test -S "$SOCK" || fail "agent socket not ready (see $AGENT_LOG)"
}

restart_agent() {
  if [[ -n "$AGENT_PID" ]] && kill -0 "$AGENT_PID" 2>/dev/null; then
    kill "$AGENT_PID" 2>/dev/null || true
    wait "$AGENT_PID" 2>/dev/null || true
  fi
  rm -f "$SOCK"
  start_agent
}

status_json() {
  pym2_cmd status --json 2>/dev/null || true
}

inspect_json() {
  local name="$1"
  pym2_cmd inspect "$name" --json 2>/dev/null || true
}

http_running() {
  local j
  j="$(status_json)"
  echo "$j" | grep -Eq '"name"[[:space:]]*:[[:space:]]*"http"' \
    && echo "$j" | grep -Eq '"status"[[:space:]]*:[[:space:]]*"running"'
}

http_stopped() {
  inspect_json http | grep -Eq '"status"[[:space:]]*:[[:space:]]*"stopped"'
}

crash_limited() {
  local j
  j="$(inspect_json crash)"
  echo "$j" | grep -Eq '"status"[[:space:]]*:[[:space:]]*"errored"' \
    && echo "$j" | grep -q "max_restarts_exceeded"
}

grace_exited() {
  inspect_json grace | grep -Eq '"last_exit_code"[[:space:]]*:[[:space:]]*1'
}

grace_running() {
  inspect_json grace | grep -Eq '"status"[[:space:]]*:[[:space:]]*"running"'
}

mkdir -p "$SMOKE_ROOT" "$SMOKE_ROOT/http" "$STATE_DIR"

cat > "$CFG" <<CONFIG
[agent]
socket = "$SOCK"
state_dir = "$STATE_DIR"
CONFIG

if [[ ! -x "$BIN" ]]; then
  log "building debug binary"
  (cd "$ROOT_DIR" && cargo build -q)
fi

log "starting agent"
start_agent

log "scenario A: basic command"
pym2_cmd add-cmd --name http --cwd "$SMOKE_ROOT/http" --command "python -m http.server 8099" --restart never --autostart false
restart_agent
pym2_cmd start http >/dev/null
wait_for 15 0.5 http_running || fail "http app did not become running"
curl -fsS http://127.0.0.1:8099 >/dev/null || fail "http endpoint not reachable"
pym2_cmd stop http >/dev/null
wait_for 10 0.5 http_stopped || fail "http app did not stop"

log "scenario B: crash loop protection"
pym2_cmd add-cmd --name crash --cwd "$SMOKE_ROOT" --command "bash -lc 'exit 1'" --autostart false >/dev/null
restart_agent
pym2_cmd start crash >/dev/null
wait_for 25 0.5 crash_limited || fail "crash app did not hit restart limiter"

log "scenario C: grace reset"
pym2_cmd add-cmd --name grace --cwd "$SMOKE_ROOT" --command "bash -lc 'sleep 12; exit 1'" --autostart false >/dev/null
restart_agent
pym2_cmd start grace >/dev/null
for cycle in 1 2 3; do
  wait_for 25 0.5 grace_exited || fail "grace app did not exit with code=1 in cycle $cycle"
  wait_for 3 0.2 grace_running || fail "grace app restart backoff too large in cycle $cycle (grace reset likely broken)"
done
GRACE_JSON="$(inspect_json grace)"
if echo "$GRACE_JSON" | grep -q "max_restarts_exceeded"; then
  fail "grace app unexpectedly hit crash limiter"
fi

log "scenario D: env_file"
cat > "$SMOKE_ROOT/.env" <<ENV
FOO=bar
ENV
pym2_cmd add-cmd --name env --cwd "$SMOKE_ROOT" --command "bash -lc 'echo \$FOO; sleep 2'" --env-file "$SMOKE_ROOT/.env" --restart never --autostart false >/dev/null
restart_agent
pym2_cmd start env >/dev/null
sleep 3
ENV_LOGS="$(pym2_cmd logs env --tail 50)"
echo "$ENV_LOGS" | grep -q "bar" || fail "env app logs do not contain FOO=bar"

log "smoke OK"
SMOKE_OK=1
exit 0
