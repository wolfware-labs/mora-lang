use text_size::{TextRange, TextSize};

/// A zero-based line and a zero-based column in UTF-8 bytes from the line start.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LineCol {
    pub line: u32,
    pub col: u32,
}

/// A zero-based line and a zero-based column in UTF-16 code units, as the LSP counts them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Utf16LineCol {
    pub line: u32,
    pub col: u32,
}

/// A character that takes more than one UTF-8 byte, recorded per line so
/// that column conversions only walk the non-ASCII characters of one line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct WideChar {
    /// Byte offset from the start of the line.
    start: u32,
    /// Length in UTF-8 bytes, 2 to 4.
    utf8_len: u32,
    /// Length in UTF-16 code units, 1 or 2.
    utf16_len: u32,
}

impl WideChar {
    fn end(self) -> u32 {
        self.start + self.utf8_len
    }
}

/// Converts between byte offsets and line/column positions for one text.
///
/// Lines end at `\n` (§3.1). In a CRLF file the `\r` belongs to the line it
/// ends, so a line's columns run from 0 up to and including the position of
/// its `\n`. A lone `\r` is an ordinary character.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LineIndex {
    /// Offset of the first byte of each line; always starts with 0.
    line_starts: Vec<TextSize>,
    /// The multi-byte characters of each line, in order.
    wide_chars: Vec<Vec<WideChar>>,
    /// Length of the text in bytes.
    len: TextSize,
}

impl LineIndex {
    /// Largest text a line index accepts: offsets are 32-bit (§3.1).
    pub const MAX_LEN: usize = u32::MAX as usize;

    /// # Panics
    ///
    /// Panics if `text` is longer than [`Self::MAX_LEN`] bytes. The driver
    /// rejects such files with `MR0003` before indexing them.
    pub fn new(text: &str) -> Self {
        assert!(
            text.len() <= Self::MAX_LEN,
            "source text of {} bytes exceeds the 4 GiB limit",
            text.len()
        );
        let mut line_starts = vec![TextSize::new(0)];
        let mut wide_chars = vec![Vec::new()];
        let mut line_start = 0usize;
        for (offset, ch) in text.char_indices() {
            if ch == '\n' {
                line_start = offset + 1;
                line_starts.push(to_text_size(line_start));
                wide_chars.push(Vec::new());
            } else if !ch.is_ascii() {
                let start = to_u32(offset - line_start);
                let wide = WideChar {
                    start,
                    utf8_len: to_u32(ch.len_utf8()),
                    utf16_len: to_u32(ch.len_utf16()),
                };
                if let Some(line) = wide_chars.last_mut() {
                    line.push(wide);
                }
            }
        }
        Self {
            line_starts,
            wide_chars,
            len: to_text_size(text.len()),
        }
    }

