use crate::error::{PyopsError, Result};
use crate::model::{
    AppDetails, AppRuntimeState, AppSpec, AppStatus, AppSummary, ConfigFile, LogSource,
    RestartPolicy,
};
use std::collections::{HashMap, VecDeque};
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

struct ManagedApp {
    spec: AppSpec,
    state: AppRuntimeState,
    child: Option<Child>,
    recent_restarts: VecDeque<Instant>,
    consecutive_restarts: u32,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct PersistedRuntimeState {
    apps: HashMap<String, AppRuntimeState>,
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
            apps.insert(
                spec.name.clone(),
                ManagedApp {
                    spec,
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
        let names = self.apps.keys().cloned().collect::<Vec<_>>();
        let mut changed = false;

        for name in names {
            let mut needs_restart = false;
            if let Some(app) = self.apps.get_mut(&name) {
                if let Some(child) = app.child.as_mut() {
                    match child.try_wait() {
                        Ok(Some(status)) => {
                            Self::record_exit(app, status);
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
            apps: self.runtime_snapshot(),
        };
        let payload = serde_json::to_vec_pretty(&snapshot)?;
        fs::write(self.runtime_state_path(), payload)?;
        Ok(())
    }

    fn restore_runtime_state(&mut self) -> Result<()> {
        let path = self.runtime_state_path();
        if !path.exists() {
            return Ok(());
        }

        let payload = fs::read(&path)?;
        let persisted: PersistedRuntimeState = serde_json::from_slice(&payload)?;

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
        let venv = if Path::new(&app.spec.venv).is_absolute() {
            PathBuf::from(&app.spec.venv)
        } else {
            cwd.join(&app.spec.venv)
        };

        let uvicorn = venv.join("bin/uvicorn");
        let python = venv.join("bin/python");

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

        let mut cmd = if uvicorn.exists() {
            let mut c = Command::new(uvicorn);
            c.arg(&app.spec.entry);
            c
        } else {
            let mut c = Command::new(python);
            c.arg("-m").arg("uvicorn").arg(&app.spec.entry);
            c
        };

        cmd.args(&app.spec.args)
            .current_dir(&cwd)
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr));

        for (k, v) in &app.spec.env {
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
                app.child = Some(child);
                Ok(())
            }
            Err(err) => {
                app.state.status = AppStatus::Errored;
                app.state.last_error = Some(format!("spawn failed: {}", err));
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
                        Self::record_exit(app, status);
                        app.state.status = AppStatus::Stopped;
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
                app.consecutive_restarts = 0;
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
        app.consecutive_restarts = 0;
        Ok(())
    }

    fn record_exit(app: &mut ManagedApp, status: ExitStatus) {
        app.child = None;
        app.state.pid = None;
        app.state.started_at = None;
        app.state.last_exit_code = status.code();
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
            if now.duration_since(*front) > Duration::from_secs(300) {
                app.recent_restarts.pop_front();
            } else {
                break;
            }
        }

        app.recent_restarts.push_back(now);
        app.state.restart_count = app.state.restart_count.saturating_add(1);

        if app.recent_restarts.len() > 10 {
            app.state.status = AppStatus::Errored;
            app.state.last_error =
                Some("crash loop protection triggered (>10 restarts in 5m)".to_string());
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
