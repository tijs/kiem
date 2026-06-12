//! Kiem core library.
//!
//! Tier 1 scope: the [`content`] module holds the **single source of truth** for
//! note title and tag derivation. The Pulp editor (Swift) mirrors these rules in
//! `ContentAnalyzer`; both implementations are verified against the same fixture
//! set (`fixtures/content-derivation.json`) so they cannot silently diverge.
//!
//! In Kiem, the Rust derivation is authoritative — persisted metadata and the
//! search index are always computed here, never from the editor's Swift copy.

pub mod content;
pub mod note;
