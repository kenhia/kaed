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
            println!("host: {}", resolved.host);
            println!("roots:");
            for r in &resolved.roots {
                let desc = r.description.as_deref().unwrap_or("");
                println!("  {:<20} {}  {}", r.name, r.path.display(), desc);
            }
            // The same answer `roots` gives an agent, for whoever is on the
            // host with a shell instead of an MCP client (korg #930).
            match &resolved.peers {
                None => println!(
                    "fleet: UNDECLARED — no [peers] table in this config, so nothing here \
                     says which hosts should run kaed"
                ),
                Some(peers) => {
                    println!("fleet:");
                    println!("  {:<20} {:<12} (this host)", resolved.host, "active");
                    let peer_tokens = config::resolve_peer_tokens(peers);
                    for p in peers {
                        let why = p
                            .reference
                            .as_deref()
                            .or(p.since.as_deref())
                            .map(|s| format!("({s}) "))
                            .unwrap_or_default();
                        // Routable = proxyable from here; which authors can
                        // be proxied is per-credential (PD-4).
                        let routing = match (&p.url, p.tokens.is_empty()) {
                            (None, _) => "no url — declaration only".to_string(),
                            (Some(url), true) => format!("url {url}, no peer tokens"),
                            (Some(url), false) => {
                                let authors: Vec<&str> = p
                                    .tokens
                                    .keys()
                                    .filter(|a| {
                                        peer_tokens.contains_key(&(p.host.clone(), (*a).clone()))
                                    })
                                    .map(String::as_str)
                                    .collect();
                                format!("url {url}, proxies for {authors:?}")
                            }
                        };
                        println!(
                            "  {:<20} {:<12} declared {why}{} — {routing}",
                            p.host,
                            p.status.as_str(),
                            p.note.as_deref().unwrap_or("")
                        );
                    }
                }
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
            println!("classified paths (served redacted, or refused if not dotenv-shaped):");
            for rule in resolved.classify.describe() {
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
