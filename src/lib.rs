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
use std::{collections::HashSet, env, fmt, fs, io, path::{Path, PathBuf}, process::Command};

/// Maximum number of characters allowed in a note's content.
///
/// Notes must be atomic — one indivisible thought. 250 characters is enough
/// for even a complex idea expressed precisely. Content is immutable once
/// written, so this limit is enforced at construction time.
pub const MAX_CONTENT_LEN: usize = 250;

mod json {
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

/// Strips one Luhmann segment from the end of `s`.
/// Returns `s` unchanged if `s` is already a root (single segment).
fn luhmann_parent_str(s: &str) -> &str {
    if s.is_empty() { return s; }
    let is_digit = s.as_bytes().last().unwrap().is_ascii_digit();
    let seg_start = s.rfind(|c: char| c.is_ascii_digit() != is_digit)
        .map_or(0, |i| i + 1);
    if seg_start == 0 { s } else { &s[..seg_start] }
}

/// Compares two Luhmann IDs (no `/`) segment by segment with numeric ordering.
fn cmp_luhmann(mut a: &str, mut b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    loop {
        match (a.is_empty(), b.is_empty()) {
            (true, true) => return Ordering::Equal,
            (true, _)    => return Ordering::Less,
            (_, true)    => return Ordering::Greater,
            _            => {}
        }
        let a_digit = a.as_bytes()[0].is_ascii_digit();
        let b_digit = b.as_bytes()[0].is_ascii_digit();
        let a_end = a.find(|c: char| c.is_ascii_digit() != a_digit).unwrap_or(a.len());
        let b_end = b.find(|c: char| c.is_ascii_digit() != b_digit).unwrap_or(b.len());
        let ord = if a_digit && b_digit {
            let an: u32 = a[..a_end].parse().unwrap();
            let bn: u32 = b[..b_end].parse().unwrap();
            an.cmp(&bn)
        } else {
            a[..a_end].cmp(&b[..b_end])
        };
        match ord {
            Ordering::Equal => { a = &a[a_end..]; b = &b[b_end..]; }
            other           => return other,
        }
    }
}

fn luhmann_next_child(s: &str) -> String {
    let i = s.rfind(|c: char| !c.is_ascii_digit()).map_or(0, |i| i + 1);
    if i == s.len() { format!("{s}1") }
    else { let n: u32 = s[i..].parse().unwrap(); format!("{}{}", &s[..i], n + 1) }
}

fn luhmann_next_sibling(s: &str) -> String {
    let i = s.rfind(|c: char| c.is_ascii_digit()).map_or(0, |i| i + 1);
    if i < s.len() {
        let mut b = s.as_bytes().to_vec();
        *b.last_mut().unwrap() += 1;
        String::from_utf8(b).unwrap()
    } else {
        format!("{s}a")
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

/// A hierarchical Luhmann-style ID encoding a note's position in the tree.
///
/// IDs alternate between numeric and alphabetic segments, e.g. `1a1b2`.
/// This makes the parent–child relationship unambiguous without separators:
/// appending a letter to a number (or a number to a letter) signals a new child level.
///
/// ```text
/// 1
/// ├── 1a          (first child branch of 1)
/// │   ├── 1a1     (first child of 1a)
/// │   │   └── 1a1b  (first child branch of 1a1)
/// │   └── 1a2     (second child of 1a)
/// └── 1b          (next sibling branch after 1a)
/// ```
///
/// IDs serialize as plain strings (`"1a1b2"`) and compare with proper numeric
/// ordering, so `9 < 10` within a segment.
#[derive(Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ID(String);

impl ID {
    /// Returns the root ID of the main NoteBox box: `ZK1`.
    pub fn root(id: &str) -> Self { ID(id.into()) }

    /// Returns the next child ID by incrementing the trailing numeric segment,
    /// or appending `1` if the ID ends with a letter segment.
    /// Operates on the last `/`-section; the section prefix is preserved.
    ///
    /// | Input      | Output    | Meaning               |
    /// |------------|-----------|-----------------------|
    /// | `1a`       | `1a1`     | first child of `1a`   |
    /// | `1c2/1a`   | `1c2/1a1` | first child of `1c2/1a` |
    pub fn next_child(&self) -> Self {
        match self.0.rfind('/') {
            Some(slash) => ID(format!("{}/{}", &self.0[..slash],
                                      luhmann_next_child(&self.0[slash + 1..]))),
            None        => ID(luhmann_next_child(&self.0)),
        }
    }

    /// Returns the next sibling ID by incrementing the trailing letter segment,
    /// or appending `a` if the ID ends with a numeric segment.
    /// Operates on the last `/`-section; the section prefix is preserved.
    ///
    /// | Input      | Output    | Meaning                     |
    /// |------------|-----------|-----------------------------|
    /// | `1`        | `1a`      | first child branch of `1`   |
    /// | `1c2/1a`   | `1c2/1b`  | next branch in section      |
    pub fn next_sibling(&self) -> Self {
        match self.0.rfind('/') {
            Some(slash) => ID(format!("{}/{}", &self.0[..slash],
                                      luhmann_next_sibling(&self.0[slash + 1..]))),
            None        => ID(luhmann_next_sibling(&self.0)),
        }
    }

    /// Strips the last Luhmann segment to infer the parent ID.
    ///
    /// Within a section: `"1c2/3c5f1"` → `"1c2/3c5f"` → ... → `"1c2/3"` → `"1c2"`.
    /// Across sections: `"1c2/1"` → `"1c2"` (first note in section, parent is section root).
    /// No section: `"1a1"` → `"1a"` → `"1"` → `"1"` (root returns itself).
    pub fn parent(&self) -> Self {
        let s = &self.0;
        if s.is_empty() { return self.clone(); }
        match s.rfind('/') {
            Some(slash) => {
                let prefix = &s[..slash];
                let local  = &s[slash + 1..];
                let p = luhmann_parent_str(local);
                if p == local { ID(prefix.to_string()) }
                else          { ID(format!("{}/{}", prefix, p)) }
            }
            None => {
                let p = luhmann_parent_str(s);
                ID(p.to_string())
            }
        }
    }

    /// Returns `true` if `self` is a direct child of `parent`.
    pub fn is_direct_child_of(&self, parent: &ID) -> bool {
        self.parent() == *parent
    }
}

impl From<&str> for ID {
    fn from(s: &str) -> Self { ID(s.into()) }
}

impl From<String> for ID {
    fn from(s: String) -> Self { ID(s) }
}

impl fmt::Display for ID {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(&self.0) }
}

impl fmt::Debug for ID {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { write!(f, "ID({})", self.0) }
}

impl PartialOrd for ID {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> { Some(self.cmp(other)) }
}

impl Ord for ID {
    /// Compares IDs section by section (split by `/`), then segment by segment
    /// within each section with proper numeric ordering.
    ///
    /// A prefix is always less than any of its extensions, so parents sort
    /// before their children regardless of section boundaries.
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        use std::cmp::Ordering;
        let mut a_parts = self.0.split('/');
        let mut b_parts = other.0.split('/');
        loop {
            match (a_parts.next(), b_parts.next()) {
                (None, None)       => return Ordering::Equal,
                (None, _)          => return Ordering::Less,
                (_, None)          => return Ordering::Greater,
                (Some(a), Some(b)) => match cmp_luhmann(a, b) {
                    Ordering::Equal => continue,
                    other           => return other,
                },
            }
        }
    }
}

/// Returns the draw identifier for a note ID.
///
/// IDs are `<draw>/<note>` — the first `/`-section names the draw file.
/// - `"1a/1c1h5"` → `"1a"`
/// - `"1a/1"`     → `"1a"`
fn draw_section(id: &ID) -> &str {
    let s = id.0.as_str();
    match s.find('/') {
        Some(slash) => &s[..slash],
        None        => "",
    }
}

/// Converts a draw ID to a filename stem.
/// Root draw (`""`) → `"1a"`. Others are single Luhmann segments with no `/`.
pub fn draw_filename(id: &ID) -> String {
    if id.0.is_empty() { "1a".into() } else { id.0.clone() }
}

fn draw_file_path(draws_dir: &Path, id: &ID) -> PathBuf {
    draws_dir.join(format!("{}.json", draw_filename(id)))
}

/// Acquires an exclusive advisory lock on the lock file for the draw containing `id`.
///
/// Blocks until the lock becomes available. Returns a [`fs::File`] that holds
/// the lock — dropping it releases the lock automatically. Use in write commands
/// to prevent concurrent agents from corrupting the same draw file:
///
pub fn acquire_draw_lock(dir: &Path, id: &ID) -> io::Result<fs::File> {
    let section = draw_section(id);
    let stem = if section.is_empty() { "root" } else { section };
    let lock_path = dir.join("draws").join(format!("{}.lock", stem));
    fs::create_dir_all(dir.join("draws"))?;
    let file = fs::OpenOptions::new().write(true).create(true).open(&lock_path)?;
    flock_exclusive(&file)?;
    Ok(file)
}

#[cfg(unix)]
fn flock_exclusive(file: &fs::File) -> io::Result<()> {
    use std::os::unix::io::AsRawFd;
    extern "C" { fn flock(fd: i32, operation: i32) -> i32; }
    const LOCK_EX: i32 = 2;
    let ret = unsafe { flock(file.as_raw_fd(), LOCK_EX) };
    if ret == 0 { Ok(()) } else { Err(io::Error::last_os_error()) }
}

#[cfg(not(unix))]
fn flock_exclusive(_file: &fs::File) -> io::Result<()> { Ok(()) }

/// Maximum notes per draw. Reflects the physical capacity of a wooden drawer.
pub const DRAW_CAPACITY: usize = 5_000;

/// A single physical drawer of the NoteBox.
///
/// Notes inside share the same draw prefix (e.g. `"1a"` for notes whose IDs
/// start with `"1a/"`). `notes` is `None` until the draw is loaded from disk;
/// after loading it is `Some(sorted Vec<Note>)`.
#[derive(Debug, PartialEq)]
pub struct Draw {
    id: ID,
    pub notes: Option<Vec<Note>>,
}

impl Draw {
    fn stub(id: impl Into<ID>) -> Self { Draw { id: id.into(), notes: None } }
    pub fn id(&self) -> &ID { &self.id }
    /// Returns loaded notes as a slice, or an empty slice if not yet loaded.
    pub fn notes(&self) -> &[Note] { self.notes.as_deref().unwrap_or(&[]) }
    pub fn len(&self) -> usize { self.notes.as_ref().map_or(0, |n| n.len()) }
    pub fn is_loaded(&self) -> bool { self.notes.is_some() }
    pub fn is_full(&self) -> bool { self.len() >= DRAW_CAPACITY }
}

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
    pub draw: ID,
    pub actions: Vec<MergeAction>,
}

