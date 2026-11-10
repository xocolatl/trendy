use lsp_types::{Position, Range};
use trendy_diag::{SourceMap, Span};

/// Converts byte offsets in one document to LSP positions.
///
/// LSP columns count UTF-16 code units, not bytes or characters.
pub struct LineIndex<'a> {
    source: &'a str,
    map: SourceMap<'a>,
}

impl<'a> LineIndex<'a> {
    pub fn new(source: &'a str) -> Self {
        LineIndex {
            source,
            map: SourceMap::new(source),
        }
    }

    pub fn position(&self, offset: usize) -> Position {
        let offset = offset.min(self.source.len());
        let line = self.map.line_col(offset).line;
        let character = self.source[self.map.line_start(line)..offset]
            .encode_utf16()
            .count();
        Position::new(saturate(line - 1), saturate(character))
    }

    /// The byte offset of `position`: as many UTF-16 code units into its line as it
    /// says, or that line's end, and the document's end for a line past its last.
    pub fn offset(&self, position: Position) -> usize {
        let mut start = 0;
        for _ in 0..position.line {
            match self.source[start..].find('\n') {
                Some(end) => start += end + 1,
                None => return self.source.len(),
            }
        }
        let end = self.source[start..]
            .find('\n')
            .map_or(self.source.len(), |end| start + end);
        let wanted = usize::try_from(position.character).unwrap_or(usize::MAX);
        let mut units = 0;
        for (at, character) in self.source[start..end].char_indices() {
            if units >= wanted {
                return start + at;
            }
            units += character.len_utf16();
        }
        end
    }

    pub fn range(&self, span: Span) -> Range {
        Range::new(self.position(span.start), self.position(span.end))
    }

    /// The range covering the whole document.
    pub fn full_range(&self) -> Range {
        Range::new(Position::new(0, 0), self.position(self.source.len()))
    }
}

/// LSP positions are `u32`; a document too large for that is clamped.
fn saturate(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_line() {
        let index = LineIndex::new("operator main{} [];");
        assert_eq!(index.position(9), Position::new(0, 9));
    }

    #[test]
    fn later_lines() {
        let index = LineIndex::new("a\nbc\n\nd");
        assert_eq!(index.position(2), Position::new(1, 0));
        assert_eq!(index.position(4), Position::new(1, 2));
        assert_eq!(index.position(5), Position::new(2, 0));
        assert_eq!(index.position(6), Position::new(3, 0));
    }

    #[test]
    fn columns_count_utf16_units() {
        // `é` is 2 bytes and 1 UTF-16 unit; `𝄞` is 4 bytes and 2 units.
        let index = LineIndex::new("é𝄞x");
        assert_eq!(index.position(2), Position::new(0, 1));
        assert_eq!(index.position(6), Position::new(0, 3));
    }

    #[test]
    fn offsets_are_positions_back() {
        let index = LineIndex::new("é𝄞x\nab\n");
        for offset in [0, 2, 6, 7, 8, 10, 11] {
            assert_eq!(index.offset(index.position(offset)), offset, "{offset}");
        }
        // Past a line's end, its end, and past the last line, the document's.
        assert_eq!(index.offset(Position::new(1, 9)), 10);
        assert_eq!(index.offset(Position::new(5, 0)), 11);
    }

    #[test]
    fn full_range_ends_after_last_character() {
        let index = LineIndex::new("ab\ncd\n");
        assert_eq!(index.full_range().end, Position::new(2, 0));
    }
}
