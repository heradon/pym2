mod web;

use crate::config::{ensure_state_dirs, load_config};
use crate::error::{PyopsError, Result};
use crate::ipc::server::{read_line_json, write_line_json};
use crate::model::{
    AgentEvent, AgentEventKind, AppRuntimeState, AppStatus, IpcRequest, IpcResponse, LogSource,
    StreamLogEvent,
};
use crate::supervisor::Supervisor;
use serde_json::json;
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, ErrorKind, Seek, SeekFrom};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Duration;

static SHOULD_STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn handle_signal(_: i32) {
    SHOULD_STOP.store(true, Ordering::SeqCst);
}

pub fn run_agent() -> Result<()> {
    install_signal_handlers();

    let cfg = load_config()?;
    let (_, socket_path, _) = ensure_state_dirs(&cfg)?;
    let _guard = SocketGuard::new(socket_path.clone());

    if socket_path.exists() {
        std::fs::remove_file(&socket_path)?;
    }

    let listener = UnixListener::bind(&socket_path)?;
    std::fs::set_permissions(
        &socket_path,
        std::os::unix::fs::PermissionsExt::from_mode(0o600),
    )?;
    listener.set_nonblocking(true)?;

    let web_cfg = cfg.agent.web.clone();
    let (state_dir, _, logs_dir) = ensure_state_dirs(&cfg)?;
    let mut supervisor = Supervisor::new(cfg, state_dir, logs_dir);
    supervisor.start_autostart();

    let supervisor = Arc::new(Mutex::new(supervisor));
    let events = Arc::new(Mutex::new(EventBus::default()));

    let web_thread = if web_cfg.enabled {
        Some(web::spawn_server(web_cfg, Arc::clone(&supervisor)))
    } else {
        None
    };

    loop {
        if SHOULD_STOP.load(Ordering::SeqCst) {
            break;
        }

        match listener.accept() {
            Ok((stream, _)) => {
                let sup = Arc::clone(&supervisor);
                let events = Arc::clone(&events);
                thread::spawn(move || {
                    if let Err(err) = handle_client(stream, sup, events) {
                        eprintln!("client handler error: {}", err);
                    }
                });
            }
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(err) => return Err(PyopsError::Io(err)),
        }

        {
            let mut sup = supervisor
                .lock()
                .map_err(|_| PyopsError::Supervisor("supervisor lock poisoned".to_string()))?;
            let before = sup.runtime_snapshot();
            sup.tick();
            let after = sup.runtime_snapshot();
            let diff_events = collect_state_change_events(&before, &after);
            if !diff_events.is_empty() {
                let mut bus = events
                    .lock()
                    .map_err(|_| PyopsError::Supervisor("event bus lock poisoned".to_string()))?;
                for event in diff_events {
                    bus.broadcast(event);
                }
            }
        }

        thread::sleep(Duration::from_millis(200));
    }

    {
        let mut sup = supervisor
            .lock()
            .map_err(|_| PyopsError::Supervisor("supervisor lock poisoned".to_string()))?;
        sup.shutdown_all();
    }

    if let Some(handle) = web_thread {
        let _ = handle.join();
    }

    Ok(())
}

pub(crate) fn should_stop() -> bool {
    SHOULD_STOP.load(Ordering::SeqCst)
}

fn handle_client(
    mut stream: UnixStream,
    supervisor: Arc<Mutex<Supervisor>>,
    events: Arc<Mutex<EventBus>>,
) -> Result<()> {
    let req: IpcRequest = read_line_json(&stream)?;

    match req {
        IpcRequest::StreamLogs {
            name,
            tail,
            source,
            follow_interval_ms,
        } => stream_logs(
            &mut stream,
            &supervisor,
            &name,
            source,
            tail,
            follow_interval_ms,
        ),
        IpcRequest::WatchEvents => watch_events(&mut stream, &supervisor, &events),
        other => {
            let response = dispatch(other, &supervisor, &events)?;
            write_line_json(&mut stream, &response)
        }
    }
}

