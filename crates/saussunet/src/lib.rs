//! SaussuNet binary crate support library: the axum HTTP surface (plan 03-02).
//!
//! Lives in the BINARY crate only — `saussunet-core` must stay axum-free
//! (Anti-Pattern 1, guarded by scripts/check.sh). A lib target exists so
//! `tests/http_ingest.rs` can drive the router via `tower::ServiceExt::oneshot`.
pub mod http;
