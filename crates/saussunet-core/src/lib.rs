//! `saussunet-core` — the library half of the SaussuNet workspace: config,
//! typed errors (`CoreError`), the SQLite store (WAL, vault-keyed APIs) and the
//! llm-wiki vault layer.
//!
//! Hard boundary (INFR-01, Anti-Pattern 1): this crate has ZERO HTTP/axum
//! dependencies so a future host (MoriNet, QUAL-05) can embed it without
//! swallowing a web server. Guarded by `scripts/check.sh`.

pub mod config;
pub mod error;
pub mod store;
pub mod vault;

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