/// A NoteBox backed by a directory of per-draw JSON files.
///
/// `open(dir)` scans `draws/` and creates `Draw` stubs; notes are loaded
/// lazily from `draws/<name>.json` on first access. `save()` writes every
/// draw that has been loaded (and therefore may have changed).
///
/// Pass `dir: None` (via `Default`) for a fully in-memory instance — useful in
/// tests and benchmarks. In that case `ensure_loaded` initialises each draw as
/// an empty `Vec` and `save()` is a no-op.
#[derive(Debug, Default, PartialEq)]
pub struct NoteBox {
    dir: Option<PathBuf>,
    draws: Vec<Draw>,  // sorted by Draw.id for binary search
}

impl NoteBox {
    /// Creates an in-memory NoteBox (no file backing). Useful for tests.
    pub fn new() -> Self { Self::default() }

    /// Creates a new NoteBox backed by `dir` (directory need not exist yet).
    pub fn create(dir: impl Into<PathBuf>) -> Self {
        NoteBox { dir: Some(dir.into()), draws: Vec::new() }
    }

    /// Opens an existing NoteBox from `dir`, discovering draws by scanning
    /// `draws/*.json`. Draw notes are not loaded yet (lazy).
    pub fn open(dir: &Path) -> io::Result<Self> {
        let draws_dir = dir.join("draws");
        let draw_ids: Vec<ID> = if draws_dir.exists() {
            let mut ids = Vec::new();
            for entry in fs::read_dir(&draws_dir)? {
                let entry = entry?;
                let name = entry.file_name();
                let s = name.to_string_lossy();
                if let Some(stem) = s.strip_suffix(".json") {
                    let draw_id = if stem == "root" { ID(String::new()) } else { ID(stem.into()) };
                    ids.push(draw_id);
                }
            }
            ids.sort();
            ids
        } else {
            Vec::new()
        };
        Ok(NoteBox {
            dir: Some(dir.to_owned()),
            draws: draw_ids.into_iter().map(Draw::stub).collect(),
        })
    }

