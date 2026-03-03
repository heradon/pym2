use crate::agent;
use crate::config::{
    default_config_path, expand_tilde, load_config_from, load_config_or_defaults_for_client,
    save_config_to,
};
use crate::error::{PyopsError, Result};
use crate::ipc::client::IpcClient;
use crate::model::{
    AgentEvent, AppSpec, AppSummary, IpcRequest, LogSource, RestartPolicy, StreamLogEvent,
};
use clap::{Parser, Subcommand, ValueEnum};

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
    Logs {
        name: String,
        #[arg(long, default_value_t = 200)]
        tail: usize,
        #[arg(long)]
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
        #[arg(long, default_value_t = true)]
        autostart: bool,
        #[arg(long, value_enum, default_value_t = CliRestartPolicy::OnFailure)]
        restart: CliRestartPolicy,
    },
    AddCmd {
        #[arg(long)]
        name: String,
        #[arg(long)]
        cwd: String,
        #[arg(long)]
        command: String,
        #[arg(long)]
        env_file: Option<String>,
        #[arg(long, default_value_t = true)]
        autostart: bool,
        #[arg(long, value_enum, default_value_t = CliRestartPolicy::OnFailure)]
        restart: CliRestartPolicy,
    },
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
            autostart,
            restart.into(),
        ),
        Commands::AddCmd {
            name,
            cwd,
            command,
            env_file,
            autostart,
            restart,
        } => add_cmd(name, cwd, command, env_file, autostart, restart.into()),
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
        Commands::Logs {
            name,
            tail,
            follow,
            source,
        } => logs(client, name, tail, follow, source.into()),
        Commands::Events { follow } => events(client, follow),
        Commands::Agent | Commands::AddFastapi { .. } | Commands::AddCmd { .. } => Ok(()),
        #[cfg(feature = "tui")]
        Commands::Tui => Ok(()),
    }
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

fn print_status(mut apps: Vec<AppSummary>) {
    apps.sort_by(|a, b| a.name.cmp(&b.name));
    for app in apps {
        let reason = app
            .runtime
            .last_reason
            .clone()
            .or(app.runtime.last_error.clone())
            .unwrap_or_else(|| "-".to_string());
        let signal = app.runtime.last_exit_signal.as_deref().unwrap_or("-");
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
            "{} | {:?} | pid={} | restarts={} | reason={} | signal={} | {}",
            app.name,
            app.runtime.status,
            app.runtime
                .pid
                .map(|p| p.to_string())
                .unwrap_or_else(|| "-".to_string()),
            app.runtime.restart_count,
            reason,
            signal,
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

fn add_fastapi(
    name: String,
    cwd: String,
    entry: String,
    host: String,
    port: u16,
    venv: String,
    env_file: Option<String>,
    autostart: bool,
    restart: RestartPolicy,
) -> Result<()> {
    let mut command = vec![
        "python".to_string(),
        "-m".to_string(),
        "uvicorn".to_string(),
    ];
    command.push(entry.clone());
    command.push("--host".to_string());
    command.push(host);
    command.push("--port".to_string());
    command.push(port.to_string());

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
        restart_schedule: None,
        env_file,
        env: std::collections::HashMap::new(),
    };

    add_app_to_config(app)
}

fn add_cmd(
    name: String,
    cwd: String,
    command: String,
    env_file: Option<String>,
    autostart: bool,
    restart: RestartPolicy,
) -> Result<()> {
    let parts = shell_words::split(&command)
        .map_err(|e| PyopsError::Config(format!("invalid --command value: {}", e)))?;
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
        restart_schedule: None,
        env_file,
        env: std::collections::HashMap::new(),
    };

    add_app_to_config(app)
}

fn add_app_to_config(app: AppSpec) -> Result<()> {
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
