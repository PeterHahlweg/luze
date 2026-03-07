//! # LuZe - Luhmann's Zettelkasten
//!
//! A digital note box inspired by Niklas Luhmann's Zettelkasten system: an organic, tree-structured
//! knowledge organization with the following properties:
//!
//! - Fixed positions prevent rigid categorization — a note's ID encodes its place in the tree
//! - A keyword search serves as entry point
//! - Changes become new branches from the relevant note
//! - Links and indices can change when necessary (small fixes, rephrasing, git merges)
//! - Main box only: raw thoughts and insights (~90,000 notes for Luhmann)
//! - Content is immutable once written; new versions are child notes with `supersedes`
//! - Links and indices can change when necessary
//! - Notes are stored as JSON in per-drawer files, lazily loaded

use std::{env, path::PathBuf};

pub mod id;
pub use id::ID;

pub mod merge;
pub use merge::{MergeAction, MergeReport, merge_conflicts, rebuild_index};

pub mod git;
pub use git::{SyncReport, git_available, git_run, git_remote, git_has_uncommitted,
              git_current_branch, git_has_upstream, git_unpushed_count, sync};

pub mod note;
pub use note::{Note, MAX_CONTENT_LEN, headline, validate_content};

pub mod lock;
pub use lock::{WriteLock, acquire_write_lock};

pub mod store;
pub use store::{Draw, DRAW_CAPACITY, NoteBox, needs_migration, migrate};

pub(crate) mod json {
    use serde::{Serialize, de::DeserializeOwned};

    pub fn to_string_pretty<T: Serialize>(value: &T) -> Result<String, String> {
        #[cfg(feature = "sonic-rs")]
        { sonic_rs::to_string_pretty(value).map_err(|e| e.to_string()) }
        #[cfg(feature = "serde-json")]
        { serde_json::to_string_pretty(value).map_err(|e| e.to_string()) }
    }

    pub fn from_str<T: DeserializeOwned>(s: &str) -> Result<T, String> {
        #[cfg(feature = "sonic-rs")]
        { sonic_rs::from_str(s).map_err(|e| e.to_string()) }
        #[cfg(feature = "serde-json")]
        { serde_json::from_str(s).map_err(|e| e.to_string()) }
    }
}

/// Resolves the NoteBox directory.
/// Precedence: `LUZE_PATH` env var → `./.luze` (if it exists) → `~/.luze`.
pub fn notes_dir() -> PathBuf {
    if let Ok(p) = env::var("LUZE_PATH") { return PathBuf::from(p); }
    let local = PathBuf::from("./.luze");
    if local.is_dir() { return local; }
    env::var("HOME").map(|h| PathBuf::from(h).join(".luze")).unwrap_or(local)
}


#[cfg(test)]
mod tests {
    use super::*;

    // --- NoteBox::add ---

    #[test]
    fn test_add_maintains_sorted_order() {
        let mut zk = NoteBox::default();
        // insert out of order
        zk.add(Note::new("1b",  "1",  "banana")).unwrap();
        zk.add(Note::new("1",   "1",  "root")).unwrap();
        zk.add(Note::new("1a1", "1a", "cherry")).unwrap();
        zk.add(Note::new("1a",  "1",  "apple")).unwrap();

        let ids: Vec<String> = zk.notes().iter().map(|n| n.id.to_string()).collect();
        assert_eq!(ids, ["1", "1a", "1a1", "1b"]);
    }

    #[test]
    fn test_add_parent_before_children() {
        let mut zk = NoteBox::default();
        zk.add(Note::new("1a1", "1a", "child")).unwrap();
        zk.add(Note::new("1a",  "1",  "parent")).unwrap();

        let notes = zk.notes();
        assert_eq!(notes[0].id, ID::from("1a"));
        assert_eq!(notes[1].id, ID::from("1a1"));
    }

    #[test]
    fn test_add_rejects_duplicate_id() {
        let mut zk = NoteBox::default();
        zk.add(Note::new("1a", "1", "first")).unwrap();
        let err = zk.add(Note::new("1a", "1", "second")).unwrap_err();
        assert!(err.contains("already exists"), "unexpected error: {err}");
        // only one note with that ID in the box
        assert_eq!(zk.draws.values().flat_map(|d| d.notes.as_deref().unwrap_or(&[])).filter(|n| n.id == ID::from("1a")).count(), 1);
    }

    #[test]
    fn test_add_draw_capacity_enforced() {
        let mut zk = NoteBox::default();
        zk.add(Note::new("1", "1", "root")).unwrap();
        assert!(!zk.draws.values().next().unwrap().is_full());
    }