    /// Saves every loaded draw to `dir/draws/<name>.json`.
    /// No-op when there is no backing directory.
    pub fn save(&self) -> io::Result<()> {
        let Some(ref dir) = self.dir else { return Ok(()); };
        let draws_dir = dir.join("draws");
        fs::create_dir_all(&draws_dir)?;
        for draw in &self.draws {
            if let Some(notes) = &draw.notes {
                let json = json::to_string_pretty(notes)
                    .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
                let dest = draw_file_path(&draws_dir, &draw.id);
                let tmp  = dest.with_extension("tmp");
                fs::write(&tmp, json)?;
                fs::rename(&tmp, &dest)?;
            }
        }
        Ok(())
    }

    /// Loads all draws from disk. After this call all draws are in memory and
    /// `&self` methods see the full note set.
    pub fn load_all(&mut self) -> io::Result<()> {
        for di in 0..self.draws.len() {
            self.ensure_loaded(di)?;
        }
        Ok(())
    }

    pub fn draws(&self) -> &[Draw] { &self.draws }

    /// Returns all currently-loaded notes in global ID order.
    /// Call `load_all()` first if you need the complete set.
    pub fn notes(&self) -> Vec<&Note> {
        let mut all: Vec<&Note> = self.draws.iter()
            .filter_map(|d| d.notes.as_ref())
            .flat_map(|n| n.iter())
            .collect();
        all.sort_unstable_by(|a, b| a.id.cmp(&b.id));
        all
    }