fn dispatch(
    req: IpcRequest,
    supervisor: &Arc<Mutex<Supervisor>>,
    events: &Arc<Mutex<EventBus>>,
) -> Result<IpcResponse> {
    let mut sup = supervisor
        .lock()
        .map_err(|_| PyopsError::Supervisor("supervisor lock poisoned".to_string()))?;

    let before = sup.runtime_snapshot();

    let resp = match req {
        IpcRequest::Start { name } => match sup.start(&name) {
            Ok(started) => IpcResponse::ok(json!({ "started": started })),
            Err(err) => IpcResponse::err(err.to_string()),
        },
        IpcRequest::Stop { name } => match sup.stop(&name) {
            Ok(stopped) => IpcResponse::ok(json!({ "stopped": stopped })),
            Err(err) => IpcResponse::err(err.to_string()),
        },
        IpcRequest::Restart { name } => match sup.restart(&name) {
            Ok(restarted) => IpcResponse::ok(json!({ "restarted": restarted })),
            Err(err) => IpcResponse::err(err.to_string()),
        },
        IpcRequest::ListApps => {
            let apps = sup.list_apps();
            IpcResponse::ok(json!({ "apps": apps }))
        }
        IpcRequest::GetApp { name } => match sup.get_app(&name) {
            Ok(app) => IpcResponse::ok(json!({ "app": app })),
            Err(err) => IpcResponse::err(err.to_string()),
        },
        IpcRequest::TailLogs { name, tail, source } => match sup.tail_logs(&name, source, tail) {
            Ok(lines) => IpcResponse::ok(json!({ "lines": lines })),
            Err(err) => IpcResponse::err(err.to_string()),
        },
        IpcRequest::WatchEvents => IpcResponse::err("invalid dispatch route"),
        IpcRequest::StreamLogs { .. } => IpcResponse::err("invalid dispatch route"),
    };

    let after = sup.runtime_snapshot();
    drop(sup);

    let diff_events = collect_state_change_events(&before, &after);
    if !diff_events.is_empty() {
        let mut bus = events
            .lock()
            .map_err(|_| PyopsError::Supervisor("event bus lock poisoned".to_string()))?;
        for event in diff_events {
            bus.broadcast(event);
        }
    }

    Ok(resp)
}

