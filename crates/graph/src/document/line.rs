//! Source lines and line spans.

/// A newline sequence attached to a source line.
///
/// A line ending is stored explicitly instead of being normalized, because a file may mix LF and
/// CRLF and rendering must reproduce each occurrence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineEnding {
    /// A single `\n`; the ending a Unix-style line carries.
    Lf,
    /// A `\r\n` pair, kept as written so a file that mixes endings round-trips byte for byte.
    Crlf,
}

impl LineEnding {
    /// The bytes that follow the line text in the source.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Lf => "\n",
            Self::Crlf => "\r\n",
        }
    }
}

/// One physical source line, split into text and optional line ending.
///
/// The final line of a file has no ending when the file does not end with a newline. Blank lines
/// are ordinary lines whose text is empty or whitespace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceLine {
    text: String,
    ending: Option<LineEnding>,
}

impl SourceLine {
    pub(crate) fn new(text: String, ending: Option<LineEnding>) -> Self {
        Self { text, ending }
    }

    /// Line text without its newline sequence.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The original newline sequence, or `None` for a final line without one.
    pub fn ending(&self) -> Option<LineEnding> {
        self.ending
    }

    /// Whether this line has any newline sequence.
    pub fn has_ending(&self) -> bool {
        self.ending.is_some()
    }

    /// Whether the text is empty or contains only whitespace.
    pub fn is_blank(&self) -> bool {
        self.text.trim().is_empty()
    }

    /// Append the exact source bytes of this line to `out`.
    pub(crate) fn write_to(&self, out: &mut String) {
        out.push_str(&self.text);
        if let Some(ending) = self.ending {
            out.push_str(ending.as_str());
        }
    }
}

/// A half-open range of line indices into a [`PageDocument`](crate::document::PageDocument).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LineSpan {
    start: usize,
    end: usize,
}

impl LineSpan {
    /// Create a span covering `start..end`.
    pub const fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }

    /// An empty span at `at`.
    pub const fn empty(at: usize) -> Self {
        Self { start: at, end: at }
    }

    /// First line index, inclusive.
    pub const fn start(self) -> usize {
        self.start
    }

    /// Last line index, exclusive.
    pub const fn end(self) -> usize {
        self.end
    }

    /// Number of lines in the span.
    pub const fn len(self) -> usize {
        self.end - self.start
    }

    /// Whether the span contains no lines.
    pub const fn is_empty(self) -> bool {
        self.start == self.end
    }

    /// Whether `line` is inside the span.
    pub const fn contains(self, line: usize) -> bool {
        line >= self.start && line < self.end
    }

    /// Return this span with a different exclusive end.
    pub(crate) const fn with_end(self, end: usize) -> Self {
        Self {
            start: self.start,
            end,
        }
    }
}
