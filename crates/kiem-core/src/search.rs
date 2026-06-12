//! Full-text search over notes (tantivy).
//!
//! The index is a **derived** structure: fully rebuildable from the store's
//! Automerge documents and never a source of truth. Every write re-indexes
//! the whole note (delete-by-id + add + commit); deleted notes are removed
//! so search never surfaces trash.
//!
//! Queries are parsed leniently (a stray `(` degrades the query, it doesn't
//! error). An empty or whitespace-only query returns no results by decision —
//! "show all notes" is a listing concern, not a search.

use std::path::Path;

use tantivy::collector::TopDocs;
use tantivy::query::QueryParser;
use tantivy::schema::{Field, Schema, Value, STORED, STRING, TEXT};
use tantivy::snippet::SnippetGenerator;
use tantivy::{Index, IndexReader, IndexWriter, ReloadPolicy, TantivyDocument, Term};

use crate::note::NoteMetadata;

#[derive(Debug, thiserror::Error)]
pub enum SearchError {
    #[error("search index error: {0}")]
    Tantivy(#[from] tantivy::TantivyError),
    #[error("search index directory error: {0}")]
    OpenDirectory(#[from] tantivy::directory::error::OpenDirectoryError),
    #[error("search index io error: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct SearchResult {
    pub note_id: String,
    pub title: String,
    pub snippet: String,
    pub score: f32,
}

struct Fields {
    note_id: Field,
    title: Field,
    body: Field,
    tags: Field,
}

pub struct SearchIndex {
    index: Index,
    reader: IndexReader,
    fields: Fields,
}

const WRITER_HEAP_BYTES: usize = 15_000_000;
/// tantivy's writer lock is exclusive per process; a daemon and a one-shot
/// CLI command can collide briefly, so writer acquisition retries.
const WRITER_LOCK_RETRIES: u32 = 40;
const WRITER_LOCK_BACKOFF: std::time::Duration = std::time::Duration::from_millis(50);

impl SearchIndex {
    /// Open (or create) a persistent index in `dir`.
    pub fn open_in_dir(dir: &Path) -> Result<Self, SearchError> {
        std::fs::create_dir_all(dir)?;
        let mmap = tantivy::directory::MmapDirectory::open(dir)?;
        Self::init(Index::open_or_create(mmap, Self::schema())?)
    }

    /// Volatile in-RAM index for tests.
    pub fn in_memory() -> Result<Self, SearchError> {
        Self::init(Index::create_in_ram(Self::schema()))
    }

    fn schema() -> Schema {
        let mut builder = Schema::builder();
        // note_id is STRING (untokenized) so delete_term matches exactly.
        builder.add_text_field("note_id", STRING | STORED);
        builder.add_text_field("title", TEXT | STORED);
        // body is STORED so SnippetGenerator can excerpt it at query time
        builder.add_text_field("body", TEXT | STORED);
        builder.add_text_field("tags", TEXT | STORED);
        builder.build()
    }

    fn init(index: Index) -> Result<Self, SearchError> {
        let schema = index.schema();
        let fields = Fields {
            note_id: schema.get_field("note_id")?,
            title: schema.get_field("title")?,
            body: schema.get_field("body")?,
            tags: schema.get_field("tags")?,
        };
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()?;
        Ok(SearchIndex { index, reader, fields })
    }

    /// Index (or re-index) one note. Delete-by-id + add + commit.
    pub fn index_note(&mut self, meta: &NoteMetadata, body: &str) -> Result<(), SearchError> {
        let writer = self.writer()?;
        writer.delete_term(Term::from_field_text(self.fields.note_id, &meta.id));
        writer.add_document(self.make_doc(meta, body))?;
        self.commit(writer)
    }

    /// Drop one note from the index (deletion, or hiding trashed notes).
    pub fn remove_note(&mut self, id: &str) -> Result<(), SearchError> {
        let writer = self.writer()?;
        writer.delete_term(Term::from_field_text(self.fields.note_id, id));
        self.commit(writer)
    }

    /// Replace the entire index contents (recovery path; the store re-feeds
    /// every live note from SQLite).
    pub fn rebuild<'a>(
        &mut self,
        notes: impl Iterator<Item = (&'a NoteMetadata, &'a str)>,
    ) -> Result<(), SearchError> {
        let writer = self.writer()?;
        writer.delete_all_documents()?;
        for (meta, body) in notes {
            writer.add_document(self.make_doc(meta, body))?;
        }
        self.commit(writer)
    }

    /// Search title, body, and tags. Empty/whitespace queries return nothing.
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchResult>, SearchError> {
        if query.trim().is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let searcher = self.reader.searcher();
        let parser = QueryParser::for_index(
            &self.index,
            vec![self.fields.title, self.fields.body, self.fields.tags],
        );
        let (parsed, _lenient_errors) = parser.parse_query_lenient(query);
        let snippets = SnippetGenerator::create(&searcher, &parsed, self.fields.body)?;

        let mut results = Vec::new();
        let collector = TopDocs::with_limit(limit).order_by_score();
        for (score, addr) in searcher.search(&parsed, &collector)? {
            let doc: TantivyDocument = searcher.doc(addr)?;
            let text = |f: Field| {
                doc.get_first(f)
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_owned()
            };
            results.push(SearchResult {
                note_id: text(self.fields.note_id),
                title: text(self.fields.title),
                snippet: snippets.snippet_from_doc(&doc).fragment().to_owned(),
                score,
            });
        }
        Ok(results)
    }

    /// Writers are opened per operation (not held) so several processes —
    /// the sync daemon plus one-shot CLI commands — can share an index dir.
    /// The exclusive lock is only contended for the duration of one write.
    fn writer(&self) -> Result<IndexWriter, SearchError> {
        let mut attempts = 0;
        loop {
            match self.index.writer(WRITER_HEAP_BYTES) {
                Ok(writer) => return Ok(writer),
                Err(tantivy::TantivyError::LockFailure(..)) if attempts < WRITER_LOCK_RETRIES => {
                    attempts += 1;
                    std::thread::sleep(WRITER_LOCK_BACKOFF);
                }
                Err(other) => return Err(other.into()),
            }
        }
    }

    fn make_doc(&self, meta: &NoteMetadata, body: &str) -> TantivyDocument {
        let mut doc = TantivyDocument::default();
        doc.add_text(self.fields.note_id, &meta.id);
        doc.add_text(self.fields.title, &meta.title);
        doc.add_text(self.fields.body, body);
        doc.add_text(self.fields.tags, meta.tags.join(" "));
        doc
    }

    fn commit(&mut self, mut writer: IndexWriter) -> Result<(), SearchError> {
        writer.commit()?;
        self.reader.reload()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::note::NoteDoc;

    const TS: &str = "2026-06-12T10:00:00Z";

    fn meta(id: &str, body: &str) -> NoteMetadata {
        NoteDoc::new_with(id.into(), body, "did:key:z6MkTest", TS.into()).metadata
    }

    fn index_with(notes: &[(&str, &str)]) -> SearchIndex {
        let mut idx = SearchIndex::in_memory().unwrap();
        for (id, body) in notes {
            idx.index_note(&meta(id, body), body).unwrap();
        }
        idx
    }

    #[test]
    fn finds_word_in_the_right_note() {
        let idx = index_with(&[
            ("n1", "# Alpha\n\nplain text"),
            ("n2", "# Beta\n\nthe zebra grazes"),
            ("n3", "# Gamma\n\nmore text"),
        ]);
        let hits = idx.search("zebra", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].note_id, "n2");
        assert_eq!(hits[0].title, "Beta");
        assert!(hits[0].score > 0.0);
        assert!(hits[0].snippet.contains("zebra"));
    }

    #[test]
    fn finds_notes_by_tag() {
        let idx = index_with(&[
            ("n1", "# A\n\nabout #gardening"),
            ("n2", "# B\n\nabout #cooking"),
        ]);
        let hits = idx.search("gardening", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].note_id, "n1");
    }

