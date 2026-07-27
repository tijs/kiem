//! Shared fixtures for the `NoteStore` test binaries.

#![allow(dead_code)] // each test binary uses a different subset

use kiem_core::note::NoteDoc;
use kiem_core::store::NoteStore;

pub const DID: &str = "did:key:z6MkTest";

pub fn note(id: &str, body: &str, ts: &str) -> NoteDoc {
    NoteDoc::new_with(id.into(), body, DID, ts.into())
}

pub fn store_with(notes: &[NoteDoc]) -> NoteStore {
    let mut store = NoteStore::open_in_memory().unwrap();
    for n in notes {
        store.insert_note(n).unwrap();
    }
    store
}
