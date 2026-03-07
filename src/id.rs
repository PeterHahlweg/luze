use std::{cmp, fmt};
use serde::{Deserialize, Serialize};

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
fn cmp_luhmann(mut a: &str, mut b: &str) -> cmp::Ordering {
    loop {
        match (a.is_empty(), b.is_empty()) {
            (true, true) => return cmp::Ordering::Equal,
            (true, _)    => return cmp::Ordering::Less,
            (_, true)    => return cmp::Ordering::Greater,
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
            cmp::Ordering::Equal => { a = &a[a_end..]; b = &b[b_end..]; }
            other                => return other,
        }
    }
}

fn luhmann_next_child(s: &str) -> String {
    let i = s.rfind(|c: char| !c.is_ascii_digit()).map_or(0, |i| i + 1);
    if i == s.len() { format!("{s}1") }
    else { let n: u32 = s[i..].parse().unwrap(); format!("{}{}", &s[..i], n + 1) }
}

fn luhmann_next_sibling(s: &str) -> Option<String> {
    let i = s.rfind(|c: char| c.is_ascii_digit()).map_or(0, |i| i + 1);
    if i < s.len() {
        let last = *s.as_bytes().last().unwrap();
        if last >= b'z' { return None; }
        let mut b = s.as_bytes().to_vec();
        *b.last_mut().unwrap() += 1;
        Some(String::from_utf8(b).unwrap())
    } else {
        Some(format!("{s}a"))
    }
}

/// Extracts the section prefix of a note ID (part before the first `/`).
/// Reserved for future use — draw routing is handled by the index.
/// - `"1a/1c1h5"` → `"1a"`
/// - `"1a1c1h5"`  → `""` (no section)
#[allow(dead_code)]
pub(crate) fn draw_section(id: &ID) -> &str {
    let s = id.0.as_str();
    match s.find('/') {
        Some(slash) => &s[..slash],
        None        => "",
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
pub struct ID(pub(crate) String);

impl ID {
    /// Returns the root ID of the main NoteBox box: `ZK1`.
    pub fn root(id: &str) -> Self { ID(id.into()) }

    /// Returns the next child ID by incrementing the trailing numeric segment,
    /// or appending `1` if the ID ends with a letter segment.
    pub fn next_child(&self) -> Self {
        match self.0.rfind('/') {
            Some(slash) => ID(format!("{}/{}", &self.0[..slash],
                                      luhmann_next_child(&self.0[slash + 1..]))),
            None        => ID(luhmann_next_child(&self.0)),
        }
    }

    /// Returns the next sibling ID, or `None` if all 26 letter slots are exhausted.
    pub fn next_sibling(&self) -> Option<Self> {
        match self.0.rfind('/') {
            Some(slash) => luhmann_next_sibling(&self.0[slash + 1..])
                .map(|s| ID(format!("{}/{}", &self.0[..slash], s))),
            None        => luhmann_next_sibling(&self.0).map(ID),
        }
    }

    /// Strips the last Luhmann segment to infer the parent ID.
    /// Roots return themselves.
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
    /// A root note (whose parent is itself) is never considered its own child.
    pub fn is_direct_child_of(&self, parent: &ID) -> bool {
        self != parent && self.parent() == *parent
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
    fn partial_cmp(&self, other: &Self) -> Option<cmp::Ordering> { Some(self.cmp(other)) }
}

impl Ord for ID {
    fn cmp(&self, other: &Self) -> cmp::Ordering {
        let mut a_parts = self.0.split('/');
        let mut b_parts = other.0.split('/');
        loop {
            match (a_parts.next(), b_parts.next()) {
                (None, None)       => return cmp::Ordering::Equal,
                (None, _)          => return cmp::Ordering::Less,
                (_, None)          => return cmp::Ordering::Greater,
                (Some(a), Some(b)) => match cmp_luhmann(a, b) {
                    cmp::Ordering::Equal => continue,
                    other                => return other,
                },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_id_root() { assert_eq!(ID::root("ZK1").to_string(), "ZK1"); }

    #[test]
    fn test_id_from_str() { assert_eq!(ID::from("1a1").to_string(), "1a1"); }

    #[test]
    fn test_id_from_string() { assert_eq!(ID::from("1a1".to_string()).to_string(), "1a1"); }

    #[test]
    fn test_next_child_from_letter_appends_one() {
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

    #[test]
    fn test_next_sibling_from_number_appends_a() {
        assert_eq!(ID::from("1").next_sibling(), Some(ID::from("1a")));
    }

    #[test]
    fn test_next_sibling_increments_trailing_letter() {
        assert_eq!(ID::from("1a").next_sibling(), Some(ID::from("1b")));
    }

    #[test]
    fn test_next_sibling_deep() {
        assert_eq!(ID::from("1a1").next_sibling(), Some(ID::from("1a1a")));
    }

    #[test]
    fn test_next_sibling_at_z_returns_none() {
        assert_eq!(ID::from("1z").next_sibling(), None);
    }

    #[test]
    fn test_id_ord_numeric_not_lexicographic() {
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
        assert!(ID::from("1a99") < ID::from("1b"));
    }

    #[test]
    fn test_parent_section_first_note() {
        assert_eq!(ID::from("1c2/1").parent(), ID::from("1c2"));
    }

    #[test]
    fn test_parent_within_section() {
        assert_eq!(ID::from("1c2/3c5f1").parent(), ID::from("1c2/3c5f"));
        assert_eq!(ID::from("1c2/3c").parent(),    ID::from("1c2/3"));
        assert_eq!(ID::from("1c2/3").parent(),     ID::from("1c2"));
    }

    #[test]
    fn test_parent_nested_section() {
        assert_eq!(ID::from("1c2/4g1/3").parent(),  ID::from("1c2/4g1"));
        assert_eq!(ID::from("1c2/4g1/3a").parent(), ID::from("1c2/4g1/3"));
    }

    #[test]
    fn test_is_direct_child_section_boundary() {
        assert!( ID::from("1c2/1").is_direct_child_of(&ID::from("1c2")));
        assert!(!ID::from("1c2/1a").is_direct_child_of(&ID::from("1c2")));
    }

    #[test]
    fn test_ord_section_after_parent() {
        assert!(ID::from("1c2") < ID::from("1c2/1"));
    }

    #[test]
    fn test_ord_section_before_next_sibling() {
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
        assert_eq!(ID::from("1c2/1a").next_child(),  ID::from("1c2/1a1"));
        assert_eq!(ID::from("1c2/1a1").next_child(), ID::from("1c2/1a2"));
    }

    #[test]
    fn test_next_sibling_in_section() {
        assert_eq!(ID::from("1c2/1").next_sibling(),  Some(ID::from("1c2/1a")));
        assert_eq!(ID::from("1c2/1a").next_sibling(), Some(ID::from("1c2/1b")));
    }
}
