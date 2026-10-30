//! `entity_registry` table: rebuildable projection of canonical entity names.

use sqlx::FromRow;

use crate::error::Result;

use super::Store;

#[derive(Debug, Clone, FromRow)]
pub struct EntityRecord {
    pub vault_id: i64,
    pub canonical_name: String,
    pub aliases_json: String,
    pub page_path: Option<String>,
    pub page_type: Option<String>,
}

impl Store {
    pub async fn upsert_entity(
        &self,
        vault_id: i64,
        canonical_name: &str,
        aliases_json: &str,
        page_path: Option<&str>,
        page_type: Option<&str>,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO entity_registry \
                (vault_id, canonical_name, aliases_json, page_path, page_type) \
             VALUES (?, ?, ?, ?, ?) \
             ON CONFLICT(vault_id, canonical_name) DO UPDATE SET \
                aliases_json = excluded.aliases_json, \
                page_path = excluded.page_path, \
                page_type = excluded.page_type",
        )
        .bind(vault_id)
        .bind(canonical_name)
        .bind(aliases_json)
        .bind(page_path)
        .bind(page_type)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Resolve-before-create lookup (Phase 3 contract, Pitfall 2): exact
    /// match of the NORMALIZED name (lowercase, trimmed, whitespace
    /// collapsed) against `canonical_name` OR any entry in `aliases_json`.
    /// Candidates are fetched for the vault and matched in Rust —
    /// personal-scale registries need no LIKE/index tricks. Fuzzy /
    /// threshold / LLM matching is explicitly NOT here (Phase 3 owns it).
    pub async fn resolve_entity(&self, vault_id: i64, name: &str) -> Result<Option<EntityRecord>> {
        let target = normalize_name(name);
        if target.is_empty() {
            return Ok(None);
        }
        for row in self.list_entities(vault_id).await? {
            if normalize_name(&row.canonical_name) == target {
                return Ok(Some(row));
            }
            let aliases: Vec<String> = serde_json::from_str(&row.aliases_json).unwrap_or_default();
            if aliases.iter().any(|a| normalize_name(a) == target) {
                return Ok(Some(row));
            }
        }
        Ok(None)
    }

