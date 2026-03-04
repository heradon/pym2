use crate::error::{PyopsError, Result};
use crate::model::{
    effective_command, AppDetails, AppRuntimeState, AppSpec, AppStatus, AppSummary, ConfigFile,
    LogSource, RestartPolicy,
};
use crate::schedule::{next_occurrence, parse_restart_schedule, RestartSchedule};
use std::collections::{HashMap, VecDeque};
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::os::unix::process::CommandExt;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const SUCCESS_AFTER_SECS: u64 = 10;
const RESTART_WINDOW_SECS: u64 = 60;
const MAX_RESTARTS_IN_WINDOW: usize = 5;

struct ManagedApp {
    spec: AppSpec,
    schedule: Option<RestartSchedule>,
    state: AppRuntimeState,
    child: Option<Child>,
    recent_restarts: VecDeque<Instant>,
    consecutive_restarts: u32,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct PersistedRuntimeState {
    #[serde(default = "default_runtime_schema_version")]
    schema_version: u32,
    apps: HashMap<String, AppRuntimeState>,
}

fn default_runtime_schema_version() -> u32 {
    1
}

pub struct Supervisor {
    state_dir: PathBuf,
    logs_dir: PathBuf,
    apps: HashMap<String, ManagedApp>,
}

impl Supervisor {
    pub fn new(cfg: ConfigFile, state_dir: PathBuf, logs_dir: PathBuf) -> Self {
        let mut apps = HashMap::new();
        for spec in cfg.apps {
            let schedule = spec
                .restart_schedule
                .as_ref()
                .and_then(|s| parse_restart_schedule(s).ok());
            apps.insert(
                spec.name.clone(),
                ManagedApp {
                    spec,
                    schedule,
                    state: AppRuntimeState::default(),
                    child: None,
                    recent_restarts: VecDeque::new(),
                    consecutive_restarts: 0,
                },
            );
        }

        let mut sup = Self {
            state_dir,
            logs_dir,
            apps,
        };
        let _ = sup.restore_runtime_state();
        sup.refresh_all_schedule_targets(unix_now());
        sup
    }

    pub fn start_autostart(&mut self) {
        let names: Vec<String> = self
            .apps
            .values()
            .filter(|a| a.spec.autostart)
            .map(|a| a.spec.name.clone())
            .collect();
        for name in names {
            let _ = self.start(&name);
        }
    }

