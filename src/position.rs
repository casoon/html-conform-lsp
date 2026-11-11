// SPDX-FileCopyrightText: 2026 CASOON
// SPDX-License-Identifier: MIT

//! Byte offsets in the checked text to LSP positions (zero-based line, UTF-16 column).
//!
//! `html-conform` counts `column` in chars, but also gives the byte offset into the text it
//! checked. The offset is converted here against the same text, so the result does not depend
//! on how the checker counts columns.

use lsp_types::Position;

/// Line starts of one text. Lines end at LF, CR or CRLF, as in the HTML Standard and the LSP.
pub struct LineIndex<'a> {
    text: &'a str,
    starts: Vec<usize>,
}

impl<'a> LineIndex<'a> {
    pub fn new(text: &'a str) -> Self {
        let bytes = text.as_bytes();
        let mut starts = vec![0];
        let mut i = 0;
        while i < bytes.len() {
            match bytes[i] {
                b'\n' => starts.push(i + 1),
                b'\r' => {
                    if bytes.get(i + 1) == Some(&b'\n') {
                        i += 1;
                    }
                    starts.push(i + 1);
                }
                _ => {}
            }
            i += 1;
        }
        Self { text, starts }
    }

    /// The position of `byte_offset`. An offset past the end or inside a char is moved back to
    /// the nearest char boundary at or before it.
    pub fn position(&self, byte_offset: usize) -> Position {
        let mut offset = byte_offset.min(self.text.len());
        while !self.text.is_char_boundary(offset) {
            offset -= 1;
        }
        let line = self.starts.partition_point(|&start| start <= offset) - 1;
        let character = self.text[self.starts[line]..offset].encode_utf16().count();
        Position::new(line as u32, character as u32)
    }

    /// The position just before the line break that ends `line` (or the text end).
    pub fn line_end(&self, line: usize) -> Position {
        let start = self.starts[line];
        let end = self
            .starts
            .get(line + 1)
            .copied()
            .unwrap_or(self.text.len());
        let content = self.text[start..end].trim_end_matches(['\n', '\r']);
        Position::new(line as u32, content.encode_utf16().count() as u32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pos(text: &str, offset: usize) -> (u32, u32) {
        let p = LineIndex::new(text).position(offset);
        (p.line, p.character)
    }

    #[test]
    fn ascii() {
        assert_eq!(pos("ab\ncd", 0), (0, 0));
        assert_eq!(pos("ab\ncd", 4), (1, 1));
    }

    #[test]
    fn multibyte_is_one_utf16_unit() {
        // 'é' is 2 bytes, 1 UTF-16 unit; '€' is 3 bytes, 1 unit.
        let text = "é€<p>";
        assert_eq!(pos(text, text.find('<').unwrap()), (0, 2));
    }

    #[test]
    fn emoji_is_two_utf16_units() {
        // U+1F600 is 4 bytes in UTF-8, a surrogate pair (2 units) in UTF-16.
        let text = "😀<p>";
        assert_eq!(pos(text, text.find('<').unwrap()), (0, 2));
        let text = "a😀😀<p>";
        assert_eq!(pos(text, text.find('<').unwrap()), (0, 5));
    }

    #[test]
    fn crlf_counts_once() {
        let text = "a\r\nb\r\n<p>";
        assert_eq!(pos(text, text.find('<').unwrap()), (2, 0));
        // between CR and LF is still on line 0 after the content
        assert_eq!(pos(text, 1), (0, 1));
    }

    #[test]
    fn lone_cr_is_a_line_break() {
        let text = "a\rb\r<p>";
        assert_eq!(pos(text, text.find('<').unwrap()), (2, 0));
    }

    #[test]
    fn mixed_breaks_and_emoji_on_later_line() {
        let text = "x\r\n😀é\n<p>";
        let at = text.find("é").unwrap();
        assert_eq!(pos(text, at), (1, 2));
        assert_eq!(pos(text, text.find('<').unwrap()), (2, 0));
    }

    #[test]
    fn offset_inside_char_and_past_end_are_clamped() {
        let text = "😀";
        assert_eq!(pos(text, 2), (0, 0));
        assert_eq!(pos(text, 99), (0, 2));
    }

    #[test]
    fn line_end_excludes_the_break() {
        let index = LineIndex::new("a😀\r\nb");
        assert_eq!(index.line_end(0), Position::new(0, 3));
        assert_eq!(index.line_end(1), Position::new(1, 1));
    }
}
