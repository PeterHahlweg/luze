use std::{collections::{HashMap, HashSet}, fs, io, path::Path};

use crate::{Note, ID, json, lock::acquire_lock_file};

/// One auto-resolved action taken during [`merge_conflicts`].
pub enum MergeAction {
    /// Note existed only in the incoming branch; added without conflict.
    Added(ID),
    /// Same content on both sides; link lists were unioned.
    LinksMerged(ID),
    /// Content conflict: their version was renamed to preserve both.
    Renamed { original: ID, renamed_to: ID },
}

/// Report for one draw file resolved by [`merge_conflicts`].
pub struct MergeReport {
    pub draw: u32,
    pub actions: Vec<MergeAction>,
}

fn has_conflict_markers(s: &str) -> bool {
    s.contains("<<<<<<<")
}

/// Splits a file with git conflict markers into (head, theirs) versions.
/// Lines outside conflict blocks appear in both sides unchanged.
fn extract_sides(content: &str) -> (String, String) {
    enum State { Normal, Head, Theirs }
    let mut head = String::new();
    let mut theirs = String::new();
    let mut state = State::Normal;
    for line in content.lines() {
        match state {
            State::Normal => {
                if line.starts_with("<<<<<<<") { state = State::Head; }
                else { head.push_str(line); head.push('\n');
                       theirs.push_str(line); theirs.push('\n'); }
            }
            State::Head => {
                if line.starts_with("=======") { state = State::Theirs; }
                else { head.push_str(line); head.push('\n'); }
            }
            State::Theirs => {
                if line.starts_with(">>>>>>>") { state = State::Normal; }
                else { theirs.push_str(line); theirs.push('\n'); }
            }
        }
    }
    (head, theirs)
}

/// Returns the first sibling of `id` (same parent, next letter/number slot)
/// that is not in `taken`. Returns `None` if all 26 letter slots are exhausted.
pub(crate) fn next_available_sibling(id: &ID, taken: &HashSet<ID>) -> Option<ID> {
    let mut candidate = id.next_sibling()?;
    while taken.contains(&candidate) {
        candidate = candidate.next_sibling()?;
    }
    Some(candidate)
}

fn merge_note_vecs(head: Vec<Note>, theirs: Vec<Note>, rename_head: bool) -> (Vec<Note>, Vec<MergeAction>) {
    let mut merged = head;
    let mut actions = Vec::new();
    let mut taken: HashSet<ID> = merged.iter().map(|n| n.id().clone()).collect();

    for their_note in theirs {
        match merged.iter().position(|n| n.id() == their_note.id()) {
            None => {
                // Only in theirs — add it.
                taken.insert(their_note.id().clone());
                actions.push(MergeAction::Added(their_note.id().clone()));
                merged.push(their_note);
            }
            Some(pos) => {
                if merged[pos].content() == their_note.content() {
                    // Same content; union the non-parent links.
                    let id = their_note.id().clone();
                    let mut changed = false;
                    for link in their_note.links().iter().skip(1) {
                        if !merged[pos].links().contains(link) {
                            merged[pos].add_link(link.clone());
                            changed = true;
                        }
                    }
                    if changed { actions.push(MergeAction::LinksMerged(id)); }
                    // else: identical — silently dedup
                } else if rename_head {
                    // Content conflict — keep theirs (upstream), rename head (ours).
                    let original = merged[pos].id().clone();
                    if let Some(new_id) = next_available_sibling(&original, &taken) {
                        taken.insert(new_id.clone());
                        actions.push(MergeAction::Renamed { original, renamed_to: new_id.clone() });
                        let head_note = merged.remove(pos);
                        merged.push(head_note.with_id(new_id));
                        merged.push(their_note);
                    }
                    // else: all letter slots exhausted — keep head, silently drop theirs
                } else {
                    // Content conflict — keep head, rename theirs to next sibling.
                    let original = their_note.id().clone();
                    if let Some(new_id) = next_available_sibling(&original, &taken) {
                        taken.insert(new_id.clone());
                        actions.push(MergeAction::Renamed { original, renamed_to: new_id.clone() });
                        merged.push(their_note.with_id(new_id));
                    }
                    // else: all letter slots exhausted — keep head, silently drop theirs
                }
            }
        }
    }

    merged.sort_by(|a, b| a.id().cmp(b.id()));
    (merged, actions)
}

/// Scans `dir/draws/` for git-conflicted JSON files and resolves them in place,
/// then rebuilds `index.json` from the resolved draw files.
///
/// Resolution rules:
/// - Note only in incoming branch → added.
/// - Same ID, same content, different links → link lists unioned.
/// - Same ID, different content → remote version keeps the original ID; local
///   version is renamed to the next available sibling ID so both are preserved.
/// - One side removes a note the other kept → the kept version wins.
pub fn merge_conflicts(dir: &Path) -> io::Result<Vec<MergeReport>> {
    merge_conflicts_inner(dir, true)
}

