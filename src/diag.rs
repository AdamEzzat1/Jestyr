//! Diagnostics.
//!
//! A diagnostic is a message plus the span it points at, optionally an error
//! code and a `help:` suggestion. The design doc (§15) promises *teaching*
//! diagnostics — a source snippet with carets under the offending span, not just
//! a `line:col`. That rendering lives in [`Diagnostic::render`].
//!
//! The *collect-don't-panic* discipline starts at the lexer: a bad character is
//! recorded and recovered from, so one typo can't hide every later error.

use crate::span::{LineIndex, Span};

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
        self.render_indexed(src, &LineIndex::new(src), path, severity)
    }

    /// [`render`](Self::render) against a prebuilt [`LineIndex`]. Rendering *n*
    /// diagnostics with the one-shot `render` is O(n · file), which a failing build
    /// feels (a duplicate-variant storm can emit tens of thousands); share an index
    /// across the loop and it is O(n · log lines).
    ///
    /// `index` must have been built from `src`.
    pub fn render_indexed(
        &self,
        src: &str,
        index: &LineIndex,
        path: &str,
        severity: Severity,
    ) -> String {
        let lc = index.line_col(src, self.span.start);
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

/// Escape a string as a JSON string **body** (without the surrounding quotes).
///
/// Hand-written because the compiler has zero runtime dependencies, and correct
/// because the input is not ours: a diagnostic message quotes user identifiers and
/// source text, so it can contain quotes, backslashes, newlines and control bytes.
/// Anything that escapes unescaped produces malformed JSON, which is worse than no
/// JSON at all — a consumer would fail to parse the whole report rather than one
/// message.
///
/// Non-ASCII passes through: JSON is UTF-8 by definition, and Rust `str` is already
/// valid UTF-8, so `\u` encoding it would only make the output larger and less
/// readable. `DEL` (0x7F) is *not* a JSON control character and is left alone.
fn json_escape(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            // The remaining C0 controls have no short form and MUST be escaped —
            // a raw one inside a JSON string is a parse error.
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
}

/// One diagnostic as a JSON object, already resolved to a file and a position.
///
/// Positions are **1-based line/column**, matching what the human renderer prints and
/// what every editor expects. `end_line`/`end_col` describe the span's end, so a
/// consumer can underline exactly what the caret renderer underlines.
#[allow(dead_code)] // the one-shot form; report loops go through `to_json_indexed`
pub fn to_json(
    d: &Diagnostic,
    path: &str,
    src: &str,
    severity: Severity,
    out: &mut String,
) {
    to_json_indexed(d, path, src, &LineIndex::new(src), severity, out)
}

/// [`to_json`] against a prebuilt [`LineIndex`] — same output, but a report over
/// many diagnostics builds the line table once instead of twice per diagnostic.
///
/// `index` must have been built from `src`.
pub fn to_json_indexed(
    d: &Diagnostic,
    path: &str,
    src: &str,
    index: &LineIndex,
    severity: Severity,
    out: &mut String,
) {
    let start = index.line_col(src, d.span.start);
    let end = index.line_col(src, d.span.end);
    out.push_str("{\"severity\":\"");
    out.push_str(severity.label());
    out.push_str("\",\"message\":\"");
    json_escape(&d.message, out);
    out.push_str("\",\"file\":\"");
    // Normalized to forward slashes so a report is identical on every platform —
    // the same normalization `#line` emission does, and for the same reason: this
    // output is compared, hashed and checked into CI.
    json_escape(&path.replace('\\', "/"), out);
    out.push_str(&format!(
        "\",\"line\":{},\"col\":{},\"endLine\":{},\"endCol\":{}",
        start.line, start.col, end.line, end.col
    ));
    match d.code {
        Some(c) => {
            out.push_str(",\"code\":\"");
            json_escape(c, out);
            out.push('"');
        }
        // Explicit `null` rather than an absent key: a consumer can then read the
        // field unconditionally, and the object shape is the same for every entry.
        None => out.push_str(",\"code\":null"),
    }
    match &d.help {
        Some(h) => {
            out.push_str(",\"help\":\"");
            json_escape(h, out);
            out.push('"');
        }
        None => out.push_str(",\"help\":null"),
    }
    out.push('}');
}

/// The schema version of the JSON diagnostic report.
///
/// Emitted in every report so a consumer can refuse a format it does not understand.
/// Bump it for any change that is not purely additive.
pub const JSON_SCHEMA_VERSION: u32 = 1;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_escapes_everything_that_would_break_a_parser() {
        let mut s = String::new();
        json_escape("a\"b\\c\nd\te\rf\u{1}g", &mut s);
        assert_eq!(s, "a\\\"b\\\\c\\nd\\te\\rf\\u0001g");
        // Non-ASCII is valid JSON as-is and stays readable.
        let mut u = String::new();
        json_escape("héllo → 世界", &mut u);
        assert_eq!(u, "héllo → 世界");
        // …and nothing that must be escaped survives unescaped.
        let mut all = String::new();
        json_escape(&(0u32..0x20).filter_map(char::from_u32).collect::<String>(), &mut all);
        assert!(!all.chars().any(|c| (c as u32) < 0x20), "a raw control byte survived: {all:?}");
    }

    #[test]
    fn a_json_diagnostic_carries_position_and_optional_fields() {
        let src = "fn f() {\n    p.z\n}";
        let d = Diagnostic::new("no field `z`", Span::new(13, 16));
        let mut out = String::new();
        to_json(&d, "t.jtr", src, Severity::Error, &mut out);
        assert!(out.contains("\"severity\":\"error\""), "{out}");
        assert!(out.contains("\"line\":2,\"col\":5,\"endLine\":2,\"endCol\":8"), "{out}");
        // Absent optionals are explicit nulls, so the object shape never varies.
        assert!(out.contains("\"code\":null"), "{out}");
        assert!(out.contains("\"help\":null"), "{out}");

        let d = d.with_code("E0001").with_help("did you mean `x`?");
        let mut out = String::new();
        to_json(&d, "t.jtr", src, Severity::Error, &mut out);
        assert!(out.contains("\"code\":\"E0001\""), "{out}");
        assert!(out.contains("\"help\":\"did you mean `x`?\""), "{out}");
    }

    /// A Windows path must not leak backslashes into the report — they would both
    /// need escaping and make the output host-dependent.
    #[test]
    fn json_paths_are_normalized() {
        let mut out = String::new();
        to_json(&Diagnostic::new("x", Span::new(0, 1)), "a\\b\\c.jtr", "x", Severity::Error, &mut out);
        assert!(out.contains("\"file\":\"a/b/c.jtr\""), "{out}");
    }

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
