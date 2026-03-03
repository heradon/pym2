use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigFile {
    #[serde(default)]
    pub agent: AgentConfig,
    #[serde(default)]
    pub apps: Vec<AppSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    #[serde(default = "default_socket")]
    pub socket: String,
    #[serde(default = "default_state_dir")]
    pub state_dir: String,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            socket: default_socket(),
            state_dir: default_state_dir(),
        }
    }
}

fn default_socket() -> String {
    "/run/pym2/pym2.sock".to_string()
}

fn default_state_dir() -> String {
    "/var/lib/pym2".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSpec {
    pub name: String,
    pub cwd: String,
    pub venv: String,
    pub entry: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default = "default_true")]
    pub autostart: bool,
    #[serde(default)]
    pub restart: RestartPolicy,
    #[serde(default = "default_stop_signal")]
    pub stop_signal: String,
    #[serde(default = "default_kill_timeout_ms")]
    pub kill_timeout_ms: u64,
    #[serde(default)]
    pub restart_schedule: Option<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
}

fn default_true() -> bool {
    true
}

fn default_stop_signal() -> String {
    "SIGTERM".to_string()
}

fn default_kill_timeout_ms() -> u64 {
    8_000
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum RestartPolicy {
    Never,
    #[default]
    OnFailure,
    Always,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppStatus {
    Running,
    Stopped,
    Errored,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AppRuntimeState {
    pub status: AppStatus,
    pub pid: Option<u32>,
    pub started_at: Option<u64>,
    pub restart_count: u32,
    pub last_exit_code: Option<i32>,
    pub last_error: Option<String>,
    pub last_start_attempt_at: Option<u64>,
    pub backoff_until: Option<u64>,
    pub next_scheduled_restart_at: Option<u64>,
}

impl Default for AppRuntimeState {
    fn default() -> Self {
        Self {
            status: AppStatus::Stopped,
            pid: None,
            started_at: None,
            restart_count: 0,
            last_exit_code: None,
            last_error: None,
            last_start_attempt_at: None,
            backoff_until: None,
            next_scheduled_restart_at: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSummary {
    pub name: String,
    pub cwd: String,
    pub entry: String,
    pub restart: RestartPolicy,
    pub runtime: AppRuntimeState,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppDetails {
    pub spec: AppSpec,
    pub runtime: AppRuntimeState,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum IpcRequest {
    Start {
        name: String,
    },
    Stop {
        name: String,
    },
    Restart {
        name: String,
    },
    ListApps,
    GetApp {
        name: String,
    },
    TailLogs {
        name: String,
        #[serde(default = "default_tail")]
        tail: usize,
        #[serde(default)]
        source: LogSource,
    },
    StreamLogs {
        name: String,
        #[serde(default = "default_tail")]
        tail: usize,
        #[serde(default)]
        source: LogSource,
        #[serde(default = "default_follow_interval_ms")]
        follow_interval_ms: u64,
    },
    WatchEvents,
}

fn default_tail() -> usize {
    200
}

fn default_follow_interval_ms() -> u64 {
    400
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LogSource {
    Stdout,
    Stderr,
    #[default]
    Both,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcResponse {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl IpcResponse {
    pub fn ok(data: serde_json::Value) -> Self {
        Self {
            ok: true,
            data: Some(data),
            error: None,
        }
    }

    pub fn err(msg: impl Into<String>) -> Self {
        Self {
            ok: false,
            data: None,
            error: Some(msg.into()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamLogEvent {
    pub source: LogSource,
    pub line: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentEvent {
    pub ts: u64,
    pub kind: AgentEventKind,
    pub app: String,
    pub runtime: AppRuntimeState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentEventKind {
    StateChanged,
    ProcessStarted,
    ProcessStopped,
    ProcessErrored,
}
