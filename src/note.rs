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
/// and a list of links to other notes. The first link is always the parent note.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Note {
    pub(crate) id: ID,
    pub(crate) content: String,
    pub(crate) links: Vec<ID>,  // first entry is always the parent
    pub(crate) created_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub(crate) supersedes: Option<ID>,
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
