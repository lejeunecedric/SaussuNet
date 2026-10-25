//! Two-stage config: raw `FileConfig` (TOML) → `RuntimeConfig` (fully
//! resolved paths), following the MoriNet `src/config.rs` pattern
//! (PATTERNS §2.4). Every section parses when absent.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::{CoreError, Result};

#[derive(Debug, Clone, Deserialize, Default)]
pub struct FileConfig {
    #[serde(default)]
    pub server: ServerSection,
    #[serde(default)]
    pub storage: StorageSection,
    #[serde(default)]
    pub vault: VaultSection,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerSection {
    /// Phase 1 opens NO listener; the localhost default is pinned here so the
    /// Phase 5 UI cannot accidentally inherit 0.0.0.0 (T-01-02).
    #[serde(default = "default_bind")]
    pub bind: String,
}

// Manual Default impls: a derived Default would return empty strings and
// bypass the `#[serde(default = "...")]` fns when a whole section is absent.

#[derive(Debug, Clone, Deserialize)]
pub struct StorageSection {
    #[serde(default = "default_data_dir")]
    pub data_dir: String,
}

/// The single v1 vault (D-04): config, schema, paths and store APIs all carry
/// the id even though exactly one vault ships.
#[derive(Debug, Clone, Deserialize)]
pub struct VaultSection {
    #[serde(default = "default_vault_id")]
    pub id: i64,
    #[serde(default = "default_vault_name")]
    pub name: String,
}

impl Default for ServerSection {
    fn default() -> Self {
        Self {
            bind: default_bind(),
        }
    }
}

impl Default for StorageSection {
    fn default() -> Self {
        Self {
            data_dir: default_data_dir(),
        }
    }
}

impl Default for VaultSection {
    fn default() -> Self {
        Self {
            id: default_vault_id(),
            name: default_vault_name(),
        }
    }
}

fn default_bind() -> String {
    "127.0.0.1:8080".to_string()
}

fn default_data_dir() -> String {
    "./data".to_string()
}

fn default_vault_id() -> i64 {
    1
}

fn default_vault_name() -> String {
    "main".to_string()
}

/// Fully-resolved runtime configuration (D-03, D-04).
#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    /// `data/saussunet.db` — one DB for all vaults (D-03).
    pub db_path: PathBuf,
    /// `data/vaults/<vault_id>/` — paths keyed by vault id (D-04).
    pub vault_root: PathBuf,
    pub bind: String,
    pub vault_id: i64,
    pub vault_name: String,
}

impl FileConfig {
    /// Resolve file values into final runtime paths (D-03, D-04).
    pub fn resolve(&self) -> RuntimeConfig {
        let data_dir = PathBuf::from(&self.storage.data_dir);
        RuntimeConfig {
            db_path: data_dir.join("saussunet.db"),
            vault_root: data_dir.join("vaults").join(self.vault.id.to_string()),
            bind: self.server.bind.clone(),
            vault_id: self.vault.id,
            vault_name: self.vault.name.clone(),
        }
    }
}

/// Load and parse the TOML config file (path kept in the error message).
pub fn load_config(path: &Path) -> Result<FileConfig> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| CoreError::Config(format!("cannot read config {}: {e}", path.display())))?;
    let cfg: FileConfig = toml::from_str(&raw)
        .map_err(|e| CoreError::Config(format!("cannot parse config {}: {e}", path.display())))?;
    Ok(cfg)
}

/// Load the config file, falling back to defaults when it is absent (used by
/// the bare-run boot path so a fresh checkout works with zero setup).
pub fn load_config_or_default(path: &Path) -> Result<FileConfig> {
    if path.exists() {
        load_config(path)
    } else {
        Ok(FileConfig::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_server_section_alone_parses_with_defaults() {
        // MoriNet config.rs:172-181 idiom: a bare `[server]` must load.
        let cfg: FileConfig = toml::from_str("[server]\n").expect("bare [server] must parse");
        assert_eq!(cfg.server.bind, "127.0.0.1:8080");
        assert_eq!(cfg.storage.data_dir, "./data");
        assert_eq!(cfg.vault.id, 1);
        assert_eq!(cfg.vault.name, "main");
    }

    #[test]
    fn empty_document_parses_with_defaults() {
        let cfg: FileConfig = toml::from_str("").expect("empty TOML must parse");
        assert_eq!(cfg.server.bind, "127.0.0.1:8080");
    }

    #[test]
    fn default_bind_is_localhost() {
        assert_eq!(FileConfig::default().server.bind, "127.0.0.1:8080");
    }

    #[test]
    fn resolved_vault_root_is_keyed_by_vault_id() {
        let rt = FileConfig::default().resolve();
        assert!(
            rt.vault_root.ends_with(Path::new("vaults/1")),
            "vault_root {:?} must end with vaults/1 (D-04)",
            rt.vault_root
        );
        assert!(rt.db_path.ends_with("saussunet.db"));
        assert_eq!(rt.vault_id, 1);
        assert_eq!(rt.vault_name, "main");
    }

    #[test]
    fn custom_vault_id_changes_path_not_layout() {
        let cfg: FileConfig =
            toml::from_str("[vault]\nid = 7\nname = \"research\"\n").expect("parses");
        let rt = cfg.resolve();
        assert!(rt.vault_root.ends_with(Path::new("vaults/7")));
        assert_eq!(rt.vault_name, "research");
    }
}