    /// Length of the text in bytes.
    pub fn len(&self) -> TextSize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == TextSize::new(0)
    }

    /// Number of lines. A text that ends with `\n` has an empty last line.
    ///
    /// This is a `usize` because a text of [`Self::MAX_LEN`] newlines has one
    /// line more than `u32` can count; every line number still fits in `u32`.
    pub fn line_count(&self) -> usize {
        self.line_starts.len()
    }

    /// The byte range of a line, without its terminating `\n`.
    pub fn line_range(&self, line: u32) -> Option<TextRange> {
        let index = line as usize;
        let start = *self.line_starts.get(index)?;
        let end = match self.line_starts.get(index + 1) {
            Some(next) => *next - TextSize::new(1),
            None => self.len,
        };
        Some(TextRange::new(start, end))
    }

    /// The line and UTF-8 column of a byte offset, or `None` when the offset
    /// is past the end of the text or inside a multi-byte character.
    pub fn line_col(&self, offset: TextSize) -> Option<LineCol> {
        if offset > self.len {
            return None;
        }
        // The last line whose start is at or before the offset.
        let line = self.line_starts.partition_point(|start| *start <= offset) - 1;
        let col = u32::from(offset - self.line_starts[line]);
        if self.wide_chars[line]
            .iter()
            .any(|w| w.start < col && col < w.end())
        {
            return None;
        }
        Some(LineCol {
            line: to_u32(line),
            col,
        })
    }

    /// The byte offset of a line and UTF-8 column, or `None` when the line
    /// does not exist, the column is past the line's end, or it falls inside
    /// a multi-byte character.
    pub fn offset(&self, pos: LineCol) -> Option<TextSize> {
        let range = self.line_range(pos.line)?;
        if pos.col > u32::from(range.len()) {
            return None;
        }
        let wide = &self.wide_chars[pos.line as usize];
        if wide.iter().any(|w| w.start < pos.col && pos.col < w.end()) {
            return None;
        }
        Some(range.start() + TextSize::new(pos.col))
    }

    /// Converts a UTF-8 column to UTF-16 code units, for the LSP.
    pub fn to_utf16(&self, pos: LineCol) -> Option<Utf16LineCol> {
        self.offset(pos)?;
        let mut col = pos.col;
        for wide in &self.wide_chars[pos.line as usize] {
            if wide.end() > pos.col {
                break;
            }
            col = col - wide.utf8_len + wide.utf16_len;
        }
        Some(Utf16LineCol {
            line: pos.line,
            col,
        })
    }

    /// Converts a UTF-16 column from the LSP to a UTF-8 column, or `None`
    /// when the column is past the line's end or splits a surrogate pair.
    pub fn to_utf8(&self, pos: Utf16LineCol) -> Option<LineCol> {
        let range = self.line_range(pos.line)?;
        let mut utf16 = 0u32;
        let mut utf8 = 0u32;
        for wide in &self.wide_chars[pos.line as usize] {
            // ASCII bytes before this character count one unit each.
            let ascii = wide.start - utf8;
            if utf16 + ascii >= pos.col {
                break;
            }
            utf16 += ascii;
            utf8 = wide.start;
            if utf16 + wide.utf16_len > pos.col {
                return None;
            }
            utf16 += wide.utf16_len;
            utf8 += wide.utf8_len;
        }
        // Checked: a column far past the line end must give `None`, not overflow.
        let col = utf8.checked_add(pos.col.checked_sub(utf16)?)?;
        (col <= u32::from(range.len())).then_some(LineCol {
            line: pos.line,
            col,
        })
    }
}

/// Converts an offset or length within a source text to a [`TextSize`].
///
/// Source texts are at most [`LineIndex::MAX_LEN`] bytes (the driver rejects
/// larger files with `MR0003`, §3.1), so for them this never saturates; a
/// larger value gives `u32::MAX`.
pub fn to_text_size(n: usize) -> TextSize {
    TextSize::new(to_u32(n))
}

