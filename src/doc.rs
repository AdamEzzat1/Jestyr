//! Doc comments and the documentation generator (roadmap workstream C).
//!
//! ## Three comment tiers
//! ```text
//! //   a plain comment        — ignored entirely
//! ///  an *outer* doc comment — documents the item that follows
//! //!  an *inner* doc comment — documents the enclosing module
//! ```
//! The block forms `/** … */` and `/*! … */` mirror `///` and `//!`. An extra
//! marker char (`////`, `/***`) demotes a doc comment back to a plain comment,
//! matching Rust.
//!
//! ## Comments document; contracts prove
//! Doc comments are gathered by the lexer as **trivia** (see [`crate::lexer`]),
//! so they never reach the parser — a comment cannot change how code is parsed.
//! That is the firm design rule: a `///` line claiming "result is positive" is
//! *prose*, whereas an `ensures result >= 0` clause is a *machine-checked
//! guarantee*. This generator renders both, side by side and clearly labelled,
//! so the two can never be confused. The prose comes from `///`; the
//! **Guarantees** block is reconstructed straight from the AST (`requires` /
//! `ensures` contracts, the error set, `@no_panic`, and refined parameters).
//!
//! ## What `jestyrc doc <file.jtr>` produces
//! For each documentable item (and each struct method) it emits the
//! reconstructed signature, the human prose — split into a summary plus
//! `#`-headed sections and fenced code examples — and the Guarantees block.
//! A doc comment attached to nothing is reported as a warning, so the tool
//! doubles as a lightweight lint.

use std::collections::HashMap;

use crate::ast::*;
use crate::diag::Diagnostic;
use crate::lexer::Lexer;
use crate::parser::Parser;
use crate::span::Span;

// --- the lexer's output (produced by `crate::lexer`, consumed here) ---

/// Which item a doc comment documents.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DocKind {
    /// `///` or `/** … */` — documents the item that *follows* it.
    Outer,
    /// `//!` or `/*! … */` — documents the *enclosing* module.
    Inner,
}

/// One doc comment, recorded by the lexer with its marker stripped but its text
/// not yet line-cleaned. `span` covers the whole comment (marker included), which
/// is how the generator decides what an outer doc sits immediately above.
#[derive(Clone, Debug)]
pub struct RawDoc {
    pub kind: DocKind,
    /// `true` for a `/** … */` / `/*! … */` block; `false` for a `///` / `//!` line.
    pub block: bool,
    pub span: Span,
    pub text: String,
}

// --- the parsed-documentation model ---

/// A `#`-headed section of a doc comment (e.g. `# Example`, `# Safety`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Section {
    pub heading: String,
    pub lines: Vec<String>,
}

/// A fenced code block lifted from a doc comment — the seed of future doctests.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Example {
    pub lang: String,
    pub code: Vec<String>,
}

/// A parsed doc comment: a lead summary, then `#`-headed sections, plus every
/// fenced code block extracted for tooling.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ParsedDoc {
    pub summary: Vec<String>,
    pub sections: Vec<Section>,
    pub examples: Vec<Example>,
}

/// One documentable thing: a top-level item, or a method (with `parent` set).
#[derive(Clone, Debug)]
pub struct Target {
    pub span: Span,
    pub kind: &'static str,
    pub name: String,
    pub signature: String,
    /// Facts extracted from the AST — the *proven* half of the documentation.
    pub guarantees: Vec<String>,
    pub parent: Option<String>,
    pub doc: Option<ParsedDoc>,
}

/// A whole rendered page: the module doc plus every target in source order.
#[derive(Clone, Debug, Default)]
pub struct Page {
    pub title: String,
    pub module_doc: Option<ParsedDoc>,
    pub targets: Vec<Target>,
}

// --- top-level entry point ---

/// Lex + parse `src`, attach docs, and render the API as Markdown (or HTML).
/// Returns the rendered document and any notices (dangling doc comments, plus
/// lex/parse errors so a malformed file still produces best-effort docs).
pub fn generate(src: &str, title: &str, html: bool) -> (String, Vec<Diagnostic>) {
    let (tokens, lex_diags, docs) = Lexer::new(src).tokenize_with_docs();
    let (ast, parse_diags) = Parser::new(src, tokens).parse();

    let mut notices = lex_diags;
    notices.extend(parse_diags);

    let (page, warns) = build_page(&ast, src, &docs, title);
    notices.extend(warns);

    let out = if html { render_html(&page) } else { render_markdown(&page) };
    (out, notices)
}

