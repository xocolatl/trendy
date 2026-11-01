use crate::Span;

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

/// The files a unit's spans are in, one after another, as a unit's tree
/// has them: a span is an offset into their texts, so the first file's
/// spans are its own, and each later file's are offset by the lengths of
/// those before it.
pub struct Sources<'a> {
    files: Vec<File<'a>>,
}

/// One of the files of [`Sources`].
pub struct File<'a> {
    /// What diagnostics call it.
    pub path: &'a str,
    /// Its lines and columns, by offsets within it.
    pub map: SourceMap<'a>,
    /// Where it starts and ends among the unit's offsets.
    start: usize,
    end: usize,
}

/// Where a span of [`Sources`] is.
pub struct Located<'s, 'a> {
    /// Which file it is in, counting from 0, in order.
    pub index: usize,
    pub file: &'s File<'a>,
    /// The span within the file.
    pub span: Span,
}

impl<'a> Sources<'a> {
    /// `files`, each its path and its text, one after another.
    pub fn new(files: impl IntoIterator<Item = (&'a str, &'a str)>) -> Self {
        let mut start = 0;
        let files = files
            .into_iter()
            .map(|(path, text)| {
                let file = File {
                    path,
                    map: SourceMap::new(text),
                    start,
                    end: start + text.len(),
                };
                start = file.end;
                file
            })
            .collect();
        Sources { files }
    }

    /// The file at `path` alone, whose spans are its own.
    #[must_use]
    pub fn one(path: &'a str, text: &'a str) -> Self {
        Sources::new([(path, text)])
    }

    /// The file `span` is in, and the span within it. An empty span where
    /// one file ends and the next starts is at the end of the first, where
    /// what is missing from it goes.
    ///
    /// # Panics
    ///
    /// If `span` isn't within one of the files.
    #[must_use]
    pub fn locate(&self, span: Span) -> Located<'_, 'a> {
        let (index, file) = self
            .files
            .iter()
            .enumerate()
            .find(|(_, file)| file.start <= span.start && span.end <= file.end)
            .expect("a span is within one of its unit's files");
        Located {
            index,
            file,
            span: Span::new(span.start - file.start, span.end - file.start),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spans_are_located_in_their_files() {
        let sources = Sources::new([("a", "one\n"), ("b", "two\nthree")]);
        let at = |span| {
            let located = sources.locate(span);
            (located.index, located.file.path, located.span)
        };
        assert_eq!(at(Span::new(0, 3)), (0, "a", Span::new(0, 3)));
        assert_eq!(at(Span::new(8, 13)), (1, "b", Span::new(4, 9)));
        // The end of the first file is in it, the start of the second in
        // the second.
        assert_eq!(at(Span::empty(4)), (0, "a", Span::empty(4)));
        assert_eq!(at(Span::new(4, 7)), (1, "b", Span::new(0, 3)));
    }

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
