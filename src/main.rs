use clap::Parser;
use fetchrix::cli::Cli;

fn main() -> miette::Result<()> {
    fetchrix::run(&Cli::parse())?;
    Ok(())
}
