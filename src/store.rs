use std::{collections::{HashMap, HashSet}, fs, io, path::{Path, PathBuf}};

use crate::{Note, ID, json, MAX_CONTENT_LEN};

/// Maximum notes per draw. Reflects the physical capacity of a wooden drawer.
pub const DRAW_CAPACITY: usize = 5_000;

fn draw_file_path(draws_dir: &Path, num: u32) -> PathBuf {
    draws_dir.join(format!("{}.json", num))
}

/// A single physical drawer of the NoteBox.
///
/// Identified by a decimal number (`num`). Notes are stored in
/// `draws/<num>.json` and loaded lazily on first access.
#[derive(Debug, PartialEq)]
pub struct Draw {
    pub num: u32,
    pub notes: Option<Vec<Note>>,
}

impl Draw {
    fn new(num: u32) -> Self { Draw { num, notes: None } }
    pub fn num(&self) -> u32 { self.num }
    /// Returns loaded notes as a slice, or an empty slice if not yet loaded.
    pub fn notes(&self) -> &[Note] { self.notes.as_deref().unwrap_or(&[]) }
    pub fn len(&self) -> usize { self.notes.as_ref().map_or(0, |n| n.len()) }
    pub fn is_loaded(&self) -> bool { self.notes.is_some() }
    pub fn is_full(&self) -> bool { self.len() >= DRAW_CAPACITY }
}

/// A NoteBox backed by a directory of per-draw JSON files and an index.
///
/// `open(dir)` loads `index.json` (a `HashMap<ID, u32>` mapping every note to
/// its draw number) and creates `Draw` stubs. Draw notes are loaded lazily
/// from `draws/<num>.json` on first access. `save()` writes modified draws
/// and updates `index.json` atomically.
///
/// Pass no directory (via `Default`) for a fully in-memory instance — useful
/// in tests and benchmarks. `save()` is a no-op in that case.
#[derive(Debug, Default, PartialEq)]
pub struct NoteBox {
    pub(crate) dir: Option<PathBuf>,
    pub(crate) index: HashMap<ID, u32>,   // note ID → draw number
    pub(crate) draws: HashMap<u32, Draw>, // draw number → draw (lazily loaded)
}

impl NoteBox {
    /// Creates an in-memory NoteBox (no file backing). Useful for tests.
    pub fn new() -> Self { Self::default() }

    /// Creates a new NoteBox backed by `dir` (directory need not exist yet).
    pub fn create(dir: impl Into<PathBuf>) -> Self {
        NoteBox { dir: Some(dir.into()), index: HashMap::new(), draws: HashMap::new() }
    }

    /// Opens an existing NoteBox from `dir`, loading `index.json` eagerly.
    /// Draw notes are not loaded yet (lazy).
    pub fn open(dir: &Path) -> io::Result<Self> {
        let index_path = dir.join("index.json");
        let index: HashMap<ID, u32> = if index_path.exists() {
            let json = fs::read_to_string(&index_path)?;
            json::from_str(&json)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?
        } else {
            HashMap::new()
        };
        let draw_nums: HashSet<u32> = index.values().copied().collect();
        let draws: HashMap<u32, Draw> = draw_nums.into_iter()
            .map(|n| (n, Draw::new(n)))
            .collect();
        Ok(NoteBox { dir: Some(dir.to_owned()), index, draws })
    }

    /// Saves every modified draw to `dir/draws/<num>.json` and writes `index.json`.
    /// No-op when there is no backing directory.
    pub fn save(&self) -> io::Result<()> {
        let Some(ref dir) = self.dir else { return Ok(()); };
        let draws_dir = dir.join("draws");
        fs::create_dir_all(&draws_dir)?;
        for (num, draw) in &self.draws {
            if let Some(notes) = &draw.notes {
                let json = json::to_string_pretty(notes)
                    .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
                let dest = draw_file_path(&draws_dir, *num);
                let tmp  = dest.with_extension("tmp");
                fs::write(&tmp, &json)?;
                fs::rename(&tmp, &dest)?;
            }
        }
        // Save index atomically
        let index_json = json::to_string_pretty(&self.index)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
        let index_path = dir.join("index.json");
        let tmp = dir.join("index.tmp");
        fs::write(&tmp, &index_json)?;
        fs::rename(&tmp, &index_path)?;
        Ok(())
    }

    /// Loads all draws from disk. After this call all draws are in memory and
    /// `&self` methods see the full note set.
    pub fn load_all(&mut self) -> io::Result<()> {
        let nums: Vec<u32> = self.draws.keys().copied().collect();
        for num in nums { self.ensure_loaded(num)?; }
        Ok(())
    }

