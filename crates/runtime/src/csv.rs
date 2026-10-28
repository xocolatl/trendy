//! CSV text, as RFC 4180 has it, which `load_csv` reads a file as: records
//! separated by line ends, a line feed or a carriage return and a line
//! feed, the last of which needs none; fields separated by commas; and a
//! field with a comma, a quote or a line end in quotes, each quote in it
//! doubled. A field's spaces are part of it. Empty lines, between records,
//! are skipped, and so is a byte order mark at the start, which isn't
//! part of the text.

/// A field of a record of CSV text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Field<'a> {
    /// Its text as written: between its quotes, if it is quoted, where
    /// each quote in it is doubled.
    written: &'a str,
    quoted: bool,
    /// The line it starts on, counting from 1.
    pub(crate) line: usize,
    /// Whether it is the last of its record.
    pub(crate) last: bool,
}

impl<'a> Field<'a> {
    /// Its value, if it is the text as written, with no doubled quotes.
    pub(crate) fn as_written(&self) -> Option<&'a str> {
        (!self.quoted || !self.written.contains('"')).then_some(self.written)
    }

    /// The number of bytes of its value: its text, each doubled quote one.
    pub(crate) fn len(&self) -> usize {
        if self.quoted {
            self.written.len() - self.written.matches("\"\"").count()
        } else {
            self.written.len()
        }
    }

    /// Write its value's bytes to `out`, which has room for
    /// [`Field::len`] of them.
    pub(crate) fn copy_to(&self, out: &mut [u8]) {
        let mut at = 0;
        let mut quote = false;
        for &b in self.written.as_bytes() {
            // The second quote of each pair is the first's escape.
            if self.quoted && b == b'"' {
                quote = !quote;
                if !quote {
                    continue;
                }
            }
            out[at] = b;
            at += 1;
        }
    }
}

/// The fields of the first record of CSV text, its header, as far as the
/// text is CSV.
pub(crate) fn header(text: &str) -> impl Iterator<Item = Field<'_>> {
    let mut fields = Fields::new(text).flatten();
    let mut ended = false;
    core::iter::from_fn(move || {
        if ended {
            return None;
        }
        let field = fields.next()?;
        ended = field.last;
        Some(field)
    })
}

/// Why text isn't CSV, and on which line, counting from 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct NotCsv {
    pub(crate) why: Malformed,
    pub(crate) line: usize,
}

/// Why text isn't CSV.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Malformed {
    /// A quote in a field that isn't quoted.
    StrayQuote,
    /// Something other than a comma or a line end after a quoted field's
    /// closing quote.
    AfterQuote,
    /// A quoted field that isn't closed before the text ends.
    Unclosed,
}

/// The fields of CSV text, in order, record by record, or why the text
/// isn't CSV, after which there are none.
pub(crate) struct Fields<'a> {
    text: &'a str,
    at: usize,
    line: usize,
    /// Whether the next field starts a record.
    starts: bool,
    failed: bool,
}

impl<'a> Fields<'a> {
    pub(crate) fn new(text: &'a str) -> Self {
        Fields {
            text: text.strip_prefix('\u{FEFF}').unwrap_or(text),
            at: 0,
            line: 1,
            starts: true,
            failed: false,
        }
    }

    /// The length of the line end at `at`, if there is one there.
    fn line_end(&self, at: usize) -> Option<usize> {
        match self.text.as_bytes().get(at..)? {
            [b'\n', ..] => Some(1),
            [b'\r', b'\n', ..] => Some(2),
            _ => None,
        }
    }

    /// `why` the text isn't CSV, on `line`, after which there are no
    /// fields.
    fn fail(&mut self, why: Malformed, line: usize) -> NotCsv {
        self.failed = true;
        NotCsv { why, line }
    }
}