    pub fn list_apps(&self) -> Vec<AppSummary> {
        let mut out = self
            .apps
            .values()
            .map(|app| AppSummary {
                name: app.spec.name.clone(),
                cwd: app.spec.cwd.clone(),
                command: effective_command(&app.spec),
                entry: app.spec.entry.clone(),
                restart: app.spec.restart,
                runtime: app.state.clone(),
            })
            .collect::<Vec<_>>();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    pub fn runtime_snapshot(&self) -> HashMap<String, AppRuntimeState> {
        let mut out = HashMap::new();
        for (name, app) in &self.apps {
            out.insert(name.clone(), app.state.clone());
        }
        out
    }

    pub fn get_app(&self, name: &str) -> Result<AppDetails> {
        let app = self
            .apps
            .get(name)
            .ok_or_else(|| PyopsError::Supervisor(format!("unknown app '{}'", name)))?;
        Ok(AppDetails {
            spec: app.spec.clone(),
            runtime: app.state.clone(),
        })
    }

    pub fn start(&mut self, name_or_all: &str) -> Result<Vec<String>> {
        let changed = if name_or_all == "all" {
            let names = self.apps.keys().cloned().collect::<Vec<_>>();
            let mut started = Vec::new();
            for name in names {
                self.start_one(&name)?;
                started.push(name);
            }
            started
        } else {
            self.start_one(name_or_all)?;
            vec![name_or_all.to_string()]
        };

        self.persist_runtime_state()?;
        Ok(changed)
    }

    pub fn stop(&mut self, name_or_all: &str) -> Result<Vec<String>> {
        let changed = if name_or_all == "all" {
            let names = self.apps.keys().cloned().collect::<Vec<_>>();
            let mut stopped = Vec::new();
            for name in names {
                self.stop_one(&name)?;
                stopped.push(name);
            }
            stopped
        } else {
            self.stop_one(name_or_all)?;
            vec![name_or_all.to_string()]
        };

        self.persist_runtime_state()?;
        Ok(changed)
    }

    pub fn restart(&mut self, name_or_all: &str) -> Result<Vec<String>> {
        let changed = if name_or_all == "all" {
            let names = self.apps.keys().cloned().collect::<Vec<_>>();
            let mut restarted = Vec::new();
            for name in names {
                self.stop_one(&name)?;
                self.start_one(&name)?;
                restarted.push(name);
            }
            restarted
        } else {
            self.stop_one(name_or_all)?;
            self.start_one(name_or_all)?;
            vec![name_or_all.to_string()]
        };

        self.persist_runtime_state()?;
        Ok(changed)
    }

    pub fn shutdown_all(&mut self) {
        let _ = self.stop("all");
        let _ = self.persist_runtime_state();
    }

    pub fn tick(&mut self) {
        let now = Instant::now();
        let now_epoch = unix_now();
        let names = self.apps.keys().cloned().collect::<Vec<_>>();
        let mut changed = false;

        for name in names {
            let mut needs_restart = false;
            let mut scheduled_restart_due = false;
            let mut scheduled_restart_allowed = false;
            if let Some(app) = self.apps.get_mut(&name) {
                if let Some(child) = app.child.as_mut() {
                    match child.try_wait() {
                        Ok(Some(status)) => {
                            Self::record_exit(app, status, now_epoch);
                            let failure = Self::is_failure(status);
                            if Self::should_restart(app.spec.restart, failure) {
                                needs_restart = Self::schedule_restart(app, now);
                            } else {
                                app.state.status = AppStatus::Stopped;
                            }
                            changed = true;
                        }
                        Ok(None) => {}
                        Err(err) => {
                            app.state.status = AppStatus::Errored;
                            app.state.last_error = Some(format!("try_wait failed: {}", err));
                            app.state.last_reason = Some("try_wait_failed".to_string());
                            app.child = None;
                            app.state.pid = None;
                            changed = true;
                        }
                    }
                }

                if app.child.is_none() {
                    if app.state.status == AppStatus::Running {
                        if let Some(pid) = app.state.pid {
                            if !is_pid_alive(pid as i32) {
                                app.state.status = AppStatus::Stopped;
                                app.state.pid = None;
                                app.state.started_at = None;
                                app.state.last_reason = Some("pid_not_alive".to_string());
                                changed = true;
                            }
                        }
                    }

                    if let Some(backoff_until_epoch) = app.state.backoff_until {
                        let now_epoch = unix_now();
                        if now_epoch >= backoff_until_epoch {
                            needs_restart = true;
                        }
                    }
                }

                if let Some(next_at) = app.state.next_scheduled_restart_at {
                    if now_epoch >= next_at {
                        scheduled_restart_due = true;
                        scheduled_restart_allowed = app.state.status == AppStatus::Running;
                    }
                }
            }

            if scheduled_restart_due {
                if scheduled_restart_allowed {
                    if self.stop_one(&name).is_ok() && self.start_one(&name).is_ok() {
                        changed = true;
                    }
                }
                self.refresh_schedule_target_for(&name, unix_now());
            }

            if needs_restart {
                let _ = self.start_one(&name);
                changed = true;
            }
        }

        if changed {
            let _ = self.persist_runtime_state();
        }
    }

    pub fn tail_logs(&self, name: &str, source: LogSource, tail: usize) -> Result<Vec<String>> {
        let app = self
            .apps
            .get(name)
            .ok_or_else(|| PyopsError::Supervisor(format!("unknown app '{}'", name)))?;

        let mut lines = Vec::new();
        match source {
            LogSource::Stdout => {
                lines.extend(Self::tail_file(
                    &self.logs_dir.join(format!("{}.out.log", app.spec.name)),
                    tail,
                )?);
            }
            LogSource::Stderr => {
                lines.extend(Self::tail_file(
                    &self.logs_dir.join(format!("{}.err.log", app.spec.name)),
                    tail,
                )?);
            }
            LogSource::Both => {
                lines.extend(Self::tail_file(
                    &self.logs_dir.join(format!("{}.out.log", app.spec.name)),
                    tail,
                )?);
                lines.extend(Self::tail_file(
                    &self.logs_dir.join(format!("{}.err.log", app.spec.name)),
                    tail,
                )?);
            }
        }
        Ok(lines)
    }

    pub fn log_paths(&self, name: &str) -> Result<(PathBuf, PathBuf)> {
        let app = self
            .apps
            .get(name)
            .ok_or_else(|| PyopsError::Supervisor(format!("unknown app '{}'", name)))?;
        Ok((
            self.logs_dir.join(format!("{}.out.log", app.spec.name)),
            self.logs_dir.join(format!("{}.err.log", app.spec.name)),
        ))
    }

    fn runtime_state_path(&self) -> PathBuf {
        self.state_dir.join("runtime_state.json")
    }

    fn persist_runtime_state(&self) -> Result<()> {
        let snapshot = PersistedRuntimeState {
            schema_version: default_runtime_schema_version(),
            apps: self.runtime_snapshot(),
        };
        let payload = serde_json::to_vec_pretty(&snapshot)?;
        write_atomic(&self.runtime_state_path(), &payload)?;
        Ok(())
    }

    fn restore_runtime_state(&mut self) -> Result<()> {
        let path = self.runtime_state_path();
        if !path.exists() {
            return Ok(());
        }

        let payload = fs::read(&path)?;
        let persisted: PersistedRuntimeState = serde_json::from_slice(&payload)?;
        if persisted.schema_version != default_runtime_schema_version() {
            return Err(PyopsError::Supervisor(format!(
                "unsupported runtime_state schema_version {}, expected {}",
                persisted.schema_version,
                default_runtime_schema_version()
            )));
        }

        for (name, state) in persisted.apps {
            if let Some(app) = self.apps.get_mut(&name) {
                app.state = state;
                if app.state.status == AppStatus::Running {
                    if let Some(pid) = app.state.pid {
                        if !is_pid_alive(pid as i32) {
                            app.state.status = AppStatus::Stopped;
                            app.state.pid = None;
                            app.state.started_at = None;
                        }
                    } else {
                        app.state.status = AppStatus::Stopped;
                    }
                }
            }
        }

        self.refresh_all_schedule_targets(unix_now());
        Ok(())
    }

    fn start_one(&mut self, name: &str) -> Result<()> {
        let app = self
            .apps
            .get_mut(name)
            .ok_or_else(|| PyopsError::Supervisor(format!("unknown app '{}'", name)))?;

        if app.child.is_some() {
            return Ok(());
        }

        if app.state.status == AppStatus::Running {
            if let Some(pid) = app.state.pid {
                if is_pid_alive(pid as i32) {
                    return Ok(());
                }
            }
        }

        let cwd = PathBuf::from(&app.spec.cwd);
        let (exec, args) = build_command(&app.spec)?;

        let stdout_path = self.logs_dir.join(format!("{}.out.log", app.spec.name));
        let stderr_path = self.logs_dir.join(format!("{}.err.log", app.spec.name));

        if let Some(parent) = stdout_path.parent() {
            fs::create_dir_all(parent)?;
        }

        let stdout = File::options()
            .create(true)
            .append(true)
            .open(stdout_path)?;
        let stderr = File::options()
            .create(true)
            .append(true)
            .open(stderr_path)?;

        let mut cmd = Command::new(exec);
        cmd.args(args)
            .current_dir(&cwd)
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr));

