//! `jestyrc attest` — a sound reproducible-build + machine-checked-guarantee
//! manifest (roadmap workstream O, the headline tool).
//!
//! ## What it emits
//! A deterministic, line-oriented *attestation manifest* for a program:
//!  1. the **SHA-256 of the emitted C** — a real attestation, not an aspiration,
//!     because codegen is a *proven* deterministic function of the source
//!     (`proptests::compilation_is_deterministic` + the locked `CC_FLAGS`/FP seam +
//!     the cross-OS numerics canary). "Same source → byte-identical C" is the
//!     invariant the hash commits to;
//!  2. the exact, **locked compile command** (`crate::CC_FLAGS`), so the manifest
//!     records *how* that C must be built (the conditional `-pthread` aside);
//!  3. every top-level item's **machine-checked Guarantees** — `requires`/`ensures`,
//!     error set `!{…}`, `@no_panic`, and refined-parameter ranges — reconstructed
//!     from the AST by the *same* `doc::fn_guarantees` the doc generator uses, so the
//!     attested behavioral ABI can never drift from the rendered docs.
//!
//! ## Why only Jestyr can emit this soundly
//! The guarantees are reconstructed from the AST, not parsed from prose: a contract
//! like `ensures result >= 0` or `@no_panic` *is* the function's public behavioral
//! ABI, and the compiler has already proven it. `cargo-semver-checks` reverse-
//! engineers compatibility from signatures plus a hand-maintained lint list and
//! cannot see those clauses; here they are first-class. This manifest is the
//! determinism + "contracts prove" thesis cashed out as a shippable artifact — the
//! input a future package registry, CI gate, and the `attest --diff` breaking-change
//! detector all consume.
//!
//! ## Format (`jestyr-attest/v1`)
//! ```text
//! jestyr-attest/v1
//! source <id>
//! c-sha256 <64 hex>
//! cc-flags -O2 -std=c11 -ffp-contract=off -fno-fast-math
//!
//! <kind> <name>
//!   vis: pub | priv
//!   sig: <faithful one-line signature>
//!   guarantee: <phrase>        (zero or more; fns only)
//! …
//! ```
//! Items are sorted by `(kind, name)`, guarantees are in `doc::fn_guarantees`'s fixed
//! order, and every line is `\n`-terminated — so the whole manifest is byte-
//! reproducible (fittingly). The `<kind> <name>` key line is the stable item identity
//! the `--diff` follow-up keys on.

use std::collections::{BTreeMap, BTreeSet};

use crate::ast::*;
use crate::cgen;
use crate::doc;
use crate::types::TypeInfo;

/// The manifest format tag — bumped if the on-disk shape changes (so `--diff` can
/// refuse to compare across incompatible versions).
pub const MANIFEST_VERSION: &str = "jestyr-attest/v1";

/// One attested top-level item: its kind, name (the stable identity), visibility,
/// faithful one-line signature, and machine-checked guarantees (fns only).
struct Record {
    kind: &'static str,
    name: String,
    is_pub: bool,
    sig: String,
    guarantees: Vec<String>,
    /// `@deprecated`: `None` absent, `Some("")` bare, `Some(msg)` with a message.
    /// Its own field rather than a guarantee or a piece of the signature — see
    /// [`doc::fn_deprecated`] for why both of those are the wrong home.
    deprecated: Option<String>,
}