    #[test]
    fn test_add_routes_to_correct_draw() {
        let mut zk = NoteBox::default();
        zk.add(Note::new("1a",  "1",  "root")).unwrap();
        zk.add(Note::new("1a1", "1a", "child")).unwrap();
        zk.add(Note::new("1b",  "1",  "other")).unwrap();

        // All notes get indexed
        assert!(zk.index.contains_key(&ID::from("1a")));
        assert!(zk.index.contains_key(&ID::from("1a1")));
        assert!(zk.index.contains_key(&ID::from("1b")));
        // child goes into same draw as parent
        assert_eq!(zk.index[&ID::from("1a1")], zk.index[&ID::from("1a")]);
    }

    // --- supersedes ---

    #[test]
    fn test_supersedes_field_roundtrip() {
        let dir = std::env::temp_dir().join("luze_test_supersedes_rt");
        let _ = std::fs::remove_dir_all(&dir);

        let mut zk = NoteBox::create(&dir);
        zk.add(Note::new("1a", "1", "original")).unwrap();
        zk.add(Note::new_version("1a1", "1a", "better", "1a")).unwrap();
        zk.save().unwrap();

        let mut loaded = NoteBox::open(&dir).unwrap();
        loaded.load_all().unwrap();
        let note = loaded.find(&ID::from("1a1")).unwrap().unwrap();
        assert_eq!(note.supersedes(), Some(&ID::from("1a")));

        let plain = loaded.find(&ID::from("1a")).unwrap().unwrap();
        assert_eq!(plain.supersedes(), None);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn test_update_creates_superseding_child() {
        let mut zk = NoteBox::default();
        zk.add(Note::new("1a", "1", "original")).unwrap();

        let child_id = zk.update(&ID::from("1a"), "better").unwrap();
        assert_eq!(child_id, ID::from("1a1"));

        let child = zk.find(&child_id).unwrap().unwrap();
        assert_eq!(child.content(), "better");
        assert_eq!(child.supersedes(), Some(&ID::from("1a")));
        assert_eq!(child.parent(), Some(&ID::from("1a")));
    }

    #[test]
    fn test_update_rejects_already_superseded() {
        let mut zk = NoteBox::default();
        zk.add(Note::new("1a", "1", "v1")).unwrap();
        zk.update(&ID::from("1a"), "v2").unwrap();
        assert!(zk.update(&ID::from("1a"), "v3").is_err());
    }

    #[test]
    fn test_is_superseded() {
        let mut zk = NoteBox::default();
        zk.add(Note::new("1a", "1", "v1")).unwrap();
        assert!(!zk.is_superseded(&ID::from("1a")));
        zk.update(&ID::from("1a"), "v2").unwrap();
        assert!(zk.is_superseded(&ID::from("1a")));
    }

    #[test]
    fn test_current_version() {
        let mut zk = NoteBox::default();
        zk.add(Note::new("1a", "1", "v1")).unwrap();
        zk.update(&ID::from("1a"), "v2").unwrap();
        // 1a1 supersedes 1a; now update 1a1
        zk.update(&ID::from("1a1"), "v3").unwrap();

        let current = zk.current_version(&ID::from("1a")).unwrap();
        assert_eq!(current.content(), "v3");
    }

    #[test]
    fn test_search_skips_superseded() {
        let mut zk = NoteBox::default();
        zk.add(Note::new("1a", "1", "thought about cats")).unwrap();
        zk.update(&ID::from("1a"), "better thought about cats").unwrap();

        let results = zk.search("cats").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id(), &ID::from("1a1"));
    }

