//! Source positions.
//!
//! Every token and (later) every AST node carries a [`Span`]: a half-open byte
//! range into the original source text. We store *byte offsets*, not line/column
//! pairs, because (a) it is 8 bytes and `Copy`, and (b) line/column is a
//! presentation concern computed lazily only when we actually print a diagnostic.
//! This is the same trade rustc makes, and it keeps the hot path allocation-free.

use std::ops::Range;

/// A half-open byte range `[start, end)` into a source file.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Span {
    pub start: u32,
    pub end: u32,
}

impl Span {
    pub fn new(start: usize, end: usize) -> Span {
        Span { start: start as u32, end: end as u32 }
    }

    /// The smallest span covering both `self` and `other`.
    /// Used by the parser to span a whole production from its first to last token.
    #[allow(dead_code)] // wired up in pipeline stage ② (parser)
    pub fn to(self, other: Span) -> Span {
        Span { start: self.start.min(other.start), end: self.end.max(other.end) }
    }

    pub fn range(self) -> Range<usize> {
        (self.start as usize)..(self.end as usize)
    }

    #[allow(dead_code)] // used by later diagnostics/codegen stages
    pub fn len(self) -> usize {
        (self.end - self.start) as usize
    }

    #[allow(dead_code)] // used by later diagnostics/codegen stages
    pub fn is_empty(self) -> bool {
        self.start == self.end
    }
}

/// A 1-based line/column position, computed on demand for human-facing output.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LineCol {
    pub line: u32,
    pub col: u32,
}

/// Resolve a byte offset to a 1-based line and column.
///
/// O(offset) per call — fine for diagnostics, which are rare. If we ever print
/// many positions per compile we'll precompute a line-start table instead.
pub fn line_col(src: &str, offset: u32) -> LineCol {
    let off = offset as usize;
    let mut line = 1u32;
    let mut col = 1u32;
    for (i, ch) in src.char_indices() {
        if i >= off {
            break;
        }
        if ch == '\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    LineCol { line, col }
}