/// Build the attestation manifest for a checked program. `src` must be the buffer
/// the AST's spans index into (a single file's text, or — for a multi-module
/// program — the loader's concatenated global buffer); `info` is its type table.
/// Codegen runs here, so the hash is over exactly the C `jestyrc build` would emit.
pub fn manifest(source_id: &str, src: &str, ast: &Ast, info: &TypeInfo) -> String {
    let (c_src, _diags) = cgen::emit(ast, info);
    let hash = crate::sha256::hex(c_src.as_bytes());
    let cc_flags = crate::CC_FLAGS.join(" ");

    let mut records = collect_records(ast, src);
    // Deterministic, grouped order: by kind then name. Names are unique per kind in
    // a well-formed program, so this is a total order — no iteration-order leak.
    records.sort_by(|a, b| (a.kind, &a.name).cmp(&(b.kind, &b.name)));

    let mut out = String::new();
    out.push_str(MANIFEST_VERSION);
    out.push('\n');
    out.push_str(&format!("source {source_id}\n"));
    out.push_str(&format!("c-sha256 {hash}\n"));
    out.push_str(&format!("cc-flags {cc_flags}\n"));
    for r in &records {
        out.push('\n');
        out.push_str(&format!("{} {}\n", r.kind, r.name));
        out.push_str(&format!("  vis: {}\n", if r.is_pub { "pub" } else { "priv" }));
        out.push_str(&format!("  sig: {}\n", r.sig));
        // Ahead of the guarantees, so a reader meets "do not call this" before the
        // proven facts about what calling it would do. A bare `@deprecated` renders
        // the bare key; `@deprecated("")` collapses to the same line, which loses
        // nothing a caller could act on.
        if let Some(msg) = &r.deprecated {
            if msg.is_empty() {
                out.push_str("  deprecated:\n");
            } else {
                out.push_str(&format!("  deprecated: {msg}\n"));
            }
        }
        for g in &r.guarantees {
            out.push_str(&format!("  guarantee: {g}\n"));
        }
    }
    out
}

/// One trait method, rendered the way `doc::fn_sig` renders a free function — same
/// parameter spelling, same `-> conv Ty`, same `!{ … }` error set — so a method moving
/// between a trait and a free function does not read as a signature change when the
/// contract is identical.
///
/// A `TraitMethod` is not a `FnDecl` (no visibility, no `@no_panic`, an optional default
/// body), so this cannot call `doc::fn_sig` directly; it deliberately mirrors it.
fn trait_method_sig(ast: &Ast, src: &str, m: &crate::ast::TraitMethod) -> String {
    let mut s = String::new();
    s.push_str("fn ");
    s.push_str(&m.name.name);
    s.push('(');
    s.push_str(&doc::params_str(ast, src, &m.params));
    s.push(')');
    if let Some(t) = m.ret_ty {
        s.push_str(" -> ");
        if m.ret_conv != Conv::Default {
            s.push_str(m.ret_conv.label());
            s.push(' ');
        }
        s.push_str(&doc::ty_str(ast, t));
    }
    if let Some(es) = &m.errors {
        let names: Vec<String> = es.names.iter().map(|n| doc::err_name_str(ast, n)).collect();
        s.push_str(&format!(" !{{ {} }}", names.join(", ")));
    }
    // A default body is part of the contract: removing one turns an optional method
    // into a required one, which breaks every implementor that relied on it.
    if m.default_body.is_some() {
        s.push_str(" { … }");
    }
    s
}

/// `trait Name { fn a(…); fn b(…) }` — the method SET inside the signature, so removing
/// or adding one shows up as a signature change rather than as nothing at all.
///
/// Methods are rendered in DECLARATION order, not sorted. Reordering a trait's methods is
/// an ABI change in a vtable-dispatched language, and this is the only record that can see
/// it; sorting here would hide it.
fn trait_sig(ast: &Ast, src: &str, t: &crate::ast::TraitDecl) -> String {
    let mut s = String::new();
    if t.is_pub {
        s.push_str("pub ");
    }
    s.push_str("trait ");
    s.push_str(&t.name.name);
    s.push_str(" { ");
    let ms: Vec<String> = t.methods.iter().map(|m| trait_method_sig(ast, src, m)).collect();
    s.push_str(&ms.join("; "));
    s.push_str(" }");
    s
}

/// `impl[T] Trait for Ty { fn a(…); fn b(…) }`. The generic parameters matter: a blanket
/// `impl[T] Drop for Vec(T)` and a concrete `impl Drop for Vec(i32)` cover different sets
/// of instantiations, so a change from one to the other is a real ABI change.
fn impl_sig(ast: &Ast, src: &str, i: &crate::ast::ImplDecl) -> String {
    let mut s = String::new();
    s.push_str("impl");
    if !i.generics.is_empty() {
        let gs: Vec<String> = i.generics.iter().map(|g| g.name.name.clone()).collect();
        s.push_str(&format!("[{}]", gs.join(", ")));
    }
    s.push(' ');
    s.push_str(&i.trait_name.name);
    s.push_str(" for ");
    s.push_str(&doc::ty_str(ast, i.ty));
    s.push_str(" { ");
    let ms: Vec<String> = i.methods.iter().map(|m| doc::fn_sig(ast, src, m)).collect();
    s.push_str(&ms.join("; "));
    s.push_str(" }");
    s
}

