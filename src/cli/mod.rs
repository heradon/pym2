use crate::agent;
use crate::config::load_config_or_defaults_for_client;
use crate::error::{PyopsError, Result};
use crate::ipc::client::IpcClient;
use crate::model::{AgentEvent, AppSummary, IpcRequest, LogSource, StreamLogEvent};
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
    Tui,
    Events {
        #[arg(long, default_value_t = true)]
        follow: bool,
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

pub fn run() -> Result<()> {
    if !cfg!(target_os = "linux") {
        return Err(PyopsError::Config("pym2 is Linux-only".to_string()));
    }

    let cli = Cli::parse();

    match cli.command {
        Commands::Agent => agent::run_agent(),
        Commands::Tui => crate::tui::run(client_from_config()?),
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
        Commands::Agent | Commands::Tui => Ok(()),
    }
}

fn client_from_config() -> Result<IpcClient> {
    let cfg = load_config_or_defaults_for_client()?;
    let socket = crate::config::expand_tilde(&cfg.agent.socket)?;
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
    print_status_table(&apps);
    Ok(())
}

fn print_status_table(apps: &[AppSummary]) {
    println!(
        "{:<20} {:<10} {:<8} {:<8} {:<12}",
        "NAME", "STATUS", "PID", "REST", "ENTRY"
    );
    for app in apps {
        println!(
            "{:<20} {:<10} {:<8} {:<8} {:<12}",
            app.name,
            format!("{:?}", app.runtime.status),
            app.runtime
                .pid
                .map(|p| p.to_string())
                .unwrap_or_else(|| "-".to_string()),
            app.runtime.restart_count,
            truncate(&app.entry, 12),
        );
    }
}

fn truncate(input: &str, max: usize) -> String {
    if input.len() <= max {
        input.to_string()
    } else {
        format!("{}...", &input[..max.saturating_sub(3)])
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
