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

/// A precomputed table of line-start byte offsets, so a position lookup is a
/// binary search instead of a scan from byte 0.
///
/// [`line_col`] is O(offset), which is fine for *one* position but quadratic over
/// a whole file's worth of them — and several callers do exactly that: the
/// diagnostic renderer (once per diagnostic, and a failing build can emit tens of
/// thousands), the token dump (once per token), and the analysis reports. Build
/// one of these per source and reuse it across the loop.
#[derive(Default)]
pub struct LineIndex {
    /// Byte offset at which each line starts. `starts[0]` is always 0, then one
    /// entry per `\n` pointing just past it — sorted by construction.
    starts: Vec<u32>,
}

impl LineIndex {
    pub fn new(src: &str) -> LineIndex {
        let mut starts = vec![0u32];
        starts.extend(src.bytes().enumerate().filter(|(_, b)| *b == b'\n').map(|(i, _)| i as u32 + 1));
        LineIndex { starts }
    }

    /// The 1-based line/column of `offset` in `src` — identical to what
    /// [`line_col`] returns for the same input, but O(log lines) for the line plus
    /// O(column) for the character count within it, rather than O(offset).
    ///
    /// `src` must be the same text the index was built from.
    pub fn line_col(&self, src: &str, offset: u32) -> LineCol {
        // `partition_point` counts the line starts at or before `offset`; the
        // always-present 0 entry supplies the 1-based bias, so this equals
        // `1 + (newlines strictly before offset)` — what `line_col` counts.
        let line = self.starts.partition_point(|&s| s <= offset).max(1);
        let line_start = self.starts[line - 1] as usize;
        // Clamp like `line_col` does implicitly: it stops at end-of-input rather
        // than indexing past it, so an out-of-range span resolves, never panics.
        let mut end = (offset as usize).min(src.len()).max(line_start);
        // `line_col` counts every character that *starts* before `offset`, so an
        // offset landing inside a multi-byte character still counts that character.
        // Round up to the enclosing character's end to reproduce that — and because
        // slicing at a non-boundary would panic. Spans from the lexer are always on
        // boundaries; a synthesized one need not be.
        while end < src.len() && !src.is_char_boundary(end) {
            end += 1;
        }
        let col = 1 + src[line_start..end].chars().count() as u32;
        LineCol { line: line as u32, col }
    }
}

/// Resolve a byte offset to a 1-based line and column.
///
/// O(offset) per call. Fine for a one-off position; for a loop over many
/// positions in the same source, build a [`LineIndex`] once instead.
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

#[cfg(test)]
mod tests {
    use super::*;

    /// `LineIndex` must agree with `line_col` at *every* byte offset, including
    /// one past the end — the whole point of the swap is that it changes nothing
    /// but the cost.
    #[test]
    fn line_index_agrees_with_the_scan_everywhere() {
        let cases = [
            "",
            "\n",
            "a",
            "a\n",
            "ab\ncd\nef",
            "\n\n\n",
            "no trailing newline",
            "unicode: héllo wörld\nsecond ligne é\n\nfourth",
            "tabs\there\r\nwindows\r\nendings\r\n",
        ];
        for src in cases {
            for off in 0..=(src.len() + 2) {
                let idx = LineIndex::new(src);
                let want = line_col(src, off as u32);
                let got = idx.line_col(src, off as u32);
                assert_eq!(got, want, "src={src:?} offset={off}");
            }
        }
    }

    /// The same agreement over every real corpus file, at every token boundary —
    /// the inputs the compiler actually resolves positions for.
    #[test]
    fn line_index_agrees_over_the_corpus() {
        let dir = std::path::Path::new("examples");
        let mut checked = 0usize;
        for entry in std::fs::read_dir(dir).expect("examples/ must exist") {
            let p = entry.expect("readable entry").path();
            if p.extension().and_then(|e| e.to_str()) != Some("jtr") {
                continue;
            }
            let src = std::fs::read_to_string(&p).expect("readable file");
            let idx = LineIndex::new(&src);
            // Every byte offset is overkill on a big corpus; step through every
            // offset of the first 4 KiB plus every line start, which covers the
            // boundary cases (line starts, ends, and the last line).
            let probes = (0..src.len().min(4096))
                .chain((0..src.len()).filter(|&i| src.as_bytes()[i] == b'\n'))
                .chain([src.len(), src.len() + 1]);
            for off in probes {
                assert_eq!(
                    idx.line_col(&src, off as u32),
                    line_col(&src, off as u32),
                    "{} at offset {off}", p.display()
                );
                checked += 1;
            }
        }
        assert!(checked > 100_000, "expected a broad sweep, checked {checked}");
    }
}
