//! Diagnostics.
//!
//! A diagnostic is a message plus the span it points at, optionally an error
//! code and a `help:` suggestion. The design doc (§15) promises *teaching*
//! diagnostics — a source snippet with carets under the offending span, not just
//! a `line:col`. That rendering lives in [`Diagnostic::render`].
//!
//! The *collect-don't-panic* discipline starts at the lexer: a bad character is
//! recorded and recovered from, so one typo can't hide every later error.

use crate::span::{line_col, Span};

/// How severe a diagnostic is (controls the leading label).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Severity {
    Error,
    #[allow(dead_code)] // used once cgen/typeck emit non-fatal warnings
    Warning,
    Note,
}

impl Severity {
    fn label(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Note => "note",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Diagnostic {
    pub message: String,
    pub span: Span,
    /// The severity this diagnostic *is* (independent of how a caller renders it).
    /// Errors are fatal; warnings are reported but don't fail the build.
    pub severity: Severity,
    /// An optional stable error code (e.g. `E0042`), shown as `error[E0042]`.
    pub code: Option<&'static str>,
    /// An optional suggestion, rendered on a trailing `= help:` line.
    pub help: Option<String>,
}

impl Diagnostic {
    pub fn new(message: impl Into<String>, span: Span) -> Diagnostic {
        Diagnostic { message: message.into(), span, severity: Severity::Error, code: None, help: None }
    }

    /// A non-fatal warning (e.g. a redundant match arm).
    pub fn warning(message: impl Into<String>, span: Span) -> Diagnostic {
        Diagnostic { message: message.into(), span, severity: Severity::Warning, code: None, help: None }
    }

    /// Whether this diagnostic is fatal (an error, not a warning/note).
    pub fn is_error(&self) -> bool {
        self.severity == Severity::Error
    }

    /// Attach a stable error code.
    #[allow(dead_code)] // codes are assigned incrementally as rules stabilize
    pub fn with_code(mut self, code: &'static str) -> Diagnostic {
        self.code = Some(code);
        self
    }

    /// Attach a `help:` suggestion line.
    #[allow(dead_code)] // suggestions are added per-rule as they're written
    pub fn with_help(mut self, help: impl Into<String>) -> Diagnostic {
        self.help = Some(help.into());
        self
    }

    /// Render a teaching-quality diagnostic: a labelled header, a `-->` locator,
    /// and the offending source line with a caret underline. For example:
    ///
    /// ```text
    /// error: cannot return borrow `p`
    ///   --> examples/escapes.jtr:11:5
    ///    |
    /// 11 |     return p
    ///    |     ^^^^^^^^
    /// ```
    pub fn render(&self, src: &str, path: &str, severity: Severity) -> String {
        let lc = line_col(src, self.span.start);
        let start = self.span.start as usize;

        // The byte range of the line containing the span's start.
        let line_start = src[..start.min(src.len())].rfind('\n').map(|i| i + 1).unwrap_or(0);
        let line_end = src[line_start..]
            .find('\n')
            .map(|i| line_start + i)
            .unwrap_or(src.len());
        let line_text = src[line_start..line_end].trim_end_matches('\r');

        // Caret position/length, in characters, clamped to this line.
        let caret_col = lc.col.saturating_sub(1) as usize;
        let end = (self.span.end as usize).min(line_end).max(start);
        let span_len = src.get(start..end).map(|s| s.chars().count()).unwrap_or(0).max(1);

        let gutter = lc.line.to_string();
        let pad = " ".repeat(gutter.len());
        let code = self.code.map(|c| format!("[{c}]")).unwrap_or_default();
        let underline = format!("{}{}", " ".repeat(caret_col), "^".repeat(span_len));

        let mut out = String::new();
        out.push_str(&format!("{}{code}: {}\n", severity.label(), self.message));
        out.push_str(&format!("{pad}--> {path}:{}:{}\n", lc.line, lc.col));
        out.push_str(&format!("{pad} |\n"));
        out.push_str(&format!("{gutter} | {line_text}\n"));
        out.push_str(&format!("{pad} | {underline}\n"));
        if let Some(help) = &self.help {
            out.push_str(&format!("{pad} = help: {help}\n"));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_a_caret_under_the_span() {
        let src = "fn f() {\n    p.z\n}";
        let span = Span::new(13, 16); // `p.z` on line 2, column 5
        let out = Diagnostic::new("no field `z` on struct `Point`", span)
            .render(src, "t.jtr", Severity::Error);
        assert!(out.contains("error: no field `z`"), "{out}");
        assert!(out.contains("--> t.jtr:2:5"), "{out}");
        assert!(out.contains("2 |     p.z"), "{out}");
        assert!(out.contains("^^^"), "{out}");
    }

    #[test]
    fn includes_code_and_help_when_present() {
        let out = Diagnostic::new("oops", Span::new(0, 1))
            .with_code("E0001")
            .with_help("try `y`")
            .render("x", "t.jtr", Severity::Error);
        assert!(out.contains("error[E0001]: oops"), "{out}");
        assert!(out.contains("= help: try `y`"), "{out}");
    }
}
