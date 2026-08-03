use clap::{Parser, Subcommand};
use kaed::config::{self, Config};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "kaed",
    version = kaed::version::FULL,
    about = "an editor whose only user is an AI agent"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the MCP server daemon
    Serve {
        /// Config file (default: ~/.config/kaed/config.toml)
        #[arg(long)]
        config: Option<PathBuf>,
    },
    /// Validate config and print resolved roots
    CheckConfig {
        #[arg(long)]
        config: Option<PathBuf>,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "kaed=info".into()),
        )
        .init();

    match Cli::parse().cmd {
        Cmd::Serve { config } => kaed::server::serve(load(config)?).await,
        Cmd::CheckConfig { config } => {
            let path = config.unwrap_or_else(Config::default_path);
            // First line, so one command answers both "is this host's
            // config sane" and "which build is asserting that".
            println!("kaed {}", kaed::version::FULL);
            println!("config: {}", path.display());
            let resolved = load(Some(path))?;
            println!("bind: {}", resolved.bind);
            println!("roots:");
            for r in &resolved.roots {
                let desc = r.description.as_deref().unwrap_or("");
                println!("  {:<12} {}  {}", r.name, r.path.display(), desc);
            }
            println!("identities:");
            for id in &resolved.identities {
                println!("  {} (token resolved)", id.author);
            }
            println!(
                "limits: max_read_bytes={} max_file_bytes={} search_max_results={}",
                resolved.limits.max_read_bytes,
                resolved.limits.max_file_bytes,
                resolved.limits.search_max_results
            );
            println!(
                "journal: {} (blob retention {} days)",
                resolved.journal_path.display(),
                resolved.journal_retention_days
            );
            println!("denied paths:");
            for rule in resolved.deny.describe() {
                println!("  {rule}");
            }
            Ok(())
        }
    }
}

fn load(path: Option<PathBuf>) -> anyhow::Result<config::Resolved> {
    let path = path.unwrap_or_else(Config::default_path);
    Config::load(&path)?.resolve(Some(&path))
}