/// Walk the top-level items into attestation records. Covers the value/ABI surface
/// — `fn`, `const`, `struct`/`record`/`union`, `enum`, `extern` — tagging each with
/// its visibility. (Traits/impls/distinct/import are not emitted as standalone C
/// items; their effect is captured by the C hash and, for methods, future work.)
fn collect_records(ast: &Ast, src: &str) -> Vec<Record> {
    let mut records = Vec::new();
    for item in &ast.items {
        match item {
            Item::Fn(f) => records.push(Record {
                kind: "fn",
                name: f.name.name.clone(),
                is_pub: f.is_pub,
                sig: doc::fn_sig(ast, src, f),
                guarantees: doc::fn_guarantees(ast, src, f),
                deprecated: doc::fn_deprecated(ast, f),
            }),
            Item::Const(c) => records.push(Record {
                kind: "const",
                name: c.name.name.clone(),
                is_pub: c.is_pub,
                sig: doc::const_sig(ast, src, c),
                guarantees: Vec::new(),
                deprecated: None,
            }),
            Item::Enum(e) => records.push(Record {
                kind: "enum",
                name: e.name.name.clone(),
                is_pub: e.is_pub,
                sig: format!("enum {}", e.name.name),
                guarantees: Vec::new(),
                deprecated: None,
            }),
            Item::Struct { is_pub, is_record, is_union, name, .. } => {
                let kind = if *is_union {
                    "union"
                } else if *is_record {
                    "record"
                } else {
                    "struct"
                };
                records.push(Record {
                    kind,
                    name: name.name.clone(),
                    is_pub: *is_pub,
                    sig: format!("{kind} {}", name.name),
                    guarantees: Vec::new(),
                    deprecated: None,
                });
            }
            Item::Extern(e) => records.push(Record {
                kind: "extern",
                name: e.name.name.clone(),
                is_pub: e.is_pub,
                sig: doc::extern_sig(ast, e),
                guarantees: Vec::new(),
                deprecated: None,
            }),
            // **A trait IS part of the ABI, and leaving it out was a false negative
            // inside the tool whose whole job is finding breakage.**
            //
            // Measured before it was fixed: two programs differing only by a removed
            // `pub` trait method produced DIFFERENT C hashes and `no API changes`,
            // exit 0. The gate passed a change that breaks every caller through the
            // trait. The old comment here said the effect was "captured by the C
            // hash" — the hash is in the manifest, but `diff` produces no `Change`
            // from it, so it can never reach `has_breaking()`. A hash cannot classify;
            // it can only say *something* moved, which is true of every rebuild.
            //
            // One record per trait, with the method set rendered INTO the signature,
            // rather than a record per method. That keeps the "one record per
            // top-level item" shape the whole manifest — and the self-hosted port's
            // `at_records` — is built on, and it still catches the case: removing a
            // method changes the trait's `sig`, which `diff_item` reports as breaking.
            // Adding one is breaking too, and correctly so: every existing implementor
            // stops compiling.
            Item::Trait(t) => records.push(Record {
                kind: "trait",
                name: t.name.name.clone(),
                is_pub: t.is_pub,
                sig: trait_sig(ast, src, t),
                guarantees: Vec::new(),
                deprecated: None,
            }),
            // An impl is the other half: a type that stops implementing a trait breaks
            // every call through it, and that is invisible if only the trait is
            // recorded. Named `<Trait> for <Type>` because coherence allows at most one
            // per pair, which makes that string a key.
            //
            // **Recorded as `pub` unconditionally**, because an `impl` carries no
            // visibility of its own and is reachable through any public trait. That is
            // the conservative direction, and this file's contract is explicitly that
            // only *provably* compatible changes get the compatible verdict.
            Item::Impl(i) => records.push(Record {
                kind: "impl",
                name: format!("{} for {}", i.trait_name.name, doc::ty_str(ast, i.ty)),
                is_pub: true,
                sig: impl_sig(ast, src, i),
                guarantees: Vec::new(),
                deprecated: None,
            }),
            // Still not standalone C items, and neither carries a callable surface:
            // a `distinct` is attested by the struct/fn records that mention it, and
            // an `import` is the module graph's business (`jestyrc manifest`).
            Item::Distinct(_) | Item::Import(_) => {}
        }
    }
    records
}

