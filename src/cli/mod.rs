use crate::agent;
use crate::config::{
    default_config_path, expand_tilde, load_config_from, load_config_or_defaults_for_client,
    save_config_to,
};
use crate::error::{PyopsError, Result};
use crate::ipc::client::IpcClient;
use crate::model::{
    effective_command, AgentEvent, AppDetails, AppSpec, AppSummary, IpcRequest, LogSource,
    PingData, RestartPolicy, StreamLogEvent,
};
use crate::schedule::parse_restart_schedule;
use clap::{Parser, Subcommand, ValueEnum};
use std::collections::HashSet;
use std::fs::OpenOptions;

#[derive(Debug, Parser)]
#[command(
    name = "pym2",
    version,
    about = "Linux process manager for Python projects"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    Agent,
    Ping,
    Doctor,
    Start {
        name: String,
    },
    Stop {
        name: String,
    },
    Restart {
        name: String,
    },
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Show detailed information for one app.
    Inspect {
        name: String,
        #[arg(long)]
        json: bool,
    },
    Logs {
        name: String,
        #[arg(long, default_value_t = 200)]
        tail: usize,
        #[arg(short = 'f', long)]
        follow: bool,
        #[arg(long, value_enum, default_value_t = CliLogSource::Both)]
        source: CliLogSource,
    },
    #[cfg(feature = "tui")]
    Tui,
    Events {
        #[arg(long, default_value_t = true)]
        follow: bool,
    },
    /// Add a FastAPI/Uvicorn app to /etc/pym2/config.toml.
    AddFastapi {
        #[arg(long)]
        name: String,
        #[arg(long)]
        cwd: String,
        #[arg(long)]
        entry: String,
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        #[arg(long, default_value_t = 8000)]
        port: u16,
        #[arg(long, default_value = ".venv")]
        venv: String,
        #[arg(long)]
        env_file: Option<String>,
        #[arg(long)]
        restart_schedule: Option<String>,
        #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
        autostart: bool,
        #[arg(long, value_enum, default_value_t = CliRestartPolicy::OnFailure)]
        restart: CliRestartPolicy,
    },
    /// Add a generic command-based app to /etc/pym2/config.toml.
    AddCmd {
        #[arg(long)]
        name: String,
        #[arg(long)]
        cwd: String,
        #[arg(long)]
        command: String,
        #[arg(long)]
        env_file: Option<String>,
        #[arg(long)]
        restart_schedule: Option<String>,
        #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
        autostart: bool,
        #[arg(long, value_enum, default_value_t = CliRestartPolicy::OnFailure)]
        restart: CliRestartPolicy,
    },
    Config {
        #[command(subcommand)]
        command: ConfigCommands,
    },
}

#[derive(Debug, Subcommand)]
enum ConfigCommands {
    Lint,
}

#[derive(Debug, Copy, Clone, ValueEnum)]
enum CliLogSource {
    Stdout,
    Stderr,
    Both,
}

impl From<CliLogSource> for LogSource {
    fn from(value: CliLogSource) -> Self {
        match value {
            CliLogSource::Stdout => LogSource::Stdout,
            CliLogSource::Stderr => LogSource::Stderr,
            CliLogSource::Both => LogSource::Both,
        }
    }
}

#[derive(Debug, Copy, Clone, ValueEnum)]
enum CliRestartPolicy {
    Never,
    OnFailure,
    Always,
}

impl From<CliRestartPolicy> for RestartPolicy {
    fn from(value: CliRestartPolicy) -> Self {
        match value {
            CliRestartPolicy::Never => RestartPolicy::Never,
            CliRestartPolicy::OnFailure => RestartPolicy::OnFailure,
            CliRestartPolicy::Always => RestartPolicy::Always,
        }
    }
}