fn watch_events(
    stream: &mut UnixStream,
    supervisor: &Arc<Mutex<Supervisor>>,
    events: &Arc<Mutex<EventBus>>,
) -> Result<()> {
    let rx = {
        let mut bus = events
            .lock()
            .map_err(|_| PyopsError::Supervisor("event bus lock poisoned".to_string()))?;
        bus.subscribe()
    };

    {
        let sup = supervisor
            .lock()
            .map_err(|_| PyopsError::Supervisor("supervisor lock poisoned".to_string()))?;
        for (app, runtime) in sup.runtime_snapshot() {
            let event = AgentEvent {
                ts: unix_now(),
                kind: AgentEventKind::StateChanged,
                app,
                runtime,
                message: Some("initial_snapshot".to_string()),
            };
            if write_event(stream, event).is_err() {
                return Ok(());
            }
        }
    }

    loop {
        if SHOULD_STOP.load(Ordering::SeqCst) {
            return Ok(());
        }

        match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(event) => {
                if write_event(stream, event).is_err() {
                    return Ok(());
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
        }
    }
}

fn write_event(stream: &mut UnixStream, event: AgentEvent) -> Result<()> {
    match write_line_json(stream, &IpcResponse::ok(serde_json::to_value(event)?)) {
        Ok(()) => Ok(()),
        Err(PyopsError::Io(err))
            if matches!(
                err.kind(),
                ErrorKind::BrokenPipe | ErrorKind::ConnectionReset
            ) =>
        {
            Err(PyopsError::Ipc("watcher disconnected".to_string()))
        }
        Err(err) => Err(err),
    }
}

fn stream_logs(
    stream: &mut UnixStream,
    supervisor: &Arc<Mutex<Supervisor>>,
    name: &str,
    source: LogSource,
    tail: usize,
    follow_interval_ms: u64,
) -> Result<()> {
    let (out_path, err_path) = {
        let sup = supervisor
            .lock()
            .map_err(|_| PyopsError::Supervisor("supervisor lock poisoned".to_string()))?;
        match sup.log_paths(name) {
            Ok(paths) => paths,
            Err(err) => {
                write_line_json(stream, &IpcResponse::err(err.to_string()))?;
                return Ok(());
            }
        }
    };

    send_tail(stream, &out_path, LogSource::Stdout, tail, source)?;
    send_tail(stream, &err_path, LogSource::Stderr, tail, source)?;

    let mut out_offset = file_len(&out_path);
    let mut err_offset = file_len(&err_path);

    loop {
        if SHOULD_STOP.load(Ordering::SeqCst) {
            return Ok(());
        }

        if matches!(source, LogSource::Stdout | LogSource::Both) {
            out_offset = stream_new_lines(stream, &out_path, out_offset, LogSource::Stdout)?;
        }
        if matches!(source, LogSource::Stderr | LogSource::Both) {
            err_offset = stream_new_lines(stream, &err_path, err_offset, LogSource::Stderr)?;
        }

        thread::sleep(Duration::from_millis(follow_interval_ms.max(100)));
    }
}

fn send_tail(
    stream: &mut UnixStream,
    path: &std::path::Path,
    actual_source: LogSource,
    tail: usize,
    requested: LogSource,
) -> Result<()> {
    let should_send = matches!(requested, LogSource::Both)
        || matches!(
            (requested, actual_source),
            (LogSource::Stdout, LogSource::Stdout)
        )
        || matches!(
            (requested, actual_source),
            (LogSource::Stderr, LogSource::Stderr)
        );

    if !should_send || !path.exists() {
        return Ok(());
    }

    let file = File::open(path)?;
    let reader = BufReader::new(file);
    let mut lines = reader.lines().collect::<std::result::Result<Vec<_>, _>>()?;

    if lines.len() > tail {
        lines.drain(0..(lines.len() - tail));
    }

    for line in lines {
        let event = StreamLogEvent {
            source: actual_source,
            line,
        };
        write_line_json(stream, &IpcResponse::ok(serde_json::to_value(event)?))?;
    }

    Ok(())
}

fn file_len(path: &std::path::Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

fn stream_new_lines(
    stream: &mut UnixStream,
    path: &std::path::Path,
    mut offset: u64,
    source: LogSource,
) -> Result<u64> {
    if !path.exists() {
        return Ok(offset);
    }

    let file = File::open(path)?;
    let len = file.metadata()?.len();
    if len < offset {
        offset = 0;
    }

    if len == offset {
        return Ok(offset);
    }

    let mut reader = BufReader::new(file);
    reader.seek(SeekFrom::Start(offset))?;
    let mut line = String::new();

    loop {
        line.clear();
        let bytes = reader.read_line(&mut line)?;
        if bytes == 0 {
            break;
        }

        let event = StreamLogEvent {
            source,
            line: line.trim_end_matches('\n').to_string(),
        };
        write_line_json(stream, &IpcResponse::ok(serde_json::to_value(event)?))?;
    }

    Ok(reader.stream_position()?)
}

#[derive(Default)]
struct EventBus {
    subscribers: Vec<mpsc::Sender<AgentEvent>>,
}

impl EventBus {
    fn subscribe(&mut self) -> mpsc::Receiver<AgentEvent> {
        let (tx, rx) = mpsc::channel();
        self.subscribers.push(tx);
        rx
    }

    fn broadcast(&mut self, event: AgentEvent) {
        self.subscribers
            .retain(|sub| sub.send(event.clone()).is_ok());
    }
}

fn collect_state_change_events(
    before: &HashMap<String, AppRuntimeState>,
    after: &HashMap<String, AppRuntimeState>,
) -> Vec<AgentEvent> {
    let mut events = Vec::new();

    for (name, next) in after {
        let prev = before.get(name);
        if prev == Some(next) {
            continue;
        }

        let kind = if next.status == AppStatus::Errored {
            AgentEventKind::ProcessErrored
        } else if next.status == AppStatus::Running {
            AgentEventKind::ProcessStarted
        } else if prev.map(|p| p.status) == Some(AppStatus::Running)
            && next.status == AppStatus::Stopped
        {
            AgentEventKind::ProcessStopped
        } else {
            AgentEventKind::StateChanged
        };

        events.push(AgentEvent {
            ts: unix_now(),
            kind,
            app: name.clone(),
            runtime: next.clone(),
            message: next.last_error.clone(),
        });
    }

    events
}

fn install_signal_handlers() {
    unsafe {
        libc::signal(
            libc::SIGINT,
            handle_signal as *const () as libc::sighandler_t,
        );
        libc::signal(
            libc::SIGTERM,
            handle_signal as *const () as libc::sighandler_t,
        );
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_else(|_| Duration::from_secs(0))
        .as_secs()
}

struct SocketGuard {
    path: PathBuf,
}

impl SocketGuard {
    fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

impl Drop for SocketGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}