    pub async fn list_entities(&self, vault_id: i64) -> Result<Vec<EntityRecord>> {
        let rows = sqlx::query_as::<_, EntityRecord>(
            "SELECT vault_id, canonical_name, aliases_json, page_path, page_type \
             FROM entity_registry WHERE vault_id = ? ORDER BY canonical_name",
        )
        .bind(vault_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }
}

impl Store {
    /// D-17: record that `canonical_name` was seen in `source_id`.
    /// `INSERT OR IGNORE` — the PK `(vault_id, canonical_name, source_id)`
    /// makes a drift re-ingest of the same source a no-op, so the same
    /// source can never double-count toward the 2+ threshold.
    pub async fn add_entity_source(
        &self,
        vault_id: i64,
        canonical_name: &str,
        source_id: &str,
    ) -> Result<()> {
        sqlx::query(
            "INSERT OR IGNORE INTO entity_sources (vault_id, canonical_name, source_id) \
             VALUES (?, ?, ?)",
        )
        .bind(vault_id)
        .bind(canonical_name)
        .bind(source_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// D-10/D-17: the "2+ sources" half of the page-creation threshold —
    /// number of DISTINCT source ids recorded for the entity.
    pub async fn count_entity_sources(&self, vault_id: i64, canonical_name: &str) -> Result<u32> {
        let (n,): (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM entity_sources WHERE vault_id = ? AND canonical_name = ?",
        )
        .bind(vault_id)
        .bind(canonical_name)
        .fetch_one(&self.pool)
        .await?;
        Ok(n.max(0) as u32)
    }

    /// D-16: merge pass-1 proposed aliases into `aliases_json` —
    /// read-modify-write, case-insensitive dedup (display variants like
    /// "React.js" vs "reactjs" are KEPT distinct — resolution applies
    /// `normalize_name` at lookup time, so storage only needs to avoid
    /// pure-case duplicates), canonical name kept OUT of the alias list.
    /// No-op if the entity row does not exist (resolve-before-create
    /// guarantees the caller only merges into known rows).
    pub async fn merge_aliases(
        &self,
        vault_id: i64,
        canonical_name: &str,
        new_aliases: &[String],
    ) -> Result<()> {
        let row = sqlx::query_as::<_, EntityRecord>(
            "SELECT vault_id, canonical_name, aliases_json, page_path, page_type \
             FROM entity_registry WHERE vault_id = ? AND canonical_name = ?",
        )
        .bind(vault_id)
        .bind(canonical_name)
        .fetch_optional(&self.pool)
        .await?;
        let Some(rec) = row else { return Ok(()) };
        let mut merged: Vec<String> = serde_json::from_str(&rec.aliases_json).unwrap_or_default();
        let lower = |s: &str| s.trim().to_lowercase();
        let mut seen: std::collections::HashSet<String> = merged.iter().map(|a| lower(a)).collect();
        seen.insert(lower(canonical_name));
        for alias in new_aliases {
            let key = lower(alias);
            if !key.is_empty() && seen.insert(key) {
                merged.push(alias.clone());
            }
        }
        let json = serde_json::to_string(&merged)
            .map_err(|e| crate::error::CoreError::Vault(format!("aliases serialize: {e}")))?;
        sqlx::query(
            "UPDATE entity_registry SET aliases_json = ? WHERE vault_id = ? AND canonical_name = ?",
        )
        .bind(json)
        .bind(vault_id)
        .bind(canonical_name)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

/// Name normalization shared by resolve-before-create (D-15,
/// deterministic-first — predictable beats clever, no fuzzy matching
/// anywhere). Exact rule list:
/// 1. trim, lowercase, collapse internal whitespace runs to one space;
/// 2. strip ASCII punctuation `. , : ! ? ' "` (internal hyphens/spaces are
///    kept — they are word separators, not noise);
/// 3. fold simple English plurals on the trailing word:
///    - `ies` → `y` (stories → story);
///    - `es` dropped when preceded by a sibilant s/x/z/o (buses → bus,
///      boxes → box, classes → class);
///    - trailing `s` dropped only when the remainder is ≥ 4 chars and the
///      word does not end in `ss` (SLS stays SLS, Glass ≠ Glas).
///
/// Deterministic and pure; known v1 limitation: "react.js" normalizes to
/// "reactjs", NOT "react" — near-misses are handled by alias accumulation
/// (D-16), never by scoring-based matching.
pub(crate) fn normalize_name(name: &str) -> String {
    let cleaned: String = name
        .trim()
        .to_lowercase()
        .chars()
        .filter(|c| !matches!(c, '.' | ',' | ':' | '!' | '?' | '\'' | '"'))
        .collect();
    let collapsed = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    fold_simple_plural(&collapsed)
}

/// Step 3 of [`normalize_name`]: simple plural fold on the trailing word.
fn fold_simple_plural(s: &str) -> String {
    if let Some(stem) = s.strip_suffix("ies") {
        if !stem.is_empty() {
            return format!("{stem}y");
        }
    }
    if let Some(stem) = s.strip_suffix("es") {
        if matches!(stem.chars().next_back(), Some('s' | 'x' | 'z' | 'o')) {
            return stem.to_string();
        }
    }
    if let Some(stem) = s.strip_suffix('s') {
        if !s.ends_with("ss") && stem.chars().count() >= 4 {
            return stem.to_string();
        }
    }
    s.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn mem_store() -> Store {
        let store = Store::connect_memory().await.expect("in-memory store");
        store.ensure_vault(1, "test").await.expect("vault row");
        store
    }

    /// D-17: the same source_id recorded twice counts ONCE — a drift
    /// re-ingest can never double-count toward the 2+ threshold.
    #[tokio::test]
    async fn add_entity_source_counts_distinct_sources_only() {
        let store = mem_store().await;
        store
            .upsert_entity(1, "React", "[]", None, Some("entity"))
            .await
            .unwrap();
        store
            .add_entity_source(1, "React", "https://a.example")
            .await
            .unwrap();
        store
            .add_entity_source(1, "React", "https://a.example")
            .await
            .unwrap();
        store
            .add_entity_source(1, "React", "https://b.example")
            .await
            .unwrap();
        assert_eq!(store.count_entity_sources(1, "React").await.unwrap(), 2);
        assert_eq!(store.count_entity_sources(1, "Unknown").await.unwrap(), 0);
    }

    /// D-15: normalization matrix — case, whitespace, punctuation, and
    /// simple English plurals fold; everything else stays untouched.
    #[test]
    fn normalize_name_folds_case_whitespace_punctuation_and_plurals() {
        // case
        assert_eq!(normalize_name("Kubernetes"), normalize_name("kubernetes"));
        // whitespace (trim + collapse)
        assert_eq!(normalize_name("  React  "), normalize_name("react"));
        // punctuation stripped
        assert_eq!(normalize_name("React."), normalize_name("react"));
        assert_eq!(normalize_name("Hello, World!"), "hello world");
        // simple plurals
        assert_eq!(normalize_name("Stories"), normalize_name("story"));
        assert_eq!(normalize_name("Boxes"), normalize_name("box"));
        assert_eq!(normalize_name("Buses"), normalize_name("bus"));
        assert_eq!(
            normalize_name("Fermat's Last Theorems"),
            normalize_name("Fermat's Last Theorem")
        );
    }

    /// D-15 deterministic-first: the fold must NOT mangle names that only
    /// look like plurals — no fuzzy behaviour smuggled in via over-folding.
    #[test]
    fn normalize_name_does_not_overfold() {
        assert_eq!(normalize_name("SLS"), "sls", "acronym not stripped to 'sl'");
        assert_ne!(normalize_name("Glass"), normalize_name("Glas"));
        assert_eq!(normalize_name("class"), normalize_name("class"));
        // explicit v1 limitation: 'react.js' normalizes to 'reactjs', NOT
        // 'react' — resolution stays exact-match, no fuzzy substring logic.
        assert_ne!(normalize_name("react.js"), normalize_name("react"));
    }

    /// D-15: resolve-before-create is exact-match on canonical + aliases
    /// through `normalize_name` — near-misses like "react.js" vs "React"
    /// deliberately do NOT resolve (fuzzy matching deferred, v1 stance);
    /// an ALIAS "reactjs" DOES resolve "ReactJS" (D-16 loop).
    #[tokio::test]
    async fn resolve_entity_exact_match_only_with_alias_support() {
        let store = mem_store().await;
        store
            .upsert_entity(1, "React", "[]", None, Some("entity"))
            .await
            .unwrap();
        // explicit v1 limitation: near-miss 'react.js' must NOT resolve
        assert!(
            store.resolve_entity(1, "react.js").await.unwrap().is_none(),
            "deterministic exact match only — no fuzzy resolution"
        );
        // alias variant resolves to the canonical row
        store
            .merge_aliases(1, "React", &["reactjs".to_string()])
            .await
            .unwrap();
        let hit = store
            .resolve_entity(1, "ReactJS")
            .await
            .unwrap()
            .expect("alias 'reactjs' resolves 'ReactJS'");
        assert_eq!(hit.canonical_name, "React");
    }

    /// D-16: merge_aliases dedups case-insensitively and keeps the
    /// canonical name out of the alias list.
    #[tokio::test]
    async fn merge_aliases_dedups_case_insensitively() {
        let store = mem_store().await;
        store
            .upsert_entity(1, "React", r#"["reactjs"]"#, None, Some("entity"))
            .await
            .unwrap();
        store
            .merge_aliases(
                1,
                "React",
                &[
                    "REACTJS".to_string(),
                    "React.js".to_string(),
                    "React".to_string(),
                ],
            )
            .await
            .unwrap();
        let rec = store
            .resolve_entity(1, "React")
            .await
            .unwrap()
            .expect("row");
        let aliases: Vec<String> = serde_json::from_str(&rec.aliases_json).unwrap();
        assert_eq!(aliases, vec!["reactjs".to_string(), "React.js".to_string()]);
    }
}