pub fn run() -> Result<()> {
    if !cfg!(target_os = "linux") {
        return Err(PyopsError::Config("pym2 is Linux-only".to_string()));
    }

    let cli = Cli::parse();

    match cli.command {
        Commands::Agent => agent::run_agent(),
        Commands::Ping => {
            let client = client_from_config()?;
            ping(&client)
        }
        Commands::Doctor => doctor(),
        #[cfg(feature = "tui")]
        Commands::Tui => crate::tui::run(client_from_config()?),
        Commands::AddFastapi {
            name,
            cwd,
            entry,
            host,
            port,
            venv,
            env_file,
            restart_schedule,
            autostart,
            restart,
        } => add_fastapi(
            name,
            cwd,
            entry,
            host,
            port,
            venv,
            env_file,
            restart_schedule,
            autostart,
            restart.into(),
        ),
        Commands::AddCmd {
            name,
            cwd,
            command,
            env_file,
            restart_schedule,
            autostart,
            restart,
        } => add_cmd(
            name,
            cwd,
            command,
            env_file,
            restart_schedule,
            autostart,
            restart.into(),
        ),
        Commands::Config { command } => match command {
            ConfigCommands::Lint => lint_config(),
        },
        command => {
            let client = client_from_config()?;
            run_client_command(command, &client)
        }
    }
}

fn run_client_command(command: Commands, client: &IpcClient) -> Result<()> {
    match command {
        Commands::Start { name } => simple_action(client, IpcRequest::Start { name }),
        Commands::Stop { name } => simple_action(client, IpcRequest::Stop { name }),
        Commands::Restart { name } => simple_action(client, IpcRequest::Restart { name }),
        Commands::Status { json } => status(client, json),
        Commands::Inspect { name, json } => inspect(client, name, json),
        Commands::Ping | Commands::Doctor => Ok(()),
        Commands::Logs {
            name,
            tail,
            follow,
            source,
        } => logs(client, name, tail, follow, source.into()),
        Commands::Events { follow } => events(client, follow),
        Commands::Config { .. }
        | Commands::Agent
        | Commands::AddFastapi { .. }
        | Commands::AddCmd { .. } => Ok(()),
        #[cfg(feature = "tui")]
        Commands::Tui => Ok(()),
    }
}

fn ping(client: &IpcClient) -> Result<()> {
    let info = client.ping()?;
    println!(
        "ok version={} agent_pid={}",
        info.version.trim(),
        info.agent_pid
    );
    Ok(())
}

fn doctor() -> Result<()> {
    let mut has_error = false;
    let os = std::env::consts::OS;
    if os == "linux" {
        println!("OS: ok (linux)");
    } else {
        println!("OS: fail ({}; pym2 is Linux-only)", os);
        has_error = true;
    }

    let cfg_path = default_config_path()?;
    println!("Config path: {}", cfg_path.display());

    let cfg = load_config_or_defaults_for_client()?;
    let socket = expand_tilde(&cfg.agent.socket)?;
    if socket.exists() {
        println!("Socket: ok ({})", socket.display());
    } else {
        println!(
            "Socket: warn ({}) missing; agent may be stopped",
            socket.display()
        );
    }

    let client = IpcClient::new(socket.clone());
    match client.ping() {
        Ok(PingData { version, agent_pid }) => {
            println!("Agent connect: ok (version={}, pid={})", version, agent_pid);
        }
        Err(err) => {
            println!("Agent connect: fail ({})", err);
            has_error = true;
        }
    }

    let state_dir = expand_tilde(&cfg.agent.state_dir)?;
    if ensure_dir_writable(&state_dir).is_ok() {
        println!("State dir writable: ok ({})", state_dir.display());
    } else {
        println!("State dir writable: warn ({})", state_dir.display());
    }

    if let Some(parent) = cfg_path.parent() {
        if ensure_dir_writable(parent).is_ok() {
            println!("Config dir writable: ok ({})", parent.display());
        } else {
            println!(
                "Config dir writable: warn ({}) (run as root to edit system config)",
                parent.display()
            );
        }
    }

    println!(
        "Web UI: {} on {}:{}",
        if cfg.agent.web.enabled {
            "enabled"
        } else {
            "disabled"
        },
        cfg.agent.web.host,
        cfg.agent.web.port
    );

    if has_error {
        return Err(PyopsError::Config(
            "doctor found critical issues".to_string(),
        ));
    }
    Ok(())
}

fn client_from_config() -> Result<IpcClient> {
    let cfg = load_config_or_defaults_for_client()?;
    let socket = expand_tilde(&cfg.agent.socket)?;
    Ok(IpcClient::new(socket))
}