        let env_map = build_app_env(&app.spec, &cwd)?;
        for (k, v) in env_map {
            cmd.env(k, v);
        }

        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }

        app.state.last_start_attempt_at = Some(unix_now());
        app.state.backoff_until = None;

        match cmd.spawn() {
            Ok(child) => {
                app.state.status = AppStatus::Running;
                app.state.pid = Some(child.id());
                app.state.started_at = Some(unix_now());
                app.state.last_error = None;
                app.state.last_reason = Some("running".to_string());
                app.child = Some(child);
                if let Some(schedule) = app.schedule {
                    app.state.next_scheduled_restart_at = next_occurrence(schedule, unix_now())
                        .or(app.state.next_scheduled_restart_at);
                }
                Ok(())
            }
            Err(err) => {
                app.state.status = AppStatus::Errored;
                app.state.last_error = Some(format!("spawn failed: {}", err));
                app.state.last_reason = Some("spawn_failed".to_string());
                Err(PyopsError::Supervisor(format!(
                    "failed to start '{}': {}",
                    app.spec.name, err
                )))
            }
        }
    }

    fn stop_one(&mut self, name: &str) -> Result<()> {
        let app = self
            .apps
            .get_mut(name)
            .ok_or_else(|| PyopsError::Supervisor(format!("unknown app '{}'", name)))?;

        let pid = match app.state.pid {
            Some(pid) => pid,
            None => {
                app.state.status = AppStatus::Stopped;
                app.state.last_reason = Some("manual_stop".to_string());
                return Ok(());
            }
        };

        let stop_sig = signal_from_name(&app.spec.stop_signal).unwrap_or(libc::SIGTERM);
        send_signal_to_group(pid as i32, stop_sig)?;

        let timeout = Duration::from_millis(app.spec.kill_timeout_ms);
        let started = Instant::now();
        loop {
            if let Some(child) = app.child.as_mut() {
                match child.try_wait() {
                    Ok(Some(status)) => {
                        Self::record_exit(app, status, unix_now());
                        app.state.status = AppStatus::Stopped;
                        app.state.last_reason = Some("manual_stop".to_string());
                        app.consecutive_restarts = 0;
                        return Ok(());
                    }
                    Ok(None) => {}
                    Err(err) => {
                        return Err(PyopsError::Supervisor(format!(
                            "failed waiting for '{}': {}",
                            app.spec.name, err
                        )));
                    }
                }
            } else {
                app.state.status = AppStatus::Stopped;
                app.state.pid = None;
                app.state.started_at = None;
                app.state.backoff_until = None;
                app.state.last_reason = Some("manual_stop".to_string());
                app.consecutive_restarts = 0;
                if let Some(schedule) = app.schedule {
                    app.state.next_scheduled_restart_at = next_occurrence(schedule, unix_now());
                }
                return Ok(());
            }

            if started.elapsed() >= timeout {
                break;
            }
            thread::sleep(Duration::from_millis(50));
        }

        send_signal_to_group(pid as i32, libc::SIGKILL)?;

        if let Some(mut child) = app.child.take() {
            let _ = child.wait();
        }

        app.state.status = AppStatus::Stopped;
        app.state.pid = None;
        app.state.started_at = None;
        app.state.backoff_until = None;
        app.state.last_reason = Some("manual_stop".to_string());
        app.consecutive_restarts = 0;
        if let Some(schedule) = app.schedule {
            app.state.next_scheduled_restart_at = next_occurrence(schedule, unix_now());
        }
        Ok(())
    }

    fn record_exit(app: &mut ManagedApp, status: ExitStatus, now_epoch: u64) {
        if let Some(started_at) = app.state.started_at {
            let runtime_secs = now_epoch.saturating_sub(started_at);
            if runtime_secs >= SUCCESS_AFTER_SECS {
                app.consecutive_restarts = 0;
                app.recent_restarts.clear();
            }
        }
        app.child = None;
        app.state.pid = None;
        app.state.started_at = None;
        app.state.last_exit_code = status.code();
        app.state.last_exit_signal = status.signal().map(signal_name);
        app.state.last_reason = if let Some(sig) = status.signal() {
            Some(format!("signal={}", signal_name(sig)))
        } else {
            Some(format!("exit_code={}", status.code().unwrap_or(1)))
        };
    }

    fn is_failure(status: ExitStatus) -> bool {
        status.code().unwrap_or(1) != 0
    }

    fn should_restart(policy: RestartPolicy, failure: bool) -> bool {
        match policy {
            RestartPolicy::Never => false,
            RestartPolicy::OnFailure => failure,
            RestartPolicy::Always => true,
        }
    }

    fn schedule_restart(app: &mut ManagedApp, now: Instant) -> bool {
        while let Some(front) = app.recent_restarts.front() {
            if now.duration_since(*front) > Duration::from_secs(RESTART_WINDOW_SECS) {
                app.recent_restarts.pop_front();
            } else {
                break;
            }
        }

        app.recent_restarts.push_back(now);
        app.state.restart_count = app.state.restart_count.saturating_add(1);

        if app.recent_restarts.len() > MAX_RESTARTS_IN_WINDOW {
            app.state.status = AppStatus::Errored;
            app.state.last_error =
                Some("crash loop protection triggered (>5 restarts in 60s)".to_string());
            app.state.last_reason = Some("max_restarts_exceeded".to_string());
            app.state.backoff_until = None;
            return false;
        }

        app.consecutive_restarts = app.consecutive_restarts.saturating_add(1);
        let exp = app.consecutive_restarts.saturating_sub(1).min(31);
        let backoff = 2_u64.saturating_pow(exp).min(30);
        app.state.status = AppStatus::Stopped;
        app.state.backoff_until = Some(unix_now().saturating_add(backoff));
        true
    }

    fn tail_file(path: &Path, tail: usize) -> Result<Vec<String>> {
        if !path.exists() {
            return Ok(Vec::new());
        }

        let file = File::open(path)?;
        let reader = BufReader::new(file);
        let mut lines = reader.lines().collect::<std::result::Result<Vec<_>, _>>()?;

        if lines.len() > tail {
            let drop_count = lines.len() - tail;
            lines.drain(0..drop_count);
        }

        Ok(lines)
    }

    fn refresh_all_schedule_targets(&mut self, now_epoch: u64) {
        let names: Vec<String> = self.apps.keys().cloned().collect();
        for name in names {
            self.refresh_schedule_target_for(&name, now_epoch);
        }
    }

    fn refresh_schedule_target_for(&mut self, name: &str, now_epoch: u64) {
        if let Some(app) = self.apps.get_mut(name) {
            app.state.next_scheduled_restart_at = app
                .schedule
                .and_then(|schedule| next_occurrence(schedule, now_epoch));
        }
    }
}

