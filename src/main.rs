mod agent;
mod cli;
mod config;
mod error;
mod ipc;
mod model;
mod schedule;
mod supervisor;
#[cfg(feature = "tui")]
mod tui;
#[cfg(not(feature = "tui"))]
mod tui {
    use crate::error::{PyopsError, Result};
    use crate::ipc::client::IpcClient;

    pub fn run(_: IpcClient) -> Result<()> {
        Err(PyopsError::Config("tui disabled at build time".to_string()))
    }
}

fn main() {
    if let Err(err) = cli::run() {
        eprintln!("error: {}", err);
        std::process::exit(1);
    }
}
