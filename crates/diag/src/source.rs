/// 1-based line and column. Columns count characters, not bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineCol {
    pub line: usize,
    pub col: usize,
}

/// Maps byte offsets in one source text to lines and columns.
pub struct SourceMap<'a> {
    source: &'a str,
    /// Byte offset at which each line starts.
    line_starts: Vec<usize>,
}

impl<'a> SourceMap<'a> {
    #[must_use]
    pub fn new(source: &'a str) -> Self {
        let line_starts = std::iter::once(0)
            .chain(source.match_indices('\n').map(|(i, _)| i + 1))
            .collect();
        SourceMap {
            source,
            line_starts,
        }
    }

    #[must_use]
    pub fn line_col(&self, offset: usize) -> LineCol {
        let offset = offset.min(self.source.len());
        let line = self.line_starts.partition_point(|&start| start <= offset);
        let col = self.source[self.line_starts[line - 1]..offset]
            .chars()
            .count()
            + 1;
        LineCol { line, col }
    }

    /// The text of 1-based `line`, without its line ending.
    #[must_use]
    pub fn line_text(&self, line: usize) -> &'a str {
        let start = self.line_starts[line - 1];
        let end = self
            .line_starts
            .get(line)
            .map_or(self.source.len(), |&next| next - 1);
        self.source[start..end].trim_end_matches('\r')
    }

    /// Byte offset at which 1-based `line` starts.
    #[must_use]
    pub fn line_start(&self, line: usize) -> usize {
        self.line_starts[line - 1]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_col_counts_characters() {
        let map = SourceMap::new("ab\né x\n");
        assert_eq!(map.line_col(0), LineCol { line: 1, col: 1 });
        assert_eq!(map.line_col(2), LineCol { line: 1, col: 3 });
        assert_eq!(map.line_col(3), LineCol { line: 2, col: 1 });
        assert_eq!(map.line_col(6), LineCol { line: 2, col: 3 });
        assert_eq!(map.line_col(8), LineCol { line: 3, col: 1 });
    }

    #[test]
    fn line_text_excludes_line_ending() {
        let map = SourceMap::new("one\r\ntwo\nthree");
        assert_eq!(map.line_text(1), "one");
        assert_eq!(map.line_text(2), "two");
        assert_eq!(map.line_text(3), "three");
    }
}