fn build_command(app: &AppSpec) -> Result<(PathBuf, Vec<String>)> {
    if !app.command.is_empty() {
        let exec = PathBuf::from(&app.command[0]);
        let args = app.command[1..].to_vec();
        return Ok((exec, args));
    }

    let cwd = PathBuf::from(&app.cwd);
    let venv = if Path::new(&app.venv).is_absolute() {
        PathBuf::from(&app.venv)
    } else {
        cwd.join(&app.venv)
    };
    let uvicorn = venv.join("bin/uvicorn");
    if uvicorn.exists() {
        let mut args = vec![app.entry.clone()];
        args.extend(app.args.clone());
        return Ok((uvicorn, args));
    }

    let python = venv.join("bin/python");
    let mut args = vec!["-m".to_string(), "uvicorn".to_string(), app.entry.clone()];
    args.extend(app.args.clone());
    Ok((python, args))
}

fn load_env_file(path: &Path) -> Result<HashMap<String, String>> {
    let payload = fs::read_to_string(path)?;
    let mut env = HashMap::new();

    for line in payload.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() {
            continue;
        }
        env.insert(key.to_string(), value.trim().to_string());
    }

    Ok(env)
}

fn build_app_env(app: &AppSpec, cwd: &Path) -> Result<HashMap<String, String>> {
    let mut env_map = HashMap::new();
    if let Some(env_file) = app.env_file.as_ref() {
        let env_path = if Path::new(env_file).is_absolute() {
            PathBuf::from(env_file)
        } else {
            cwd.join(env_file)
        };
        env_map.extend(load_env_file(&env_path)?);
    }
    for (k, v) in &app.env {
        env_map.insert(k.clone(), v.clone());
    }
    Ok(env_map)
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, bytes)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

