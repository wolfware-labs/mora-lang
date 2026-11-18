//! The 16-byte `string` value and the heap object behind it (§13.4).
//!
//! This is the one definition of the encoding. An inline string holds its
//! bytes at offsets 0 to `len - 1`, zeros after them, and its length (0 to
//! 15) in the byte at [`TAG_OFFSET`]. A heap string holds a pointer to its
//! object at offset 0, its length as a `u32` in the target's byte order at
//! [`LEN_OFFSET`], zeros in the bytes between, and [`HEAP_TAG`] in the last
//! byte. [`NONE_TAG`] is reserved for `none` of `string?`. All-zero bytes are
//! the empty string.
//!
//! The heap object is `{ strong: u32, weak: u32, capacity: u32, bytes }`,
//! aligned to [`OBJECT_ALIGN`].

/// The size of a string value in bytes.
pub const SIZE: usize = 16;
/// The most bytes an inline string holds.
pub const INLINE_MAX: usize = 15;
/// The offset of the byte that holds an inline length or a tag.
pub const TAG_OFFSET: usize = 15;
/// The offset of a heap string's length.
pub const LEN_OFFSET: usize = 8;
/// The tag of a heap string.
pub const HEAP_TAG: u8 = 0x80;
/// The tag of `none` for `string?`.
pub const NONE_TAG: u8 = 0x81;
/// The size of the header every shared object starts with (§13.3).
pub const HEADER_SIZE: usize = 8;
/// The offset of a heap object's capacity.
pub const CAPACITY_OFFSET: usize = 8;
/// The offset of a heap object's bytes.
pub const BYTES_OFFSET: usize = 12;
/// The alignment of a heap object.
pub const OBJECT_ALIGN: usize = 4;
/// The strong count of an object that is never retained or released.
pub const IMMORTAL: u32 = 1 << 31;

/// A string value as it sits in memory.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct MoraString {
    bytes: [u8; SIZE],
    // Gives the value the alignment of a pointer.
    _align: [usize; 0],
}

impl MoraString {
    /// The empty string.
    pub const EMPTY: MoraString = MoraString {
        bytes: [0; SIZE],
        _align: [],
    };

    /// An inline string of `text`, or `None` when it is longer than
    /// [`INLINE_MAX`] bytes.
    pub fn inline(text: &[u8]) -> Option<MoraString> {
        if text.len() > INLINE_MAX {
            return None;
        }
        let mut string = MoraString::EMPTY;
        string.bytes[..text.len()].copy_from_slice(text);
        // The length is at most 15.
        string.bytes[TAG_OFFSET] = text.len() as u8;
        Some(string)
    }

    /// A heap string of `len` bytes that lives in `object`.
    pub fn heap(object: *const u8, len: u32) -> MoraString {
        let mut string = MoraString::EMPTY;
        let address = object.expose_provenance().to_ne_bytes();
        string.bytes[..address.len()].copy_from_slice(&address);
        string.bytes[LEN_OFFSET..LEN_OFFSET + 4].copy_from_slice(&len.to_ne_bytes());
        string.bytes[TAG_OFFSET] = HEAP_TAG;
        string
    }

    /// Whether the bytes are inside the value.
    pub fn is_inline(&self) -> bool {
        usize::from(self.bytes[TAG_OFFSET]) <= INLINE_MAX
    }

    /// The length in bytes. Meaningless for `none`.
    pub fn len(&self) -> usize {
        if self.is_inline() {
            usize::from(self.bytes[TAG_OFFSET])
        } else {
            let mut len = [0; 4];
            len.copy_from_slice(&self.bytes[LEN_OFFSET..LEN_OFFSET + 4]);
            u32::from_ne_bytes(len) as usize
        }
    }

    /// Whether the string has no bytes.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The heap object of a heap string.
    pub fn object(&self) -> *const u8 {
        let mut address = [0; size_of::<usize>()];
        address.copy_from_slice(&self.bytes[..size_of::<usize>()]);
        core::ptr::with_exposed_provenance(usize::from_ne_bytes(address))
    }

    /// The bytes of the string.
    ///
    /// # Safety
    ///
    /// For a heap string, the object is alive and holds [`len`](Self::len)
    /// bytes at [`BYTES_OFFSET`]. The result borrows the value for an inline
    /// string.
    pub unsafe fn as_bytes(&self) -> &[u8] {
        if self.is_inline() {
            &self.bytes[..self.len()]
        } else {
            // SAFETY: the caller guarantees the object holds `len` bytes.
            unsafe { core::slice::from_raw_parts(self.object().add(BYTES_OFFSET), self.len()) }
        }
    }

    /// The value's 16 bytes.
    pub fn raw(&self) -> [u8; SIZE] {
        self.bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_text_is_inline_with_its_length_in_the_last_byte() {
        let s = MoraString::inline(b"Ada").unwrap();
        assert_eq!(s.raw()[..4], *b"Ada\0");
        assert_eq!(s.raw()[TAG_OFFSET], 3);
        assert!(s.is_inline());
        assert_eq!(s.len(), 3);
        assert!(!s.is_empty());
    }

    #[test]
    fn sixteen_bytes_do_not_fit_inline() {
        assert!(MoraString::inline(&[b'a'; 16]).is_none());
        let full = MoraString::inline(&[b'a'; 15]).unwrap();
        assert_eq!(full.len(), 15);
        assert!(full.is_inline());
    }

    #[test]
    fn heap_strings_hold_pointer_length_and_tag() {
        let object = [0u8; 4];
        let p = object.as_ptr();
        let s = MoraString::heap(p, 40);
        assert_eq!(s.object(), p);
        assert_eq!(s.len(), 40);
        assert_eq!(s.raw()[TAG_OFFSET], HEAP_TAG);
        assert_eq!(s.raw()[12..15], [0, 0, 0]);
        assert_eq!(s.raw()[LEN_OFFSET..LEN_OFFSET + 4], 40u32.to_ne_bytes());
        assert!(!s.is_inline());
    }

    #[test]
    fn zero_bytes_are_the_empty_string() {
        assert_eq!(MoraString::EMPTY.len(), 0);
        assert!(MoraString::EMPTY.is_inline());
        assert!(MoraString::EMPTY.is_empty());
        assert_eq!(MoraString::EMPTY.raw(), [0; SIZE]);
    }

    #[test]
    fn the_none_tag_is_not_inline() {
        let mut raw = MoraString::EMPTY;
        raw.bytes[TAG_OFFSET] = NONE_TAG;
        assert!(!raw.is_inline());
    }

    #[test]
    fn inline_bytes_come_back_out() {
        let s = MoraString::inline(b"Ada").unwrap();
        // SAFETY: an inline string needs no object.
        assert_eq!(unsafe { s.as_bytes() }, b"Ada");
    }

    #[test]
    fn heap_bytes_start_after_the_capacity() {
        let mut object = [0u8; BYTES_OFFSET + 3];
        object[BYTES_OFFSET..].copy_from_slice(b"xyz");
        let s = MoraString::heap(object.as_ptr(), 3);
        // SAFETY: the object holds three bytes at `BYTES_OFFSET`.
        assert_eq!(unsafe { s.as_bytes() }, b"xyz");
    }

    #[test]
    fn the_layout_constants_agree() {
        assert_eq!(size_of::<MoraString>(), SIZE);
        assert_eq!(align_of::<MoraString>(), align_of::<usize>());
        assert_eq!(TAG_OFFSET, SIZE - 1);
        assert_eq!(BYTES_OFFSET, CAPACITY_OFFSET + 4);
        assert_eq!(CAPACITY_OFFSET, HEADER_SIZE);
    }
}
