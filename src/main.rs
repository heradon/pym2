mod agent;
mod cli;
mod config;
mod error;
mod ipc;
mod model;
mod supervisor;
mod tui;

fn main() {
    if let Err(err) = cli::run() {
        eprintln!("error: {}", err);
        std::process::exit(1);
    }
}