fn send_signal_to_group(pid: i32, signal: i32) -> Result<()> {
    let rc = unsafe { libc::kill(-pid, signal) };
    if rc == -1 {
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::ESRCH) {
            return Ok(());
        }
        return Err(PyopsError::Supervisor(format!(
            "failed to send signal {} to pgid {}: {}",
            signal, pid, err
        )));
    }
    Ok(())
}

fn signal_from_name(name: &str) -> Option<i32> {
    match name {
        "SIGTERM" => Some(libc::SIGTERM),
        "SIGINT" => Some(libc::SIGINT),
        "SIGQUIT" => Some(libc::SIGQUIT),
        "SIGHUP" => Some(libc::SIGHUP),
        "SIGKILL" => Some(libc::SIGKILL),
        _ => None,
    }
}

fn signal_name(sig: i32) -> String {
    match sig {
        libc::SIGTERM => "SIGTERM".to_string(),
        libc::SIGINT => "SIGINT".to_string(),
        libc::SIGQUIT => "SIGQUIT".to_string(),
        libc::SIGHUP => "SIGHUP".to_string(),
        libc::SIGKILL => "SIGKILL".to_string(),
        libc::SIGSEGV => "SIGSEGV".to_string(),
        libc::SIGABRT => "SIGABRT".to_string(),
        _ => format!("SIG{}", sig),
    }
}