fn simple_action(client: &IpcClient, req: IpcRequest) -> Result<()> {
    let resp = client.request(req)?;
    if !resp.ok {
        return Err(PyopsError::Ipc(
            resp.error.unwrap_or_else(|| "operation failed".to_string()),
        ));
    }

    if let Some(data) = resp.data {
        println!("{}", serde_json::to_string_pretty(&data)?);
    }

    Ok(())
}

fn status(client: &IpcClient, as_json: bool) -> Result<()> {
    let resp = client.request(IpcRequest::ListApps)?;
    if !resp.ok {
        return Err(PyopsError::Ipc(
            resp.error.unwrap_or_else(|| "status failed".to_string()),
        ));
    }

    let data = resp
        .data
        .ok_or_else(|| PyopsError::Ipc("status returned empty payload".to_string()))?;

    if as_json {
        println!("{}", serde_json::to_string_pretty(&data)?);
        return Ok(());
    }

    let apps_val = data
        .get("apps")
        .ok_or_else(|| PyopsError::Ipc("status payload missing 'apps'".to_string()))?
        .clone();

    let apps: Vec<AppSummary> = serde_json::from_value(apps_val)?;
    print_status(apps);
    Ok(())
}

fn inspect(client: &IpcClient, name: String, as_json: bool) -> Result<()> {
    let resp = client.request(IpcRequest::GetApp { name })?;
    if !resp.ok {
        return Err(PyopsError::Ipc(
            resp.error.unwrap_or_else(|| "inspect failed".to_string()),
        ));
    }

    let data = resp
        .data
        .ok_or_else(|| PyopsError::Ipc("inspect returned empty payload".to_string()))?;

    if as_json {
        println!("{}", serde_json::to_string_pretty(&data)?);
        return Ok(());
    }

    let app_val = data
        .get("app")
        .ok_or_else(|| PyopsError::Ipc("inspect payload missing 'app'".to_string()))?
        .clone();
    let app: AppDetails = serde_json::from_value(app_val)?;

    let command = effective_command(&app.spec).join(" ");
    let reason = app
        .runtime
        .last_reason
        .clone()
        .or(app.runtime.last_error.clone())
        .unwrap_or_else(|| "-".to_string());
    let (stdout_log, stderr_log) = log_paths_for_app(&app.spec.name)?;

    println!("name: {}", app.spec.name);
    println!("status: {:?}", app.runtime.status);
    println!(
        "pid: {}",
        app.runtime
            .pid
            .map(|p| p.to_string())
            .unwrap_or_else(|| "-".to_string())
    );
    println!("cwd: {}", app.spec.cwd);
    println!("command: {}", command.trim());
    println!("env_file: {}", app.spec.env_file.as_deref().unwrap_or("-"));
    println!(
        "last_start: {}",
        app.runtime
            .last_start_attempt_at
            .map(|v| v.to_string())
            .unwrap_or_else(|| "-".to_string())
    );
    println!(
        "started_at: {}",
        app.runtime
            .started_at
            .map(|v| v.to_string())
            .unwrap_or_else(|| "-".to_string())
    );
    println!(
        "last_exit: code={} signal={}",
        app.runtime
            .last_exit_code
            .map(|v| v.to_string())
            .unwrap_or_else(|| "-".to_string()),
        app.runtime.last_exit_signal.as_deref().unwrap_or("-")
    );
    println!(
        "restart: {:?} | count: {} | next_schedule: {}",
        app.spec.restart,
        app.runtime.restart_count,
        app.runtime
            .next_scheduled_restart_at
            .map(|v| v.to_string())
            .unwrap_or_else(|| "-".to_string())
    );
    println!("reason: {}", reason);
    println!("stdout_log: {}", stdout_log.display());
    println!("stderr_log: {}", stderr_log.display());
    Ok(())
}

fn print_status(mut apps: Vec<AppSummary>) {
    apps.sort_by(|a, b| a.name.cmp(&b.name));
    for app in apps {
        let reason = app
            .runtime
            .last_reason
            .clone()
            .or(app.runtime.last_error.clone())
            .unwrap_or_else(|| "-".to_string());
        let cmd = if app.command.is_empty() {
            if app.entry.is_empty() {
                "-".to_string()
            } else {
                format!("legacy:{}", app.entry)
            }
        } else {
            app.command.join(" ")
        };

        println!(
            "{} | {:?} | pid={} | restarts={} | reason={} | {}",
            app.name,
            app.runtime.status,
            app.runtime
                .pid
                .map(|p| p.to_string())
                .unwrap_or_else(|| "-".to_string()),
            app.runtime.restart_count,
            reason,
            cmd
        );
    }
}

