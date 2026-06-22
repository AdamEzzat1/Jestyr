//! The hand-written lexer.
//!
//! It turns a `&str` of Jestyr source into a `Vec<Token>` ending in `Eof`, plus
//! a `Vec<Diagnostic>` for anything it couldn't make sense of. It never panics
//! and never stops early: a bad character becomes a `TokenKind::Unknown` token
//! with a recorded diagnostic, and lexing continues. That error-recovery
//! discipline is what lets later passes report *all* the problems in a file, not
//! just the first.
//!
//! It is deliberately dependency-free and operates on byte offsets with small
//! character-level lookahead (`peek`/`peek2`), which is enough for every
//! multi-character operator in the grammar (`->`, `..=`, `.*`, `<<`, `>=`, ...).

use crate::diag::Diagnostic;
use crate::span::Span;
use crate::token::{Token, TokenKind};

pub struct Lexer<'src> {
    src: &'src str,
    pos: usize,
    /// One past the last byte this lexer may read. For a whole-file lex this is
    /// `src.len()`; the module loader lexes one file's *region* of a shared,
    /// concatenated buffer so every token's span is global (and thus unique
    /// across files) while the buffer itself slices correctly.
    end: usize,
    pub diagnostics: Vec<Diagnostic>,
}

impl<'src> Lexer<'src> {
    pub fn new(src: &'src str) -> Lexer<'src> {
        let end = src.len();
        Lexer { src, pos: 0, end, diagnostics: Vec::new() }
    }

    /// Lex the byte region `[start, end)` of `src`, emitting spans relative to
    /// `src` (i.e. *global* offsets). Used by the module loader so a single
    /// concatenated source buffer backs every module's tokens and diagnostics.
    pub fn new_slice(src: &'src str, start: usize, end: usize) -> Lexer<'src> {
        Lexer { src, pos: start, end, diagnostics: Vec::new() }
    }

    /// Lex the entire input. The returned token vector always ends with `Eof`.
    pub fn tokenize(mut self) -> (Vec<Token>, Vec<Diagnostic>) {
        let mut tokens = Vec::new();
        loop {
            let tok = self.next_token();
            let done = tok.kind == TokenKind::Eof;
            tokens.push(tok);
            if done {
                break;
            }
        }
        (tokens, self.diagnostics)
    }

    // --- character cursor helpers ---

    fn peek(&self) -> Option<char> {
        if self.pos >= self.end {
            return None;
        }
        self.src[self.pos..self.end].chars().next()
    }

    fn peek2(&self) -> Option<char> {
        if self.pos >= self.end {
            return None;
        }
        let mut it = self.src[self.pos..self.end].chars();
        it.next();
        it.next()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += c.len_utf8();
        Some(c)
    }

    fn at(&self, c: char) -> bool {
        self.peek() == Some(c)
    }

    /// Consume `c` if it is next; report whether we did.
    fn eat(&mut self, c: char) -> bool {
        if self.at(c) {
            self.pos += c.len_utf8();
            true
        } else {
            false
        }
    }

    fn eat_while(&mut self, mut pred: impl FnMut(char) -> bool) {
        while let Some(c) = self.peek() {
            if pred(c) {
                self.pos += c.len_utf8();
            } else {
                break;
            }
        }
    }

    fn error(&mut self, start: usize, message: impl Into<String>) {
        self.diagnostics
            .push(Diagnostic::new(message, Span::new(start, self.pos)));
    }

    // --- the main dispatch ---

    fn next_token(&mut self) -> Token {
        self.skip_trivia();
        let start = self.pos;

        let c = match self.bump() {
            Some(c) => c,
            None => return Token::new(TokenKind::Eof, Span::new(start, start)),
        };

        use TokenKind::*;
        let kind = match c {
            // Identifiers & keywords (digits are excluded from ident-start, so a
            // leading digit falls through to the number arm below).
            c if is_ident_start(c) => return self.ident(start),
            c if c.is_ascii_digit() => return self.number(start),
            '"' => return self.string(start),
            '\'' => return self.char_lit(start),

            '(' => LParen,
            ')' => RParen,
            '{' => LBrace,
            '}' => RBrace,
            '[' => LBracket,
            ']' => RBracket,
            ',' => Comma,
            ';' => Semi,
            '@' => At,
            '~' => Tilde,
            '?' => Question,

            ':' => if self.eat(':') { ColonColon } else { Colon },

            '.' => {
                if self.eat('.') {
                    if self.eat('=') { DotDotEq } else { DotDot }
                } else if self.eat('*') {
                    DotStar
                } else {
                    Dot
                }
            }

            '+' => if self.eat('=') { PlusEq } else { Plus },
            '-' => {
                if self.eat('>') { Arrow }
                else if self.eat('=') { MinusEq }
                else { Minus }
            }
            '*' => if self.eat('=') { StarEq } else { Star },
            // `//` and `/*` were already consumed as trivia, so a `/` here is real.
            '/' => if self.eat('=') { SlashEq } else { Slash },
            '%' => if self.eat('=') { PercentEq } else { Percent },

            '=' => {
                if self.eat('=') { EqEq }
                else if self.eat('>') { FatArrow }
                else { Eq }
            }
            '!' => if self.eat('=') { Ne } else { Bang },
            '<' => {
                if self.eat('=') { Le }
                else if self.eat('<') { Shl }
                else { Lt }
            }
            '>' => {
                if self.eat('=') { Ge }
                else if self.eat('>') { Shr }
                else { Gt }
            }
            '&' => {
                if self.eat('&') { AmpAmp }
                else if self.eat('=') { AmpEq }
                else { Amp }
            }
            '|' => {
                if self.eat('|') { PipePipe }
                else if self.eat('=') { PipeEq }
                else { Pipe }
            }
            '^' => if self.eat('=') { CaretEq } else { Caret },

            other => {
                self.error(start, format!("unexpected character `{}`", other));
                Unknown
            }
        };

        Token::new(kind, Span::new(start, self.pos))
    }

    // --- sub-lexers (each assumes its first char was already consumed) ---

    fn ident(&mut self, start: usize) -> Token {
        self.eat_while(is_ident_continue);
        let text = &self.src[start..self.pos];
        let kind = if text == "_" {
            TokenKind::Underscore
        } else {
            TokenKind::keyword(text).unwrap_or(TokenKind::Ident)
        };
        Token::new(kind, Span::new(start, self.pos))
    }

    fn number(&mut self, start: usize) -> Token {
        // Radix prefixes. `start` still points at the leading `0`.
        let rest = &self.src[start..];
        if rest.starts_with("0x") || rest.starts_with("0X") {
            self.pos = start + 2;
            self.eat_while(|c| c.is_ascii_hexdigit() || c == '_');
            return Token::new(TokenKind::Int, Span::new(start, self.pos));
        }
        if rest.starts_with("0b") || rest.starts_with("0B") {
            self.pos = start + 2;
            self.eat_while(|c| c == '0' || c == '1' || c == '_');
            return Token::new(TokenKind::Int, Span::new(start, self.pos));
        }

        let mut is_float = false;

        // Integer part (first digit already consumed by the caller).
        self.eat_while(|c| c.is_ascii_digit() || c == '_');

        // Fractional part — but only if a digit follows the dot, so that `1..2`
        // lexes as `1`, `..`, `2` and not as a malformed float.
        if self.at('.') && self.peek2().map_or(false, |c| c.is_ascii_digit()) {
            is_float = true;
            self.bump(); // consume '.'
            self.eat_while(|c| c.is_ascii_digit() || c == '_');
        }

        // Exponent.
        if matches!(self.peek(), Some('e' | 'E')) {
            is_float = true;
            self.bump();
            if matches!(self.peek(), Some('+' | '-')) {
                self.bump();
            }
            self.eat_while(|c| c.is_ascii_digit() || c == '_');
        }

        let kind = if is_float { TokenKind::Float } else { TokenKind::Int };
        Token::new(kind, Span::new(start, self.pos))
    }

    fn string(&mut self, start: usize) -> Token {
        loop {
            match self.bump() {
                None => {
                    self.error(start, "unterminated string literal");
                    break;
                }
                Some('"') => break,
                Some('\\') => {
                    // Skip the escaped character (escape *validation* happens later,
                    // during literal lowering; the lexer only needs to find the end).
                    self.bump();
                }
                Some(_) => {}
            }
        }
        Token::new(TokenKind::Str, Span::new(start, self.pos))
    }

    fn char_lit(&mut self, start: usize) -> Token {
        match self.bump() {
            None => {
                self.error(start, "unterminated character literal");
                return Token::new(TokenKind::Char, Span::new(start, self.pos));
            }
            Some('\'') => {
                self.error(start, "empty character literal");
                return Token::new(TokenKind::Char, Span::new(start, self.pos));
            }
            Some('\\') => {
                self.bump(); // escaped char
            }
            Some(_) => {}
        }
        if !self.eat('\'') {
            self.error(start, "unterminated or multi-character character literal");
        }
        Token::new(TokenKind::Char, Span::new(start, self.pos))
    }

    /// Skip whitespace, `// line comments`, and `/* nested block comments */`.
    fn skip_trivia(&mut self) {
        loop {
            match self.peek() {
                Some(c) if c.is_whitespace() => {
                    self.pos += c.len_utf8();
                }
                Some('/') if self.peek2() == Some('/') => {
                    self.pos += 2;
                    self.eat_while(|c| c != '\n');
                }
                Some('/') if self.peek2() == Some('*') => {
                    let start = self.pos;
                    self.pos += 2;
                    let mut depth = 1;
                    while depth > 0 {
                        match self.bump() {
                            None => {
                                self.error(start, "unterminated block comment");
                                break;
                            }
                            Some('/') if self.at('*') => {
                                self.bump();
                                depth += 1;
                            }
                            Some('*') if self.at('/') => {
                                self.bump();
                                depth -= 1;
                            }
                            Some(_) => {}
                        }
                    }
                }
                _ => break,
            }
        }
    }
}

fn is_ident_start(c: char) -> bool {
    c == '_' || c.is_alphabetic()
}

fn is_ident_continue(c: char) -> bool {
    c == '_' || c.is_alphanumeric()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::token::TokenKind::*;

    /// Lex `src`, assert it produced no diagnostics, and return the non-`Eof`
    /// token kinds.
    fn kinds(src: &str) -> Vec<TokenKind> {
        let (toks, diags) = Lexer::new(src).tokenize();
        assert!(diags.is_empty(), "unexpected diagnostics: {:?}", diags);
        toks.into_iter().map(|t| t.kind).filter(|k| *k != Eof).collect()
    }

    #[test]
    fn keywords_and_conventions_vs_idents() {
        assert_eq!(
            kinds("fn foo read mut take out self Self"),
            vec![Fn, Ident, Read, Mut, Take, Out, SelfValue, SelfType]
        );
    }

    #[test]
    fn numbers() {
        assert_eq!(
            kinds("0 42 1_000 0xFF 0x4000_C000 0b1010 3.14 1.5e-3 1e10"),
            vec![Int, Int, Int, Int, Int, Int, Float, Float, Float]
        );
    }

    #[test]
    fn ranges_are_not_floats() {
        assert_eq!(kinds("1..2"), vec![Int, DotDot, Int]);
        assert_eq!(kinds("0..=10"), vec![Int, DotDotEq, Int]);
    }

    #[test]
    fn multi_char_operators() {
        assert_eq!(
            kinds("-> => :: .. ..= .* << >> >= <= == != += &&"),
            vec![Arrow, FatArrow, ColonColon, DotDot, DotDotEq, DotStar, Shl, Shr, Ge, Le, EqEq, Ne, PlusEq, AmpAmp]
        );
    }

    #[test]
    fn pointer_deref() {
        // The `(p + i).*` raw-store idiom from the Vec example.
        assert_eq!(
            kinds("(p + i).*"),
            vec![LParen, Ident, Plus, Ident, RParen, DotStar]
        );
    }

    #[test]
    fn error_set_syntax() {
        // `!{ Io, Parse }` is just `!` then a brace group.
        assert_eq!(
            kinds("!{ Io , Parse }"),
            vec![Bang, LBrace, Ident, Comma, Ident, RBrace]
        );
    }

    #[test]
    fn refinement_range() {
        // `i: usize in 0..len`
        assert_eq!(
            kinds("i : usize in 0 .. len"),
            vec![Ident, Colon, Ident, In, Int, DotDot, Ident]
        );
    }

    #[test]
    fn strings_and_chars() {
        assert_eq!(kinds(r#""hello" '0' '\n'"#), vec![Str, Char, Char]);
    }

    #[test]
    fn comments_are_trivia() {
        assert_eq!(
            kinds("a // line comment\n b /* block /* nested */ still */ c"),
            vec![Ident, Ident, Ident]
        );
    }

    #[test]
    fn lone_underscore_is_wildcard() {
        assert_eq!(kinds("_ _x x_"), vec![Underscore, Ident, Ident]);
    }

    #[test]
    fn attribute_sigil() {
        assert_eq!(kinds("@volatile u32"), vec![At, Ident, Ident]);
    }

    #[test]
    fn recovers_from_unterminated_string() {
        let (_toks, diags) = Lexer::new("\"oops").tokenize();
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("unterminated string"));
    }

    #[test]
    fn recovers_from_stray_character() {
        // A backtick is not part of the grammar; we record one error and continue.
        let (toks, diags) = Lexer::new("a ` b").tokenize();
        assert_eq!(diags.len(), 1);
        let ks: Vec<_> = toks.iter().map(|t| t.kind).filter(|k| *k != Eof).collect();
        assert_eq!(ks, vec![Ident, Unknown, Ident]);
    }
}