/// Reconstruct the loader's concatenated global source buffer from a `Modules`'
/// per-module `srcs` — the buffer the merged AST's global spans index into. The
/// loader appends each module's text followed by a `\n` separator, so this mirrors
/// that exactly. (For a single-file program this is just the file text plus a
/// trailing newline; spans never reach it.)
pub fn global_src(modules: &crate::module::Modules) -> String {
    let mut s = String::new();
    for src in &modules.srcs {
        s.push_str(src);
        s.push('\n');
    }
    s
}

// ============================================================================
// `jestyrc attest --diff` — a *sound* semantic breaking-change detector.
//
// It parses two manifests back into structured contracts and classifies every
// per-item change as **breaking** or **compatible**. The asymmetry that keeps it
// sound: a false negative (calling a real break "compatible") is the dangerous
// error, so anything not *provably* compatible is reported breaking. The verdicts
// follow Liskov/behavioral-subtyping:
//
//   precondition (`requires`, refined params): strengthening = breaking
//   postcondition (`ensures`, `@no_panic`):     weakening    = breaking
//   error set (`!{…}`):                         widening     = breaking
//   a `pub` item removed / demoted to `priv`:                  breaking
//   the type signature (params/return) changed:                breaking
//   …and each dual is compatible.
// ============================================================================

/// A manifest parsed back into structured contracts, keyed by `<kind> <name>` (so
/// the diff matches items by stable identity, not by signature).
#[derive(Default)]
pub struct ParsedManifest {
    pub version: String,
    pub source: String,
    pub c_sha256: String,
    pub cc_flags: String,
    pub items: BTreeMap<String, ParsedItem>,
}

/// One item's parsed contract: the raw signature plus the structured guarantees the
/// classifier compares (preconditions, postconditions, error set, refinements).
#[derive(Default, Clone)]
pub struct ParsedItem {
    pub kind: String,
    pub name: String,
    pub is_pub: bool,
    pub sig: String,
    pub no_panic: bool,
    pub requires: BTreeSet<String>,
    pub ensures: BTreeSet<String>,
    pub errors: BTreeSet<String>,
    /// param name → its refinement-range text (e.g. `0..100`).
    pub refines: BTreeMap<String, String>,
    /// `@deprecated`: `None` absent, `Some("")` bare, `Some(msg)` with a message.
    pub deprecated: Option<String>,
}

impl ParsedItem {
    fn key(&self) -> String {
        format!("{} {}", self.kind, self.name)
    }

    /// Fold one rendered guarantee phrase back into its structured field. The
    /// phrasings are exactly those `doc::fn_guarantees` emits — round-trip-tested.
    fn absorb_guarantee(&mut self, phrase: &str) {
        if phrase.starts_with("`@no_panic`") {
            self.no_panic = true;
        } else if let Some(rest) = phrase.strip_prefix("`requires ") {
            self.requires.insert(rest.trim_end_matches('`').to_string());
        } else if let Some(rest) = phrase.strip_prefix("`ensures ") {
            self.ensures.insert(rest.trim_end_matches('`').to_string());
        } else if phrase.starts_with("parameter `") {
            if let Some((name, range)) = parse_refine_phrase(phrase) {
                self.refines.insert(name, range);
            }
        } else if let Some(rest) = phrase.strip_prefix("may fail with `!{ ") {
            let inner = rest.trim_end_matches('`').trim_end_matches('}');
            for e in inner.split(',').map(str::trim).filter(|e| !e.is_empty()) {
                self.errors.insert(e.to_string());
            }
        }
    }

