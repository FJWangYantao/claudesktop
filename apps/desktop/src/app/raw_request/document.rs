use std::ops::Range;

const EDITOR_BYTES: usize = 128 * 1024;
const EDITOR_LINES: usize = 1024;
const CHUNK_BYTES: usize = 1024;

pub(super) enum Text {
    Small(String),
    Large(Document),
}

impl Text {
    pub(super) fn prepare(text: String) -> Self {
        if text.len() <= EDITOR_BYTES
            && text.split('\n').take(EDITOR_LINES + 1).count() <= EDITOR_LINES
            && text.split('\n').all(|line| line.len() <= CHUNK_BYTES)
        {
            Self::Small(text)
        } else {
            Self::Large(Document::new(text))
        }
    }
}

pub(super) struct Document {
    pub(super) text: String,
    rows: Vec<Row>,
    pub(super) line_count: usize,
}

struct Row {
    start: usize,
    line: usize,
    continuation: bool,
}

impl Document {
    fn new(text: String) -> Self {
        let mut rows = Vec::new();
        let mut offset = 0;
        let mut line_count = 0;
        for line in text.split('\n') {
            line_count += 1;
            rows.push(Row { start: offset, line: line_count, continuation: false });
            let display_len = line.strip_suffix('\r').unwrap_or(line).len();
            let mut start = 0;
            while display_len - start > CHUNK_BYTES {
                start = line.floor_char_boundary(start + CHUNK_BYTES);
                rows.push(Row { start: offset + start, line: line_count, continuation: true });
            }
            offset += line.len() + 1;
        }
        Self { text, rows, line_count }
    }

    pub(super) fn len(&self) -> usize {
        self.rows.len()
    }

    pub(super) fn row(&self, index: usize) -> (usize, Range<usize>) {
        let row = &self.rows[index];
        let end = self.rows.get(index + 1).map_or(self.text.len(), |next| next.start);
        let displayed = self.text[row.start..end].trim_end_matches('\n');
        let displayed = if displayed.len() != end - row.start {
            displayed.strip_suffix('\r').unwrap_or(displayed)
        } else {
            displayed
        };
        let number = if row.continuation { 0 } else { row.line };
        (number, row.start..row.start + displayed.len())
    }

    pub(super) fn line_at(&self, offset: usize) -> usize {
        self.rows[self.row_at(offset)].line
    }

    pub(super) fn row_at(&self, offset: usize) -> usize {
        self.rows.partition_point(|row| row.start <= offset).saturating_sub(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_chunks_keep_utf8_and_logical_lines() {
        let long = format!("{}界😀tail", "a".repeat(CHUNK_BYTES - 1));
        let text = format!("head\r\n\n{long}\r\nlast\r\n");
        let doc = Document::new(text.clone());
        let rows: Vec<_> = (0..doc.len())
            .map(|i| {
                let (number, bytes) = doc.row(i);
                (number, &doc.text[bytes])
            })
            .collect();
        assert_eq!(
            rows,
            vec![
                (1, "head"),
                (2, ""),
                (3, &long[..CHUNK_BYTES - 1]),
                (0, "界😀tail"),
                (4, "last"),
                (5, "")
            ]
        );
        assert_eq!(doc.row_at(text.find("界").unwrap() - 1), 2);
        assert_eq!(doc.row_at(text.find("界").unwrap()), 3);
        assert_eq!(doc.text, text);
    }

    #[test]
    fn editor_bounds_cover_bytes_lines_and_single_long_lines() {
        assert!(matches!(Text::prepare("hello\n世界".into()), Text::Small(_)));
        assert!(matches!(Text::prepare("x".repeat(CHUNK_BYTES + 1)), Text::Large(_)));
        assert!(matches!(Text::prepare("\n".repeat(EDITOR_LINES)), Text::Large(_)));
        let text = format!("{}\n", "x".repeat(CHUNK_BYTES - 1)).repeat(129);
        assert!(matches!(Text::prepare(text), Text::Large(_)));
    }
}
