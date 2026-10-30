//! Extraction pipeline (Phase 3): durable job kinds + their handlers.
//!
//! Layout (03-01 plan decision): `fetch.rs` = SSRF-guarded URL fetch,
//! `extract.rs` = readability+htmd HTML→Markdown, `mod.rs` = job kind
//! constants, payload schemas, and the kind-dispatch handler called by
//! `queue::process_job`. D-02: one job covers a whole source end-to-end
//! (fetch → extract → raw write → LLM passes inside one handler), so a
//! crash resumes the source via queue re-claim.

pub mod extract;
pub mod fetch;
