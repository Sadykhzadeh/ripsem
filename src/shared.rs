//! Cheap handles into big shared buffers. An index holds hundreds of
//! thousands of records, and giving each its own heap allocations for the
//! vector, the terms and the text made loading an index slower than reading
//! the file. A handle is a range of one buffer that every record of the index
//! shares.

use std::borrow::Borrow;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::ops::{Deref, Range};
use std::sync::Arc;

/// A range of a shared buffer. Cloning clones the `Arc`, not the elements.
pub struct Shared<T> {
    buf: Arc<[T]>,
    start: u32,
    len: u32,
}

impl<T> Shared<T> {
    /// The elements of `range`, or `None` when it lies outside the buffer or
    /// the buffer is too large for a 32-bit offset.
    pub fn slice_of(buf: &Arc<[T]>, range: Range<usize>) -> Option<Self> {
        if range.start > range.end || range.end > buf.len() {
            return None;
        }
        Some(Self {
            buf: Arc::clone(buf),
            start: u32::try_from(range.start).ok()?,
            len: u32::try_from(range.end - range.start).ok()?,
        })
    }
}

impl<T> Clone for Shared<T> {
    fn clone(&self) -> Self {
        Self {
            buf: Arc::clone(&self.buf),
            start: self.start,
            len: self.len,
        }
    }
}

impl<T> Deref for Shared<T> {
    type Target = [T];

    fn deref(&self) -> &[T] {
        let start = self.start as usize;
        &self.buf[start..start + self.len as usize]
    }
}

/// A record built while indexing owns a buffer of its own.
impl<T> From<Vec<T>> for Shared<T> {
    fn from(items: Vec<T>) -> Self {
        let len = u32::try_from(items.len()).expect("a record holds fewer than 2^32 elements");
        Self {
            buf: items.into(),
            start: 0,
            len,
        }
    }
}

impl<T> Default for Shared<T> {
    fn default() -> Self {
        Vec::new().into()
    }
}

impl<T: PartialEq> PartialEq for Shared<T> {
    fn eq(&self, other: &Self) -> bool {
        **self == **other
    }
}

impl<T: Eq> Eq for Shared<T> {}

impl<T: fmt::Debug> fmt::Debug for Shared<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&**self, f)
    }
}

/// A range of a shared string, on character boundaries.
#[derive(Clone)]
pub struct SharedStr {
    buf: Arc<str>,
    start: u32,
    len: u32,
}

impl SharedStr {
    /// The text of `range`, or `None` when it is not on character boundaries
    /// inside the buffer or the buffer is too large for a 32-bit offset.
    pub fn slice_of(buf: &Arc<str>, range: Range<usize>) -> Option<Self> {
        buf.get(range.clone())?;
        Some(Self {
            buf: Arc::clone(buf),
            start: u32::try_from(range.start).ok()?,
            len: u32::try_from(range.end - range.start).ok()?,
        })
    }

    pub fn as_str(&self) -> &str {
        let start = self.start as usize;
        &self.buf[start..start + self.len as usize]
    }
}

impl Deref for SharedStr {
    type Target = str;

    fn deref(&self) -> &str {
        self.as_str()
    }
}

impl From<&str> for SharedStr {
    fn from(text: &str) -> Self {
        let len = u32::try_from(text.len()).expect("a record holds fewer than 4 GiB of text");
        Self {
            buf: text.into(),
            start: 0,
            len,
        }
    }
}

impl From<String> for SharedStr {
    fn from(text: String) -> Self {
        text.as_str().into()
    }
}

impl AsRef<str> for SharedStr {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl Borrow<str> for SharedStr {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

impl Default for SharedStr {
    fn default() -> Self {
        "".into()
    }
}

impl PartialEq for SharedStr {
    fn eq(&self, other: &Self) -> bool {
        self.as_str() == other.as_str()
    }
}

impl Eq for SharedStr {}

impl PartialEq<str> for SharedStr {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}

impl PartialEq<&str> for SharedStr {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

impl PartialEq<String> for SharedStr {
    fn eq(&self, other: &String) -> bool {
        self.as_str() == other.as_str()
    }
}

impl PartialEq<SharedStr> for String {
    fn eq(&self, other: &SharedStr) -> bool {
        self.as_str() == other.as_str()
    }
}

impl PartialEq<SharedStr> for &str {
    fn eq(&self, other: &SharedStr) -> bool {
        *self == other.as_str()
    }
}

impl Hash for SharedStr {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.as_str().hash(state);
    }
}

impl fmt::Display for SharedStr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self.as_str(), f)
    }
}

impl fmt::Debug for SharedStr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.as_str(), f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_handle_reads_its_range_of_the_buffer() {
        let buf: Arc<[u32]> = vec![1, 2, 3, 4, 5].into();
        let middle = Shared::slice_of(&buf, 1..4).unwrap();
        assert_eq!(&*middle, &[2, 3, 4]);
        assert_eq!(middle, Shared::from(vec![2, 3, 4]));
        assert_eq!(format!("{middle:?}"), "[2, 3, 4]");
        assert!(Shared::slice_of(&buf, 5..5).unwrap().is_empty());
    }

    #[test]
    fn a_range_outside_the_buffer_is_refused() {
        let buf: Arc<[u8]> = vec![0; 4].into();
        assert!(Shared::slice_of(&buf, 2..5).is_none());
        assert!(Shared::slice_of(&buf, 5..6).is_none());
        #[allow(clippy::reversed_empty_ranges)]
        let backwards = 3..2;
        assert!(Shared::slice_of(&buf, backwards).is_none());
    }

    #[test]
    fn cloning_shares_the_buffer() {
        let buf: Arc<[u8]> = vec![7; 8].into();
        let one = Shared::slice_of(&buf, 0..4).unwrap();
        let two = one.clone();
        assert_eq!(Arc::strong_count(&buf), 3);
        assert_eq!(one, two);
    }

    #[test]
    fn a_string_handle_compares_with_the_types_it_replaces() {
        let text: SharedStr = "src/a.rs".into();
        assert_eq!(text, "src/a.rs");
        assert_eq!(text, *"src/a.rs");
        assert_eq!(text, String::from("src/a.rs"));
        assert_eq!(text.to_string(), "src/a.rs");
        assert_eq!(format!("{text:?}"), "\"src/a.rs\"");
        assert!(text.starts_with("src/"));
    }

    #[test]
    fn string_handles_to_one_file_share_its_buffer() {
        let buf: Arc<str> = "alpha beta".into();
        let beta = SharedStr::slice_of(&buf, 6..10).unwrap();
        assert_eq!(beta, "beta");
        assert_eq!(Arc::strong_count(&buf), 2);
    }

    #[test]
    fn a_string_range_must_sit_on_character_boundaries() {
        let buf: Arc<str> = "h\u{e9}llo".into();
        assert!(SharedStr::slice_of(&buf, 0..2).is_none());
        assert!(SharedStr::slice_of(&buf, 0..7).is_none());
        assert_eq!(SharedStr::slice_of(&buf, 1..3).unwrap(), "\u{e9}");
    }
}
