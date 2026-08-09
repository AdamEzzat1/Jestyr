//! A **lossless** view of the token stream, for formatter/LSP tooling.
//!
//! The compiler proper throws trivia away: whitespace and comments never become
//! tokens, which is what keeps the grammar independent of them (a comment can
//! never change how code parses — see `docs/comments.md`). A formatter needs the
//! opposite: it must be able to reprint a file byte-for-byte, comments included.
//!
//! **Nothing is actually lost today.** [`Span`]s are exact byte ranges and the
//! token stream is ordered and gap-free by construction, so the trivia before
//! token *i* is exactly `src[tokens[i-1].end .. tokens[i].start]`. This module
//! only *materializes* that, which is why it needs no change to the lexer, the
//! parser, or the AST — and therefore owes nothing to the byte-identity goldens
//! or to the self-hosted port. (The port's doc generator already relies on the
//! same fact: `tokens.collect_docs` finds comments by scanning these gaps.)
//!
//! The load-bearing guarantee is [`render`]'s round trip:
//!
//! ```text
//! render(&attach(src, &tokens)) == src        for every input
//! ```
//!
//! It is checked as a property over arbitrary text and over the whole corpus. A
//! lossless tree that cannot reproduce its input is not lossless, and that one
//! equation is impossible to satisfy by accident.
//!
//! This is Stage 1 of the plan in `docs/frontend-roadmap.md` §3: a token-level
//! CST. Attaching syntax nodes to token ranges (Stage 2) and a full green/red
//! tree (Stage 3) build on this and are deliberately not here.

// Stage 1 is a complete, tested capability with no in-tree consumer yet: the
// compiler proper has no reason to materialize trivia, and the formatter/LSP that
// will are Stage 2+. Its correctness is pinned by the round-trip properties over
// arbitrary text and the whole corpus (`proptests::cst_props`), so it does not
// rot while it waits.
#![allow(dead_code)]

use crate::span::Span;
use crate::token::Token;

/// One token together with the trivia that immediately precedes it.
///
/// Every byte of the source belongs to exactly one `CstToken` — either to its
/// `trivia` or to its `token`'s own span — which is what makes the stream
/// losslessly renderable. The final `Eof` token carries any trailing whitespace
/// or comments as its trivia and contributes an empty lexeme.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CstToken {
    /// Whitespace and/or comments before `token`. Empty (`start == end`) when the
    /// previous token abuts this one.
    pub trivia: Span,
    pub token: Token,
}

impl CstToken {
    /// The full source range this element covers: its trivia and its lexeme.
    #[allow(dead_code)] // Stage 2 (syntax-node → token-range mapping) is its first consumer
    pub fn full_span(self) -> Span {
        Span { start: self.trivia.start, end: self.token.span.end }
    }
}

/// Pair every token with the trivia in front of it.
///
/// `tokens` must be the whole-file stream from [`crate::lexer::Lexer::tokenize`]
/// over `src` (it relies on the spans being ordered and within `src`).
pub fn attach(src: &str, tokens: &[Token]) -> Vec<CstToken> {
    let mut out = Vec::with_capacity(tokens.len());
    let mut prev_end = 0u32;
    for &token in tokens {
        // `max` is defensive rather than expected: a well-formed stream never
        // goes backwards, and going backwards would silently duplicate source.
        let start = prev_end.min(token.span.start);
        out.push(CstToken { trivia: Span { start, end: token.span.start }, token });
        prev_end = token.span.end.max(prev_end);
        let _ = src;
    }
    out
}

/// Reconstruct the source text from a CST stream. The inverse of [`attach`].
pub fn render(src: &str, cst: &[CstToken]) -> String {
    let mut out = String::with_capacity(src.len());
    for e in cst {
        out.push_str(&src[e.trivia.range()]);
        out.push_str(&src[e.token.span.range()]);
    }
    out
}

/// What a run of trivia is made of. A formatter needs to tell "blank line" from
/// "comment" to decide what it may reflow and what it must preserve verbatim.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TriviaKind {
    Whitespace,
    /// `// …` (including the doc forms `///` and `//!`), up to but not including
    /// the newline.
    LineComment,
    /// `/* … */`, honouring nesting, including the doc forms `/**` and `/*!`.
    BlockComment,
}

/// One classified run inside a trivia span.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TriviaPiece {
    pub kind: TriviaKind,
    pub span: Span,
}