impl<'a> Iterator for Fields<'a> {
    type Item = Result<Field<'a>, NotCsv>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed {
            return None;
        }
        if self.starts {
            while let Some(end) = self.line_end(self.at) {
                self.at += end;
                self.line += 1;
            }
            if self.at == self.text.len() {
                return None;
            }
        }
        let bytes = self.text.as_bytes();
        let line = self.line;
        let quoted = bytes.get(self.at) == Some(&b'"');
        let (written, mut end) = if quoted {
            let start = self.at + 1;
            let mut i = start;
            loop {
                match bytes.get(i) {
                    None => return Some(Err(self.fail(Malformed::Unclosed, line))),
                    Some(b'"') if bytes.get(i + 1) == Some(&b'"') => i += 2,
                    Some(b'"') => break,
                    Some(b'\n') => {
                        self.line += 1;
                        i += 1;
                    }
                    Some(_) => i += 1,
                }
            }
            (&self.text[start..i], i + 1)
        } else {
            let start = self.at;
            let mut i = start;
            while i < bytes.len() && bytes[i] != b',' && self.line_end(i).is_none() {
                if bytes[i] == b'"' {
                    return Some(Err(self.fail(Malformed::StrayQuote, line)));
                }
                i += 1;
            }
            (&self.text[start..i], i)
        };
        let last = if end == bytes.len() {
            true
        } else if bytes[end] == b',' {
            end += 1;
            false
        } else if let Some(length) = self.line_end(end) {
            end += length;
            self.line += 1;
            true
        } else {
            return Some(Err(self.fail(Malformed::AfterQuote, self.line)));
        };
        self.at = end;
        self.starts = last;
        Some(Ok(Field {
            written,
            quoted,
            line,
            last,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A record: the line it starts on, and its fields' values.
    type Record = (usize, Vec<String>);

    /// The records of `text`, or why it isn't CSV.
    fn records(text: &str) -> Result<Vec<Record>, NotCsv> {
        let mut records: Vec<Record> = Vec::new();
        let mut starts = true;
        for field in Fields::new(text) {
            let field = field?;
            let mut value = vec![0; field.len()];
            field.copy_to(&mut value);
            let value = String::from_utf8(value).unwrap();
            assert_eq!(
                field.as_written().is_some(),
                !value.contains('"') || !field.quoted
            );
            if starts {
                records.push((field.line, Vec::new()));
            }
            records.last_mut().unwrap().1.push(value);
            starts = field.last;
        }
        Ok(records)
    }

    fn record(line: usize, fields: &[&str]) -> Record {
        (line, fields.iter().map(|f| (*f).to_string()).collect())
    }

    #[test]
    fn records_are_lines_of_fields_separated_by_commas() {
        assert_eq!(
            records("river,km\nNile,6650\r\nAmazon, 6400\n"),
            Ok(vec![
                record(1, &["river", "km"]),
                record(2, &["Nile", "6650"]),
                record(3, &["Amazon", " 6400"]),
            ])
        );
        // The last line end is optional, and an empty field is a field.
        assert_eq!(
            records("a,,\n,b"),
            Ok(vec![record(1, &["a", "", ""]), record(2, &["", "b"])])
        );
        assert_eq!(records(""), Ok(vec![]));
    }

    #[test]
    fn quoted_fields_hold_commas_quotes_and_line_ends() {
        assert_eq!(
            records("name,note\n\"Paraná\",\"long, \"\"wide\"\"\nand slow\"\n\"\",x\n"),
            Ok(vec![
                record(1, &["name", "note"]),
                record(2, &["Paraná", "long, \"wide\"\nand slow"]),
                record(4, &["", "x"]),
            ])
        );
    }

    #[test]
    fn empty_lines_and_a_byte_order_mark_are_skipped() {
        assert_eq!(
            records("\u{FEFF}a,b\n\n1,2\r\n\r\n\n3,4\n\n"),
            Ok(vec![
                record(1, &["a", "b"]),
                record(3, &["1", "2"]),
                record(6, &["3", "4"]),
            ])
        );
        // A bare carriage return ends no line.
        assert_eq!(records("a\rb\n"), Ok(vec![record(1, &["a\rb"])]));
    }

    #[test]
    fn the_header_is_the_first_records_fields() {
        let names = |text| {
            header(text)
                .map(|f| f.as_written().unwrap_or("?"))
                .collect::<Vec<_>>()
        };
        assert_eq!(names("\n\nriver,\"km\"\nNile,6650\n"), ["river", "km"]);
        assert_eq!(names("\"say \"\"hi\"\"\",x"), ["?", "x"]);
        assert!(names("").is_empty());
        // As far as it is CSV.
        assert_eq!(names("a,b\"c\n"), ["a"]);
    }

    #[test]
    fn malformed_text_is_reported_on_its_line() {
        assert_eq!(
            records("a,b\nsay \"hi\",x\n"),
            Err(NotCsv {
                why: Malformed::StrayQuote,
                line: 2
            })
        );
        assert_eq!(
            records("a\n\"hi\"!,x\n"),
            Err(NotCsv {
                why: Malformed::AfterQuote,
                line: 2
            })
        );
        assert_eq!(
            records("a\n\"one\ntwo\" x\n"),
            Err(NotCsv {
                why: Malformed::AfterQuote,
                line: 3
            })
        );
        assert_eq!(
            records("a\nb\n\"never\nclosed\n"),
            Err(NotCsv {
                why: Malformed::Unclosed,
                line: 3
            })
        );
    }
}