    // ── internals ────────────────────────────────────────────────────────────

    fn draw_idx(&self, section: &str) -> Result<usize, usize> {
        self.draws.binary_search_by(|d| d.id.0.as_str().cmp(section))
    }

    /// Loads draw `di` from disk if not already loaded.
    /// For in-memory instances (`dir == None`) initialises to an empty vec.
    fn ensure_loaded(&mut self, di: usize) -> io::Result<()> {
        if self.draws[di].notes.is_some() { return Ok(()); }
        let dir = match self.dir.clone() {
            None    => { self.draws[di].notes = Some(Vec::new()); return Ok(()); }
            Some(d) => d,
        };
        let path = draw_file_path(&dir.join("draws"), &self.draws[di].id.clone());
        let mut notes: Vec<Note> = if path.exists() {
            let json = fs::read_to_string(&path)?;
            json::from_str(&json)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?
        } else {
            Vec::new()
        };
        notes.sort_by(|a, b| a.id.cmp(&b.id));
        self.draws[di].notes = Some(notes);
        Ok(())
    }

    // ── public API ───────────────────────────────────────────────────────────

    /// Finds a note by ID, loading its draw lazily. O(log k + log(n/k)).
    pub fn find(&mut self, id: &ID) -> io::Result<Option<&Note>> {
        let section = draw_section(id).to_string();
        let di = match self.draw_idx(&section) { Ok(i) => i, Err(_) => return Ok(None) };
        self.ensure_loaded(di)?;
        let notes = self.draws[di].notes.as_ref().unwrap();
        Ok(notes.binary_search_by(|n| n.id.cmp(id)).ok().map(|ni| &notes[ni]))
    }

    /// Finds a note by ID (mutable), loading its draw lazily.
    pub fn find_mut(&mut self, id: &ID) -> io::Result<Option<&mut Note>> {
        let section = draw_section(id).to_string();
        let di = match self.draw_idx(&section) { Ok(i) => i, Err(_) => return Ok(None) };
        self.ensure_loaded(di)?;
        let ni = match self.draws[di].notes.as_ref().unwrap().binary_search_by(|n| n.id.cmp(id)) {
            Ok(i) => i, Err(_) => return Ok(None),
        };
        Ok(Some(&mut self.draws[di].notes.as_mut().unwrap()[ni]))
    }

    /// Case-insensitive substring search, skipping superseded notes. Loads all draws.
    /// Results are ranked: headline matches first, body-only matches second.
    pub fn search(&mut self, query: &str) -> io::Result<Vec<&Note>> {
        self.load_all()?;
        let q = query.to_lowercase();
        let superseded: HashSet<&ID> = self.superseded_ids();
        let mut results: Vec<&Note> = self.draws.iter()
            .flat_map(|d| d.notes.as_ref().unwrap().iter())
            .filter(|n| n.content.to_lowercase().contains(&q) && !superseded.contains(&n.id))
            .collect();
        results.sort_by_key(|n| {
            let headline = n.content().lines().next().unwrap_or("");
            if headline.to_lowercase().contains(&q) { 0u8 } else { 1u8 }
        });
        Ok(results)
    }

