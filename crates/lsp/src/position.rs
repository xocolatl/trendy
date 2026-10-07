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
    fn full_range_ends_after_last_character() {
        let index = LineIndex::new("ab\ncd\n");
        assert_eq!(index.full_range().end, Position::new(2, 0));
    }
}
