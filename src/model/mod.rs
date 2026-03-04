use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

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
    #[serde(default)]
    pub web: WebConfig,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            socket: default_socket(),
            state_dir: default_state_dir(),
            web: WebConfig::default(),
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
pub struct WebConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_web_host")]
    pub host: String,
    #[serde(default = "default_web_port")]
    pub port: u16,
    #[serde(default)]
    pub password: Option<String>,
}

impl Default for WebConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            host: default_web_host(),
            port: default_web_port(),
            password: None,
        }
    }
}

fn default_web_host() -> String {
    "127.0.0.1".to_string()
}

fn default_web_port() -> u16 {
    17877
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSpec {
    pub name: String,
    pub cwd: String,
    #[serde(default)]
    pub command: Vec<String>,
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
    pub env_file: Option<String>,
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
    #[serde(alias = "blocked")]
    Errored,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AppRuntimeState {
    pub status: AppStatus,
    pub pid: Option<u32>,
    pub started_at: Option<u64>,
    pub restart_count: u32,
    pub last_exit_code: Option<i32>,
    pub last_exit_signal: Option<String>,
    pub last_error: Option<String>,
    pub last_reason: Option<String>,
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
            last_exit_signal: None,
            last_error: None,
            last_reason: None,
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
    #[serde(default)]
    pub command: Vec<String>,
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

pub fn effective_command(spec: &AppSpec) -> Vec<String> {
    if !spec.command.is_empty() {
        return spec.command.clone();
    }

    let cwd = PathBuf::from(&spec.cwd);
    let venv = if Path::new(&spec.venv).is_absolute() {
        PathBuf::from(&spec.venv)
    } else {
        cwd.join(&spec.venv)
    };
    let uvicorn = venv.join("bin/uvicorn");
    let mut cmd = if uvicorn.exists() {
        vec![uvicorn.to_string_lossy().to_string(), spec.entry.clone()]
    } else {
        vec![
            venv.join("bin/python").to_string_lossy().to_string(),
            "-m".to_string(),
            "uvicorn".to_string(),
            spec.entry.clone(),
        ]
    };
    cmd.extend(spec.args.clone());
    cmd
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn legacy_spec(cwd: String) -> AppSpec {
        AppSpec {
            name: "api".to_string(),
            cwd,
            command: Vec::new(),
            venv: ".venv".to_string(),
            entry: "app.main:app".to_string(),
            args: vec!["--port".to_string(), "8000".to_string()],
            autostart: true,
            restart: RestartPolicy::OnFailure,
            stop_signal: "SIGTERM".to_string(),
            kill_timeout_ms: 8000,
            restart_schedule: None,
            env_file: None,
            env: HashMap::new(),
        }
    }

    #[test]
    fn effective_command_prefers_uvicorn_binary_in_legacy_mode() {
        let root = std::env::temp_dir().join(format!("pym2-model-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join(".venv/bin")).expect("create test dirs");
        fs::write(root.join(".venv/bin/uvicorn"), b"#!/bin/sh\n").expect("touch uvicorn");

        let spec = legacy_spec(root.to_string_lossy().to_string());
        let cmd = effective_command(&spec);

        assert!(cmd[0].ends_with("/.venv/bin/uvicorn"));
        assert_eq!(cmd[1], "app.main:app");
        let _ = fs::remove_dir_all(&root);
    }
}
