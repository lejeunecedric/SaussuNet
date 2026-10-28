//! SaussuNet binary (MoriNet main.rs skeleton, PATTERNS §2.7).
//!
//! Phase 1 operability = clap subcommands, no HTTP (planner decision,
//! SKELETON.md): `version`, `init` (config template), `init-vault`
//! (boot-and-verify); bare run = same boot as `init-vault` (ROADMAP SC1).

use std::path::PathBuf;

use anyhow::Context;
use clap::{Parser, Subcommand};

use saussunet_core::config::load_config_or_default;
use saussunet_core::error::CoreError;

#[derive(Parser)]
#[command(
    name = "saussunet",
    version,
    about = "Self-hosted llm-wiki vault server"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Print the version and exit.
    Version,
    /// Write the example config.toml (refuses to overwrite unless --force).
    Init {
        /// Output path for the config template.
        #[arg(long, default_value = "config.toml")]
        out: PathBuf,
        /// Overwrite an existing file.
        #[arg(long)]
        force: bool,
    },
    /// Boot: config → SQLite (WAL) + migrations → vault FS init → "vault ready".
    InitVault,
    /// Write a wiki page through the sole vault writer (validation + atomic +
    /// index/log updates). `--from` is the source markdown file.
    /// Exit codes: 0 written · 1 validation error · 2 conflict or refusal
    /// (recorded in pending_proposals, awaiting review).
    WritePage {
        /// Vault-relative page path, e.g. concepts/foo.md.
        #[arg(long)]
        path: String,
        /// Local markdown file with the page content (frontmatter included).
        #[arg(long)]
        from: PathBuf,
    },
    /// List pending conflicts (skipped/refused writes awaiting review, D-06).
    /// Exit 0 in both cases — an empty list is not an error state.
    Conflicts {
        /// Print the full JSON array including proposed_content.
        #[arg(long)]
        json: bool,
    },
    /// Rebuild page_registry from the vault filesystem (D-01: the DB is a
    /// disposable projection — deleting it costs zero content).
    Reindex,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Some(Command::Version) => {
            println!("saussunet {}", saussunet_core::version());
        }
        Some(Command::Init { out, force }) => {
            if out.exists() && !force {
                eprintln!(
                    "error: {} already exists; refusing to overwrite (pass --force)",
                    out.display()
                );
                std::process::exit(1);
            }
            std::fs::write(&out, saussunet_core::default_config())
                .with_context(|| format!("cannot write {}", out.display()))?;
            println!("Wrote example config to {}", out.display());
        }
        Some(Command::InitVault) | None => {
            // Bare run and `init-vault` share one boot path (SC1).
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?
                .block_on(boot())?;
        }
        Some(Command::WritePage { path, from }) => {
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?
                .block_on(write_page_cmd(&path, &from))?;
        }
        Some(Command::Conflicts { json }) => {
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?
                .block_on(conflicts_cmd(json))?;
        }
        Some(Command::Reindex) => {
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?
                .block_on(reindex_cmd())?;
        }
    }

    Ok(())
}

/// Boot wiring (PATTERNS §2.7 + plan 02-05 daemon): load config (defaults
/// when the file is absent) → T-01-01 config-placement check →
/// `saussunet_core::boot_open` (Store::connect → ensure_vault → Vault::init_fs)
/// → `vault ready: <path>` → provider factory from `[llm]` (D-02) → bounded
/// worker pool sized by `max_concurrency` (D-04) → block until ctrl_c, then
/// abort the pool. First long-lived mode — every other subcommand still exits.
async fn boot() -> anyhow::Result<()> {
    let config_path = std::path::Path::new("config.toml");
    let cfg = load_config_or_default(config_path)?;
    let rt = cfg.resolve()?;
    // Threat T-01-01: a config file living inside the vault would leak when
    // the vault is served/previewed later.
    if config_path.exists() {
        saussunet_core::assert_outside_vault(config_path, &rt.vault_root)?;
    }
    let (store, vault) = saussunet_core::boot_open(&rt).await?;
    println!("vault ready: {}", vault.root.display());

    // Daemon wiring: single active provider (D-02) + bounded pool (D-04).
    let store = std::sync::Arc::new(store);
    let provider = saussunet_core::llm::provider_from_config(&rt)?;
    let worker_cfg = saussunet_core::queue::WorkerConfig {
        max_concurrency: rt.llm_max_concurrency,
        ..Default::default()
    };
    let handles = saussunet_core::queue::spawn_workers(store, provider, rt.vault_id, worker_cfg);
    println!(
        "queue ready: provider={} workers={}",
        rt.llm_provider,
        handles.len()
    );

    // NO ANALOG precedent: first long-lived mode. Ctrl-C → abort workers.
    tokio::signal::ctrl_c().await?;
    for h in handles {
        h.abort();
    }
    println!("shutdown complete");
    Ok(())
}