    /// Direct children of `parent`. Loads all draws.
    pub fn children(&mut self, parent: &ID) -> io::Result<Vec<&Note>> {
        self.load_all()?;
        Ok(self.draws.iter()
            .flat_map(|d| d.notes.as_ref().unwrap().iter())
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
        Ok(self.draws.iter()
            .flat_map(|d| d.notes.as_ref().unwrap().iter())
            .filter(|n| n.links.contains(id))
            .collect())
    }

    /// Inserts a note into its draw (lazy-loaded).
    /// Returns `Err` if the content exceeds [`MAX_CONTENT_LEN`] or the draw is full.
    pub fn add(&mut self, z: Note) -> Result<(), String> {
        if z.content.len() > MAX_CONTENT_LEN {
            return Err(format!(
                "note content exceeds {MAX_CONTENT_LEN} characters ({} chars): a note must express one atomic thought",
                z.content.len()
            ));
        }
        let section = draw_section(&z.id).to_string();
        let di = match self.draw_idx(&section) {
            Ok(i)  => i,
            Err(i) => { self.draws.insert(i, Draw::stub(section.as_str())); i }
        };
        self.ensure_loaded(di).map_err(|e| e.to_string())?;
        if self.draws[di].is_full() {
            let name = if section.is_empty() { "root" } else { &section };
            return Err(format!("draw '{name}' is full ({DRAW_CAPACITY})"));
        }
        let notes = self.draws[di].notes.as_ref().unwrap();
        let pos = notes.partition_point(|n| n.id < z.id);
        if notes.get(pos).map_or(false, |n| n.id == z.id) {
            return Err(format!("note {} already exists", z.id));
        }
        self.draws[di].notes.as_mut().unwrap().insert(pos, z);
        Ok(())
    }

    /// Returns the first child ID of `id` that has no note yet.
    ///
    /// - Letter-ending IDs (e.g. `1c`): tries `1c1`, `1c2`, …
    /// - Digit-ending IDs (e.g. `1a1`): tries `1a1a`, `1a1b`, …
    ///
    /// All draws must be loaded before calling this.
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
        self.draws.iter()
            .flat_map(|d| d.notes.as_deref().unwrap_or(&[]).iter())
            .filter_map(|n| n.supersedes.as_ref())
            .collect()
    }

    /// Returns true if any loaded note has `supersedes == Some(id)`.
    /// All draws must be loaded first (call `load_all()`).
    pub fn is_superseded(&self, id: &ID) -> bool {
        self.draws.iter()
            .flat_map(|d| d.notes.as_deref().unwrap_or(&[]).iter())
            .any(|n| n.supersedes.as_ref() == Some(id))
    }

    /// Returns the ID of the note that supersedes `id`, if any.
    /// All draws must be loaded first.
    pub fn superseded_by(&self, id: &ID) -> Option<&ID> {
        self.draws.iter()
            .flat_map(|d| d.notes.as_deref().unwrap_or(&[]).iter())
            .find(|n| n.supersedes.as_ref() == Some(id))
            .map(|n| &n.id)
    }

    /// Follows the supersedes chain from `id` to the leaf (current version).
    /// Returns `None` if `id` is not found. Returns the note at `id` itself if not superseded.
    /// All draws must be loaded first.
    pub fn current_version(&self, id: &ID) -> Option<&Note> {
        let all: Vec<&Note> = self.draws.iter()
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
        let mut results: Vec<&Note> = self.draws.iter()
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

// ── git conflict resolution ───────────────────────────────────────────────────

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
/// that is not in `taken`.
fn next_available_sibling(id: &ID, taken: &HashSet<ID>) -> ID {
    let mut candidate = id.next_sibling();
    while taken.contains(&candidate) { candidate = candidate.next_sibling(); }
    candidate
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
                    let new_id = next_available_sibling(&original, &taken);
                    taken.insert(new_id.clone());
                    actions.push(MergeAction::Renamed { original, renamed_to: new_id.clone() });
                    let head_note = merged.remove(pos);
                    merged.push(head_note.with_id(new_id));
                    merged.push(their_note);
                } else {
                    // Content conflict — keep head, rename theirs to next sibling.
                    let original = their_note.id().clone();
                    let new_id = next_available_sibling(&original, &taken);
                    taken.insert(new_id.clone());
                    actions.push(MergeAction::Renamed { original, renamed_to: new_id.clone() });
                    merged.push(their_note.with_id(new_id));
                }
            }
        }
    }

    merged.sort_by(|a, b| a.id().cmp(b.id()));
    (merged, actions)
}

/// Scans `dir/draws/` for git-conflicted JSON files and resolves them in place.
///
/// Resolution rules:
/// - Note only in incoming branch → added.
/// - Same ID, same content, different links → link lists unioned.
/// - Same ID, different content → head version kept, their version renamed to
///   the next available sibling ID so both are preserved.
/// - One side removes a note the other kept → the kept version wins.
pub fn merge_conflicts(dir: &Path) -> io::Result<Vec<MergeReport>> {
    merge_conflicts_inner(dir, false)
}

pub fn merge_conflicts_rename_head(dir: &Path) -> io::Result<Vec<MergeReport>> {
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
        let lock_path = draws_dir.join(format!("{}.lock", stem));
        let lock_file = fs::OpenOptions::new().write(true).create(true).open(&lock_path)?;
        flock_exclusive(&lock_file)?;
        let _lock = lock_file;

        let content = fs::read_to_string(&path)?;
        if !has_conflict_markers(&content) { continue; }

        let (head_str, theirs_str) = extract_sides(&content);

        let head_notes: Vec<Note> = json::from_str(&head_str)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData,
                format!("{}: head side: {}", path.display(), e)))?;
        let theirs_notes: Vec<Note> = json::from_str(&theirs_str)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData,
                format!("{}: their side: {}", path.display(), e)))?;

        let draw_id = if stem == "root" { ID(String::new()) } else { ID(stem.to_string()) };

        let (resolved, actions) = merge_note_vecs(head_notes, theirs_notes, rename_head);

        let json = json::to_string_pretty(&resolved)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
        let tmp = path.with_extension("tmp");
        fs::write(&tmp, json)?;
        fs::rename(&tmp, &path)?;

        reports.push(MergeReport { draw: draw_id, actions });
    }

    Ok(reports)
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