    pub fn draws(&self) -> &HashMap<u32, Draw> { &self.draws }

    /// Returns all currently-loaded notes in global ID order.
    /// Call `load_all()` first if you need the complete set.
    pub fn notes(&self) -> Vec<&Note> {
        let mut all: Vec<&Note> = self.draws.values()
            .filter_map(|d| d.notes.as_ref())
            .flat_map(|n| n.iter())
            .collect();
        all.sort_unstable_by(|a, b| a.id.cmp(&b.id));
        all
    }

    // ── internals ────────────────────────────────────────────────────────────

    /// Loads draw `num` from disk if not already loaded.
    /// For in-memory instances (`dir == None`) initialises to an empty vec.
    fn ensure_loaded(&mut self, num: u32) -> io::Result<()> {
        if self.draws.get(&num).map_or(false, |d| d.notes.is_some()) { return Ok(()); }
        let dir = match self.dir.clone() {
            None => {
                self.draws.entry(num).or_insert_with(|| Draw::new(num)).notes = Some(Vec::new());
                return Ok(());
            }
            Some(d) => d,
        };
        let path = draw_file_path(&dir.join("draws"), num);
        let mut notes: Vec<Note> = if path.exists() {
            let json = fs::read_to_string(&path)?;
            json::from_str(&json)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?
        } else {
            Vec::new()
        };
        notes.sort_by(|a, b| a.id.cmp(&b.id));
        self.draws.entry(num).or_insert_with(|| Draw::new(num)).notes = Some(notes);
        Ok(())
    }

    /// Assigns a draw number for a new note.
    /// Prefers the parent's draw (to keep related notes together).
    /// Falls back to any non-full draw, or creates a new one.
    pub(crate) fn assign_draw(&mut self, note: &Note) -> io::Result<u32> {
        // Prefer parent's draw
        if let Some(parent) = note.parent() {
            if let Some(&pnum) = self.index.get(parent) {
                self.ensure_loaded(pnum)?;
                if !self.draws[&pnum].is_full() { return Ok(pnum); }
            }
        }
        // Any non-full existing draw
        let nums: Vec<u32> = self.draws.keys().copied().collect();
        for num in nums {
            self.ensure_loaded(num)?;
            if !self.draws[&num].is_full() { return Ok(num); }
        }
        // All full — open a new draw
        let new_num = self.draws.keys().copied().max().unwrap_or(0) + 1;
        self.draws.insert(new_num, Draw::new(new_num));
        self.ensure_loaded(new_num)?;
        Ok(new_num)
    }

    // ── public API ───────────────────────────────────────────────────────────

    /// Finds a note by ID via the index, loading its draw lazily.
    pub fn find(&mut self, id: &ID) -> io::Result<Option<&Note>> {
        let num = match self.index.get(id).copied() {
            Some(n) => n,
            None    => return Ok(None),
        };
        self.ensure_loaded(num)?;
        let notes = self.draws[&num].notes.as_ref().unwrap();
        Ok(notes.binary_search_by(|n| n.id.cmp(id)).ok().map(|ni| &notes[ni]))
    }

    /// Finds a note by ID (mutable), loading its draw lazily.
    pub fn find_mut(&mut self, id: &ID) -> io::Result<Option<&mut Note>> {
        let num = match self.index.get(id).copied() {
            Some(n) => n,
            None    => return Ok(None),
        };
        self.ensure_loaded(num)?;
        let ni = match self.draws[&num].notes.as_ref().unwrap()
            .binary_search_by(|n| n.id.cmp(id))
        {
            Ok(i)  => i,
            Err(_) => return Ok(None),
        };
        Ok(Some(&mut self.draws.get_mut(&num).unwrap().notes.as_mut().unwrap()[ni]))
    }

    /// Case-insensitive substring search, skipping superseded notes. Loads all draws.
    /// Results are ranked: headline matches first, body-only matches second.
    pub fn search(&mut self, query: &str) -> io::Result<Vec<&Note>> {
        self.load_all()?;
        let q = query.to_lowercase();
        let superseded: HashSet<&ID> = self.superseded_ids();
        let mut results: Vec<&Note> = self.draws.values()
            .flat_map(|d| d.notes.as_ref().unwrap().iter())
            .filter(|n| n.content.to_lowercase().contains(&q) && !superseded.contains(&n.id))
            .collect();
        results.sort_by_key(|n| {
            let headline = n.content().lines().next().unwrap_or("");
            if headline.to_lowercase().contains(&q) { 0u8 } else { 1u8 }
        });
        Ok(results)
    }

