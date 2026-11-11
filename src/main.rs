use clap::Parser;
use fetchrix::cli::Cli;

#[tokio::main]
async fn main() -> miette::Result<()> {
    fetchrix::run(&Cli::parse()).await?;
    Ok(())
}