// ── git utilities ─────────────────────────────────────────────────────────────

/// Returns `true` if the `git` executable is available on PATH.
pub fn git_available() -> bool {
    Command::new("git").arg("--version").output()
        .map(|o| o.status.success()).unwrap_or(false)
}

/// Runs a git command in `dir`. Returns trimmed stdout on success, trimmed stderr on failure.
pub fn git_run(dir: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git").args(args).current_dir(dir).output()
        .map_err(|e| format!("failed to run git: {}", e))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

/// Returns the name of the first configured remote, if any.
pub fn git_remote(dir: &Path) -> Option<String> {
    git_run(dir, &["remote"]).ok()
        .and_then(|s| s.lines().next().map(|l| l.to_string()))
        .filter(|s| !s.is_empty())
}

/// Returns `true` if there are uncommitted changes in `dir`.
pub fn git_has_uncommitted(dir: &Path) -> bool {
    git_run(dir, &["status", "--porcelain"]).map(|s| !s.is_empty()).unwrap_or(false)
}

/// Returns the current branch name, or `None` if in detached HEAD state.
pub fn git_current_branch(dir: &Path) -> Option<String> {
    git_run(dir, &["rev-parse", "--abbrev-ref", "HEAD"]).ok()
        .filter(|s| !s.is_empty() && s != "HEAD")
}

/// Returns `true` if the current branch has a tracking upstream.
pub fn git_has_upstream(dir: &Path) -> bool {
    git_run(dir, &["rev-parse", "--abbrev-ref", "@{u}"]).is_ok()
}

/// Returns the number of local commits not yet pushed to the upstream.
pub fn git_unpushed_count(dir: &Path) -> usize {
    git_run(dir, &["rev-list", "--count", "@{u}..HEAD"])
        .ok().and_then(|s| s.parse().ok()).unwrap_or(0)
}

// ── sync ──────────────────────────────────────────────────────────────────────

/// Outcome of a [`sync`] call.
pub struct SyncReport {
    /// Number of commits pulled from the remote.
    pub updates: usize,
    /// Short commit hash before the pull (empty if unknown).
    pub commit_before: String,
    /// Short commit hash after the pull.
    pub commit_after: String,
    /// Draw-conflict renames applied during the pull: `(original_id, renamed_to_id)`.
    pub renames: Vec<(ID, ID)>,
}

/// Commits local changes (if any), pulls, resolves draw conflicts, and pushes.
///
/// Returns `Err` if `dir` is not a git repository, has no remote, or a git
/// step fails with a conflict that cannot be resolved automatically.
pub fn sync(dir: &Path, message: &str) -> io::Result<SyncReport> {
    if !dir.join(".git").is_dir() {
        return Err(io::Error::new(io::ErrorKind::NotFound,
            format!("{} is not a git repository", dir.display())));
    }
    let remote = git_remote(dir).ok_or_else(|| {
        io::Error::new(io::ErrorKind::NotFound, "no git remote configured")
    })?;

    // Step 1: commit local changes if any.
    if git_has_uncommitted(dir) {
        git_run(dir, &["add", "-A"])
            .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("git add failed: {}", e)))?;
        git_run(dir, &["commit", "-m", message])
            .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("git commit failed: {}", e)))?;
    }

    let branch  = git_current_branch(dir).unwrap_or_else(|| "main".to_string());
    let tracking = git_has_upstream(dir);

    // Step 2: pull (or fetch+merge on first push when no upstream is set yet).
    let commit_before = git_run(dir, &["rev-parse", "HEAD"]).unwrap_or_default();
    let pull_result = if tracking {
        git_run(dir, &["pull"])
    } else {
        match git_run(dir, &["fetch", &remote, &branch]) {
            Ok(_)  => git_run(dir, &["merge", &format!("{}/{}", remote, branch)]),
            Err(_) => Ok(String::new()), // remote branch doesn't exist yet
        }
    };

    let mut renames = Vec::new();
    if let Err(e) = pull_result {
        let has_conflicts = git_run(dir, &["status", "--porcelain"])
            .map(|s| s.contains("UU")).unwrap_or(false);
        if !has_conflicts {
            return Err(io::Error::new(io::ErrorKind::Other,
                format!("git pull failed: {}", e)));
        }
        let reports = merge_conflicts_rename_head(dir)?;
        if reports.is_empty() {
            return Err(io::Error::new(io::ErrorKind::Other,
                format!("git pull failed with non-draw conflicts: {}", e)));
        }
        for report in &reports {
            for action in &report.actions {
                if let MergeAction::Renamed { original, renamed_to } = action {
                    renames.push((original.clone(), renamed_to.clone()));
                }
            }
        }
        git_run(dir, &["add", "-A"])
            .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("git add after merge failed: {}", e)))?;
        git_run(dir, &["commit", "-m", "luze sync: merge"])
            .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("git commit after merge failed: {}", e)))?;
    }

    let commit_after  = git_run(dir, &["rev-parse", "--short", "HEAD"]).unwrap_or_default();
    let commit_before_short = if commit_before.is_empty() { String::new() }
        else { git_run(dir, &["rev-parse", "--short", &commit_before]).unwrap_or_default() };
    let updates: usize = if commit_before.is_empty() { 0 }
        else { git_run(dir, &["rev-list", "--count", &format!("{}..HEAD", commit_before)])
                   .ok().and_then(|s| s.parse().ok()).unwrap_or(0) };

    // Step 3: push (set upstream tracking on first push).
    if tracking { git_run(dir, &["push"]) } else { git_run(dir, &["push", "-u", &remote, &branch]) }
        .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("git push failed: {}", e)))?;

    Ok(SyncReport { updates, commit_before: commit_before_short, commit_after, renames })
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- ID construction ---

    #[test]
    fn test_id_root() {
        assert_eq!(ID::root("ZK1").to_string(), "ZK1");
    }

    #[test]
    fn test_id_from_str() {
        let id = ID::from("1a1");
        assert_eq!(id.to_string(), "1a1");
    }

    #[test]
    fn test_id_from_string() {
        let id = ID::from("1a1".to_string());
        assert_eq!(id.to_string(), "1a1");
    }

    // --- next_child ---

    #[test]
    fn test_next_child_from_letter_appends_one() {
        // ends with letter → append '1'
        assert_eq!(ID::from("1a").next_child(), ID::from("1a1"));
    }

    #[test]
    fn test_next_child_increments_trailing_number() {
        assert_eq!(ID::from("1a1").next_child(), ID::from("1a2"));
    }

    #[test]
    fn test_next_child_carries_past_nine() {
        assert_eq!(ID::from("1a9").next_child(), ID::from("1a10"));
    }

    // --- next_sibling ---

    #[test]
    fn test_next_sibling_from_number_appends_a() {
        // ends with number → append 'a'
        assert_eq!(ID::from("1").next_sibling(), ID::from("1a"));
    }

    #[test]
    fn test_next_sibling_increments_trailing_letter() {
        assert_eq!(ID::from("1a").next_sibling(), ID::from("1b"));
    }

    #[test]
    fn test_next_sibling_deep() {
        assert_eq!(ID::from("1a1").next_sibling(), ID::from("1a1a"));
    }

    // --- Ord ---

    #[test]
    fn test_id_ord_numeric_not_lexicographic() {
        // lexicographically "9" > "10", but numerically 9 < 10
        assert!(ID::from("9") < ID::from("10"));
    }

    #[test]
    fn test_id_ord_parent_before_child() {
        assert!(ID::from("1")  < ID::from("1a"));
        assert!(ID::from("1a") < ID::from("1a1"));
    }

    #[test]
    fn test_id_ord_siblings_in_order() {
        assert!(ID::from("1a") < ID::from("1b"));
        assert!(ID::from("1a1") < ID::from("1a2"));
    }

    #[test]
    fn test_id_ord_subtree_before_next_sibling() {
        // entire subtree of 1a (including deep children) comes before 1b
        assert!(ID::from("1a99") < ID::from("1b"));
    }

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
        assert_eq!(zk.draws.iter().flat_map(|d| d.notes.as_deref().unwrap_or(&[])).filter(|n| n.id == ID::from("1a")).count(), 1);
    }

    #[test]
    fn test_add_draw_capacity_enforced() {
        let mut zk = NoteBox::default();
        zk.add(Note::new("1", "1", "root")).unwrap();
        assert!(!zk.draws[0].is_full());
    }

    #[test]
    fn test_add_routes_to_correct_draw() {
        let mut zk = NoteBox::default();
        // first '/'-section is the draw name
        zk.add(Note::new("1a/1",    "1a/1",  "index")).unwrap();
        zk.add(Note::new("1a/1a",   "1a/1",  "child")).unwrap();
        zk.add(Note::new("1b/1",    "1b/1",  "other draw")).unwrap();

        assert_eq!(zk.draws.len(), 2);
        assert_eq!(zk.draws[0].id, ID::from("1a"));
        assert_eq!(zk.draws[0].len(), 2);             // "1a/1", "1a/1a"
        assert_eq!(zk.draws[1].id, ID::from("1b"));
        assert_eq!(zk.draws[1].len(), 1);             // "1b/1"
    }

    // --- Sections (/ separator) ---

    #[test]
    fn test_parent_section_first_note() {
        // 1c2/1 is the first note in section rooted at 1c2; parent is 1c2
        assert_eq!(ID::from("1c2/1").parent(), ID::from("1c2"));
    }

    #[test]
    fn test_parent_within_section() {
        // strip last Luhmann segment within the section part
        assert_eq!(ID::from("1c2/3c5f1").parent(), ID::from("1c2/3c5f"));
        assert_eq!(ID::from("1c2/3c5f").parent(),  ID::from("1c2/3c5"));
        assert_eq!(ID::from("1c2/3c5").parent(),   ID::from("1c2/3c"));
        assert_eq!(ID::from("1c2/3c").parent(),    ID::from("1c2/3"));
        assert_eq!(ID::from("1c2/3").parent(),     ID::from("1c2"));
    }

    #[test]
    fn test_parent_nested_section() {
        assert_eq!(ID::from("1c2/4g1/3").parent(), ID::from("1c2/4g1"));
        assert_eq!(ID::from("1c2/4g1/3a").parent(), ID::from("1c2/4g1/3"));
    }

    #[test]
    fn test_is_direct_child_section_boundary() {
        assert!( ID::from("1c2/1").is_direct_child_of(&ID::from("1c2")));
        // 1c2/1a is a grandchild of 1c2 (child of 1c2/1), not direct child
        assert!(!ID::from("1c2/1a").is_direct_child_of(&ID::from("1c2")));
    }

    #[test]
    fn test_ord_section_after_parent() {
        assert!(ID::from("1c2") < ID::from("1c2/1"));
    }

    #[test]
    fn test_ord_section_before_next_sibling() {
        // entire section subtree under 1c2 comes before 1c3
        assert!(ID::from("1c2/1")  < ID::from("1c3"));
        assert!(ID::from("1c2/1a") < ID::from("1c3"));
    }

    #[test]
    fn test_ord_section_sorted_internally() {
        assert!(ID::from("1c2/1")  < ID::from("1c2/1a"));
        assert!(ID::from("1c2/1a") < ID::from("1c2/1b"));
        assert!(ID::from("1c2/9")  < ID::from("1c2/10"));
    }

    #[test]
    fn test_next_child_in_section() {
        assert_eq!(ID::from("1c2/1a").next_child(),   ID::from("1c2/1a1"));
        assert_eq!(ID::from("1c2/1a1").next_child(),  ID::from("1c2/1a2"));
    }

    #[test]
    fn test_next_sibling_in_section() {
        assert_eq!(ID::from("1c2/1").next_sibling(),  ID::from("1c2/1a"));
        assert_eq!(ID::from("1c2/1a").next_sibling(), ID::from("1c2/1b"));
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

    // --- File-based round-trip ---

    #[test]
    fn test_acquire_draw_lock_same_draw_serializes() {
        use std::sync::{Arc, Mutex};
        use std::thread;

        let dir = std::env::temp_dir().join("luze_test_draw_lock");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("draws")).unwrap();

        let id = ID::from("1a/1");
        let order: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));

        // Acquire the lock on the main thread.
        let lock = acquire_draw_lock(&dir, &id).unwrap();
        order.lock().unwrap().push(1);

        let dir2 = dir.clone();
        let order2 = order.clone();
        let handle = thread::spawn(move || {
            // This blocks until the main thread releases.
            let _l = acquire_draw_lock(&dir2, &id).unwrap();
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
    fn test_acquire_draw_lock_different_draws_do_not_block() {
        use std::thread;

        let dir = std::env::temp_dir().join("luze_test_draw_lock_parallel");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("draws")).unwrap();

        let id_a = ID::from("1a/1");
        let id_b = ID::from("1b/1");

        let lock_a = acquire_draw_lock(&dir, &id_a).unwrap();

        let dir2 = dir.clone();
        // Different draw — must not block even though 1a is held.
        let handle = thread::spawn(move || {
            acquire_draw_lock(&dir2, &id_b).unwrap();
        });
        handle.join().unwrap(); // would hang if draws shared a lock

        drop(lock_a);
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

        let draw_path = dir.join("draws").join("1a.json");
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

}
