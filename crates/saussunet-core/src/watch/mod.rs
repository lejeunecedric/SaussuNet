//! Continuous source watching (Phase 4, SRCC-01/SRCC-02).
//!
//! Pure logic first: `multistatus` parses RFC 4918 multistatus / RFC 6578
//! sync-report bodies, `diff` classifies walk-vs-state deltas. 04-04 adds
//! `pub mod dav;` (HTTP client) and `poll_once` wiring here.

pub mod dav;
pub mod diff;
pub mod multistatus;