    /// Direct children of `parent`. Only loads draws that contain children.
    pub fn children(&mut self, parent: &ID) -> io::Result<Vec<&Note>> {
        let draw_nums: Vec<u32> = self.index.iter()
            .filter(|(id, _)| id.is_direct_child_of(parent))
            .map(|(_, &num)| num)
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        for num in draw_nums {
            self.ensure_loaded(num)?;
        }
        Ok(self.draws.values()
            .filter_map(|d| d.notes.as_ref())
            .flat_map(|notes| notes.iter())
            .filter(|n| n.id.is_direct_child_of(parent))
            .collect())
    }

    /// Breadcrumb path to `id` (exclusive). Loads ancestor draws lazily;
    /// returns owned `Note`s to avoid holding multiple mutable borrows.
    pub fn ancestors(&mut self, id: &ID) -> io::Result<Vec<Note>> {
        let mut result: Vec<Note> = Vec::new();
        let mut current = id.parent();
        loop {
            if current == *id { break; }
            if let Some(note) = self.find(&current)? { result.push(note.clone()); }
            let parent = current.parent();
            if parent == current { break; }
            current = parent;
        }
        result.reverse();
        Ok(result)
    }

    /// Notes that link to `id`. Loads all draws.
    pub fn backlinks(&mut self, id: &ID) -> io::Result<Vec<&Note>> {
        self.load_all()?;
        Ok(self.draws.values()
            .flat_map(|d| d.notes.as_ref().unwrap().iter())
            .filter(|n| n.links.iter().skip(1).any(|l| l == id))
            .collect())
    }

    /// Inserts a note, assigning it to a draw automatically.
    /// Returns `Err` if the content exceeds [`MAX_CONTENT_LEN`] or the ID already exists.
    pub fn add(&mut self, z: Note) -> Result<(), String> {
        let char_count = z.content.chars().count();
        if char_count > MAX_CONTENT_LEN {
            return Err(format!(
                "note content exceeds {MAX_CONTENT_LEN} characters ({char_count} chars): a note must express one atomic thought"
            ));
        }
        if self.index.contains_key(&z.id) {
            return Err(format!("note {} already exists", z.id));
        }
        let num = self.assign_draw(&z).map_err(|e| e.to_string())?;
        let notes = self.draws[&num].notes.as_ref().unwrap();
        let pos = notes.partition_point(|n| n.id < z.id);
        self.index.insert(z.id.clone(), num);
        self.draws.get_mut(&num).unwrap().notes.as_mut().unwrap().insert(pos, z);
        Ok(())
    }

    /// Returns the first child ID of `id` that has no note yet.
    ///
    /// - Letter-ending IDs (e.g. `1c`): tries `1c1`, `1c2`, …
    /// - Digit-ending IDs (e.g. `1a1`): tries `1a1a`, `1a1b`, …
    fn first_available_child(&mut self, id: &ID) -> io::Result<ID> {
        let s = id.0.clone();
        if s.as_bytes().last().map_or(false, |b| b.is_ascii_digit()) {
            for c in b'a'..=b'z' {
                let candidate = ID(format!("{}{}", s, c as char));
                if self.find(&candidate)?.is_none() { return Ok(candidate); }
            }
            Err(io::Error::new(io::ErrorKind::Other,
                format!("no available child slot for {}: all 26 letter slots taken", id)))
        } else {
            for n in 1u32.. {
                let candidate = ID(format!("{}{}", s, n));
                if self.find(&candidate)?.is_none() { return Ok(candidate); }
            }
            unreachable!()
        }
    }

    /// Returns the set of all IDs that have been superseded by another note.
    /// All draws must be loaded first.
    pub fn superseded_ids(&self) -> HashSet<&ID> {
        self.draws.values()
            .flat_map(|d| d.notes.as_deref().unwrap_or(&[]).iter())
            .filter_map(|n| n.supersedes.as_ref())
            .collect()
    }

    /// Returns true if any loaded note has `supersedes == Some(id)`.
    /// All draws must be loaded first (call `load_all()`).
    pub fn is_superseded(&self, id: &ID) -> bool {
        self.draws.values()
            .flat_map(|d| d.notes.as_deref().unwrap_or(&[]).iter())
            .any(|n| n.supersedes.as_ref() == Some(id))
    }