/// Split a trivia span into whitespace and comment runs, in source order.
///
/// The pieces tile the input exactly — `pieces(t).map(span).concat() == t` — which
/// is asserted as a property, so this cannot drift from [`render`]'s round trip
/// into silently dropping or duplicating text.
///
/// An unterminated block comment runs to the end of the span rather than being
/// reported: this is a *view*, not a second lexer, and the real lexer has already
/// recorded that diagnostic.
pub fn pieces(src: &str, trivia: Span) -> Vec<TriviaPiece> {
    let bytes = src.as_bytes();
    let end = (trivia.end as usize).min(src.len());
    let mut i = (trivia.start as usize).min(end);
    let mut out = Vec::new();
    while i < end {
        let start = i;
        if bytes[i].is_ascii_whitespace() {
            while i < end && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            out.push(TriviaPiece { kind: TriviaKind::Whitespace, span: Span::new(start, i) });
        } else if bytes[i] == b'/' && i + 1 < end && bytes[i + 1] == b'/' {
            while i < end && bytes[i] != b'\n' {
                i += 1;
            }
            out.push(TriviaPiece { kind: TriviaKind::LineComment, span: Span::new(start, i) });
        } else if bytes[i] == b'/' && i + 1 < end && bytes[i + 1] == b'*' {
            i += 2;
            let mut depth = 1usize;
            while i < end && depth > 0 {
                if i + 1 < end && bytes[i] == b'/' && bytes[i + 1] == b'*' {
                    depth += 1;
                    i += 2;
                } else if i + 1 < end && bytes[i] == b'*' && bytes[i + 1] == b'/' {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
            out.push(TriviaPiece { kind: TriviaKind::BlockComment, span: Span::new(start, i) });
        } else {
            // Not reachable for trivia the lexer produced (it is whitespace and
            // comments by construction). Consume one byte so this cannot spin,
            // and classify it as whitespace so the tiling property still holds.
            i += 1;
            out.push(TriviaPiece { kind: TriviaKind::Whitespace, span: Span::new(start, i) });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::Lexer;

    fn cst_of(src: &str) -> Vec<CstToken> {
        let (tokens, _) = Lexer::new(src).tokenize();
        attach(src, &tokens)
    }

    #[test]
    fn round_trips_a_program_with_comments_and_blank_lines() {
        let src = "// header\n\nfn f() {\n    /* inline */ let a = 1  // trailing\n}\n\n";
        let cst = cst_of(src);
        assert_eq!(render(src, &cst), src);
    }

    #[test]
    fn round_trips_the_empty_and_trivia_only_inputs() {
        for src in ["", "   ", "\n\n", "// only a comment", "/* only a block */\n"] {
            let cst = cst_of(src);
            assert_eq!(render(src, &cst), src, "failed on {src:?}");
        }
    }

    #[test]
    fn trailing_trivia_lands_on_eof() {
        let src = "fn f() {}\n// trailing\n";
        let cst = cst_of(src);
        let last = cst.last().expect("always at least Eof");
        assert_eq!(last.token.kind, crate::token::TokenKind::Eof);
        assert_eq!(&src[last.trivia.range()], "\n// trailing\n");
        assert_eq!(render(src, &cst), src);
    }

    #[test]
    fn abutting_tokens_get_empty_trivia() {
        let src = "f(x)";
        let cst = cst_of(src);
        // `f` `(` `x` `)` all abut; only the leading trivia of `f` could be empty too.
        assert!(cst.iter().all(|e| e.trivia.is_empty()), "unexpected trivia in {src:?}");
        assert_eq!(render(src, &cst), src);
    }

    #[test]
    fn classifies_trivia_runs() {
        let src = "fn f() {}\n  // line\n  /* block */\nfn g() {}\n";
        let cst = cst_of(src);
        // The trivia before `fn g` carries whitespace, a line comment, and a block one.
        let before_g = cst
            .iter()
            .find(|e| &src[e.token.span.range()] == "fn" && e.trivia.len() > 1)
            .expect("found the second fn");
        let kinds: Vec<TriviaKind> = pieces(src, before_g.trivia).iter().map(|p| p.kind).collect();
        assert!(kinds.contains(&TriviaKind::LineComment), "{kinds:?}");
        assert!(kinds.contains(&TriviaKind::BlockComment), "{kinds:?}");
        assert!(kinds.contains(&TriviaKind::Whitespace), "{kinds:?}");
    }

    #[test]
    fn pieces_tile_their_span_exactly() {
        let src = "a  // c\n /* b /* nested */ */\t\nb";
        let cst = cst_of(src);
        for e in &cst {
            let mut rebuilt = String::new();
            for p in pieces(src, e.trivia) {
                rebuilt.push_str(&src[p.span.range()]);
            }
            assert_eq!(rebuilt, &src[e.trivia.range()], "pieces lost bytes");
        }
    }

    #[test]
    fn round_trips_an_unterminated_block_comment() {
        // The lexer reports this; the CST must still reproduce the bytes.
        let src = "fn f() {}\n/* never closed\n";
        let cst = cst_of(src);
        assert_eq!(render(src, &cst), src);
    }
}
