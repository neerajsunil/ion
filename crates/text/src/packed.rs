//! Values kept lz4-compressed while idle, such as the text of a hidden tab.

use std::cell::OnceCell;
use std::ops::{Deref, DerefMut};
use std::sync::Arc;

use ropey::Rope;

/// Text that can be turned into bytes and back.
pub trait Packable: Sized {
    fn to_text(&self) -> String;
    fn from_text(text: String) -> Self;
}

impl Packable for Rope {
    fn to_text(&self) -> String {
        self.to_string()
    }

    fn from_text(text: String) -> Self {
        Rope::from_str(&text)
    }
}

impl Packable for Arc<str> {
    fn to_text(&self) -> String {
        self.to_string()
    }

    fn from_text(text: String) -> Self {
        text.into()
    }
}

/// Compresses `value` (call from a background thread).
pub fn pack<T: Packable>(value: &T) -> Box<[u8]> {
    lz4_flex::compress_prepend_size(value.to_text().as_bytes()).into()
}

/// A value, or its lz4-compressed bytes while it's idle. Reading it
/// decompresses on demand (a few milliseconds per MB), so callers don't need
/// to know which form it's in; writing it drops the compressed copy.
pub struct Packed<T> {
    value: OnceCell<T>,
    packed: Option<Box<[u8]>>,
}

impl<T: Packable> Packed<T> {
    pub fn new(value: T) -> Self {
        Self {
            value: OnceCell::from(value),
            packed: None,
        }
    }

    /// The value is decompressed and in memory.
    pub fn is_unpacked(&self) -> bool {
        self.value.get().is_some()
    }

    /// Compressed bytes of the current value exist.
    pub fn has_packed(&self) -> bool {
        self.packed.is_some()
    }

    /// Keeps only `packed`, the [`pack`]ed form of the current value.
    pub fn set_packed(&mut self, packed: Box<[u8]>) {
        self.packed = Some(packed);
        self.value = OnceCell::new();
    }

    /// Drops the decompressed value again after a read, when the compressed
    /// bytes are still current. Returns whether it did.
    pub fn repack(&mut self) -> bool {
        if self.packed.is_none() {
            return false;
        }
        self.value = OnceCell::new();
        true
    }

    /// Decompresses for good (the value is about to be used a lot).
    pub fn unpack(&mut self) {
        let _ = &mut **self;
    }
}

impl<T: Packable> Deref for Packed<T> {
    type Target = T;

    fn deref(&self) -> &T {
        self.value.get_or_init(|| {
            let packed = self.packed.as_deref().expect("packed value has no bytes");
            let bytes = lz4_flex::decompress_size_prepended(packed).expect("corrupt packed value");
            T::from_text(String::from_utf8(bytes).expect("packed value isn't UTF-8"))
        })
    }
}

impl<T: Packable> DerefMut for Packed<T> {
    fn deref_mut(&mut self) -> &mut T {
        let _ = &**self;
        self.packed = None;
        self.value.get_mut().expect("value was just unpacked")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_through_packing() {
        let mut text = Packed::new(Rope::from_str("fn main() {}\n".repeat(100).as_str()));
        let bytes = pack(&*text);
        text.set_packed(bytes);
        assert!(!text.is_unpacked());
        assert_eq!(text.len_lines(), 101);
        assert!(text.is_unpacked() && text.has_packed());
        assert!(text.repack());
        text.insert(0, "// hi\n");
        assert!(!text.has_packed());
        assert!(text.to_string().starts_with("// hi\nfn main"));
        assert!(!text.repack());
    }
}