    /// Returns the ID of the note that supersedes `id`, if any.
    /// All draws must be loaded first.
    pub fn superseded_by(&self, id: &ID) -> Option<&ID> {
        self.draws.values()
            .flat_map(|d| d.notes.as_deref().unwrap_or(&[]).iter())
            .find(|n| n.supersedes.as_ref() == Some(id))
            .map(|n| &n.id)
    }

    /// Follows the supersedes chain from `id` to the leaf (current version).
    /// Returns `None` if `id` is not found. Returns the note at `id` itself if not superseded.
    /// All draws must be loaded first.
    pub fn current_version(&self, id: &ID) -> Option<&Note> {
        let all: Vec<&Note> = self.draws.values()
            .flat_map(|d| d.notes.as_deref().unwrap_or(&[]).iter())
            .collect();
        let mut current = all.iter().find(|n| n.id == *id).copied()?;
        loop {
            match all.iter().find(|n| n.supersedes.as_ref() == Some(&current.id)) {
                Some(next) => current = next,
                None => break,
            }
        }
        Some(current)
    }

    /// Case-insensitive substring search, including superseded notes.
    /// Results are ranked: headline matches first, body-only matches second.
    pub fn search_all(&mut self, query: &str) -> io::Result<Vec<&Note>> {
        self.load_all()?;
        let q = query.to_lowercase();
        let mut results: Vec<&Note> = self.draws.values()
            .flat_map(|d| d.notes.as_ref().unwrap().iter())
            .filter(|n| n.content.to_lowercase().contains(&q))
            .collect();
        results.sort_by_key(|n| {
            let headline = n.content().lines().next().unwrap_or("");
            if headline.to_lowercase().contains(&q) { 0u8 } else { 1u8 }
        });
        Ok(results)
    }

    /// Creates a new child note that supersedes `id`.
    /// Returns the new note's ID, or an error if:
    /// - `id` doesn't exist
    /// - `id` is already superseded (linear chain enforced)
    pub fn update(&mut self, id: &ID, new_content: &str) -> io::Result<ID> {
        self.load_all()?;

        if self.find(id)?.is_none() {
            return Err(io::Error::new(io::ErrorKind::NotFound,
                format!("note {} not found", id)));
        }

        if self.is_superseded(id) {
            return Err(io::Error::new(io::ErrorKind::AlreadyExists,
                format!("{} is already superseded", id)));
        }

        let child_id = self.first_available_child(id)?;
        let note = Note::new_version(child_id.clone(), id.clone(), new_content, id.clone());
        self.add(note).map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
        Ok(child_id)
    }
}

/// Returns `true` if `dir` contains an old-format NoteBox that needs migration
/// (a `draws/` subdirectory exists but `index.json` does not).
pub fn needs_migration(dir: &Path) -> bool {
    dir.join("draws").is_dir() && !dir.join("index.json").exists()
}

/// Migrates an old-format NoteBox (named-section draw files, no `index.json`)
/// to the current format (decimal-numbered draw files + `index.json`).
///
/// Returns the number of notes migrated, or `0` if the directory is already
/// in the new format or contains no draw files.
pub fn migrate(dir: &Path) -> io::Result<usize> {
    // Already new format — nothing to do.
    if dir.join("index.json").exists() { return Ok(0); }

    let draws_dir = dir.join("draws");
    if !draws_dir.exists() { return Ok(0); }

    // Read all JSON files from the old draws directory.
    let mut all_notes: Vec<Note> = Vec::new();
    let mut old_paths: Vec<PathBuf> = Vec::new();

    for entry in fs::read_dir(&draws_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") { continue; }
        let json = fs::read_to_string(&path)?;
        let notes: Vec<Note> = json::from_str(&json)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        all_notes.extend(notes);
        old_paths.push(path);
    }

    if all_notes.is_empty() { return Ok(0); }

    // Sort by ID so related notes land in the same draw.
    all_notes.sort_by(|a, b| a.id.cmp(&b.id));
    let count = all_notes.len();

    // Build new NoteBox and insert all notes, bypassing content-length validation
    // (notes were already accepted when originally written).
    let mut zk = NoteBox::create(dir);
    for note in all_notes {
        let num = zk.assign_draw(&note)?;
        let notes_vec = zk.draws[&num].notes.as_ref().unwrap();
        let pos = notes_vec.partition_point(|n| n.id < note.id);
        zk.index.insert(note.id.clone(), num);
        zk.draws.get_mut(&num).unwrap().notes.as_mut().unwrap().insert(pos, note);
    }

    // Write new-format draw files and index.json.
    zk.save()?;

    // Remove old draw files only after successful save.
    for path in old_paths {
        fs::remove_file(&path)?;
    }

    Ok(count)
}