    /// The *type* signature only — the `@no_panic` prefix, the `!{…}` error set, and
    /// every `in <range>` refinement removed, since those are compared structurally.
    /// What's left (params' base types, conventions, the return type) is the part a
    /// change to which is an unconditional ABI break. The exact substrings are known
    /// from the parsed structure, so the subtraction is precise, not heuristic.
    fn sig_core(&self) -> String {
        // `@no_panic` (a postcondition) and `pub` (visibility) are compared
        // structurally, so strip both — wherever they sit (the sig renders them as
        // `pub @no_panic fn …`) — to avoid double-reporting them as a sig change.
        let mut s = self.sig.replace("@no_panic ", "");
        if let Some(rest) = s.strip_prefix("pub ") {
            s = rest.to_string();
        }
        if let Some(i) = s.find(" !{") {
            if let Some(rel) = s[i..].find('}') {
                s.replace_range(i..i + rel + 1, "");
            }
        }
        for r in self.refines.values() {
            s = s.replace(&format!(" in {r}"), "");
        }
        s.trim().to_string()
    }
}

/// `parameter `i` is constrained to `0..n`` → `("i", "0..n")`.
fn parse_refine_phrase(phrase: &str) -> Option<(String, String)> {
    let rest = phrase.strip_prefix("parameter `")?;
    let (name, rest) = rest.split_once('`')?;
    let range = rest.strip_prefix(" is constrained to `")?.strip_suffix('`')?;
    Some((name.to_string(), range.to_string()))
}

/// Parse a manifest's text back into structured form. Tolerant of unknown lines
/// (forward compatibility) but rejects a buffer whose first line isn't the format
/// tag, so a non-manifest file fails fast rather than diffing as empty.
pub fn parse_manifest(text: &str) -> Result<ParsedManifest, String> {
    let mut m = ParsedManifest::default();
    let mut lines = text.lines();

    let first = lines.next().unwrap_or("");
    if !first.starts_with("jestyr-attest/") {
        return Err(format!("not a jestyr-attest manifest (first line was {first:?})"));
    }
    m.version = first.to_string();

    // Header: lines up to the first blank.
    for line in lines.by_ref() {
        if line.is_empty() {
            break;
        }
        if let Some(v) = line.strip_prefix("source ") {
            m.source = v.to_string();
        } else if let Some(v) = line.strip_prefix("c-sha256 ") {
            m.c_sha256 = v.to_string();
        } else if let Some(v) = line.strip_prefix("cc-flags ") {
            m.cc_flags = v.to_string();
        }
    }

    // Items: a non-indented `<kind> <name>` line opens a block; indented lines fill
    // it; a blank line (or the next key line) closes it.
    let mut cur: Option<ParsedItem> = None;
    let flush = |cur: &mut Option<ParsedItem>, m: &mut ParsedManifest| {
        if let Some(it) = cur.take() {
            m.items.insert(it.key(), it);
        }
    };
    for line in lines {
        if line.is_empty() {
            flush(&mut cur, &mut m);
        } else if let Some(body) = line.strip_prefix("  ") {
            if let Some(it) = cur.as_mut() {
                if let Some(v) = body.strip_prefix("vis: ") {
                    it.is_pub = v.trim() == "pub";
                } else if let Some(v) = body.strip_prefix("sig: ") {
                    it.sig = v.to_string();
                } else if let Some(v) = body.strip_prefix("guarantee: ") {
                    it.absorb_guarantee(v);
                } else if let Some(v) = body.strip_prefix("deprecated:") {
                    // `deprecated:` bare, `deprecated: <msg>` with one. A manifest
                    // written before this line existed simply has none, and parses to
                    // `None` — so an old manifest still diffs against a new one, and
                    // reads as "not deprecated then, deprecated now".
                    it.deprecated = Some(v.strip_prefix(' ').unwrap_or("").to_string());
                }
            }
        } else {
            flush(&mut cur, &mut m);
            let (kind, name) = line.split_once(' ').unwrap_or((line, ""));
            cur = Some(ParsedItem { kind: kind.to_string(), name: name.to_string(), ..Default::default() });
        }
    }
    flush(&mut cur, &mut m);
    Ok(m)
}

/// The verdict for one change. `Breaking` flips the `--diff` exit code (a CI gate).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Verdict {
    Breaking,
    Compatible,
}

/// One classified change: which item, what changed, and the verdict.
pub struct Change {
    pub verdict: Verdict,
    pub item: String,
    pub detail: String,
}

/// The result of diffing two manifests — a deterministically-ordered change list.
pub struct DiffReport {
    pub old_source: String,
    pub new_source: String,
    pub old_hash: String,
    pub new_hash: String,
    pub changes: Vec<Change>,
}