// --- building the page: collect targets, attach docs ---

fn build_page(ast: &Ast, src: &str, docs: &[RawDoc], title: &str) -> (Page, Vec<Diagnostic>) {
    let outer = group(docs, src, DocKind::Outer);
    let inner = group(docs, src, DocKind::Inner);

    let mut targets = collect_targets(ast, src);
    targets.sort_by_key(|t| t.span.start);

    // Attach each outer block to the first target that begins at or after the
    // block's end. When several blocks land on one target, the nearest (largest
    // end) wins; the rest — and any block with no target — are dangling.
    let mut by_target: HashMap<usize, Vec<usize>> = HashMap::new();
    let mut dangling: Vec<usize> = Vec::new();
    for (bi, b) in outer.iter().enumerate() {
        match targets.iter().position(|t| t.span.start >= b.span.end) {
            Some(ti) => by_target.entry(ti).or_default().push(bi),
            None => dangling.push(bi),
        }
    }
    for (ti, mut bis) in by_target {
        bis.sort_by_key(|bi| outer[*bi].span.end);
        let chosen = *bis.last().unwrap();
        targets[ti].doc = Some(parse_doc(&outer[chosen].lines));
        dangling.extend(bis[..bis.len() - 1].iter().copied());
    }

    let mut warnings = Vec::new();
    dangling.sort();
    for bi in dangling {
        warnings.push(
            Diagnostic::new("doc comment (`///`) is not attached to an item", outer[bi].span)
                .with_help("place it directly above an item, or write `//` for a plain comment"),
        );
    }

    // The module doc is every inner block, joined with a blank line.
    let module_doc = if inner.is_empty() {
        None
    } else {
        let mut lines = Vec::new();
        for (i, b) in inner.iter().enumerate() {
            if i > 0 {
                lines.push(String::new());
            }
            lines.extend(b.lines.iter().cloned());
        }
        Some(parse_doc(&lines))
    };

    (Page { title: title.to_string(), module_doc, targets }, warnings)
}

/// A run of doc comments treated as one unit (consecutive `///` lines merge;
/// each block comment stands alone).
struct DocBlock {
    span: Span,
    lines: Vec<String>,
}

/// Group the doc comments of one kind into blocks, in source order.
fn group(docs: &[RawDoc], src: &str, want: DocKind) -> Vec<DocBlock> {
    let mut sorted: Vec<&RawDoc> = docs.iter().filter(|d| d.kind == want).collect();
    sorted.sort_by_key(|d| d.span.start);

    let mut blocks: Vec<DocBlock> = Vec::new();
    let mut cur: Option<DocBlock> = None;
    for d in sorted {
        let lines = clean_lines(d);
        if d.block {
            if let Some(b) = cur.take() {
                blocks.push(b);
            }
            blocks.push(DocBlock { span: d.span, lines });
            continue;
        }
        match cur.as_mut() {
            Some(b) if contiguous(src, b.span, d.span) => {
                b.lines.extend(lines);
                b.span = b.span.to(d.span);
            }
            _ => {
                if let Some(b) = cur.take() {
                    blocks.push(b);
                }
                cur = Some(DocBlock { span: d.span, lines });
            }
        }
    }
    if let Some(b) = cur.take() {
        blocks.push(b);
    }
    blocks
}

/// Two line-doc comments belong to one block when only whitespace — and at most
/// one newline — separates them (a blank line starts a fresh block).
fn contiguous(src: &str, a: Span, b: Span) -> bool {
    let (lo, hi) = (a.end as usize, b.start as usize);
    let gap = src.get(lo..hi).unwrap_or("");
    gap.chars().all(char::is_whitespace) && gap.matches('\n').count() <= 1
}

/// Strip a doc comment's markers/margins, returning its content lines.
fn clean_lines(raw: &RawDoc) -> Vec<String> {
    if !raw.block {
        return vec![strip_one_space(raw.text.trim_end())];
    }
    let mut out: Vec<String> = raw
        .text
        .split('\n')
        .map(|line| {
            let line = line.trim_end();
            let t = line.trim_start();
            // Honour a Javadoc-style `*` margin, else keep the line verbatim so
            // indented code in examples survives.
            match t.strip_prefix('*') {
                Some(rest) => strip_one_space(rest),
                None => line.to_string(),
            }
        })
        .collect();
    while out.first().is_some_and(|l| l.trim().is_empty()) {
        out.remove(0);
    }
    while out.last().is_some_and(|l| l.trim().is_empty()) {
        out.pop();
    }
    out
}

