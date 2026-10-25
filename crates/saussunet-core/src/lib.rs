//! `saussunet-core` — the library half of the SaussuNet workspace: config,
//! typed errors (`CoreError`), the SQLite store (WAL, vault-keyed APIs) and the
//! llm-wiki vault layer.
//!
//! Hard boundary (INFR-01, Anti-Pattern 1): this crate has ZERO HTTP/axum
//! dependencies so a future host (MoriNet, QUAL-05) can embed it without
//! swallowing a web server. Guarded by `scripts/check.sh`.

pub mod config;
pub mod error;
pub mod reindex;
pub mod store;
pub mod vault;

use std::path::{Path, PathBuf};

use crate::config::RuntimeConfig;
use crate::error::{CoreError, Result};
use crate::vault::Vault;

pub use crate::store::Store;

/// Shared boot path (ROADMAP SC1): the bare run and `init-vault` both call
/// this, and `tests/init_vault.rs` drives the exact production sequence.
/// Returns the canonical vault root.
pub async fn boot(cfg: &RuntimeConfig) -> Result<PathBuf> {
    let (_store, vault) = boot_open(cfg).await?;
    Ok(vault.root)
}

/// Boot and hand back the live `(Store, Vault)` pair — the shape the
/// `write-page` CLI needs (same production sequence as [`boot`]).
pub async fn boot_open(cfg: &RuntimeConfig) -> Result<(Store, Vault)> {
    // Threat T-01-01: the DB must never live inside the vault — a vault is
    // served/previewed later (mdbook, MCP) and everything inside it leaks.
    assert_outside_vault(&cfg.db_path, &cfg.vault_root)?;

    let store = Store::connect(&cfg.db_path).await?;
    let row = store.ensure_vault(cfg.vault_id, &cfg.vault_name).await?;
    if row.id != cfg.vault_id {
        // ensure_vault pins the id; a mismatch means row/path drift (D-04) —
        // fail boot loudly rather than operate on the wrong directory.
        return Err(CoreError::Vault(format!(
            "vault row id {} != configured vault id {} — refusing to start (D-04)",
            row.id, cfg.vault_id
        )));
    }
    // vault_root is always data/vaults/<db row id> (D-04): RuntimeConfig keys
    // it by the configured id and the check above proves they are equal.
    let vault = Vault::new(row.id, row.name.clone(), cfg.vault_root.clone());
    vault.init_fs().await?;
    let root = vault.root.canonicalize().map_err(|e| CoreError::Io {
        path: vault.root.clone(),
        source: e,
    })?;
    let vault = Vault::new(row.id, row.name, root);
    Ok((store, vault))
}

/// Fail with an actionable error if `path` resolves inside `vault_root`
/// (threat T-01-01). Used for `db_path` in [`boot`] and for the config file
/// path in the binary.
pub fn assert_outside_vault(path: &Path, vault_root: &Path) -> Result<()> {
    let p = absolutize(path);
    let v = absolutize(vault_root);
    if p.starts_with(&v) {
        return Err(CoreError::Vault(format!(
            "refusing to start: {} resolves inside vault root {} — a vault is served/previewed \
             later and everything inside it leaks; move [storage].data_dir (and config) \
             outside the vault directory (T-01-01)",
            path.display(),
            vault_root.display()
        )));
    }
    Ok(())
}

/// Best-effort absolute, symlink-resolved form of `path` for containment
/// checks: canonicalize when possible; otherwise canonicalize the nearest
/// existing ancestor and re-append the missing tail.
fn absolutize(path: &Path) -> PathBuf {
    let abs = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    let mut cur = abs.clone();
    loop {
        if let Ok(real) = cur.canonicalize() {
            let mut out = real;
            for name in tail.iter().rev() {
                out.push(name);
            }
            return out;
        }
        let parent = cur.parent().map(Path::to_path_buf);
        match (cur.file_name(), parent) {
            (Some(name), Some(parent)) => {
                tail.push(name.to_os_string());
                cur = parent;
            }
            _ => return abs,
        }
    }
}

#[cfg(test)]
mod boot_tests {
    use super::assert_outside_vault;

    #[test]
    fn db_inside_vault_is_rejected() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let vault_root = tmp.path().join("vaults/1");
        std::fs::create_dir_all(&vault_root).unwrap();
        let db_inside = vault_root.join("saussunet.db");
        let err = assert_outside_vault(&db_inside, &vault_root)
            .expect_err("db inside vault root must fail (T-01-01)");
        assert!(err.to_string().contains("T-01-01"));
        // The normal layout passes.
        assert_outside_vault(&tmp.path().join("saussunet.db"), &vault_root).unwrap();
    }
}

/// Crate version, surfaced by `saussunet version`.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Example `config.toml` shipped by `saussunet init` (MoriNet
/// `lib.rs::default_config()` idiom). A unit test asserts the committed root
/// `config.toml` is byte-identical to this string.
pub fn default_config() -> &'static str {
    r#"# SaussuNet configuration (template written by `saussunet init`).

[server]
# Phase 1 opens no listener; the default is pinned to localhost (INFR-02, T-01-02).
bind = "127.0.0.1:8080"

[storage]
# Data root: holds saussunet.db and vaults/<id>/. Must never live inside a
# vault directory (threat T-01-01 — a vault is served/previewed later).
data_dir = "./data"

[vault]
# The single v1 vault (D-04): filesystem path is <data_dir>/vaults/<id>/.
id = 1
name = "main"
"#
}

#[cfg(test)]
mod tests {
    use super::default_config;

    #[test]
    fn default_config_parses_as_valid_toml() {
        let parsed: crate::config::FileConfig =
            toml::from_str(default_config()).expect("default_config() must parse as FileConfig");
        assert_eq!(parsed.server.bind, "127.0.0.1:8080");
    }

    #[test]
    fn committed_root_config_matches_default_config() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|p| p.parent())
            .expect("crate lives under <root>/crates/<name>")
            .join("config.toml");
        let committed = std::fs::read_to_string(&root)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", root.display()));
        assert_eq!(committed, default_config());
    }
}
