use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ID;

/// Maximum number of characters allowed in a note's content.
///
/// Notes must be atomic — one indivisible thought. 250 characters is enough
/// for even a complex idea expressed precisely. Content is immutable once
/// written, so this limit is enforced at construction time.
pub const MAX_CONTENT_LEN: usize = 250;

/// A single note (slip) in the box.
///
/// Each note has a unique hierarchical [`ID`], freeform text content,
/// a list of links to other notes (first link is always the parent),
/// and an optional list of tags.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Note {
    pub(crate) id: ID,
    pub(crate) content: String,
    pub(crate) links: Vec<ID>,  // first entry is always the parent
    pub(crate) created_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub(crate) supersedes: Option<ID>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub(crate) tags: Vec<String>,
}

/// Normalises a tag: strips a leading `#` and lowercases.
fn normalize_tag(tag: &str) -> String {
    tag.trim_start_matches('#').to_lowercase()
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
            tags: Vec::new(),
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
            tags: Vec::new(),
        }
    }

    pub fn id(&self) -> &ID { &self.id }
    pub fn content(&self) -> &str { &self.content }
    pub fn created_at(&self) -> &DateTime<Utc> { &self.created_at }
    /// Returns the parent ID (first link), if any.
    pub fn parent(&self) -> Option<&ID> { self.links.first() }
    pub fn links(&self) -> &[ID] { &self.links }
    pub fn supersedes(&self) -> Option<&ID> { self.supersedes.as_ref() }
    pub fn tags(&self) -> &[String] { &self.tags }

    pub fn add_link(&mut self, id: impl Into<ID>) { self.links.push(id.into()); }

    /// Adds a tag (normalised: lowercase, leading `#` stripped). No-op if already present.
    pub fn add_tag(&mut self, tag: &str) {
        let t = normalize_tag(tag);
        if !self.tags.contains(&t) {
            self.tags.push(t);
            self.tags.sort();
        }
    }

    /// Removes a tag. Returns `true` if it was present.
    pub fn remove_tag(&mut self, tag: &str) -> bool {
        let t = normalize_tag(tag);
        if let Some(pos) = self.tags.iter().position(|x| x == &t) {
            self.tags.remove(pos);
            true
        } else {
            false
        }
    }

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_add_tag_normalises_hash_prefix() {
        let mut note = Note::new("1", "1", "content");
        note.add_tag("#rust");
        assert_eq!(note.tags(), &["rust"]);
    }

    #[test]
    fn test_add_tag_normalises_case() {
        let mut note = Note::new("1", "1", "content");
        note.add_tag("Rust");
        assert_eq!(note.tags(), &["rust"]);
    }

    #[test]
    fn test_add_tag_deduplicates() {
        let mut note = Note::new("1", "1", "content");
        note.add_tag("rust");
        note.add_tag("#rust");
        assert_eq!(note.tags(), &["rust"]);
    }

    #[test]
    fn test_add_tag_sorted() {
        let mut note = Note::new("1", "1", "content");
        note.add_tag("zig");
        note.add_tag("ada");
        assert_eq!(note.tags(), &["ada", "zig"]);
    }

    #[test]
    fn test_remove_tag_present() {
        let mut note = Note::new("1", "1", "content");
        note.add_tag("rust");
        assert!(note.remove_tag("rust"));
        assert!(note.tags().is_empty());
    }

    #[test]
    fn test_remove_tag_absent() {
        let mut note = Note::new("1", "1", "content");
        assert!(!note.remove_tag("rust"));
    }

    #[test]
    fn test_remove_tag_normalises() {
        let mut note = Note::new("1", "1", "content");
        note.add_tag("rust");
        assert!(note.remove_tag("#Rust"));
        assert!(note.tags().is_empty());
    }
}

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