fn is_pid_alive(pid: i32) -> bool {
    let rc = unsafe { libc::kill(pid, 0) };
    if rc == 0 {
        return true;
    }

    let err = std::io::Error::last_os_error();
    matches!(err.raw_os_error(), Some(libc::EPERM))
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_else(|_| Duration::from_secs(0))
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn base_spec() -> AppSpec {
        AppSpec {
            name: "api".to_string(),
            cwd: "/tmp".to_string(),
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
    fn build_command_uses_command_mode_when_present() {
        let mut spec = base_spec();
        spec.command = vec![
            "python".to_string(),
            "-m".to_string(),
            "http.server".to_string(),
            "9000".to_string(),
        ];
        let (exec, args) = build_command(&spec).expect("build command");
        assert_eq!(exec, PathBuf::from("python"));
        assert_eq!(args, vec!["-m", "http.server", "9000"]);
    }

    #[test]
    fn build_command_uses_legacy_python_fallback() {
        let mut spec = base_spec();
        let unique = format!("pym2-supervisor-test-{}", std::process::id());
        let root = std::env::temp_dir().join(unique);
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join(".venv/bin")).expect("create dir");

        spec.cwd = root.to_str().expect("temp path to utf8").to_string();
        let (exec, args) = build_command(&spec).expect("build command");

        assert_eq!(exec, root.join(".venv/bin/python"));
        assert_eq!(
            args,
            vec![
                "-m".to_string(),
                "uvicorn".to_string(),
                "app.main:app".to_string(),
                "--port".to_string(),
                "8000".to_string()
            ]
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn load_env_file_parses_simple_lines() {
        let unique = format!("pym2-env-test-{}", std::process::id());
        let path = std::env::temp_dir().join(format!("{}.env", unique));
        fs::write(
            &path,
            "# comment\nA=1\nB =  two \ninvalid\n =skip\nEMPTY=\n",
        )
        .expect("write env file");

        let env = load_env_file(&path).expect("parse env file");
        assert_eq!(env.get("A").map(String::as_str), Some("1"));
        assert_eq!(env.get("B").map(String::as_str), Some("two"));
        assert_eq!(env.get("EMPTY").map(String::as_str), Some(""));
        assert!(!env.contains_key("invalid"));
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn build_app_env_prefers_inline_env_over_env_file() {
        let mut spec = base_spec();
        let unique = format!("pym2-env-merge-test-{}", std::process::id());
        let cwd = std::env::temp_dir().join(unique);
        let _ = fs::remove_dir_all(&cwd);
        fs::create_dir_all(&cwd).expect("create dir");

        let env_path = cwd.join("app.env");
        fs::write(&env_path, "A=file\nB=file\n").expect("write env file");

        spec.env_file = Some("app.env".to_string());
        spec.env.insert("B".to_string(), "inline".to_string());
        spec.env.insert("C".to_string(), "inline".to_string());

        let merged = build_app_env(&spec, &cwd).expect("merge env");
        assert_eq!(merged.get("A").map(String::as_str), Some("file"));
        assert_eq!(merged.get("B").map(String::as_str), Some("inline"));
        assert_eq!(merged.get("C").map(String::as_str), Some("inline"));

        let _ = fs::remove_dir_all(&cwd);
    }

    #[test]
    fn schedule_restart_blocks_after_limit() {
        let mut app = ManagedApp {
            spec: base_spec(),
            schedule: None,
            state: AppRuntimeState::default(),
            child: None,
            recent_restarts: VecDeque::new(),
            consecutive_restarts: 0,
        };

        let now = Instant::now();
        for _ in 0..MAX_RESTARTS_IN_WINDOW {
            assert!(Supervisor::schedule_restart(&mut app, now));
        }
        assert!(!Supervisor::schedule_restart(&mut app, now));
        assert_eq!(app.state.status, AppStatus::Errored);
        assert_eq!(
            app.state.last_reason.as_deref(),
            Some("max_restarts_exceeded")
        );
    }

    #[test]
    fn record_exit_resets_restart_counters_after_success_grace() {
        let mut app = ManagedApp {
            spec: base_spec(),
            schedule: None,
            state: AppRuntimeState {
                started_at: Some(100),
                ..AppRuntimeState::default()
            },
            child: None,
            recent_restarts: VecDeque::from([Instant::now()]),
            consecutive_restarts: 3,
        };

        let status = Command::new("sh")
            .arg("-c")
            .arg("exit 1")
            .status()
            .expect("status");
        Supervisor::record_exit(&mut app, status, 100 + SUCCESS_AFTER_SECS);

        assert_eq!(app.consecutive_restarts, 0);
        assert!(app.recent_restarts.is_empty());
        assert_eq!(app.state.last_reason.as_deref(), Some("exit_code=1"));
    }

    #[test]
    fn record_exit_sets_last_exit_signal() {
        let mut app = ManagedApp {
            spec: base_spec(),
            schedule: None,
            state: AppRuntimeState {
                started_at: Some(100),
                ..AppRuntimeState::default()
            },
            child: None,
            recent_restarts: VecDeque::new(),
            consecutive_restarts: 0,
        };

        let status = Command::new("sh")
            .arg("-c")
            .arg("kill -TERM $$")
            .status()
            .expect("status");
        Supervisor::record_exit(&mut app, status, 101);

        assert_eq!(app.state.last_exit_signal.as_deref(), Some("SIGTERM"));
        assert_eq!(app.state.last_reason.as_deref(), Some("signal=SIGTERM"));
    }
}
