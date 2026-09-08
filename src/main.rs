use anyhow::Result;
use changes::run;
use clap::Parser;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "changes", version, about = "Live git diff viewer")]
struct Cli {
    /// Path to a git repo or directory containing git repos
    #[arg(default_value = ".")]
    path: PathBuf,

    /// Color theme: "dark" (default) or "light". Also read from CHANGES_THEME.
    #[arg(long)]
    theme: Option<String>,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let theme_name = cli
        .theme
        .or_else(|| std::env::var("CHANGES_THEME").ok())
        .unwrap_or_else(|| "dark".to_string());
    changes::theme::init(changes::theme::ThemeKind::parse(&theme_name));
    let path = cli.path.canonicalize()?;
    run(path).await
}