    #[test]
    fn reindex_replaces_old_content() {
        let mut idx = index_with(&[("n1", "# A\n\noriginal walrus")]);
        let body = "# A\n\nupdated narwhal";
        idx.index_note(&meta("n1", body), body).unwrap();
        assert!(idx.search("walrus", 10).unwrap().is_empty());
        assert_eq!(idx.search("narwhal", 10).unwrap().len(), 1);
    }

    #[test]
    fn no_match_and_empty_query_return_empty() {
        let idx = index_with(&[("n1", "# A\n\ntext")]);
        assert!(idx.search("nonexistentword", 10).unwrap().is_empty());
        assert!(idx.search("", 10).unwrap().is_empty());
        assert!(idx.search("   ", 10).unwrap().is_empty());
    }

    #[test]
    fn lenient_parsing_survives_query_syntax_noise() {
        let idx = index_with(&[("n1", "# A\n\nzebra text")]);
        // Unbalanced parens/quotes must degrade, not error.
        assert!(idx.search("zebra (", 10).is_ok());
        assert!(idx.search("\"zebra", 10).is_ok());
    }

    #[test]
    fn removed_note_disappears_from_results() {
        let mut idx = index_with(&[("n1", "# A\n\nzebra")]);
        idx.remove_note("n1").unwrap();
        assert!(idx.search("zebra", 10).unwrap().is_empty());
    }

    #[test]
    fn rebuild_replaces_index_contents() {
        let mut idx = index_with(&[("stale", "# Old\n\nstale content")]);
        let m = meta("fresh", "# New\n\nfresh content");
        idx.rebuild([(&m, "# New\n\nfresh content")].into_iter()).unwrap();
        assert!(idx.search("stale", 10).unwrap().is_empty());
        assert_eq!(idx.search("fresh", 10).unwrap()[0].note_id, "fresh");
    }

    #[test]
    fn persistent_index_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        {
            let mut idx = SearchIndex::open_in_dir(dir.path()).unwrap();
            let body = "# Durable\n\nsearchable zebra";
            idx.index_note(&meta("n1", body), body).unwrap();
        }
        let idx = SearchIndex::open_in_dir(dir.path()).unwrap();
        assert_eq!(idx.search("zebra", 10).unwrap()[0].note_id, "n1");
    }
}