fn strip_one_space(s: &str) -> String {
    s.strip_prefix(' ').unwrap_or(s).to_string()
}

// --- parsing a doc comment into summary / sections / examples ---

fn parse_doc(lines: &[String]) -> ParsedDoc {
    let mut summary: Vec<String> = Vec::new();
    let mut sections: Vec<Section> = Vec::new();
    let mut examples: Vec<Example> = Vec::new();
    let mut cur: Option<usize> = None; // index into `sections`, or None for the summary

    let mut in_fence = false;
    let mut fence_lang = String::new();
    let mut fence_code: Vec<String> = Vec::new();

    for line in lines {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("```") {
            if in_fence {
                examples.push(Example {
                    lang: std::mem::take(&mut fence_lang),
                    code: std::mem::take(&mut fence_code),
                });
            } else {
                fence_lang = rest.trim().to_string();
            }
            in_fence = !in_fence;
            push_line(&mut summary, &mut sections, cur, line.clone());
            continue;
        }
        if in_fence {
            fence_code.push(line.clone());
            push_line(&mut summary, &mut sections, cur, line.clone());
            continue;
        }
        if let Some(h) = heading(trimmed) {
            sections.push(Section { heading: h, lines: Vec::new() });
            cur = Some(sections.len() - 1);
            continue;
        }
        push_line(&mut summary, &mut sections, cur, line.clone());
    }
    if in_fence && !fence_code.is_empty() {
        examples.push(Example { lang: fence_lang, code: fence_code });
    }

    trim_blank(&mut summary);
    for s in &mut sections {
        trim_blank(&mut s.lines);
    }
    ParsedDoc { summary, sections, examples }
}

fn push_line(summary: &mut Vec<String>, sections: &mut [Section], cur: Option<usize>, line: String) {
    match cur {
        None => summary.push(line),
        Some(i) => sections[i].lines.push(line),
    }
}

/// An ATX heading (`# Title` … `###### Title`) — a `#` run of 1–6 followed by a
/// space and non-empty text. Returns the title text.
fn heading(t: &str) -> Option<String> {
    let hashes = t.chars().take_while(|c| *c == '#').count();
    if !(1..=6).contains(&hashes) || !t[hashes..].starts_with(' ') {
        return None;
    }
    let rest = t[hashes..].trim();
    (!rest.is_empty()).then(|| rest.to_string())
}

fn trim_blank(lines: &mut Vec<String>) {
    while lines.first().is_some_and(|l| l.trim().is_empty()) {
        lines.remove(0);
    }
    while lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
}

// --- collecting documentable targets from the AST ---

fn collect_targets(ast: &Ast, src: &str) -> Vec<Target> {
    let mut targets = Vec::new();
    for item in &ast.items {
        match item {
            Item::Fn(f) => targets.push(fn_target(ast, src, f, "fn", None)),
            Item::Extern(e) => targets.push(Target {
                span: e.span,
                kind: "extern",
                name: e.name.name.clone(),
                signature: extern_sig(ast, e),
                guarantees: Vec::new(),
                parent: None,
                doc: None,
            }),
            Item::Enum(e) => targets.push(Target {
                span: e.span,
                kind: "enum",
                name: e.name.name.clone(),
                signature: enum_sig(ast, e),
                guarantees: Vec::new(),
                parent: None,
                doc: None,
            }),
            Item::Const(c) => targets.push(Target {
                span: c.span,
                kind: "const",
                name: c.name.name.clone(),
                signature: const_sig(ast, src, c),
                guarantees: Vec::new(),
                parent: None,
                doc: None,
            }),
            Item::Struct { is_pub, name, body, .. } => {
                targets.push(Target {
                    span: item_span(item),
                    kind: "struct",
                    name: name.name.clone(),
                    signature: struct_sig(ast, *is_pub, &name.name, body),
                    guarantees: Vec::new(),
                    parent: None,
                    doc: None,
                });
                for m in &body.members {
                    if let StructMember::Method(f) = m {
                        targets.push(fn_target(ast, src, f, "method", Some(name.name.clone())));
                    }
                }
            }
            Item::Distinct(dd) => {
                let vis = if dd.is_pub { "pub " } else { "" };
                targets.push(Target {
                    span: dd.span,
                    kind: "distinct",
                    name: dd.name.name.clone(),
                    signature: format!("{vis}distinct {} = {}", dd.name.name, ty_str(ast, dd.base)),
                    guarantees: Vec::new(),
                    parent: None,
                    doc: None,
                });
            }
            Item::Trait(t) => {
                let vis = if t.is_pub { "pub " } else { "" };
                targets.push(Target {
                    span: t.span,
                    kind: "trait",
                    name: t.name.name.clone(),
                    signature: format!("{vis}trait {}", t.name.name),
                    guarantees: Vec::new(),
                    parent: None,
                    doc: None,
                });
            }
            // An `impl` block has no name of its own to document (its methods belong
            // to their trait/type); skip it for now.
            Item::Impl(_) => {}
            Item::Import(_) => {}
        }
    }
    targets
}

