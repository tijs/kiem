//! Automerge note document model.
//!
//! [`NoteDoc`] is the canonical shape of a note as stored in an Automerge
//! document: a `metadata` map (denormalized, queryable fields) next to a
//! `body` text sequence (the CRDT-merged note content). The nested-map
//! layout means peers editing different metadata fields never produce a
//! conflict on the parent object.
//!
//! Title and tags are never set directly — they are derived from the body
//! via the [`content`](crate::content) module (the cross-language contract)
//! every time the body changes. `metadata.title`/`metadata.tags` exist only
//! so stores can list and filter notes without hydrating the full document.
//!
//! This module owns the document *shape*; persistence and the lifecycle of
//! the backing `AutoCommit` (which must live across edits to preserve CRDT
//! history) belong to the store layer.

use autosurgeon::{Hydrate, Reconcile, Text};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use uuid::Uuid;

/// Default `note_type` for ordinary notes.
pub const DEFAULT_NOTE_TYPE: &str = "note";

/// Denormalized note metadata, stored as a nested Automerge map.
#[derive(Debug, Clone, PartialEq, Eq, Reconcile, Hydrate)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct NoteMetadata {
    pub id: String,
    /// Derived from the body (first H1, else first line). Never user-set.
    pub title: String,
    /// Derived from body hashtags. Never user-set.
    pub tags: Vec<String>,
    pub pinned: bool,
    pub deleted: bool,
    /// RFC 3339 UTC timestamp.
    pub created_at: String,
    /// RFC 3339 UTC timestamp.
    pub modified_at: String,
    pub author_did: String,
    pub note_type: String,
    /// Read from a leading `---\nstatus: <value>\n---` frontmatter fence in the
    /// body, if present. Never user-set through a dedicated API — like
    /// `title`/`tags`, it's derived from the body on every write. `None` for
    /// the overwhelming majority of notes, which carry no frontmatter at all.
    pub status: Option<String>,
}

/// A note as an Automerge document: nested metadata map + text body.
#[derive(Debug, Clone, PartialEq, Reconcile, Hydrate)]
pub struct NoteDoc {
    pub metadata: NoteMetadata,
    pub body: Text,
}

impl NoteDoc {
    /// Create a note with a fresh UUID and the current UTC time.
    pub fn new(body: &str, author_did: &str) -> Self {
        Self::new_with(Uuid::new_v4().to_string(), body, author_did, now_rfc3339())
    }

    /// Create a note with explicit id and timestamp (deterministic seam for
    /// tests and for stores that batch-assign timestamps).
    pub fn new_with(id: String, body: &str, author_did: &str, timestamp: String) -> Self {
        let (status, rest) = crate::content::parse_frontmatter_status(body);
        NoteDoc {
            metadata: NoteMetadata {
                id,
                title: crate::content::derive_title(rest),
                tags: crate::content::extract_tags(rest),
                pinned: false,
                deleted: false,
                created_at: timestamp.clone(),
                modified_at: timestamp,
                author_did: author_did.to_owned(),
                note_type: DEFAULT_NOTE_TYPE.to_owned(),
                status,
            },
            body: Text::with_value(body),
        }
    }

    /// Replace the body. The change is applied as a minimal splice (so
    /// concurrent edits merge at character level), and title/tags/modified_at
    /// are re-derived.
    pub fn update_body(&mut self, body: &str) {
        self.update_body_at(body, now_rfc3339());
    }

    /// [`update_body`](Self::update_body) with an explicit timestamp.
    pub fn update_body_at(&mut self, body: &str, timestamp: String) {
        self.body.update(body);
        let (status, rest) = crate::content::parse_frontmatter_status(body);
        self.metadata.title = crate::content::derive_title(rest);
        self.metadata.tags = crate::content::extract_tags(rest);
        self.metadata.status = status;
        self.metadata.modified_at = timestamp;
    }

    pub fn set_pinned(&mut self, pinned: bool) {
        self.metadata.pinned = pinned;
        self.metadata.modified_at = now_rfc3339();
    }

    pub fn set_deleted(&mut self, deleted: bool) {
        self.metadata.deleted = deleted;
        self.metadata.modified_at = now_rfc3339();
    }
}