    #[test]
    fn test_search_headline_match_ranked_first() {
        let mut zk = NoteBox::default();
        // body-only match
        zk.add(Note::new("1a", "1", "musings\ncats are interesting")).unwrap();
        // headline match
        zk.add(Note::new("1b", "1", "cats in history")).unwrap();

        let results = zk.search("cats").unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].id(), &ID::from("1b"), "headline match should come first");
        assert_eq!(results[1].id(), &ID::from("1a"), "body-only match should come second");
    }

    #[test]
    fn test_search_all_includes_superseded() {
        let mut zk = NoteBox::default();
        zk.add(Note::new("1a", "1", "thought about cats")).unwrap();
        zk.update(&ID::from("1a"), "better thought about cats").unwrap();

        let results = zk.search_all("cats").unwrap();
        assert_eq!(results.len(), 2);
    }

    // --- backlinks ---

    #[test]
    fn test_backlinks_excludes_parent_links() {
        // Child notes have parent as links[0]; backlinks must not include them.
        let mut zk = NoteBox::default();
        zk.add(Note::new("1",   "1",  "root")).unwrap();
        zk.add(Note::new("1a",  "1",  "child of root")).unwrap();
        zk.add(Note::new("1a1", "1a", "grandchild")).unwrap();

        // backlinks("1") must NOT return 1a (whose parent is 1)
        let bl = zk.backlinks(&ID::from("1")).unwrap();
        assert!(bl.is_empty(), "backlinks should not include children: got {:?}", bl.iter().map(|n| n.id().to_string()).collect::<Vec<_>>());
    }

    #[test]
    fn test_backlinks_includes_explicit_links() {
        let mut zk = NoteBox::default();
        zk.add(Note::new("1",  "1", "root")).unwrap();
        zk.add(Note::new("2",  "2", "other root")).unwrap();
        // explicitly link 2 → 1
        zk.find_mut(&ID::from("2")).unwrap().unwrap().add_link(ID::from("1"));

        let bl = zk.backlinks(&ID::from("1")).unwrap();
        assert_eq!(bl.len(), 1);
        assert_eq!(bl[0].id(), &ID::from("2"));
    }

    // --- File-based round-trip ---

    #[test]
    fn test_acquire_write_lock_serializes() {
        use std::sync::{Arc, Mutex};
        use std::thread;

        let dir = std::env::temp_dir().join("luze_test_write_lock");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let order: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));

        // Acquire the lock on the main thread.
        let lock = acquire_write_lock(&dir).unwrap();
        order.lock().unwrap().push(1);

        let dir2 = dir.clone();
        let order2 = order.clone();
        let handle = thread::spawn(move || {
            // This blocks until the main thread releases.
            let _l = acquire_write_lock(&dir2).unwrap();
            order2.lock().unwrap().push(2);
        });

        // Small yield to let the spawned thread reach flock() and block.
        thread::sleep(std::time::Duration::from_millis(50));
        drop(lock); // release — unblocks the thread
        handle.join().unwrap();

        assert_eq!(*order.lock().unwrap(), vec![1, 2]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn test_atomic_write_no_tmp_left_after_save() {
        let dir = std::env::temp_dir().join("luze_test_atomic_notmp");
        let _ = std::fs::remove_dir_all(&dir);

        let mut zk = NoteBox::create(&dir);
        zk.add(Note::new("1a/1", "1a/1", "alpha")).unwrap();
        zk.save().unwrap();

        let tmp_files: Vec<_> = std::fs::read_dir(dir.join("draws")).unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("tmp"))
            .collect();
        assert!(tmp_files.is_empty(), "tmp files left after save: {:?}", tmp_files);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn test_atomic_write_reader_sees_valid_json() {
        use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
        use std::thread;

        let dir = std::env::temp_dir().join("luze_test_atomic_reader");
        let _ = std::fs::remove_dir_all(&dir);

        // Seed with an initial draw so the reader has a file to open.
        let mut zk = NoteBox::create(&dir);
        zk.add(Note::new("1a/1", "1a/1", "seed")).unwrap();
        zk.save().unwrap();

        // Discover the actual draw file written (new format uses decimal numbers).
        let draw_path = std::fs::read_dir(dir.join("draws")).unwrap()
            .filter_map(|e| e.ok())
            .find(|e| e.path().extension().and_then(|x| x.to_str()) == Some("json"))
            .expect("no draw file found after seed save")
            .path();
        let done = Arc::new(AtomicBool::new(false));

        // Writer: repeatedly save a growing draw.
        let dir_w = dir.clone();
        let done_w = done.clone();
        let writer = thread::spawn(move || {
            let mut zk = NoteBox::open(&dir_w).unwrap();
            zk.load_all().unwrap();
            for i in 0..200u32 {
                let id   = format!("1a/{}", i + 2);
                let cont = format!("note number {}", i);
                let _ = zk.add(Note::new(id.as_str(), "1a/1", &cont));
                zk.save().unwrap();
            }
            done_w.store(true, Ordering::Release);
        });

        // Reader: read the draw file while the writer is active; must always be valid JSON.
        let reader = thread::spawn(move || {
            while !done.load(Ordering::Acquire) {
                if let Ok(bytes) = std::fs::read(&draw_path) {
                    let s = String::from_utf8_lossy(&bytes);
                    if !s.is_empty() {
                        assert!(
                            json::from_str::<Vec<Note>>(&s).is_ok(),
                            "reader saw invalid JSON: {}", &s[..s.len().min(120)]
                        );
                    }
                }
            }
        });

        writer.join().unwrap();
        reader.join().unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn test_save_open_roundtrip() {
        let dir = std::env::temp_dir().join("luze_test_roundtrip");
        let _ = std::fs::remove_dir_all(&dir);

        let mut zk = NoteBox::create(&dir);
        zk.add(Note::new("1a/1",   "1a/1",  "apple")).unwrap();
        zk.add(Note::new("1a/1a",  "1a/1",  "banana")).unwrap();
        zk.add(Note::new("1a/1a1", "1a/1a", "cherry")).unwrap();
        zk.save().unwrap();

        let mut loaded = NoteBox::open(&dir).unwrap();
        loaded.load_all().unwrap();

        assert_eq!(zk.notes(), loaded.notes());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    // --- migrate ---

    #[test]
    fn test_migrate_noop_if_already_new_format() {
        let dir = std::env::temp_dir().join("luze_test_migrate_noop");
        let _ = std::fs::remove_dir_all(&dir);

        let mut zk = NoteBox::create(&dir);
        zk.add(Note::new("1", "1", "root")).unwrap();
        zk.save().unwrap();

        assert!(dir.join("index.json").exists());
        let count = migrate(&dir).unwrap();
        assert_eq!(count, 0);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn test_migrate_noop_if_no_draws_dir() {
        let dir = std::env::temp_dir().join("luze_test_migrate_empty");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let count = migrate(&dir).unwrap();
        assert_eq!(count, 0);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn test_migrate_from_old_format() {
        let dir = std::env::temp_dir().join("luze_test_migrate_old");
        let _ = std::fs::remove_dir_all(&dir);
        let draws_dir = dir.join("draws");
        std::fs::create_dir_all(&draws_dir).unwrap();

        // Write old-format draw files (named-section style, no index.json)
        let notes_root = vec![
            Note::new("1",  "1",  "root note"),
            Note::new("1a", "1",  "first child"),
        ];
        let notes_section = vec![
            Note::new("1a1", "1a", "grandchild"),
        ];
        std::fs::write(
            draws_dir.join("root.json"),
            json::to_string_pretty(&notes_root).unwrap(),
        ).unwrap();
        std::fs::write(
            draws_dir.join("1a.json"),
            json::to_string_pretty(&notes_section).unwrap(),
        ).unwrap();

        assert!(!dir.join("index.json").exists());

        let count = migrate(&dir).unwrap();
        assert_eq!(count, 3);

        // index.json must now exist
        assert!(dir.join("index.json").exists());
        // old files must be gone
        assert!(!draws_dir.join("root.json").exists());
        assert!(!draws_dir.join("1a.json").exists());

        // all notes must be accessible
        let mut zk = NoteBox::open(&dir).unwrap();
        assert!(zk.find(&ID::from("1")).unwrap().is_some());
        assert!(zk.find(&ID::from("1a")).unwrap().is_some());
        assert!(zk.find(&ID::from("1a1")).unwrap().is_some());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn test_migrate_preserves_note_content_and_links() {
        let dir = std::env::temp_dir().join("luze_test_migrate_content");
        let _ = std::fs::remove_dir_all(&dir);
        let draws_dir = dir.join("draws");
        std::fs::create_dir_all(&draws_dir).unwrap();

        let mut note = Note::new("2", "2", "standalone");
        note.add_link(ID::from("1"));
        let notes = vec![note];
        std::fs::write(
            draws_dir.join("root.json"),
            json::to_string_pretty(&notes).unwrap(),
        ).unwrap();

        migrate(&dir).unwrap();

        let mut zk = NoteBox::open(&dir).unwrap();
        let found = zk.find(&ID::from("2")).unwrap().unwrap().clone();
        assert_eq!(found.content(), "standalone");
        assert!(found.links().iter().any(|l| l == &ID::from("1")));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    // --- validate_content ---

    #[test]
    fn test_validate_content_accepts_short_single_line() {
        assert!(validate_content("a short note").is_ok());
    }

    #[test]
    fn test_validate_content_rejects_long_single_line() {
        let long = "a".repeat(151);
        assert!(validate_content(&long).is_err());
    }

    #[test]
    fn test_validate_content_accepts_long_multiline() {
        let long = format!("headline\n{}", "a".repeat(300));
        assert!(validate_content(&long).is_ok());
    }

    // --- add content length ---

    #[test]
    fn test_add_rejects_content_over_250_chars() {
        let mut zk = NoteBox::default();
        let long = "a".repeat(251);
        assert!(zk.add(Note::new("1", "1", &long)).is_err());
    }

    #[test]
    fn test_add_accepts_content_at_250_chars() {
        let mut zk = NoteBox::default();
        let at_limit = "a".repeat(250);
        assert!(zk.add(Note::new("1", "1", &at_limit)).is_ok());
    }

    // --- Note::remove_link / with_id ---

    #[test]
    fn test_remove_link_present() {
        let mut note = Note::new("1a", "1", "content");
        note.add_link(ID::from("2"));
        assert!(note.remove_link(&ID::from("2")));
        assert!(!note.links().contains(&ID::from("2")));
    }

    #[test]
    fn test_remove_link_absent() {
        let mut note = Note::new("1a", "1", "content");
        assert!(!note.remove_link(&ID::from("99")));
    }

    #[test]
    fn test_with_id_changes_id() {
        let note = Note::new("1", "1", "content");
        let renamed = note.with_id(ID::from("2"));
        assert_eq!(renamed.id(), &ID::from("2"));
        assert_eq!(renamed.content(), "content");
    }

    // --- NoteBox::children ---

    #[test]
    fn test_children_returns_direct_children_only() {
        let mut zk = NoteBox::default();
        zk.add(Note::new("1",    "1",    "root")).unwrap();
        zk.add(Note::new("1a",   "1",    "child a")).unwrap();
        zk.add(Note::new("1b",   "1",    "child b")).unwrap();
        zk.add(Note::new("1a1",  "1a",   "grandchild")).unwrap();

        let children = zk.children(&ID::from("1")).unwrap();
        let ids: Vec<&ID> = children.iter().map(|n| n.id()).collect();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&&ID::from("1a")));
        assert!(ids.contains(&&ID::from("1b")));
    }

    #[test]
    fn test_children_empty_for_leaf() {
        let mut zk = NoteBox::default();
        zk.add(Note::new("1",  "1",  "root")).unwrap();
        zk.add(Note::new("1a", "1",  "child")).unwrap();
        let children = zk.children(&ID::from("1a")).unwrap();
        assert!(children.is_empty());
    }

    // --- NoteBox::ancestors ---

    #[test]
    fn test_ancestors_returns_path_to_root() {
        let mut zk = NoteBox::default();
        zk.add(Note::new("1",    "1",    "root")).unwrap();
        zk.add(Note::new("1a",   "1",    "mid")).unwrap();
        zk.add(Note::new("1a1",  "1a",   "leaf")).unwrap();

        let ancestors = zk.ancestors(&ID::from("1a1")).unwrap();
        let ids: Vec<&ID> = ancestors.iter().map(|n| n.id()).collect();
        assert_eq!(ids, vec![&ID::from("1"), &ID::from("1a")]);
    }

    #[test]
    fn test_ancestors_root_returns_empty() {
        let mut zk = NoteBox::default();
        zk.add(Note::new("1", "1", "root")).unwrap();
        let ancestors = zk.ancestors(&ID::from("1")).unwrap();
        assert!(ancestors.is_empty());
    }

    // --- update preserves cross-links ---

    #[test]
    fn test_update_preserves_cross_links() {
        let mut zk = NoteBox::default();
        zk.add(Note::new("1a", "1", "original")).unwrap();
        // add cross-links to the original
        zk.find_mut(&ID::from("1a")).unwrap().unwrap().add_link(ID::from("2"));
        zk.find_mut(&ID::from("1a")).unwrap().unwrap().add_link(ID::from("3"));

        let child_id = zk.update(&ID::from("1a"), "updated").unwrap();
        let child = zk.find(&child_id).unwrap().unwrap();

        assert_eq!(child.links(), &[ID::from("1a"), ID::from("2"), ID::from("3")],
            "update should copy cross-links from the superseded note");
    }

    // --- NoteBox::superseded_by ---

    #[test]
    fn test_superseded_by_found() {
        let mut zk = NoteBox::default();
        zk.add(Note::new("1",  "1", "original")).unwrap();
        zk.add(Note::new_version("1a", "1", "updated", "1")).unwrap();
        zk.load_all().unwrap();
        assert_eq!(zk.superseded_by(&ID::from("1")), Some(&ID::from("1a")));
    }

    #[test]
    fn test_superseded_by_not_found() {
        let mut zk = NoteBox::default();
        zk.add(Note::new("1", "1", "original")).unwrap();
        zk.load_all().unwrap();
        assert_eq!(zk.superseded_by(&ID::from("1")), None);
    }

}