fn logs(
    client: &IpcClient,
    name: String,
    tail: usize,
    follow: bool,
    source: LogSource,
) -> Result<()> {
    if follow {
        client.stream_logs(
            IpcRequest::StreamLogs {
                name,
                tail,
                source,
                follow_interval_ms: 400,
            },
            |event: StreamLogEvent| match event.source {
                LogSource::Stdout => println!("[OUT] {}", event.line),
                LogSource::Stderr => eprintln!("[ERR] {}", event.line),
                LogSource::Both => println!("{}", event.line),
            },
        )?;
        return Ok(());
    }

    let resp = client.request(IpcRequest::TailLogs { name, tail, source })?;
    if !resp.ok {
        return Err(PyopsError::Ipc(
            resp.error.unwrap_or_else(|| "logs failed".to_string()),
        ));
    }

    let data = resp
        .data
        .ok_or_else(|| PyopsError::Ipc("logs returned empty payload".to_string()))?;
    let lines: Vec<String> = serde_json::from_value(
        data.get("lines")
            .ok_or_else(|| PyopsError::Ipc("logs payload missing 'lines'".to_string()))?
            .clone(),
    )?;

    for line in lines {
        println!("{}", line);
    }

    Ok(())
}

fn events(client: &IpcClient, follow: bool) -> Result<()> {
    if !follow {
        return Ok(());
    }

    client.stream_events(|event: AgentEvent| {
        println!("{}", serde_json::to_string(&event).unwrap_or_default());
    })?;
    Ok(())
}

fn ensure_dir_writable(dir: &std::path::Path) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    let probe = dir.join(format!(".pym2-write-test-{}", std::process::id()));
    OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&probe)?;
    std::fs::remove_file(&probe)?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn add_fastapi(
    name: String,
    cwd: String,
    entry: String,
    host: String,
    port: u16,
    venv: String,
    env_file: Option<String>,
    restart_schedule: Option<String>,
    autostart: bool,
    restart: RestartPolicy,
) -> Result<()> {
    if venv.trim().is_empty() {
        return Err(PyopsError::Config(
            "--venv cannot be empty for add-fastapi".to_string(),
        ));
    }

    let command = build_fastapi_command(&venv, &entry, &host, port);

    let app = AppSpec {
        name,
        cwd,
        command,
        venv,
        entry,
        args: Vec::new(),
        autostart,
        restart,
        stop_signal: "SIGTERM".to_string(),
        kill_timeout_ms: 8_000,
        restart_schedule,
        env_file,
        env: std::collections::HashMap::new(),
    };

    add_app_to_config(app)
}

fn build_fastapi_command(venv: &str, entry: &str, host: &str, port: u16) -> Vec<String> {
    vec![
        format!("{}/bin/python", venv.trim_end_matches('/')),
        "-m".to_string(),
        "uvicorn".to_string(),
        entry.to_string(),
        "--host".to_string(),
        host.to_string(),
        "--port".to_string(),
        port.to_string(),
    ]
}

fn add_cmd(
    name: String,
    cwd: String,
    command: String,
    env_file: Option<String>,
    restart_schedule: Option<String>,
    autostart: bool,
    restart: RestartPolicy,
) -> Result<()> {
    let parts = parse_command_string(&command)?;
    if parts.is_empty() {
        return Err(PyopsError::Config(
            "--command must contain at least one executable".to_string(),
        ));
    }

    let app = AppSpec {
        name,
        cwd,
        command: parts,
        venv: String::new(),
        entry: String::new(),
        args: Vec::new(),
        autostart,
        restart,
        stop_signal: "SIGTERM".to_string(),
        kill_timeout_ms: 8_000,
        restart_schedule,
        env_file,
        env: std::collections::HashMap::new(),
    };

    add_app_to_config(app)
}

