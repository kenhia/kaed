//! `kaed-redact-hook` — a Claude Code PostToolUse hook that seals secrets in
//! tool output before the model sees them (sprint 027, a spike: see
//! `sprints/027-shell-redaction-hook-spike/`). Reads the hook input on
//! stdin; prints `updatedToolOutput` when it redacted something, nothing
//! otherwise.
//!
//! **Fails open**: any error leaves the original output in place and says
//! so on stderr. A hook that broke every tool call on a parse error would
//! be uninstalled the same day.

use clap::Parser;
use kaed::hook;
use kaed::leak::Tiers;
use std::io::Read;
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    version,
    about = "Redact secrets from Claude Code tool output (PostToolUse hook)"
)]
struct Args {
    /// kaed journal holding the known-digest index (opened read-only).
    /// Defaults to kaed's own default location; absent → index-free tiers only.
    #[arg(long)]
    journal: Option<PathBuf>,
    /// Also apply the high-entropy heuristic (off by default: the 027
    /// measurement found it is mostly git SHAs and checksums).
    #[arg(long)]
    shaped: bool,
}

fn main() {
    let args = Args::parse();
    let mut raw = String::new();
    if let Err(e) = std::io::stdin().read_to_string(&mut raw) {
        eprintln!("kaed-redact-hook: reading stdin: {e}; output left unredacted");
        return;
    }
    let input: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("kaed-redact-hook: input is not JSON ({e}); output left unredacted");
            return;
        }
    };
    let journal = args
        .journal
        .unwrap_or_else(kaed::config::default_journal_path);
    let index = if journal.exists() {
        hook::load_index(&journal).unwrap_or_else(|e| {
            eprintln!("kaed-redact-hook: digest index unavailable ({e}); index-free tiers only");
            Default::default()
        })
    } else {
        Default::default()
    };
    let tiers = Tiers {
        secret_shaped: args.shaped,
        ..Tiers::PRECISE
    };
    if let Some(out) = hook::respond(&input, tiers, &index) {
        println!("{out}");
    }
}