fn merge_conflicts_inner(dir: &Path, rename_head: bool) -> io::Result<Vec<MergeReport>> {
    let draws_dir = dir.join("draws");
    if !draws_dir.exists() { return Ok(Vec::new()); }

    let mut reports = Vec::new();

    for entry in fs::read_dir(&draws_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") { continue; }

        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        let draw_num: u32 = match stem.parse() {
            Ok(n)  => n,
            Err(_) => continue, // skip non-numeric files (e.g. stale old-format files)
        };

        let lock_path = draws_dir.join(format!("{}.lock", draw_num));
        let _lock = acquire_lock_file(&lock_path)?;

        let content = fs::read_to_string(&path)?;
        if !has_conflict_markers(&content) { continue; }

        let (head_str, theirs_str) = extract_sides(&content);

        let head_notes: Vec<Note> = json::from_str(&head_str)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData,
                format!("{}: head side: {}", path.display(), e)))?;
        let theirs_notes: Vec<Note> = json::from_str(&theirs_str)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData,
                format!("{}: their side: {}", path.display(), e)))?;

        let (resolved, actions) = merge_note_vecs(head_notes, theirs_notes, rename_head);

        let json = json::to_string_pretty(&resolved)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
        let tmp = path.with_extension("tmp");
        fs::write(&tmp, json)?;
        fs::rename(&tmp, &path)?;

        reports.push(MergeReport { draw: draw_num, actions });
    }

    // Rebuild index from all resolved draw files
    rebuild_index(dir)?;

    Ok(reports)
}

