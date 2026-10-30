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
    #[serde(default)]
    pub llm: LlmSection,
    #[serde(default)]
    pub pipeline: PipelineSection,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerSection {
    /// Phase 1 opens NO listener; the localhost default is pinned here so the
    /// Phase 5 UI cannot accidentally inherit 0.0.0.0 (T-01-02).
    #[serde(default = "default_bind")]
    pub bind: String,
    /// D-13: every HTTP route is nested under this base path from day one.
    /// Normalized at resolve time (leading `/`, no trailing `/`).
    #[serde(default = "default_base_path")]
    pub base_path: String,
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

/// A secret string that never prints its value: `Debug` renders
/// `SecretString("***")` so `{:?}` of any containing struct is log-safe (D-03).
#[derive(Clone, Deserialize)]
pub struct SecretString(String);

impl SecretString {
    pub fn new(value: String) -> Self {
        Self(value)
    }

    /// The only sanctioned way to read the secret — call sites must be
    /// deliberate (never a blanket `{:?}`).
    pub fn reveal(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for SecretString {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("SecretString").field(&"***").finish()
    }
}

/// The single active LLM provider (D-02: one `[llm]` section, no registry).
#[derive(Debug, Clone, Deserialize)]
pub struct LlmSection {
    /// `"mock"` (default — zero secrets, deterministic) | `"openai-compatible"`.
    #[serde(default = "default_llm_provider")]
    pub provider: String,
    #[serde(default = "default_llm_base_url")]
    pub base_url: String,
    #[serde(default = "default_llm_model")]
    pub model: String,
    /// Gitignored file value; `SAUSSUNET_LLM__API_KEY` env wins at resolve() (D-03).
    #[serde(default)]
    pub api_key: Option<SecretString>,
    /// Worker-pool bound; default 1 is safe for a local iGPU LLM (D-04).
    #[serde(default = "default_max_concurrency")]
    pub max_concurrency: usize,
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
}

/// Extraction-pipeline thresholds (D-14). Every field has a sane default so
/// the section may be absent entirely.
#[derive(Debug, Clone, Deserialize)]
pub struct PipelineSection {
    /// D-14: minimum distinct sources before a page may be created.
    #[serde(default = "default_min_sources")]
    pub min_sources: u32,
    /// Upload size cap for the future PDF route (25 MiB default).
    #[serde(default = "default_max_upload_bytes")]
    pub max_upload_bytes: u64,
    /// Fetch size cap for URL ingestion (5 MiB default).
    #[serde(default = "default_max_fetch_bytes")]
    pub max_fetch_bytes: u64,
}

impl Default for ServerSection {
    fn default() -> Self {
        Self {
            bind: default_bind(),
            base_path: default_base_path(),
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

impl Default for LlmSection {
    fn default() -> Self {
        Self {
            provider: default_llm_provider(),
            base_url: default_llm_base_url(),
            model: default_llm_model(),
            api_key: None,
            max_concurrency: default_max_concurrency(),
            timeout_secs: default_timeout_secs(),
        }
    }
}

impl Default for PipelineSection {
    fn default() -> Self {
        Self {
            min_sources: default_min_sources(),
            max_upload_bytes: default_max_upload_bytes(),
            max_fetch_bytes: default_max_fetch_bytes(),
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

fn default_llm_provider() -> String {
    "mock".to_string()
}

fn default_llm_base_url() -> String {
    "https://api.openai.com/v1".to_string()
}

fn default_llm_model() -> String {
    String::new()
}

fn default_max_concurrency() -> usize {
    1
}

fn default_timeout_secs() -> u64 {
    300
}

fn default_base_path() -> String {
    "/saussunet".to_string()
}

fn default_min_sources() -> u32 {
    2
}

fn default_max_upload_bytes() -> u64 {
    26_214_400
}

fn default_max_fetch_bytes() -> u64 {
    5_242_880
}

/// D-13: normalize a configured base path to `/<segments>` — leading slash,
/// no trailing slash. An empty/blank result falls back to the default.
fn normalize_base_path(s: &str) -> String {
    let trimmed = s.trim().trim_matches('/');
    if trimmed.is_empty() {
        return default_base_path();
    }
    format!("/{trimmed}")
}

/// Env var that overrides `[llm].api_key` (D-03).
const ENV_LLM_API_KEY: &str = "SAUSSUNET_LLM__API_KEY";

/// Fully-resolved runtime configuration (D-03, D-04).
#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    /// `data/saussunet.db` — one DB for all vaults (D-03).
    pub db_path: PathBuf,
    /// `data/vaults/<vault_id>/` — paths keyed by vault id (D-04).
    pub vault_root: PathBuf,
    pub bind: String,
    /// D-13: normalized base path every HTTP route nests under (e.g. `/saussunet`).
    pub base_path: String,
    pub vault_id: i64,
    pub vault_name: String,
    /// Resolved `[llm]` section (D-02/D-03/D-04). `Debug` is safe: the api_key
    /// renders as `SecretString("***")`.
    pub llm_provider: String,
    pub llm_base_url: String,
    pub llm_model: String,
    pub llm_api_key: Option<SecretString>,
    pub llm_max_concurrency: usize,
    pub llm_timeout_secs: u64,
    /// Resolved `[pipeline]` thresholds (D-14).
    pub pipeline_min_sources: u32,
    pub pipeline_max_upload_bytes: u64,
    pub pipeline_max_fetch_bytes: u64,
}

impl FileConfig {
    /// Resolve file values into final runtime paths (D-03, D-04).
    /// WR-02: `[llm]` numerics are validated here — a zero would boot a
    /// silently broken daemon (`max_concurrency = 0` spawns zero workers,
    /// `timeout_secs = 0` gives reqwest a zero-duration timeout), so a
    /// clear `Config` error at resolve time is the only safe behavior.
    pub fn resolve(&self) -> Result<RuntimeConfig> {
        if self.llm.max_concurrency == 0 {
            return Err(CoreError::Config(
                "llm.max_concurrency must be >= 1 (0 would spawn zero workers)".to_string(),
            ));
        }
        if self.llm.timeout_secs == 0 {
            return Err(CoreError::Config(
                "llm.timeout_secs must be > 0 (0 would fail every provider request)".to_string(),
            ));
        }
        let data_dir = PathBuf::from(&self.storage.data_dir);
        // D-03: env wins over the file value; an empty env var is ignored.
        let llm_api_key = std::env::var(ENV_LLM_API_KEY)
            .ok()
            .filter(|s| !s.is_empty())
            .map(SecretString::new)
            .or_else(|| self.llm.api_key.clone());
        Ok(RuntimeConfig {
            db_path: data_dir.join("saussunet.db"),
            vault_root: data_dir.join("vaults").join(self.vault.id.to_string()),
            bind: self.server.bind.clone(),
            base_path: normalize_base_path(&self.server.base_path),
            vault_id: self.vault.id,
            vault_name: self.vault.name.clone(),
            llm_provider: self.llm.provider.clone(),
            llm_base_url: self.llm.base_url.clone(),
            llm_model: self.llm.model.clone(),
            llm_api_key,
            llm_max_concurrency: self.llm.max_concurrency,
            llm_timeout_secs: self.llm.timeout_secs,
            pipeline_min_sources: self.pipeline.min_sources,
            pipeline_max_upload_bytes: self.pipeline.max_upload_bytes,
            pipeline_max_fetch_bytes: self.pipeline.max_fetch_bytes,
        })
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

    /// Serialize tests that mutate process-global env vars (cargo runs tests in
    /// parallel threads; env is shared — MoriNet config.rs:179-181 discipline).
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

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
        let rt = FileConfig::default().resolve().expect("resolve");
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
        let rt = cfg.resolve().expect("resolve");
        assert!(rt.vault_root.ends_with(Path::new("vaults/7")));
        assert_eq!(rt.vault_name, "research");
    }

    #[test]
    fn bare_llm_section_parses_with_defaults() {
        // Mirror of empty_server_section_alone_parses_with_defaults (D-02/D-04).
        let cfg: FileConfig = toml::from_str("[llm]\n").expect("bare [llm] must parse");
        assert_eq!(cfg.llm.provider, "mock");
        assert_eq!(cfg.llm.base_url, "https://api.openai.com/v1");
        assert_eq!(cfg.llm.model, "");
        assert!(cfg.llm.api_key.is_none());
        assert_eq!(cfg.llm.max_concurrency, 1);
        assert_eq!(cfg.llm.timeout_secs, 300);
    }

    #[test]
    fn full_llm_section_parses() {
        let cfg: FileConfig = toml::from_str(
            "[llm]\n\
             provider = \"openai-compatible\"\n\
             base_url = \"http://127.0.0.1:11434/v1\"\n\
             model = \"llama3\"\n\
             api_key = \"test-pass-9f3a\"\n\
             max_concurrency = 4\n\
             timeout_secs = 600\n",
        )
        .expect("full [llm] must parse");
        assert_eq!(cfg.llm.provider, "openai-compatible");
        assert_eq!(cfg.llm.base_url, "http://127.0.0.1:11434/v1");
        assert_eq!(cfg.llm.model, "llama3");
        assert_eq!(
            cfg.llm.api_key.as_ref().map(SecretString::reveal),
            Some("test-pass-9f3a")
        );
        assert_eq!(cfg.llm.max_concurrency, 4);
        assert_eq!(cfg.llm.timeout_secs, 600);
    }

    #[test]
    fn env_api_key_overrides_file_value() {
        // D-03: env wins over file. Env is process-global → serialize behind a lock.
        let _guard = ENV_LOCK.lock().expect("env lock");
        std::env::set_var(ENV_LLM_API_KEY, "test-env-7c21");
        let cfg: FileConfig =
            toml::from_str("[llm]\napi_key = \"test-file-4b8e\"\n").expect("parses");
        let rt = cfg.resolve().expect("resolve");
        std::env::remove_var(ENV_LLM_API_KEY);
        assert_eq!(
            rt.llm_api_key.as_ref().map(SecretString::reveal),
            Some("test-env-7c21"),
            "SAUSSUNET_LLM__API_KEY must win over the file value (D-03)"
        );
    }

    #[test]
    fn empty_env_api_key_is_ignored_and_file_value_used() {
        let _guard = ENV_LOCK.lock().expect("env lock");
        std::env::set_var(ENV_LLM_API_KEY, "");
        let cfg: FileConfig =
            toml::from_str("[llm]\napi_key = \"test-file-4b8e\"\n").expect("parses");
        let rt = cfg.resolve().expect("resolve");
        std::env::remove_var(ENV_LLM_API_KEY);
        assert_eq!(
            rt.llm_api_key.as_ref().map(SecretString::reveal),
            Some("test-file-4b8e"),
            "an empty env var must not clobber the file value"
        );
    }

    #[test]
    fn zero_max_concurrency_is_rejected() {
        // WR-02: 0 would spawn zero workers and silently stall the queue.
        let cfg: FileConfig = toml::from_str("[llm]\nmax_concurrency = 0\n").expect("parses");
        let err = cfg
            .resolve()
            .expect_err("max_concurrency = 0 must be rejected");
        assert!(
            err.to_string().contains("max_concurrency"),
            "error must name the offending key: {err}"
        );
    }

    #[test]
    fn zero_timeout_secs_is_rejected() {
        // WR-02: 0 gives reqwest a zero-duration timeout — every request fails.
        let cfg: FileConfig = toml::from_str("[llm]\ntimeout_secs = 0\n").expect("parses");
        let err = cfg
            .resolve()
            .expect_err("timeout_secs = 0 must be rejected");
        assert!(
            err.to_string().contains("timeout_secs"),
            "error must name the offending key: {err}"
        );
    }

    #[test]
    fn positive_llm_numerics_resolve() {
        let cfg: FileConfig =
            toml::from_str("[llm]\nmax_concurrency = 1\ntimeout_secs = 1\n").expect("parses");
        let rt = cfg.resolve().expect("minimum valid values must resolve");
        assert_eq!(rt.llm_max_concurrency, 1);
        assert_eq!(rt.llm_timeout_secs, 1);
    }

    #[test]
    fn debug_output_never_leaks_api_key() {
        // D-03 proof: the raw key must be absent from any Debug rendering.
        let cfg: FileConfig =
            toml::from_str("[llm]\napi_key = \"test-pass-9f3a\"\n").expect("parses");
        let rt = cfg.resolve().expect("resolve");
        let rendered = format!("{rt:?}");
        assert!(
            !rendered.contains("test-pass-9f3a"),
            "api_key leaked into Debug: {rendered}"
        );
        assert!(
            rendered.contains("***"),
            "Debug must redact as *** : {rendered}"
        );
        // The section itself is equally safe.
        assert!(!format!("{:?}", cfg.llm).contains("test-pass-9f3a"));
    }

    #[test]
    fn bare_server_section_gets_base_path_default() {
        // D-13: base_path rides [server]; a bare section must still load.
        let cfg: FileConfig = toml::from_str("[server]\n").expect("bare [server] must parse");
        assert_eq!(cfg.server.base_path, "/saussunet");
    }

    #[test]
    fn bare_pipeline_section_parses_with_defaults() {
        let cfg: FileConfig = toml::from_str("[pipeline]\n").expect("bare [pipeline] must parse");
        assert_eq!(cfg.pipeline.min_sources, 2);
        assert_eq!(cfg.pipeline.max_upload_bytes, 26_214_400);
        assert_eq!(cfg.pipeline.max_fetch_bytes, 5_242_880);
    }

    #[test]
    fn absent_pipeline_section_gets_sane_defaults() {
        // D-14: sane defaults when the section is absent entirely.
        let cfg: FileConfig = toml::from_str("").expect("empty TOML must parse");
        assert_eq!(cfg.pipeline.min_sources, 2);
        let rt = cfg.resolve().expect("resolve");
        assert_eq!(rt.base_path, "/saussunet");
        assert_eq!(rt.pipeline_min_sources, 2);
        assert_eq!(rt.pipeline_max_upload_bytes, 26_214_400);
        assert_eq!(rt.pipeline_max_fetch_bytes, 5_242_880);
    }

    #[test]
    fn full_pipeline_section_resolves() {
        let cfg: FileConfig = toml::from_str(
            "[pipeline]\nmin_sources = 5\nmax_upload_bytes = 1024\nmax_fetch_bytes = 512\n",
        )
        .expect("full [pipeline] must parse");
        let rt = cfg.resolve().expect("resolve");
        assert_eq!(rt.pipeline_min_sources, 5);
        assert_eq!(rt.pipeline_max_upload_bytes, 1024);
        assert_eq!(rt.pipeline_max_fetch_bytes, 512);
    }

    #[test]
    fn base_path_normalization() {
        // D-13: leading slash, no trailing slash; empty/blank falls back to default.
        let cases = [
            ("", "/saussunet"),
            ("saussunet", "/saussunet"),
            ("/x/", "/x"),
            ("/", "/saussunet"),
            ("/ui/app/", "/ui/app"),
        ];
        for (input, expected) in cases {
            let cfg: FileConfig =
                toml::from_str(&format!("[server]\nbase_path = \"{input}\"\n")).expect("parses");
            let rt = cfg.resolve().expect("resolve");
            assert_eq!(rt.base_path, expected, "base_path {input:?} must normalize");
        }
    }
}
