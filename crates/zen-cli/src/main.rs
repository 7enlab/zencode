//! zen-cli binary entry point. All wiring lives in the `zen_cli` library crate.

use clap::Parser;
use zen_cli::Cli;

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    match zen_cli::run(cli).await {
        Ok(code) => std::process::exit(code),
        Err(err) => {
            eprintln!("Error: {}", err);
            std::process::exit(1);
        }
    }
}