/// Rebuilds `index.json` by scanning all draw files in `dir/draws/`.
pub fn rebuild_index(dir: &Path) -> io::Result<()> {
    let draws_dir = dir.join("draws");
    let mut index: HashMap<ID, u32> = HashMap::new();
    if draws_dir.exists() {
        for entry in fs::read_dir(&draws_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") { continue; }
            let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            let draw_num: u32 = match stem.parse() { Ok(n) => n, Err(_) => continue };
            let content = fs::read_to_string(&path)?;
            let notes: Vec<Note> = json::from_str(&content)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            for note in notes {
                index.insert(note.id, draw_num);
            }
        }
    }
    let index_json = json::to_string_pretty(&index)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
    let index_path = dir.join("index.json");
    let tmp = dir.join("index.tmp");
    fs::write(&tmp, &index_json)?;
    fs::rename(&tmp, &index_path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Note, NoteBox, ID};

    #[test]
    fn test_next_available_sibling_exhausted_letters_returns_none() {
        let taken: HashSet<ID> = ('a'..='z')
            .map(|c| ID::from(format!("1{c}").as_str()))
            .collect();
        assert_eq!(next_available_sibling(&ID::from("1z"), &taken), None);
    }

    // --- extract_sides / has_conflict_markers ---

    #[test]
    fn test_has_conflict_markers_positive() {
        assert!(has_conflict_markers("<<<<<<< HEAD\nfoo\n=======\nbar\n>>>>>>> theirs"));
    }

    #[test]
    fn test_has_conflict_markers_negative() {
        assert!(!has_conflict_markers(r#"[{"id":"1","content":"hi"}]"#));
    }

    #[test]
    fn test_extract_sides_splits_correctly() {
        let input = "pre\n<<<<<<< HEAD\nhead_line\n=======\ntheir_line\n>>>>>>> branch\npost\n";
        let (head, theirs) = extract_sides(input);
        assert!(head.contains("pre\n"));
        assert!(head.contains("head_line\n"));
        assert!(head.contains("post\n"));
        assert!(!head.contains("their_line"));

        assert!(theirs.contains("pre\n"));
        assert!(theirs.contains("their_line\n"));
        assert!(theirs.contains("post\n"));
        assert!(!theirs.contains("head_line"));
    }

    // --- merge_note_vecs ---

    #[test]
    fn test_merge_note_vecs_added_from_theirs() {
        let head = vec![Note::new("1", "1", "root")];
        let theirs = vec![
            Note::new("1",  "1", "root"),
            Note::new("1a", "1", "only in theirs"),
        ];
        let (merged, actions) = merge_note_vecs(head, theirs, false);
        assert_eq!(merged.len(), 2);
        assert!(matches!(&actions[0], MergeAction::Added(id) if id == &ID::from("1a")));
    }

    #[test]
    fn test_merge_note_vecs_same_content_unions_links() {
        let mut head_note = Note::new("1", "1", "same");
        head_note.add_link(ID::from("2"));
        let mut their_note = Note::new("1", "1", "same");
        their_note.add_link(ID::from("3"));

        let (merged, actions) = merge_note_vecs(vec![head_note], vec![their_note], false);
        assert_eq!(merged.len(), 1);
        assert!(merged[0].links().contains(&ID::from("3")));
        assert!(matches!(&actions[0], MergeAction::LinksMerged(id) if id == &ID::from("1")));
    }

    #[test]
    fn test_merge_note_vecs_same_content_same_links_no_action() {
        let head = vec![Note::new("1", "1", "same")];
        let theirs = vec![Note::new("1", "1", "same")];
        let (merged, actions) = merge_note_vecs(head, theirs, false);
        assert_eq!(merged.len(), 1);
        assert!(actions.is_empty());
    }

    #[test]
    fn test_merge_note_vecs_content_conflict_renames_theirs() {
        let head = vec![Note::new("1", "1", "head version")];
        let theirs = vec![Note::new("1", "1", "their version")];
        let (merged, actions) = merge_note_vecs(head, theirs, false);
        assert_eq!(merged.len(), 2);
        assert!(matches!(&actions[0], MergeAction::Renamed { original, renamed_to }
            if original == &ID::from("1") && renamed_to == &ID::from("1a")));
    }

    #[test]
    fn test_merge_note_vecs_content_conflict_rename_head_mode() {
        let head = vec![Note::new("1", "1", "head version")];
        let theirs = vec![Note::new("1", "1", "their version")];
        let (merged, actions) = merge_note_vecs(head, theirs, true);
        assert_eq!(merged.len(), 2);
        // In rename_head mode: theirs keeps the original ID, head gets renamed.
        assert!(matches!(&actions[0], MergeAction::Renamed { original, .. }
            if original == &ID::from("1")));
        // The original ID must be held by the "their" content.
        assert!(merged.iter().any(|n| n.id() == &ID::from("1") && n.content() == "their version"));
    }

    // --- rebuild_index ---

    #[test]
    fn test_rebuild_index_recreates_from_draws() {
        let dir = std::env::temp_dir().join("luze_test_rebuild_index");
        let _ = std::fs::remove_dir_all(&dir);

        let mut zk = NoteBox::create(&dir);
        zk.add(Note::new("1",  "1",  "root")).unwrap();
        zk.add(Note::new("1a", "1",  "child")).unwrap();
        zk.save().unwrap();

        // Delete index and rebuild.
        std::fs::remove_file(dir.join("index.json")).unwrap();
        assert!(!dir.join("index.json").exists());

        rebuild_index(&dir).unwrap();
        assert!(dir.join("index.json").exists());

        // NoteBox should work again after rebuild.
        let mut loaded = NoteBox::open(&dir).unwrap();
        assert!(loaded.find(&ID::from("1")).unwrap().is_some());
        assert!(loaded.find(&ID::from("1a")).unwrap().is_some());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    // --- merge_conflicts ---

    fn make_conflicted_draw(dir: &std::path::Path, draw_num: u32, head: &Vec<Note>, theirs: &Vec<Note>) {
        let draws_dir = dir.join("draws");
        std::fs::create_dir_all(&draws_dir).unwrap();
        let head_json  = crate::json::to_string_pretty(head).unwrap();
        let their_json = crate::json::to_string_pretty(theirs).unwrap();
        let content = format!(
            "<<<<<<< HEAD\n{}\n=======\n{}\n>>>>>>> branch\n",
            head_json, their_json
        );
        std::fs::write(draws_dir.join(format!("{}.json", draw_num)), content).unwrap();
    }

    #[test]
    fn test_merge_conflicts_resolves_added_note() {
        let dir = std::env::temp_dir().join("luze_test_merge_added");
        let _ = std::fs::remove_dir_all(&dir);

        let head   = vec![Note::new("1", "1", "root")];
        let theirs = vec![Note::new("1", "1", "root"), Note::new("1a", "1", "new")];
        make_conflicted_draw(&dir, 0, &head, &theirs);

        let reports = merge_conflicts(&dir).unwrap();
        assert_eq!(reports.len(), 1);
        assert!(matches!(&reports[0].actions[0], MergeAction::Added(id) if id == &ID::from("1a")));

        let mut zk = NoteBox::open(&dir).unwrap();
        assert!(zk.find(&ID::from("1a")).unwrap().is_some());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn test_merge_conflicts_resolves_content_conflict() {
        let dir = std::env::temp_dir().join("luze_test_merge_conflict");
        let _ = std::fs::remove_dir_all(&dir);

        let head   = vec![Note::new("1", "1", "head version")];
        let theirs = vec![Note::new("1", "1", "their version")];
        make_conflicted_draw(&dir, 0, &head, &theirs);

        let reports = merge_conflicts(&dir).unwrap();
        assert_eq!(reports.len(), 1);
        assert!(matches!(&reports[0].actions[0], MergeAction::Renamed { .. }));

        // Remote keeps the original ID; local is renamed.
        let mut zk = NoteBox::open(&dir).unwrap();
        zk.load_all().unwrap();
        assert_eq!(zk.notes().len(), 2);
        let at_original = zk.find(&ID::from("1")).unwrap().unwrap();
        assert_eq!(at_original.content(), "their version");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn test_merge_conflicts_no_conflict_markers_skipped() {
        let dir = std::env::temp_dir().join("luze_test_merge_skip");
        let _ = std::fs::remove_dir_all(&dir);

        // Write a clean draw (no conflict markers).
        let mut zk = NoteBox::create(&dir);
        zk.add(Note::new("1", "1", "clean")).unwrap();
        zk.save().unwrap();

        let reports = merge_conflicts(&dir).unwrap();
        assert!(reports.is_empty());

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
