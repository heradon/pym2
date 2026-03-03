use crate::error::{PyopsError, Result};
use crate::ipc::client::IpcClient;
use crate::model::{AgentEvent, AppSummary, IpcRequest, LogSource, StreamLogEvent};
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActivePane {
    List,
    Details,
    Logs,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InputMode {
    Normal,
    Filter,
}

#[derive(Debug)]
struct UiState {
    selected_app_index: usize,
    active_pane: ActivePane,
    filter_query: String,
    log_follow_enabled: bool,
    log_source: LogSource,
    show_logs: bool,
    input_mode: InputMode,
    esc_state: u8,
}

impl Default for UiState {
    fn default() -> Self {
        Self {
            selected_app_index: 0,
            active_pane: ActivePane::List,
            filter_query: String::new(),
            log_follow_enabled: true,
            log_source: LogSource::Both,
            show_logs: true,
            input_mode: InputMode::Normal,
            esc_state: 0,
        }
    }
}

struct LogStreamCtl {
    app: String,
    source: LogSource,
    stop: Arc<AtomicBool>,
    handle: std::thread::JoinHandle<()>,
}

pub fn run(client: IpcClient) -> Result<()> {
    let _raw = RawModeGuard::new()?;

    let (key_tx, key_rx) = mpsc::channel::<u8>();
    let (event_tx, event_rx) = mpsc::channel::<AgentEvent>();
    let (log_tx, log_rx) = mpsc::channel::<String>();

    std::thread::spawn(move || {
        let mut stdin = std::io::stdin();
        let mut buf = [0_u8; 1];
        loop {
            match stdin.read(&mut buf) {
                Ok(0) => std::thread::sleep(Duration::from_millis(5)),
                Ok(_) => {
                    if key_tx.send(buf[0]).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    {
        let event_client = client.clone();
        std::thread::spawn(move || {
            let _ = event_client.stream_events(|event| {
                let _ = event_tx.send(event);
            });
        });
    }

    let mut ui = UiState::default();
    let mut all_apps = fetch_apps(&client)?;
    let mut filtered_apps = apply_filter(&all_apps, &ui.filter_query);
    let mut log_lines = VecDeque::<String>::new();
    let mut log_stream: Option<LogStreamCtl> = None;
    let mut last_log_poll = Instant::now();
    let mut dirty = true;

    loop {
        while let Ok(event) = event_rx.try_recv() {
            apply_event(&mut all_apps, event);
            filtered_apps = apply_filter(&all_apps, &ui.filter_query);
            clamp_selection(&mut ui, filtered_apps.len());
            dirty = true;
        }

        while let Ok(line) = log_rx.try_recv() {
            push_log_line(&mut log_lines, line);
            dirty = true;
        }

        while let Ok(key) = key_rx.try_recv() {
            if handle_key(key, &mut ui, &mut filtered_apps, &all_apps)? {
                stop_log_stream(&mut log_stream);
                cleanup_screen()?;
                return Ok(());
            }

            if ui.input_mode == InputMode::Normal {
                if let Some(app) = selected_app(&filtered_apps, ui.selected_app_index) {
                    match key as char {
                        's' => {
                            let _ = client.request(IpcRequest::Start {
                                name: app.name.clone(),
                            });
                        }
                        't' => {
                            let _ = client.request(IpcRequest::Stop {
                                name: app.name.clone(),
                            });
                        }
                        'r' => {
                            let _ = client.request(IpcRequest::Restart {
                                name: app.name.clone(),
                            });
                        }
                        'l' => {
                            ui.show_logs = !ui.show_logs;
                            ui.active_pane = if ui.show_logs {
                                ActivePane::Logs
                            } else {
                                ActivePane::List
                            };
                        }
                        'f' => {
                            ui.log_follow_enabled = !ui.log_follow_enabled;
                        }
                        '\t' => {
                            ui.log_source = next_log_source(ui.log_source);
                        }
                        _ => {}
                    }
                }
            }

            dirty = true;
        }

        let selected_name =
            selected_app(&filtered_apps, ui.selected_app_index).map(|a| a.name.clone());

        if ui.show_logs && ui.log_follow_enabled {
            ensure_log_stream(
                &client,
                &mut log_stream,
                &log_tx,
                selected_name.clone(),
                ui.log_source,
                &mut log_lines,
            );
        } else {
            stop_log_stream(&mut log_stream);

            if ui.show_logs && last_log_poll.elapsed() >= Duration::from_millis(500) {
                if let Some(name) = selected_name {
                    log_lines = fetch_logs(&client, &name, ui.log_source, 25)?;
                    dirty = true;
                }
                last_log_poll = Instant::now();
            }
        }

        if dirty {
            render(&ui, &filtered_apps, &log_lines)?;
            dirty = false;
        }

        std::thread::sleep(Duration::from_millis(40));
    }
}

fn ensure_log_stream(
    client: &IpcClient,
    log_stream: &mut Option<LogStreamCtl>,
    log_tx: &mpsc::Sender<String>,
    selected_name: Option<String>,
    source: LogSource,
    log_lines: &mut VecDeque<String>,
) {
    let Some(app_name) = selected_name else {
        stop_log_stream(log_stream);
        return;
    };

    if let Some(existing) = log_stream.as_ref() {
        if existing.app == app_name && existing.source == source {
            return;
        }
    }

    stop_log_stream(log_stream);
    log_lines.clear();

    let stop = Arc::new(AtomicBool::new(false));
    let stop_for_thread = Arc::clone(&stop);
    let tx = log_tx.clone();
    let app_for_thread = app_name.clone();
    let client_for_thread = client.clone();

    let handle = std::thread::spawn(move || {
        let req = IpcRequest::StreamLogs {
            name: app_for_thread,
            tail: 60,
            source,
            follow_interval_ms: 250,
        };

        let _ = client_for_thread.stream_logs_until(
            req,
            || stop_for_thread.load(Ordering::Relaxed),
            |event: StreamLogEvent| {
                let prefix = match event.source {
                    LogSource::Stdout => "OUT",
                    LogSource::Stderr => "ERR",
                    LogSource::Both => "LOG",
                };
                let _ = tx.send(format!("[{}] {}", prefix, event.line));
            },
        );
    });

    *log_stream = Some(LogStreamCtl {
        app: app_name,
        source,
        stop,
        handle,
    });
}

fn stop_log_stream(log_stream: &mut Option<LogStreamCtl>) {
    if let Some(stream) = log_stream.take() {
        stream.stop.store(true, Ordering::Relaxed);
        let _ = stream.handle.join();
    }
}

fn push_log_line(lines: &mut VecDeque<String>, line: String) {
    lines.push_back(line);
    while lines.len() > 25 {
        lines.pop_front();
    }
}

fn next_log_source(source: LogSource) -> LogSource {
    match source {
        LogSource::Both => LogSource::Stdout,
        LogSource::Stdout => LogSource::Stderr,
        LogSource::Stderr => LogSource::Both,
    }
}

fn handle_key(
    key: u8,
    ui: &mut UiState,
    filtered_apps: &mut Vec<AppSummary>,
    all_apps: &[AppSummary],
) -> Result<bool> {
    if ui.input_mode == InputMode::Filter {
        match key {
            b'\n' | b'\r' => {
                ui.input_mode = InputMode::Normal;
                ui.selected_app_index = 0;
                *filtered_apps = apply_filter(all_apps, &ui.filter_query);
            }
            27 => {
                ui.input_mode = InputMode::Normal;
            }
            127 => {
                ui.filter_query.pop();
                *filtered_apps = apply_filter(all_apps, &ui.filter_query);
                clamp_selection(ui, filtered_apps.len());
            }
            byte if byte.is_ascii_graphic() || byte == b' ' => {
                ui.filter_query.push(byte as char);
                *filtered_apps = apply_filter(all_apps, &ui.filter_query);
                clamp_selection(ui, filtered_apps.len());
            }
            _ => {}
        }
        return Ok(false);
    }

    match key {
        b'q' => return Ok(true),
        b'/' => {
            ui.input_mode = InputMode::Filter;
            return Ok(false);
        }
        b'\n' | b'\r' => {
            ui.active_pane = match ui.active_pane {
                ActivePane::List => ActivePane::Details,
                ActivePane::Details => ActivePane::Logs,
                ActivePane::Logs => ActivePane::List,
            };
            return Ok(false);
        }
        b'k' => move_up(ui, filtered_apps.len()),
        b'j' => move_down(ui, filtered_apps.len()),
        27 => {
            ui.esc_state = 1;
            return Ok(false);
        }
        b'[' if ui.esc_state == 1 => {
            ui.esc_state = 2;
            return Ok(false);
        }
        b'A' if ui.esc_state == 2 => {
            move_up(ui, filtered_apps.len());
            ui.esc_state = 0;
            return Ok(false);
        }
        b'B' if ui.esc_state == 2 => {
            move_down(ui, filtered_apps.len());
            ui.esc_state = 0;
            return Ok(false);
        }
        _ => {
            ui.esc_state = 0;
        }
    }

    Ok(false)
}

fn move_up(ui: &mut UiState, len: usize) {
    if len == 0 {
        ui.selected_app_index = 0;
        return;
    }
    if ui.selected_app_index > 0 {
        ui.selected_app_index -= 1;
    }
}

fn move_down(ui: &mut UiState, len: usize) {
    if len == 0 {
        ui.selected_app_index = 0;
        return;
    }
    if ui.selected_app_index + 1 < len {
        ui.selected_app_index += 1;
    }
}

fn selected_app(apps: &[AppSummary], index: usize) -> Option<&AppSummary> {
    apps.get(index)
}

fn clamp_selection(ui: &mut UiState, len: usize) {
    if len == 0 {
        ui.selected_app_index = 0;
        return;
    }

    if ui.selected_app_index >= len {
        ui.selected_app_index = len - 1;
    }
}

fn apply_filter(apps: &[AppSummary], query: &str) -> Vec<AppSummary> {
    if query.trim().is_empty() {
        return apps.to_vec();
    }

    let q = query.to_lowercase();
    apps.iter()
        .filter(|a| a.name.to_lowercase().contains(&q) || a.entry.to_lowercase().contains(&q))
        .cloned()
        .collect()
}

fn fetch_apps(client: &IpcClient) -> Result<Vec<AppSummary>> {
    let resp = client.request(IpcRequest::ListApps)?;
    if !resp.ok {
        return Err(PyopsError::Ipc(
            resp.error
                .unwrap_or_else(|| "failed to fetch app list".to_string()),
        ));
    }

    let data = resp
        .data
        .ok_or_else(|| PyopsError::Ipc("list_apps returned empty payload".to_string()))?;

    let apps = data
        .get("apps")
        .ok_or_else(|| PyopsError::Ipc("list_apps payload missing 'apps'".to_string()))?;

    Ok(serde_json::from_value(apps.clone())?)
}

fn fetch_logs(
    client: &IpcClient,
    app: &str,
    source: LogSource,
    tail: usize,
) -> Result<VecDeque<String>> {
    let resp = client.request(IpcRequest::TailLogs {
        name: app.to_string(),
        tail,
        source,
    })?;

    if !resp.ok {
        return Ok(VecDeque::new());
    }

    let data = match resp.data {
        Some(data) => data,
        None => return Ok(VecDeque::new()),
    };

    let lines = match data.get("lines") {
        Some(lines) => lines.clone(),
        None => return Ok(VecDeque::new()),
    };

    let parsed: Vec<String> = serde_json::from_value(lines).unwrap_or_default();
    Ok(VecDeque::from(parsed))
}

fn apply_event(all_apps: &mut [AppSummary], event: AgentEvent) {
    if let Some(app) = all_apps.iter_mut().find(|a| a.name == event.app) {
        app.runtime = event.runtime;
    }
}

fn render(ui: &UiState, apps: &[AppSummary], logs: &VecDeque<String>) -> Result<()> {
    print!("\x1b[2J\x1b[H");
    println!(
        "pym2 tui | pane={:?} | keys: ↑/↓ or j/k, Enter pane, s start, t stop, r restart, l logs, f follow, Tab source, / filter, q quit",
        ui.active_pane
    );
    println!(
        "filter='{}' mode={:?} logs={} follow={} source={:?}",
        ui.filter_query, ui.input_mode, ui.show_logs, ui.log_follow_enabled, ui.log_source
    );
    println!();

    println!(
        "{:<2} {:<20} {:<10} {:<8} {:<8} {:<24}",
        "", "NAME", "STATUS", "PID", "REST", "ENTRY"
    );
    for (idx, app) in apps.iter().enumerate() {
        let marker = if idx == ui.selected_app_index {
            ">"
        } else {
            " "
        };
        println!(
            "{:<2} {:<20} {:<10} {:<8} {:<8} {:<24}",
            marker,
            app.name,
            format!("{:?}", app.runtime.status),
            app.runtime
                .pid
                .map(|p| p.to_string())
                .unwrap_or_else(|| "-".to_string()),
            app.runtime.restart_count,
            truncate(&app.entry, 24)
        );
    }

    println!();
    if let Some(app) = selected_app(apps, ui.selected_app_index) {
        println!("details: {}", app.name);
        println!("  cwd: {}", app.cwd);
        println!("  entry: {}", app.entry);
        println!("  status: {:?}", app.runtime.status);
        println!(
            "  pid: {}",
            app.runtime
                .pid
                .map(|p| p.to_string())
                .unwrap_or_else(|| "-".to_string())
        );
        println!("  restart_count: {}", app.runtime.restart_count);
    } else {
        println!("details: no app selected");
    }

    if ui.show_logs {
        println!();
        println!("logs:");
        for line in logs {
            println!("{}", line);
        }
    }

    std::io::stdout().flush()?;
    Ok(())
}

fn cleanup_screen() -> Result<()> {
    print!("\x1b[2J\x1b[H");
    std::io::stdout().flush()?;
    Ok(())
}

fn truncate(input: &str, max: usize) -> String {
    if input.len() <= max {
        input.to_string()
    } else {
        format!("{}...", &input[..max.saturating_sub(3)])
    }
}

struct RawModeGuard {
    fd: i32,
    original: libc::termios,
}

impl RawModeGuard {
    fn new() -> Result<Self> {
        let stdin = std::io::stdin();
        let fd = stdin.as_raw_fd();

        let mut term = std::mem::MaybeUninit::<libc::termios>::uninit();
        let rc = unsafe { libc::tcgetattr(fd, term.as_mut_ptr()) };
        if rc != 0 {
            return Err(PyopsError::Io(std::io::Error::last_os_error()));
        }
        let original = unsafe { term.assume_init() };
        let mut raw = original;

        raw.c_lflag &= !(libc::ICANON | libc::ECHO);
        raw.c_cc[libc::VMIN] = 0;
        raw.c_cc[libc::VTIME] = 1;

        let rc = unsafe { libc::tcsetattr(fd, libc::TCSANOW, &raw) };
        if rc != 0 {
            return Err(PyopsError::Io(std::io::Error::last_os_error()));
        }

        Ok(Self { fd, original })
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = unsafe { libc::tcsetattr(self.fd, libc::TCSANOW, &self.original) };
    }
}
