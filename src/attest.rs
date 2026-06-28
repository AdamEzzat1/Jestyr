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
        for g in &r.guarantees {
            out.push_str(&format!("  guarantee: {g}\n"));
        }
    }
    out
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
            }),
            Item::Const(c) => records.push(Record {
                kind: "const",
                name: c.name.name.clone(),
                is_pub: c.is_pub,
                sig: doc::const_sig(ast, src, c),
                guarantees: Vec::new(),
            }),
            Item::Enum(e) => records.push(Record {
                kind: "enum",
                name: e.name.name.clone(),
                is_pub: e.is_pub,
                sig: format!("enum {}", e.name.name),
                guarantees: Vec::new(),
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
                });
            }
            Item::Extern(e) => records.push(Record {
                kind: "extern",
                name: e.name.name.clone(),
                is_pub: e.is_pub,
                sig: doc::extern_sig(ast, e),
                guarantees: Vec::new(),
            }),
            // Not standalone C items: their behavior is attested via the C hash.
            Item::Trait(_) | Item::Impl(_) | Item::Distinct(_) | Item::Import(_) => {}
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
