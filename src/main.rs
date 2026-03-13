use anyhow::Result;
use changes::runtime;
use clap::Parser;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "changes", version, about = "Live git diff viewer")]
struct Cli {
    /// Path to a git repo or directory containing git repos
    #[arg(default_value = ".")]
    path: PathBuf,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let path = cli.path.canonicalize()?;
    runtime::run(path).await
}