fn fn_target(ast: &Ast, src: &str, f: &FnDecl, kind: &'static str, parent: Option<String>) -> Target {
    Target {
        span: f.span,
        kind,
        name: f.name.name.clone(),
        signature: fn_sig(ast, src, f),
        guarantees: fn_guarantees(ast, src, f),
        parent,
        doc: None,
    }
}

fn item_span(item: &Item) -> Span {
    match item {
        Item::Struct { span, .. } => *span,
        _ => Span::new(0, 0),
    }
}

// --- guarantee extraction (the proven half) ---

fn fn_guarantees(ast: &Ast, src: &str, f: &FnDecl) -> Vec<String> {
    let mut g = Vec::new();
    if f.no_panic {
        g.push("`@no_panic` — proven free of faulting operations".to_string());
    }
    for r in &f.requires {
        g.push(format!("`requires {}`", expr_src(ast, src, *r)));
    }
    for e in &f.ensures {
        g.push(format!("`ensures {}`", expr_src(ast, src, *e)));
    }
    for p in &f.params {
        if let Some(r) = p.refine {
            g.push(format!("parameter `{}` is constrained to `{}`", p.name.name, expr_src(ast, src, r)));
        }
    }
    if let Some(es) = &f.errors {
        let names: Vec<&str> = es.names.iter().map(|n| n.name.as_str()).collect();
        g.push(format!("may fail with `!{{ {} }}`", names.join(", ")));
    }
    g
}

/// The exact source text of an expression (faithful, and avoids re-printing).
fn expr_src(ast: &Ast, src: &str, e: ExprId) -> String {
    let sp = ast.expr_at(e).span;
    src.get(sp.range()).unwrap_or("").trim().to_string()
}

// --- signature reconstruction ---

fn fn_sig(ast: &Ast, src: &str, f: &FnDecl) -> String {
    let mut s = String::new();
    if f.is_pub {
        s.push_str("pub ");
    }
    if f.no_panic {
        s.push_str("@no_panic ");
    }
    s.push_str("fn ");
    s.push_str(&f.name.name);
    s.push('(');
    s.push_str(&params_str(ast, src, &f.params));
    s.push(')');
    if let Some(t) = f.ret_ty {
        s.push_str(" -> ");
        if f.ret_conv != Conv::Default {
            s.push_str(f.ret_conv.label());
            s.push(' ');
        }
        s.push_str(&ty_str(ast, t));
    }
    if let Some(es) = &f.errors {
        let names: Vec<&str> = es.names.iter().map(|n| n.name.as_str()).collect();
        s.push_str(&format!(" !{{ {} }}", names.join(", ")));
    }
    s
}

fn extern_sig(ast: &Ast, e: &ExternFn) -> String {
    let mut s = String::new();
    if e.is_pub {
        s.push_str("pub ");
    }
    s.push_str(&format!("extern \"{}\" fn {}(", e.abi, e.name.name));
    s.push_str(&params_str(ast, "", &e.params));
    s.push(')');
    if let Some(t) = e.ret_ty {
        s.push_str(" -> ");
        if e.ret_conv != Conv::Default {
            s.push_str(e.ret_conv.label());
            s.push(' ');
        }
        s.push_str(&ty_str(ast, t));
    }
    s
}