impl DiffReport {
    pub fn breaking(&self) -> usize {
        self.changes.iter().filter(|c| c.verdict == Verdict::Breaking).count()
    }
    pub fn compatible(&self) -> usize {
        self.changes.iter().filter(|c| c.verdict == Verdict::Compatible).count()
    }
    pub fn has_breaking(&self) -> bool {
        self.changes.iter().any(|c| c.verdict == Verdict::Breaking)
    }

    /// A human- and grep-friendly report. Deterministic: changes are already sorted
    /// by `(item, verdict, detail)`.
    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str("jestyr-attest diff\n");
        out.push_str(&format!("old: {} (c-sha256 {})\n", self.old_source, short(&self.old_hash)));
        out.push_str(&format!("new: {} (c-sha256 {})\n", self.new_source, short(&self.new_hash)));
        if self.old_hash != self.new_hash {
            out.push_str("note: emitted C differs (a rebuild); per-item ABI verdicts below\n");
        }
        out.push('\n');
        if self.changes.is_empty() {
            out.push_str("no API changes\n");
        } else {
            for c in &self.changes {
                let tag = match c.verdict {
                    Verdict::Breaking => "BREAKING  ",
                    Verdict::Compatible => "compatible",
                };
                out.push_str(&format!("{tag}  {}  {}\n", c.item, c.detail));
            }
            out.push('\n');
            out.push_str(&format!("{} breaking, {} compatible\n", self.breaking(), self.compatible()));
        }
        out
    }
}

/// The first 12 hex of a digest, for the report header (full hashes are noise there).
fn short(h: &str) -> String {
    h.chars().take(12).collect::<String>() + if h.len() > 12 { "…" } else { "" }
}

/// Classify every per-item change between two manifests. Sound by construction:
/// only *provably* compatible changes get the `Compatible` verdict; everything else
/// — including any change a heuristic can't prove safe — is `Breaking`.
pub fn diff(old: &ParsedManifest, new: &ParsedManifest) -> DiffReport {
    let mut changes = Vec::new();
    let keys: BTreeSet<&String> = old.items.keys().chain(new.items.keys()).collect();

    for key in keys {
        match (old.items.get(key), new.items.get(key)) {
            // Removed: only a public item's removal breaks the ABI.
            (Some(o), None) => changes.push(Change {
                verdict: if o.is_pub { Verdict::Breaking } else { Verdict::Compatible },
                item: key.clone(),
                detail: if o.is_pub { "removed (was pub)".into() } else { "removed (internal)".into() },
            }),
            // Added: never breaks an existing caller.
            (None, Some(_)) => changes.push(Change {
                verdict: Verdict::Compatible,
                item: key.clone(),
                detail: "added".into(),
            }),
            (Some(o), Some(n)) => diff_item(key, o, n, &mut changes),
            (None, None) => unreachable!(),
        }
    }

    // Total, content-based order — no map-iteration leak reaches the output.
    changes.sort_by(|a, b| {
        (&a.item, a.verdict as u8, &a.detail).cmp(&(&b.item, b.verdict as u8, &b.detail))
    });

    DiffReport {
        old_source: old.source.clone(),
        new_source: new.source.clone(),
        old_hash: old.c_sha256.clone(),
        new_hash: new.c_sha256.clone(),
        changes,
    }
}