/// `write-page` command: same boot wiring (config → store → vault), then
/// the sole writer. Exit codes: 0 written · 1 validation rejection
/// (frontmatter/vault path errors, cause on stderr) · 2 conflict or refusal
/// (write skipped/recorded in pending_proposals, awaiting review).
async fn write_page_cmd(rel_path: &str, from: &std::path::Path) -> anyhow::Result<()> {
    let config_path = std::path::Path::new("config.toml");
    let cfg = load_config_or_default(config_path)?;
    let rt = cfg.resolve()?;
    if config_path.exists() {
        saussunet_core::assert_outside_vault(config_path, &rt.vault_root)?;
    }
    let content =
        std::fs::read_to_string(from).with_context(|| format!("cannot read {}", from.display()))?;
    let (store, vault) = saussunet_core::boot_open(&rt).await?;
    match saussunet_core::vault::writer::write_page(&vault, &store, rel_path, &content).await {
        Ok(saussunet_core::vault::writer::WriteOutcome::Written { .. }) => {
            println!("wrote {rel_path}");
            Ok(())
        }
        Ok(saussunet_core::vault::writer::WriteOutcome::ConflictRecorded { reason, .. }) => {
            println!("conflict recorded: {rel_path} ({reason})");
            std::process::exit(2);
        }
        Ok(saussunet_core::vault::writer::WriteOutcome::Refused { reason }) => {
            println!("refused: {rel_path} ({reason})");
            std::process::exit(2);
        }
        Err(e @ (CoreError::Frontmatter(_) | CoreError::Vault(_))) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
        Err(e) => Err(e.into()),
    }
}

/// `conflicts` command: list pending_proposals rows for the configured
/// vault. Human-readable lines `<id>  <reason>  <path>  <created_at>`
/// (content never dumped), or the full JSON array with `--json`.
/// Exit 0 always — an empty list is not an error state.
async fn conflicts_cmd(json: bool) -> anyhow::Result<()> {
    let config_path = std::path::Path::new("config.toml");
    let cfg = load_config_or_default(config_path)?;
    let rt = cfg.resolve()?;
    if config_path.exists() {
        saussunet_core::assert_outside_vault(config_path, &rt.vault_root)?;
    }
    let (store, vault) = saussunet_core::boot_open(&rt).await?;
    if json {
        let rows = store.list_pending_proposals(vault.id).await?;
        println!(
            "{}",
            serde_json::to_string_pretty(&rows).context("cannot serialize proposals")?
        );
    } else {
        let rows = store.list_pending_summary(vault.id).await?;
        if rows.is_empty() {
            println!("no pending conflicts");
        }
        for r in rows {
            println!(
                "{}  {}  {}  {}",
                r.id, r.reason, r.target_path, r.created_at
            );
        }
    }
    Ok(())
}

/// `reindex` command: same boot wiring, then the D-01 rebuildability proof —
/// scan the vault and rebuild `page_registry` from scratch. Prints
/// `reindexed: N pages`; malformed pages print a `skipped` stderr line and
/// are excluded from the count (exit stays 0, T-04-02).
async fn reindex_cmd() -> anyhow::Result<()> {
    let config_path = std::path::Path::new("config.toml");
    let cfg = load_config_or_default(config_path)?;
    let rt = cfg.resolve()?;
    if config_path.exists() {
        saussunet_core::assert_outside_vault(config_path, &rt.vault_root)?;
    }
    let (store, vault) = saussunet_core::boot_open(&rt).await?;
    let n = saussunet_core::reindex::reindex(&vault, &store).await?;
    println!("reindexed: {n} pages");
    Ok(())
}