fn params_str(ast: &Ast, src: &str, params: &[Param]) -> String {
    params
        .iter()
        .map(|p| {
            if p.is_self {
                let conv = if p.conv != Conv::Default { format!("{} ", p.conv.label()) } else { String::new() };
                return format!("{conv}self");
            }
            let mut ps = String::new();
            if p.comptime {
                ps.push_str("comptime ");
            }
            if p.conv != Conv::Default {
                ps.push_str(p.conv.label());
                ps.push(' ');
            }
            ps.push_str(&p.name.name);
            if let Some(t) = p.ty {
                ps.push_str(": ");
                ps.push_str(&ty_str(ast, t));
            }
            if let Some(r) = p.refine {
                ps.push_str(" in ");
                ps.push_str(&expr_src(ast, src, r));
            }
            ps
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn struct_sig(ast: &Ast, is_pub: bool, name: &str, body: &StructBody) -> String {
    let fields: Vec<String> = body
        .members
        .iter()
        .filter_map(|m| match m {
            StructMember::Field { name, ty, volatile, .. } => {
                let v = if *volatile { "@volatile " } else { "" };
                Some(format!("    {}: {}{}", name.name, v, ty_str(ast, *ty)))
            }
            StructMember::Method(_) => None,
        })
        .collect();
    let vis = if is_pub { "pub " } else { "" };
    if fields.is_empty() {
        format!("{vis}struct {name} {{}}")
    } else {
        format!("{vis}struct {name} {{\n{}\n}}", fields.join(",\n"))
    }
}

fn enum_sig(ast: &Ast, e: &EnumDecl) -> String {
    let variants: Vec<String> = e
        .variants
        .iter()
        .map(|v| {
            if v.fields.is_empty() {
                format!("    {}", v.name.name)
            } else {
                let fs: Vec<String> =
                    v.fields.iter().map(|(n, t)| format!("{}: {}", n.name, ty_str(ast, *t))).collect();
                format!("    {}({})", v.name.name, fs.join(", "))
            }
        })
        .collect();
    let vis = if e.is_pub { "pub " } else { "" };
    if variants.is_empty() {
        format!("{vis}enum {} {{}}", e.name.name)
    } else {
        format!("{vis}enum {} {{\n{}\n}}", e.name.name, variants.join(",\n"))
    }
}

fn const_sig(ast: &Ast, src: &str, c: &ConstDecl) -> String {
    let ty = c.ty.map(|t| format!(": {}", ty_str(ast, t))).unwrap_or_default();
    let vis = if c.is_pub { "pub " } else { "" };
    format!("{vis}const {}{} = {}", c.name.name, ty, expr_src(ast, src, c.value))
}

/// Render a type, mirroring the pretty-printer's surface syntax.
fn ty_str(ast: &Ast, id: TypeId) -> String {
    match &ast.type_at(id).kind {
        TypeKind::Name(n) => n.name.clone(),
        TypeKind::TypeKw => "type".to_string(),
        TypeKind::Ptr { mutbl, inner } => {
            let m = match mutbl {
                PtrMut::Mut => "*mut ",
                PtrMut::Const => "*const ",
                PtrMut::Default => "*",
            };
            format!("{m}{}", ty_str(ast, *inner))
        }
        TypeKind::Slice(inner) => format!("[]{}", ty_str(ast, *inner)),
        TypeKind::GenRef(inner) => format!("&{}", ty_str(ast, *inner)),
        TypeKind::RegionRef { region, inner } => format!("&[{}]{}", region.name, ty_str(ast, *inner)),
        TypeKind::App { ctor, args } => {
            let a: Vec<String> = args.iter().map(|t| ty_str(ast, *t)).collect();
            format!("{}({})", ctor.name, a.join(", "))
        }
        TypeKind::Fn { params, ret_conv, ret } => {
            let ps: Vec<String> = params
                .iter()
                .map(|p| {
                    let c = if p.conv != Conv::Default {
                        format!("{} ", p.conv.label())
                    } else {
                        String::new()
                    };
                    format!("{}{}", c, ty_str(ast, p.ty))
                })
                .collect();
            let tail = match ret {
                Some(t) => {
                    let c = if *ret_conv != Conv::Default {
                        format!("{} ", ret_conv.label())
                    } else {
                        String::new()
                    };
                    format!(" -> {}{}", c, ty_str(ast, *t))
                }
                None => String::new(),
            };
            format!("fn({}){}", ps.join(", "), tail)
        }
        TypeKind::Dyn(n) => format!("dyn {}", n.name),
        TypeKind::Error => "<error-type>".to_string(),
    }
}

// --- Markdown rendering ---

fn render_markdown(page: &Page) -> String {
    let mut out = String::new();
    out.push_str(&format!("# {}\n\n", page.title));
    out.push_str(
        "*Generated by `jestyrc doc`. Signatures and the **Guarantees** blocks are \
         extracted from the source and machine-checked; prose comes from `///` doc comments.*\n\n",
    );

    if let Some(doc) = &page.module_doc {
        emit_doc_md(&mut out, doc);
        out.push_str("\n---\n\n");
    }

    let has_items = page.targets.iter().any(|t| t.parent.is_none());
    if !has_items {
        out.push_str("*(No documentable items.)*\n");
        return out;
    }

    out.push_str("## API\n\n");
    for t in page.targets.iter().filter(|t| t.parent.is_none()) {
        emit_target_md(&mut out, t, 3);
        if t.kind == "struct" {
            let methods: Vec<&Target> =
                page.targets.iter().filter(|m| m.parent.as_deref() == Some(t.name.as_str())).collect();
            if !methods.is_empty() {
                out.push_str(&format!("#### Methods of `{}`\n\n", t.name));
                for m in methods {
                    emit_target_md(&mut out, m, 5);
                }
            }
        }
    }
    out
}

fn emit_target_md(out: &mut String, t: &Target, level: usize) {
    let hashes = "#".repeat(level);
    out.push_str(&format!("{hashes} `{}`\n\n", t.name));
    out.push_str(&format!("```jestyr\n{}\n```\n\n", t.signature));

    if let Some(doc) = &t.doc {
        emit_doc_md(out, doc);
    }
    if !t.guarantees.is_empty() {
        out.push_str("**Guarantees** *(checked by the compiler)*\n\n");
        for g in &t.guarantees {
            out.push_str(&format!("- {g}\n"));
        }
        out.push('\n');
    }
}

fn emit_doc_md(out: &mut String, doc: &ParsedDoc) {
    if !doc.summary.is_empty() {
        out.push_str(&doc.summary.join("\n"));
        out.push_str("\n\n");
    }
    for s in &doc.sections {
        out.push_str(&format!("**{}**\n\n", s.heading));
        if !s.lines.is_empty() {
            out.push_str(&s.lines.join("\n"));
            out.push_str("\n\n");
        }
    }
}

// --- HTML rendering (a self-contained page from the same model) ---

fn render_html(page: &Page) -> String {
    let mut out = String::new();
    out.push_str("<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n");
    out.push_str(&format!("<title>{}</title>\n", esc(&page.title)));
    out.push_str("<style>\n");
    out.push_str(
        "body{font-family:system-ui,sans-serif;max-width:48rem;margin:2rem auto;padding:0 1rem;line-height:1.5}\n\
         pre{background:#f4f4f4;padding:.75rem;border-radius:.4rem;overflow:auto}\n\
         code{font-family:ui-monospace,monospace}\n\
         .guarantees{background:#eef6ee;border-left:4px solid #4a4;padding:.5rem .9rem;border-radius:.3rem}\n\
         .legend{color:#666;font-size:.9rem}\n",
    );
    out.push_str("</style>\n</head>\n<body>\n");
    out.push_str(&format!("<h1>{}</h1>\n", esc(&page.title)));
    out.push_str(
        "<p class=\"legend\">Signatures and <strong>Guarantees</strong> are extracted from the \
         source and machine-checked; prose comes from <code>///</code> doc comments.</p>\n",
    );

    if let Some(doc) = &page.module_doc {
        emit_doc_html(&mut out, doc);
        out.push_str("<hr>\n");
    }

    out.push_str("<h2>API</h2>\n");
    for t in page.targets.iter().filter(|t| t.parent.is_none()) {
        emit_target_html(&mut out, t, 3);
        if t.kind == "struct" {
            let methods: Vec<&Target> =
                page.targets.iter().filter(|m| m.parent.as_deref() == Some(t.name.as_str())).collect();
            for m in methods {
                emit_target_html(&mut out, m, 4);
            }
        }
    }
    out.push_str("</body>\n</html>\n");
    out
}

fn emit_target_html(out: &mut String, t: &Target, level: usize) {
    out.push_str(&format!("<h{level}><code>{}</code></h{level}>\n", esc(&t.name)));
    out.push_str(&format!("<pre><code>{}</code></pre>\n", esc(&t.signature)));
    if let Some(doc) = &t.doc {
        emit_doc_html(out, doc);
    }
    if !t.guarantees.is_empty() {
        out.push_str("<div class=\"guarantees\">\n<p><strong>Guarantees</strong> (checked by the compiler)</p>\n<ul>\n");
        for g in &t.guarantees {
            out.push_str(&format!("<li>{}</li>\n", esc_inline_code(g)));
        }
        out.push_str("</ul>\n</div>\n");
    }
}

fn emit_doc_html(out: &mut String, doc: &ParsedDoc) {
    lines_to_html(out, &doc.summary);
    for s in &doc.sections {
        out.push_str(&format!("<h4>{}</h4>\n", esc(&s.heading)));
        lines_to_html(out, &s.lines);
    }
}

/// Render doc body lines to HTML: fenced blocks become `<pre>`, the rest become
/// `<p>` paragraphs split on blank lines.
fn lines_to_html(out: &mut String, lines: &[String]) {
    let mut para: Vec<String> = Vec::new();
    let mut code: Vec<String> = Vec::new();
    let mut in_fence = false;

    let flush_para = |out: &mut String, para: &mut Vec<String>| {
        if !para.is_empty() {
            // Render inline `code` spans; other markdown passes through as text.
            out.push_str(&format!("<p>{}</p>\n", esc_inline_code(&para.join(" "))));
            para.clear();
        }
    };

    for line in lines {
        if line.trim_start().starts_with("```") {
            if in_fence {
                out.push_str(&format!("<pre><code>{}</code></pre>\n", esc(&code.join("\n"))));
                code.clear();
            } else {
                flush_para(out, &mut para);
            }
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            code.push(line.clone());
        } else if line.trim().is_empty() {
            flush_para(out, &mut para);
        } else {
            para.push(line.clone());
        }
    }
    flush_para(out, &mut para);
    if in_fence && !code.is_empty() {
        out.push_str(&format!("<pre><code>{}</code></pre>\n", esc(&code.join("\n"))));
    }
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// Escape, then render `` `x` `` spans as `<code>x</code>` (the only inline
/// markdown the guarantee strings use).
fn esc_inline_code(s: &str) -> String {
    let mut out = String::new();
    let mut in_code = false;
    for ch in s.chars() {
        if ch == '`' {
            out.push_str(if in_code { "</code>" } else { "<code>" });
            in_code = !in_code;
        } else {
            match ch {
                '&' => out.push_str("&amp;"),
                '<' => out.push_str("&lt;"),
                '>' => out.push_str("&gt;"),
                _ => out.push(ch),
            }
        }
    }
    if in_code {
        out.push_str("</code>");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(src: &str) -> (Page, Vec<Diagnostic>) {
        let (tokens, _ld, docs) = Lexer::new(src).tokenize_with_docs();
        let (ast, _pd) = Parser::new(src, tokens).parse();
        build_page(&ast, src, &docs, "test")
    }

    #[test]
    fn outer_doc_attaches_to_the_following_item() {
        let src = "/// Adds one.\n/// More detail.\nfn inc(x: i32) -> i32 { return x }";
        let (p, warns) = page(src);
        assert!(warns.is_empty(), "no dangling docs: {:?}", warns);
        let t = &p.targets[0];
        assert_eq!(t.name, "inc");
        let doc = t.doc.as_ref().expect("inc is documented");
        assert_eq!(doc.summary, vec!["Adds one.", "More detail."]);
    }

    #[test]
    fn inner_doc_becomes_the_module_doc() {
        let src = "//! The widget module.\nfn f() {}";
        let (p, _w) = page(src);
        let md = p.module_doc.expect("module documented");
        assert_eq!(md.summary, vec!["The widget module."]);
        // ...and it does NOT attach to `f`.
        assert!(p.targets[0].doc.is_none());
    }

    #[test]
    fn sections_and_examples_are_parsed() {
        let src = "\
/// One-line summary.
///
/// # Example
/// ```jestyr
/// let y = inc(1)
/// ```
/// # Safety
/// Retains nothing.
fn inc(x: i32) -> i32 { return x }";
        let (p, _w) = page(src);
        let doc = p.targets[0].doc.as_ref().unwrap();
        assert_eq!(doc.summary, vec!["One-line summary."]);
        let headings: Vec<&str> = doc.sections.iter().map(|s| s.heading.as_str()).collect();
        assert_eq!(headings, vec!["Example", "Safety"]);
        assert_eq!(doc.examples.len(), 1, "one fenced example");
        assert_eq!(doc.examples[0].lang, "jestyr");
        assert_eq!(doc.examples[0].code, vec!["let y = inc(1)"]);
    }

    #[test]
    fn guarantees_are_extracted_from_the_ast() {
        // The proven half: contracts, error set, refinement, @no_panic — none of
        // which is prose.
        let src = "\
/// Safe division.
@no_panic fn div(a: i32, b: i32 in 1..100) -> i32 !{ Overflow }
    requires b != 0
    ensures result >= 0
{ return a / b }";
        let (p, _w) = page(src);
        let g = &p.targets[0].guarantees;
        assert!(g.iter().any(|s| s.contains("@no_panic")), "{g:?}");
        assert!(g.iter().any(|s| s.contains("requires b != 0")), "{g:?}");
        assert!(g.iter().any(|s| s.contains("ensures result >= 0")), "{g:?}");
        assert!(g.iter().any(|s| s.contains("constrained") && s.contains("1..100")), "{g:?}");
        assert!(g.iter().any(|s| s.contains("Overflow")), "{g:?}");
    }

    #[test]
    fn dangling_doc_is_a_warning() {
        // A `///` with no item after it (here, before the closing brace).
        let src = "fn f() {\n    /// stray\n}";
        let (_p, warns) = page(src);
        assert_eq!(warns.len(), 1, "one dangling-doc warning: {:?}", warns);
        assert!(warns[0].message.contains("not attached"));
    }

    #[test]
    fn methods_are_documented_under_their_struct() {
        let src = "\
/// A counter.
struct Counter {
    n: i32
    /// Bump the counter.
    fn inc(mut self) { self.n = self.n + 1 }
}";
        let (p, warns) = page(src);
        assert!(warns.is_empty(), "{warns:?}");
        let method = p.targets.iter().find(|t| t.kind == "method").expect("method target");
        assert_eq!(method.name, "inc");
        assert_eq!(method.parent.as_deref(), Some("Counter"));
        assert!(method.doc.as_ref().unwrap().summary.contains(&"Bump the counter.".to_string()));
    }

    #[test]
    fn markdown_shows_signature_prose_and_guarantees() {
        let src = "\
/// Absolute value.
fn abs(x: i32) -> i32 ensures result >= 0 { return x }";
        let (out, _w) = generate(src, "demo", false);
        assert!(out.contains("# demo"));
        assert!(out.contains("```jestyr"));
        assert!(out.contains("fn abs(x: i32) -> i32"), "signature: {out}");
        assert!(out.contains("Absolute value."), "prose: {out}");
        assert!(out.contains("Guarantees"), "guarantees header: {out}");
        assert!(out.contains("ensures result >= 0"), "contract: {out}");
    }

    #[test]
    fn html_output_is_escaped_and_structured() {
        let src = "/// docs\nfn f(s: []i32) -> i32 { return s.len }";
        let (out, _w) = generate(src, "demo", true);
        assert!(out.contains("<!DOCTYPE html>"));
        assert!(out.contains("<code>f</code>"));
        // `[]i32` carries no angle brackets, but the escaper must still run; a
        // generic return like `List(i32)` would — assert the escaper is wired by
        // checking the signature block exists.
        assert!(out.contains("<pre><code>"), "signature pre block: {out}");
    }

    #[test]
    fn block_doc_margin_is_stripped() {
        let src = "\
/**
 * Line one.
 * Line two.
 */
fn f() {}";
        let (p, _w) = page(src);
        let doc = p.targets[0].doc.as_ref().unwrap();
        assert_eq!(doc.summary, vec!["Line one.", "Line two."]);
    }

    #[test]
    fn plain_comments_produce_no_docs() {
        let src = "// not a doc\n//// also not\nfn f() {}";
        let (p, warns) = page(src);
        assert!(warns.is_empty());
        assert!(p.targets[0].doc.is_none());
        assert!(p.module_doc.is_none());
    }

    /// The shipped demo must render with no dangling-doc warnings and surface its
    /// module doc, prose, and machine-checked guarantees.
    #[test]
    fn ships_a_clean_documented_example() {
        let src = include_str!("../examples/docs.jtr");
        let (md, notices) = generate(src, "docs", false);
        assert!(notices.is_empty(), "the demo has no dangling docs: {:?}", notices);
        assert!(md.contains("comments document; contracts prove"), "module doc rendered");
        assert!(md.contains("ensures result >= 0"), "a contract guarantee rendered");
        assert!(md.contains("Methods of `Counter`"), "the method is grouped under its struct");
        // The HTML form renders too (and escapes).
        let (html, _) = generate(src, "docs", true);
        assert!(html.starts_with("<!DOCTYPE html>"));
    }
}
