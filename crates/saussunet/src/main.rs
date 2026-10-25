//! SaussuNet binary (MoriNet main.rs skeleton, PATTERNS §2.7).
//!
//! Phase 1 operability = clap subcommands, no HTTP (planner decision,
//! SKELETON.md): `version`, `init` (config template), `init-vault`
//! (boot-and-verify); bare run = same boot as `init-vault` (ROADMAP SC1).

use std::path::PathBuf;

use anyhow::Context;
use clap::{Parser, Subcommand};

use saussunet_core::config::{load_config_or_default, FileConfig};

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
    }

    Ok(())
}

/// Task 1 stub: resolve config (defaults when the file is absent) and report
/// the vault root. Task 2 replaces the body with the real boot wiring
/// (Store::connect → ensure_vault → Vault::init_fs).
async fn boot() -> anyhow::Result<()> {
    let cfg: FileConfig = load_config_or_default(std::path::Path::new("config.toml"))?;
    let rt = cfg.resolve();
    let root = if rt.vault_root.exists() {
        rt.vault_root.canonicalize()?
    } else {
        rt.vault_root
    };
    println!("vault ready: {}", root.display());
    Ok(())
}