fn parse_command_string(command: &str) -> Result<Vec<String>> {
    shell_words::split(command)
        .map_err(|e| PyopsError::Config(format!("invalid --command value: {}", e)))
}

fn add_app_to_config(app: AppSpec) -> Result<()> {
    if let Some(schedule) = app.restart_schedule.as_ref() {
        parse_restart_schedule(schedule)?;
    }

    let path = default_config_path()?;
    let mut cfg = if path.exists() {
        load_config_from(&path)?
    } else {
        load_config_or_defaults_for_client()?
    };
    if cfg.apps.iter().any(|a| a.name == app.name) {
        return Err(PyopsError::Config(format!(
            "app '{}' already exists",
            app.name
        )));
    }
    cfg.apps.push(app);

    match save_config_to(&path, &cfg) {
        Ok(()) => {
            println!("saved {}", path.display());
            Ok(())
        }
        Err(PyopsError::Io(err)) if err.kind() == std::io::ErrorKind::PermissionDenied => {
            Err(PyopsError::Config(format!(
                "cannot write {} (permission denied). run as root or edit /etc/pym2/config.toml",
                path.display()
            )))
        }
        Err(err) => Err(err),
    }
}

fn lint_config() -> Result<()> {
    let path = default_config_path()?;
    let content = std::fs::read_to_string(&path).map_err(|e| {
        PyopsError::Config(format!("failed to read config {}: {}", path.display(), e))
    })?;
    let cfg: crate::model::ConfigFile = toml::from_str(&content).map_err(|e| {
        PyopsError::Config(format!("failed to parse config {}: {}", path.display(), e))
    })?;
    let mut errors = Vec::new();
    let mut names = HashSet::new();

    for app in &cfg.apps {
        if app.name.trim().is_empty() {
            errors.push("app name cannot be empty".to_string());
            continue;
        }
        if !names.insert(app.name.clone()) {
            errors.push(format!("duplicate app name '{}'", app.name));
        }
        if app.cwd.trim().is_empty() {
            errors.push(format!("app '{}' has empty cwd", app.name));
        }
        if app.command.is_empty() {
            if app.venv.trim().is_empty() || app.entry.trim().is_empty() {
                errors.push(format!(
                    "app '{}' missing command and legacy fields (need command[] or venv+entry)",
                    app.name
                ));
            }
        } else if app.command[0].trim().is_empty() {
            errors.push(format!("app '{}' command executable is empty", app.name));
        }
        if let Some(schedule) = app.restart_schedule.as_ref() {
            if let Err(err) = parse_restart_schedule(schedule) {
                errors.push(format!(
                    "app '{}' invalid restart_schedule '{}': {}",
                    app.name, schedule, err
                ));
            }
        }
    }

    if errors.is_empty() {
        println!("config lint OK: {}", path.display());
        return Ok(());
    }

    eprintln!("config lint FAILED: {}", path.display());
    for err in errors {
        eprintln!("- {}", err);
    }
    Err(PyopsError::Config("config lint failed".to_string()))
}

fn log_paths_for_app(name: &str) -> Result<(std::path::PathBuf, std::path::PathBuf)> {
    let cfg = load_config_or_defaults_for_client()?;
    let state_dir = expand_tilde(&cfg.agent.state_dir)?;
    let logs_dir = state_dir.join("logs");
    Ok((
        logs_dir.join(format!("{}.out.log", name)),
        logs_dir.join(format!("{}.err.log", name)),
    ))
}

#[cfg(test)]
mod tests {
    use super::{build_fastapi_command, parse_command_string};

    #[test]
    fn build_fastapi_command_uses_venv_python() {
        let cmd = build_fastapi_command(".venv", "app.main:app", "0.0.0.0", 8000);
        assert_eq!(cmd[0], ".venv/bin/python");
        assert_eq!(cmd[1], "-m");
        assert_eq!(cmd[2], "uvicorn");
        assert_eq!(cmd[3], "app.main:app");
    }

    #[test]
    fn parse_command_string_handles_quotes() {
        let parsed = parse_command_string("python -m http.server \"9000\"").expect("parse command");
        assert_eq!(
            parsed,
            vec![
                "python".to_string(),
                "-m".to_string(),
                "http.server".to_string(),
                "9000".to_string()
            ]
        );
    }
}