fn now_rfc3339() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .expect("RFC 3339 formatting of a valid UTC time cannot fail")
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::AutoCommit;
    use autosurgeon::{hydrate, reconcile};

    const TS: &str = "2026-06-12T10:00:00Z";

    fn sample() -> NoteDoc {
        NoteDoc::new_with(
            "note-1".into(),
            "# Groceries\n\nBuy milk #errands",
            "did:key:z6MkTest",
            TS.into(),
        )
    }

    fn roundtrip(note: &NoteDoc) -> NoteDoc {
        let mut doc = AutoCommit::new();
        reconcile(&mut doc, note).expect("reconcile");
        let bytes = doc.save();
        let loaded = AutoCommit::load(&bytes).expect("load");
        hydrate(&loaded).expect("hydrate")
    }

    #[test]
    fn new_derives_title_and_tags_from_body() {
        let note = sample();
        assert_eq!(note.metadata.title, "Groceries");
        assert_eq!(note.metadata.tags, vec!["errands"]);
        assert_eq!(note.metadata.created_at, TS);
        assert_eq!(note.metadata.modified_at, TS);
        assert_eq!(note.metadata.note_type, DEFAULT_NOTE_TYPE);
        assert!(!note.metadata.pinned);
        assert!(!note.metadata.deleted);
    }

    #[test]
    fn roundtrip_preserves_all_fields() {
        let note = sample();
        assert_eq!(roundtrip(&note), note);
    }

    #[test]
    fn update_body_rederives_metadata_and_roundtrips() {
        let mut note = sample();
        note.update_body_at("# Chores\n\nMow lawn #home #errands", "2026-06-12T11:00:00Z".into());
        assert_eq!(note.metadata.title, "Chores");
        assert_eq!(note.metadata.tags, vec!["home", "errands"]);
        assert_eq!(note.metadata.modified_at, "2026-06-12T11:00:00Z");
        assert_eq!(note.metadata.created_at, TS, "created_at must not move");
        assert_eq!(roundtrip(&note).body.as_str(), "# Chores\n\nMow lawn #home #errands");
    }

    #[test]
    fn empty_body_creates_valid_document() {
        let note = NoteDoc::new_with("empty".into(), "", "did:key:z6MkTest", TS.into());
        assert_eq!(note.metadata.title, "");
        assert!(note.metadata.tags.is_empty());
        assert_eq!(roundtrip(&note), note);
    }

    #[test]
    fn special_characters_roundtrip() {
        let body = "# 🍐 ノート\n\n改行あり\nemoji 🎉 and #日本語tag";
        let note = NoteDoc::new_with("unicode".into(), body, "did:key:z6MkTest", TS.into());
        let back = roundtrip(&note);
        assert_eq!(back.body.as_str(), body);
        assert_eq!(back.metadata.title, "🍐 ノート");
    }

    #[test]
    fn two_notes_save_and_load_independently() {
        let a = NoteDoc::new_with("a".into(), "# A", "did:key:z6MkA", TS.into());
        let b = NoteDoc::new_with("b".into(), "# B", "did:key:z6MkB", TS.into());
        let (mut doc_a, mut doc_b) = (AutoCommit::new(), AutoCommit::new());
        reconcile(&mut doc_a, &a).unwrap();
        reconcile(&mut doc_b, &b).unwrap();
        let (bytes_a, bytes_b) = (doc_a.save(), doc_b.save());

        let back_a: NoteDoc = hydrate(&AutoCommit::load(&bytes_a).unwrap()).unwrap();
        let back_b: NoteDoc = hydrate(&AutoCommit::load(&bytes_b).unwrap()).unwrap();
        assert_eq!(back_a, a);
        assert_eq!(back_b, b);
    }

    #[test]
    fn concurrent_edits_to_body_and_metadata_merge_without_conflict() {
        // The nested-map schema exists so peers editing different fields
        // merge cleanly; verify that on a real fork/merge.
        let note = sample();
        let mut base = AutoCommit::new();
        reconcile(&mut base, &note).unwrap();

        let mut fork = base.fork();
        let mut left: NoteDoc = hydrate(&base).unwrap();
        left.set_pinned(true);
        reconcile(&mut base, &left).unwrap();

        let mut right: NoteDoc = hydrate(&fork).unwrap();
        right.update_body_at("# Groceries\n\nBuy milk and eggs #errands", TS.into());
        reconcile(&mut fork, &right).unwrap();

        base.merge(&mut fork).unwrap();
        let merged: NoteDoc = hydrate(&base).unwrap();
        assert!(merged.metadata.pinned);
        assert_eq!(merged.body.as_str(), "# Groceries\n\nBuy milk and eggs #errands");
    }

    #[test]
    fn new_generates_unique_uuid_ids() {
        let a = NoteDoc::new("x", "did:key:z6MkTest");
        let b = NoteDoc::new("x", "did:key:z6MkTest");
        assert_ne!(a.metadata.id, b.metadata.id);
        assert_eq!(a.metadata.id.len(), 36);
    }
}