/// Classify the changes within one item present in both manifests.
fn diff_item(key: &str, o: &ParsedItem, n: &ParsedItem, out: &mut Vec<Change>) {
    let mut push = |verdict, detail: String| out.push(Change { verdict, item: key.to_string(), detail });

    // Visibility: leaving the public API is breaking; entering it is compatible.
    if o.is_pub && !n.is_pub {
        push(Verdict::Breaking, "no longer `pub`".into());
    } else if !o.is_pub && n.is_pub {
        push(Verdict::Compatible, "now `pub`".into());
    }

    // Type signature (params/return), with the structurally-compared bits removed.
    if o.sig_core() != n.sig_core() {
        push(Verdict::Breaking, format!("signature changed: `{}` → `{}`", o.sig_core(), n.sig_core()));
    }

    // Deprecation. **Every verdict here is `Compatible`, and that is the point.** A
    // deprecated function still compiles, still links, and still returns what it always
    // did — nothing a caller wrote stops working. Classifying it `Breaking` would fire
    // the CI gate on the one action a library author takes to AVOID breaking people, and
    // a gate that fires on good behaviour is a gate that gets switched off.
    //
    // It still has to be REPORTED, though: "this is going away" is exactly what someone
    // reads a contract diff to find out, and until now it was the one thing `@deprecated`
    // could not tell them.
    match (&o.deprecated, &n.deprecated) {
        (None, Some(m)) if m.is_empty() => push(Verdict::Compatible, "now `@deprecated`".into()),
        (None, Some(m)) => push(Verdict::Compatible, format!("now `@deprecated`: {m}")),
        (Some(_), None) => push(Verdict::Compatible, "no longer `@deprecated`".into()),
        (Some(a), Some(b)) if a != b => {
            push(Verdict::Compatible, format!("deprecation message changed: `{a}` → `{b}`"))
        }
        _ => {}
    }

    // `@no_panic` is a postcondition: losing it is breaking, gaining it compatible.
    if o.no_panic && !n.no_panic {
        push(Verdict::Breaking, "lost `@no_panic`".into());
    } else if !o.no_panic && n.no_panic {
        push(Verdict::Compatible, "gained `@no_panic`".into());
    }

    // Error set: a new error a caller must handle is breaking; a removed one frees them.
    for e in n.errors.difference(&o.errors) {
        push(Verdict::Breaking, format!("error added `{e}`"));
    }
    for e in o.errors.difference(&n.errors) {
        push(Verdict::Compatible, format!("error removed `{e}`"));
    }

    // Precondition `requires`: a new demand on the caller is breaking; dropping one frees them.
    for r in n.requires.difference(&o.requires) {
        push(Verdict::Breaking, format!("`requires {r}` added"));
    }
    for r in o.requires.difference(&n.requires) {
        push(Verdict::Compatible, format!("`requires {r}` removed"));
    }

    // Postcondition `ensures`: a new promise is compatible; dropping one a caller relied on is breaking.
    for s in n.ensures.difference(&o.ensures) {
        push(Verdict::Compatible, format!("`ensures {s}` added"));
    }
    for s in o.ensures.difference(&n.ensures) {
        push(Verdict::Breaking, format!("`ensures {s}` removed"));
    }

    // Refinements: narrowing a param's accepted set strengthens a precondition (breaking);
    // widening it relaxes one (compatible). A change not *provably* a widening is breaking.
    let params: BTreeSet<&String> = o.refines.keys().chain(n.refines.keys()).collect();
    for p in params {
        match (o.refines.get(p), n.refines.get(p)) {
            (None, Some(r)) => push(Verdict::Breaking, format!("param `{p}` newly constrained to `{r}`")),
            (Some(_), None) => push(Verdict::Compatible, format!("param `{p}` constraint removed")),
            (Some(ro), Some(rn)) if ro != rn => {
                if range_widened(ro, rn) {
                    push(Verdict::Compatible, format!("param `{p}` widened `{ro}` → `{rn}`"));
                } else {
                    push(Verdict::Breaking, format!("param `{p}` narrowed `{ro}` → `{rn}`"));
                }
            }
            _ => {}
        }
    }
}

/// Is `new` a provable *superset* of `old` — both integer-literal ranges of the same
/// inclusivity? Only then is a refinement change a widening (compatible). Anything
/// non-literal or incomparable returns `false`, so the caller stays conservative.
fn range_widened(old: &str, new: &str) -> bool {
    match (parse_int_range(old), parse_int_range(new)) {
        (Some((ol, oh, oi)), Some((nl, nh, ni))) if oi == ni => nl <= ol && nh >= oh,
        _ => false,
    }
}

/// `lo..hi` or `lo..=hi` over integer literals → `(lo, hi, inclusive)`.
fn parse_int_range(s: &str) -> Option<(i64, i64, bool)> {
    let (inclusive, lo, hi) = if let Some((a, b)) = s.split_once("..=") {
        (true, a, b)
    } else if let Some((a, b)) = s.split_once("..") {
        (false, a, b)
    } else {
        return None;
    };
    Some((lo.trim().parse().ok()?, hi.trim().parse().ok()?, inclusive))
}