/// [`to_text_size`] as a plain `u32`, for the columns and lengths of this module.
fn to_u32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lc(line: u32, col: u32) -> LineCol {
        LineCol { line, col }
    }

    fn u16(line: u32, col: u32) -> Utf16LineCol {
        Utf16LineCol { line, col }
    }

    /// Every char-boundary offset maps to a position and back, and every
    /// position converts to UTF-16 and back.
    fn assert_round_trips(text: &str) {
        let index = LineIndex::new(text);
        for offset in 0..=text.len() {
            let size = to_text_size(offset);
            let Some(pos) = index.line_col(size) else {
                assert!(
                    !text.is_char_boundary(offset),
                    "{text:?}: offset {offset} has no position"
                );
                continue;
            };
            assert!(
                text.is_char_boundary(offset),
                "{text:?}: offset {offset} is inside a char"
            );
            assert_eq!(index.offset(pos), Some(size), "{text:?}: {pos:?}");
            let wide = index.to_utf16(pos).unwrap();
            assert_eq!(index.to_utf8(wide), Some(pos), "{text:?}: {wide:?}");
        }
    }

    #[test]
    fn round_trips() {
        for text in [
            "",
            "a",
            "Console.WriteLine(\"Hello\");\n",
            "let x = 1;\nlet y = 2;",
            "line one\r\nline two\r\n\r\nend",
            "naïve façade\nÜber",
            "let clef = '𝄞';\n// 𐐷 and 𝄞 are outside the BMP\n",
            "a\rb\r\nc",
            "\n\n\n",
            "日本語\n한국어\r\nعربى",
        ] {
            assert_round_trips(text);
        }
    }

    #[test]
    fn ascii_positions() {
        let index = LineIndex::new("ab\ncd\n");
        assert_eq!(index.line_count(), 3);
        assert_eq!(index.line_col(TextSize::new(0)), Some(lc(0, 0)));
        assert_eq!(index.line_col(TextSize::new(2)), Some(lc(0, 2)));
        assert_eq!(index.line_col(TextSize::new(3)), Some(lc(1, 0)));
        assert_eq!(index.line_col(TextSize::new(6)), Some(lc(2, 0)));
        assert_eq!(index.line_col(TextSize::new(7)), None);
    }

    #[test]
    fn crlf_keeps_the_carriage_return_on_its_line() {
        let index = LineIndex::new("ab\r\ncd");
        assert_eq!(
            index.line_range(0),
            Some(TextRange::new(0.into(), 3.into()))
        );
        assert_eq!(index.line_col(TextSize::new(2)), Some(lc(0, 2)));
        assert_eq!(index.line_col(TextSize::new(3)), Some(lc(0, 3)));
        assert_eq!(index.line_col(TextSize::new(4)), Some(lc(1, 0)));
        assert_eq!(index.offset(lc(0, 4)), None);
    }

    #[test]
    fn lone_carriage_return_is_not_a_line_break() {
        let index = LineIndex::new("a\rb");
        assert_eq!(index.line_count(), 1);
        assert_eq!(index.line_col(TextSize::new(2)), Some(lc(0, 2)));
    }

    #[test]
    fn multibyte_columns() {
        // 'é' is 2 bytes and 1 UTF-16 unit; '𝄞' is 4 bytes and 2 units.
        let index = LineIndex::new("é𝄞x");
        assert_eq!(index.line_col(TextSize::new(1)), None);
        assert_eq!(index.line_col(TextSize::new(2)), Some(lc(0, 2)));
        assert_eq!(index.line_col(TextSize::new(4)), None);
        assert_eq!(index.line_col(TextSize::new(6)), Some(lc(0, 6)));
        assert_eq!(index.to_utf16(lc(0, 2)), Some(u16(0, 1)));
        assert_eq!(index.to_utf16(lc(0, 6)), Some(u16(0, 3)));
        assert_eq!(index.to_utf16(lc(0, 7)), Some(u16(0, 4)));
        assert_eq!(index.to_utf8(u16(0, 3)), Some(lc(0, 6)));
    }

    #[test]
    fn utf16_column_inside_a_surrogate_pair_is_rejected() {
        let index = LineIndex::new("a𝄞b");
        assert_eq!(index.to_utf8(u16(0, 1)), Some(lc(0, 1)));
        assert_eq!(index.to_utf8(u16(0, 2)), None);
        assert_eq!(index.to_utf8(u16(0, 3)), Some(lc(0, 5)));
        assert_eq!(index.to_utf8(u16(0, 5)), None);
    }

    #[test]
    fn positions_past_the_end_are_rejected() {
        let index = LineIndex::new("ab\ncd");
        assert_eq!(index.offset(lc(0, 3)), None);
        assert_eq!(index.offset(lc(2, 0)), None);
        assert_eq!(index.to_utf8(u16(1, 3)), None);
        assert_eq!(index.to_utf16(lc(5, 0)), None);
    }

    #[test]
    fn huge_utf16_column_is_rejected_without_overflow() {
        let index = LineIndex::new("𝄞é\nx");
        assert_eq!(index.to_utf8(u16(0, u32::MAX)), None);
        assert_eq!(index.to_utf8(u16(1, u32::MAX)), None);
        assert_eq!(index.offset(lc(0, u32::MAX)), None);
    }

    #[test]
    fn to_text_size_keeps_offsets_and_saturates_past_the_limit() {
        assert_eq!(to_text_size(0), TextSize::new(0));
        assert_eq!(to_text_size(LineIndex::MAX_LEN), TextSize::new(u32::MAX));
        assert_eq!(to_text_size(usize::MAX), TextSize::new(u32::MAX));
        assert_eq!(to_text_size(1234), TextSize::new(1234));
    }

    #[test]
    fn len_and_is_empty_follow_the_text() {
        let empty = LineIndex::new("");
        assert_eq!(empty.len(), TextSize::new(0));
        assert!(empty.is_empty());
        let text = LineIndex::new("ab\nc");
        assert_eq!(text.len(), TextSize::new(4));
        assert!(!text.is_empty());
    }
}
