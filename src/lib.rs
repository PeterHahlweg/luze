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

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::{collections::{HashMap, HashSet}, env, fs, io, path::{Path, PathBuf}};

pub mod id;
pub use id::ID;

pub mod merge;
pub use merge::{MergeAction, MergeReport, merge_conflicts, rebuild_index};

pub mod git;
pub use git::{SyncReport, git_available, git_run, git_remote, git_has_uncommitted,
              git_current_branch, git_has_upstream, git_unpushed_count, sync};

/// Maximum number of characters allowed in a note's content.
///
/// Notes must be atomic — one indivisible thought. 250 characters is enough
/// for even a complex idea expressed precisely. Content is immutable once
/// written, so this limit is enforced at construction time.
pub const MAX_CONTENT_LEN: usize = 250;

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

/// A single note (slip) in the box.
///
/// Each note has a unique hierarchical [`ID`], freeform text content,
/// and a list of links to other notes. The first link is always the parent note.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Note {
    id: ID,
    content: String,
    links: Vec<ID>,  // first entry is always the parent
    created_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    supersedes: Option<ID>,
}

impl Note {
    /// Creates a new Note with the given `id`, `parent` ID, and `content`.
    ///
    /// The parent is recorded as the first entry in `links`.
    pub fn new(id: impl Into<ID>, parent: impl Into<ID>, content: &str) -> Self {
        Note {
            id: id.into(),
            content: content.into(),
            links: vec![parent.into()],
            created_at: Utc::now(),
            supersedes: None,
        }
    }

    /// Creates a new version of an existing note.
    /// The new note is a child of the superseded note.
    pub fn new_version(id: impl Into<ID>, parent: impl Into<ID>, content: &str, supersedes: impl Into<ID>) -> Self {
        Note {
            id: id.into(),
            content: content.into(),
            links: vec![parent.into()],
            created_at: Utc::now(),
            supersedes: Some(supersedes.into()),
        }
    }

    pub fn id(&self) -> &ID { &self.id }
    pub fn content(&self) -> &str { &self.content }
    pub fn created_at(&self) -> &DateTime<Utc> { &self.created_at }
    /// Returns the parent ID (first link), if any.
    pub fn parent(&self) -> Option<&ID> { self.links.first() }
    pub fn links(&self) -> &[ID] { &self.links }
    pub fn supersedes(&self) -> Option<&ID> { self.supersedes.as_ref() }
    pub fn add_link(&mut self, id: impl Into<ID>) { self.links.push(id.into()); }
    /// Returns a clone of this note with a different ID.
    pub fn with_id(mut self, id: impl Into<ID>) -> Self { self.id = id.into(); self }
    /// Removes the first occurrence of `id` from links. Returns `true` if found.
    pub fn remove_link(&mut self, id: &ID) -> bool {
        if let Some(pos) = self.links.iter().position(|l| l == id) {
            self.links.remove(pos);
            true
        } else {
            false
        }
    }
}

fn draw_file_path(draws_dir: &Path, num: u32) -> PathBuf {
    draws_dir.join(format!("{}.json", num))
}

/// RAII guard returned by [`acquire_write_lock`].
///
/// On Unix the guard holds the open file whose fd carries the `flock(2)` lease;
/// dropping it closes the fd and releases the lock.  On other platforms the
/// guard holds the path of the presence-lock file and deletes it on drop.
pub struct WriteLock {
    _file: fs::File,
    #[cfg(not(unix))]
    _path: PathBuf,
}

#[cfg(not(unix))]
impl Drop for WriteLock {
    fn drop(&mut self) { let _ = fs::remove_file(&self._path); }
}

/// Acquires a global exclusive write lock for the NoteBox.
///
/// Blocks until the lock becomes available. On Unix: `flock(2)` on
/// `writes.lock`. On other platforms: presence-based spin lock (creates
/// `writes.lock` exclusively; deletes it on drop). Acquire before any
/// write operation to prevent concurrent corruption of draw files or the index.
pub fn acquire_write_lock(dir: &Path) -> io::Result<WriteLock> {
    fs::create_dir_all(dir)?;
    acquire_lock_file(&dir.join("writes.lock"))
}

/// Acquires an exclusive lock on `path`.
/// Unix: opens (or creates) the file and calls `flock(LOCK_EX)`.
/// Other: spins on `create_new`, retrying every 10 ms for up to 30 s.
fn acquire_lock_file(path: &Path) -> io::Result<WriteLock> {
    #[cfg(unix)]
    {
        let file = fs::OpenOptions::new().write(true).create(true).open(path)?;
        use std::os::unix::io::AsRawFd;
        extern "C" { fn flock(fd: i32, operation: i32) -> i32; }
        const LOCK_EX: i32 = 2;
        let ret = unsafe { flock(file.as_raw_fd(), LOCK_EX) };
        if ret != 0 { return Err(io::Error::last_os_error()); }
        Ok(WriteLock { _file: file })
    }
    #[cfg(not(unix))]
    {
        use std::io::Write;
        const STALE_SECS: u64 = 30;
        const RETRY_MS:   u64 = 10;

        fn now_secs() -> u64 {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs()
        }

        fn is_stale(path: &Path, stale_secs: u64) -> bool {
            let age = if let Ok(s) = fs::read_to_string(path) {
                if let Ok(t) = s.trim().parse::<u64>() {
                    now_secs().saturating_sub(t)
                } else {
                    // No readable timestamp — fall back to file mtime.
                    fs::metadata(path)
                        .and_then(|m| m.modified())
                        .map(|t| t.elapsed().unwrap_or_default().as_secs())
                        .unwrap_or(0)
                }
            } else {
                0
            };
            age > stale_secs
        }

        loop {
            match fs::OpenOptions::new().write(true).create_new(true).open(path) {
                Ok(mut file) => {
                    let _ = write!(file, "{}", now_secs());
                    return Ok(WriteLock { _file: file, _path: path.to_owned() });
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                    if is_stale(path, STALE_SECS) {
                        eprintln!("warning: removing stale write lock at {}", path.display());
                        let _ = fs::remove_file(path);
                        // retry immediately
                    } else {
                        std::thread::sleep(std::time::Duration::from_millis(RETRY_MS));
                    }
                }
                Err(e) => return Err(e),
            }
        }
    }
}

/// Maximum notes per draw. Reflects the physical capacity of a wooden drawer.
pub const DRAW_CAPACITY: usize = 5_000;

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
    dir: Option<PathBuf>,
    index: HashMap<ID, u32>,   // note ID → draw number
    draws: HashMap<u32, Draw>, // draw number → draw (lazily loaded)
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
    fn assign_draw(&mut self, note: &Note) -> io::Result<u32> {
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

// ── content utilities ─────────────────────────────────────────────────────────

/// Returns the first line of content (the headline).
pub fn headline(content: &str) -> &str {
    content.lines().next().unwrap_or("")
}

/// Validates note content: a single-line note must not exceed 150 characters.
/// Multi-line notes (headline + body) are always accepted.
pub fn validate_content(content: &str) -> Result<(), String> {
    if !content.contains('\n') && content.chars().count() > 150 {
        Err("content is a single line with more than 150 characters; \
             add a newline after the headline to include a longer body".into())
    } else {
        Ok(())
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
