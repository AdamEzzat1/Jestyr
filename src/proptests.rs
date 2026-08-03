//! Property-based and fuzz tests for the whole pipeline.
//!
//! Unit tests pin down *examples*; these pin down *invariants* that must hold for
//! every input:
//!  * the lexer never panics and always terminates in `Eof`, with in-bounds spans;
//!  * the parser never panics, even on adversarial token soup;
//!  * the full pipeline (lex → parse → typeck → escape → cgen) never panics;
//!  * a *generated valid* program always parses without diagnostics.
//!
//! `proptest` shrinks failing cases to a minimal reproducer; `bolero` drives the
//! same pipeline as a coverage-guided fuzz target (and replays its corpus under
//! plain `cargo test`).

#![cfg(test)]

use crate::cgen;
use crate::escape;
use crate::lexer::Lexer;
use crate::parser::Parser;
use crate::token::TokenKind;
use crate::typeck;

/// Run the entire compiler front-to-back. Returns nothing — the point is that it
/// must not panic for *any* input.
fn run_pipeline(src: &str) {
    let (tokens, _lex_diags) = Lexer::new(src).tokenize();

    // Invariant: the stream is non-empty and ends in exactly one Eof.
    assert!(!tokens.is_empty());
    assert_eq!(tokens.last().unwrap().kind, TokenKind::Eof);

    // Invariant: every span is within bounds and lands on char boundaries.
    let len = src.len() as u32;
    for t in &tokens {
        assert!(t.span.start <= t.span.end, "span start after end");
        assert!(t.span.end <= len, "span past end of source");
        assert!(src.is_char_boundary(t.span.start as usize));
        assert!(src.is_char_boundary(t.span.end as usize));
    }

    let (ast, _parse_diags) = Parser::new(src, tokens).parse();
    let (info, _type_diags) = typeck::check(&ast);
    let _escapes = escape::check(&ast, &info);
    let (_c, _cgen_diags) = cgen::emit(&ast, &info);
}

/// Compile front-to-back, returning the emitted C and the *total* diagnostic count
/// across all stages. The workhorse for the determinism and metamorphic properties.
fn compile(src: &str) -> (String, usize) {
    let (tokens, ld) = Lexer::new(src).tokenize();
    let (ast, pd) = Parser::new(src, tokens).parse();
    let (info, td) = typeck::check(&ast);
    let ed = escape::check(&ast, &info);
    let (c, cd) = cgen::emit(&ast, &info);
    (c, ld.len() + pd.len() + td.len() + ed.len() + cd.len())
}

/// The type-checker's diagnostic messages for a source — used by the coherence
/// properties to assert *which* diagnostic fires (not just how many).
fn typeck_diags(src: &str) -> Vec<String> {
    let (tokens, _) = Lexer::new(src).tokenize();
    let (ast, _) = Parser::new(src, tokens).parse();
    let (_info, td) = typeck::check(&ast);
    td.iter().map(|d| d.message.clone()).collect()
}

/// Like [`compile`] but stops after type-checking and hands back the AST plus the
/// inferred-type table, so a property can locate an expression and assert its
/// inferred type (the teeth for a typeck-completeness invariant).
fn typeck_full(src: &str) -> (crate::ast::Ast, crate::types::TypeInfo) {
    let (tokens, _) = Lexer::new(src).tokenize();
    let (ast, _) = Parser::new(src, tokens).parse();
    let (info, _td) = typeck::check(&ast);
    (ast, info)
}

/// The escape checker's diagnostic messages for a source — used by the
/// `@no_alloc` soundness/completeness property to assert *which* error fires.
fn escape_diags(src: &str) -> Vec<String> {
    let (tokens, _) = Lexer::new(src).tokenize();
    let (ast, _) = Parser::new(src, tokens).parse();
    let (info, _) = typeck::check(&ast);
    escape::check(&ast, &info).iter().map(|d| d.message.clone()).collect()
}

/// The token-kind sequence (Eof dropped) — the lexer's structural output, used by
/// the whitespace-insensitivity metamorphic property.
fn token_kinds(src: &str) -> Vec<TokenKind> {
    let (tokens, _) = Lexer::new(src).tokenize();
    tokens.iter().map(|t| t.kind).filter(|k| *k != TokenKind::Eof).collect()
}

/// Run `f` on a thread with a large stack, mirroring the compiler's own worker
/// thread (`WORKER_STACK` in `main.rs`). The recursive back-end passes
/// (`typeck`, `escape`, `cgen`, the printer) need more than a test thread's
/// default stack to walk an expression nested up to the parser's
/// [`crate::parser::MAX_EXPR_DEPTH`] cap — production always provides it, so the
/// deep-input tests reproduce that environment rather than assuming a stack size.
fn with_big_stack<R: Send + 'static>(f: impl FnOnce() -> R + Send + 'static) -> R {
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(f)
        .expect("spawn big-stack test thread")
        .join()
        .expect("big-stack test thread panicked")
}

/// A left-associative fold `1+1+…+1` of `depth` `+` operators — parses
/// iteratively (no parser recursion) yet builds a left-deep tree of height
/// ~`depth`, the exact shape that overflowed a later pass before the depth guard.
fn add_chain(depth: usize) -> String {
    let mut body = String::with_capacity(1 + depth * 2);
    body.push('1');
    for _ in 0..depth {
        body.push_str("+1");
    }
    format!("fn main() -> i64 {{ return {body} }}")
}

/// **The deepest expression the parser accepts is walkable by every later pass.**
/// The guard admits nesting up to [`crate::parser::MAX_EXPR_DEPTH`]; the recursive
/// back end must handle a tree exactly that tall. Mirrors production by running on
/// a large stack, then asserts the whole pipeline is total on the max depth that
/// parses — the case the handoff calls out ("test with the max depth that parses").
#[test]
fn max_parseable_depth_is_walkable_end_to_end() {
    with_big_stack(|| {
        let src = add_chain(crate::parser::MAX_EXPR_DEPTH - 8); // just under the cap
        let (tokens, _) = Lexer::new(&src).tokenize();
        let (ast, pd) = Parser::new(&src, tokens).parse();
        assert!(
            !pd.iter().any(|d| d.message.contains("too deep")),
            "a fold just under the cap must parse clean: {pd:?}"
        );
        // typeck → escape → cgen all complete without overflowing.
        let (info, td) = typeck::check(&ast);
        assert!(td.is_empty(), "the integer fold is well-typed: {td:?}");
        let _ = escape::check(&ast, &info);
        let (c, cd) = cgen::emit(&ast, &info);
        assert!(cd.is_empty(), "no cgen notes for a plain integer fold: {cd:?}");
        assert!(!c.is_empty(), "emitted C is non-empty");
    });
}

/// A chain *past* the cap is height-bounded — the parser drains the surplus — so
/// the full pipeline stays total even far beyond the limit. Guards against a
/// regression where the guard reports but still builds (and forwards) a tall tree.
#[test]
fn over_cap_chain_keeps_the_pipeline_total() {
    with_big_stack(|| {
        run_pipeline(&add_chain(crate::parser::MAX_EXPR_DEPTH * 50));
    });
}

/// The *recursive* deepening paths — prefix-op recursion (`!!!…`) and parenthesis
/// recursion (`((…))`) — hit the depth guard rather than overflowing the parser.
/// These recurse O(depth) in the parser, so (unlike the iterative folds) reaching
/// the cap needs the compiler's worker-sized stack; run there and assert the guard
/// fires. Removing the `parse_unary` / `parse_binary` entry guards regresses this.
#[test]
fn recursive_deep_shapes_report_on_the_worker_stack() {
    with_big_stack(|| {
        for shape in ["not", "paren"] {
            let depth = crate::parser::MAX_EXPR_DEPTH * 3;
            let body = if shape == "not" {
                let mut e = String::with_capacity(depth + 4);
                for _ in 0..depth {
                    e.push('!');
                }
                e.push_str("true");
                e
            } else {
                let mut e = String::with_capacity(depth * 2 + 1);
                for _ in 0..depth {
                    e.push('(');
                }
                e.push('1');
                for _ in 0..depth {
                    e.push(')');
                }
                e
            };
            let src = format!("fn main() -> i64 {{ return {body} }}");
            let (tokens, _) = Lexer::new(&src).tokenize();
            let (_ast, pd) = Parser::new(&src, tokens).parse();
            assert!(
                pd.iter().any(|d| d.message.contains("expression nesting too deep")),
                "shape `{shape}` should report the depth guard, got {pd:?}"
            );
        }
    });
}

/// A minimal well-formedness check for a JSON document's **string literals**.
///
/// Written by hand because the compiler has no JSON dependency, and aimed at the one
/// thing that can actually go wrong when emitting JSON without a library: a string body
/// that was not escaped. It walks the document tracking whether it is inside a string,
/// and rejects an unterminated string, an illegal escape, or a raw control byte inside
/// one — each of which makes the whole report unparseable for a consumer.
///
/// Returns the offending description, or `Ok(())`.
fn json_strings_wellformed(s: &str) -> Result<(), String> {
    let b: Vec<char> = s.chars().collect();
    let mut i = 0;
    let mut in_str = false;
    while i < b.len() {
        let c = b[i];
        if !in_str {
            if c == '"' {
                in_str = true;
            }
            i += 1;
            continue;
        }
        match c {
            '"' => in_str = false,
            '\\' => {
                let Some(&n) = b.get(i + 1) else {
                    return Err("trailing backslash inside a string".into());
                };
                match n {
                    '"' | '\\' | '/' | 'b' | 'f' | 'n' | 'r' | 't' => {}
                    'u' => {
                        let hex: String = b.iter().skip(i + 2).take(4).collect();
                        if hex.len() != 4 || !hex.chars().all(|h| h.is_ascii_hexdigit()) {
                            return Err(format!("bad \\u escape: {hex:?}"));
                        }
                        i += 4;
                    }
                    other => return Err(format!("illegal escape `\\{other}`")),
                }
                i += 1;
            }
            c if (c as u32) < 0x20 => {
                return Err(format!("raw control byte U+{:04X} inside a string", c as u32))
            }
            _ => {}
        }
        i += 1;
    }
    if in_str {
        Err("unterminated string".into())
    } else {
        Ok(())
    }
}

/// **The JSON diagnostic report is well-formed for every corpus program that has
/// diagnostics.** The escaping is exercised by real messages — which quote user
/// identifiers, type renderings and source text — rather than by invented ones.
#[test]
fn json_diagnostics_are_wellformed_over_the_corpus() {
    let mut with_diags = 0;
    for dir in ["examples", "examples/std"] {
        let Ok(rd) = std::fs::read_dir(dir) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().and_then(|s| s.to_str()) != Some("jtr") {
                continue;
            }
            let prog = crate::module::load(p.to_str().unwrap());
            let mut diags = prog.diags.clone();
            if diags.is_empty() {
                let (info, td) = crate::typeck::check_program(&prog.ast, &prog.modules);
                diags = td;
                diags.extend(crate::escape::check(&prog.ast, &info));
            }
            let json = prog.modules.render_json(&diags);
            json_strings_wellformed(&json)
                .unwrap_or_else(|e| panic!("{}: malformed JSON report: {e}\n{json}", p.display()));
            assert!(json.starts_with("{\"version\":1,\"diagnostics\":["), "{}", p.display());
            assert!(json.ends_with("]}\n"), "{}", p.display());
            // The report is deterministic — it is meant to be diffed and checked in.
            assert_eq!(json, prog.modules.render_json(&diags));
            if !diags.is_empty() {
                with_diags += 1;
            }
        }
    }
    // The corpus deliberately contains files that fail to check (`typeerr.jtr`,
    // `match_check.jtr`, …). If none produced a diagnostic, this test is vacuous.
    assert!(with_diags >= 3, "only {with_diags} corpus files produced diagnostics");
}

mod prop {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        /// The lexer never panics and preserves its span invariants on any string.
        #[test]
        fn lexer_is_total(s in ".{0,400}") {
            let (tokens, _diags) = Lexer::new(&s).tokenize();
            prop_assert_eq!(tokens.last().unwrap().kind, TokenKind::Eof);
            let len = s.len() as u32;
            for t in &tokens {
                prop_assert!(t.span.end <= len);
                prop_assert!(t.span.start <= t.span.end);
            }
        }

        /// The full pipeline never panics on arbitrary text.
        #[test]
        fn pipeline_is_total(s in ".{0,400}") {
            run_pipeline(&s);
        }

        /// **A diagnostic renders to well-formed JSON whatever it says.**
        ///
        /// Message and help text are not the compiler's own: they quote user
        /// identifiers, rendered types and source excerpts, so they can contain
        /// quotes, backslashes, newlines and control bytes. One unescaped character
        /// makes the *entire* report unparseable — a consumer loses every diagnostic,
        /// not one — which is why this is a property over arbitrary strings rather
        /// than a few hand-picked cases.
        #[test]
        fn a_diagnostic_always_renders_valid_json(msg in ".{0,120}", help in ".{0,80}") {
            let src = "fn f() {}\n";
            let d = crate::diag::Diagnostic::new(msg, crate::span::Span::new(3, 4))
                .with_help(help);
            let mut out = String::new();
            crate::diag::to_json(&d, "t.jtr", src, crate::diag::Severity::Error, &mut out);
            prop_assert!(json_strings_wellformed(&out).is_ok(), "malformed: {}", out);
            // …and the object is still shaped like one. (Bound first: `prop_assert!`
            // stringifies the expression into a format string, where a literal brace
            // would be read as a placeholder.)
            let shaped = out.starts_with('{') && out.ends_with('}');
            prop_assert!(shaped, "not a JSON object: {}", out);
        }

        /// **Deeply-nested expressions error, they don't crash.** For any depth
        /// past the parser's [`crate::parser::MAX_EXPR_DEPTH`] cap, a left-deep
        /// fold parses without overflowing the stack and reports the "too deep"
        /// diagnostic exactly once — no cascade — rather than building an
        /// unbounded tree a later recursive pass would blow up on. (Parse-only,
        /// so this is safe on the default test stack: a left-associative fold
        /// parses iteratively regardless of depth.)
        #[test]
        fn deep_expressions_error_not_crash(extra in 1usize..2048) {
            let depth = crate::parser::MAX_EXPR_DEPTH + extra;
            let src = add_chain(depth);
            let (tokens, _) = Lexer::new(&src).tokenize();
            let (_ast, pd) = Parser::new(&src, tokens).parse();
            prop_assert_eq!(pd.len(), 1, "expected exactly one diagnostic at depth {}", depth);
            prop_assert!(
                pd[0].message.contains("expression nesting too deep"),
                "wrong diagnostic: {:?}", pd[0].message
            );
        }

        /// The doc generator (lex-with-docs → parse → attach → render) never
        /// panics on arbitrary text, in either output format.
        #[test]
        fn doc_generator_is_total(s in ".{0,400}") {
            let _ = crate::doc::generate(&s, "t", false);
            let _ = crate::doc::generate(&s, "t", true);
        }

        /// The pipeline is total even on raw ASCII soup (lots of operators/braces).
        #[test]
        fn pipeline_is_total_on_ascii_soup(s in "[\\[\\](){}<>!?@.,;:|&^~*/+=%a-zA-Z0-9 \n]{0,400}") {
            run_pipeline(&s);
        }

        /// A generated *valid* arithmetic program parses with no diagnostics.
        #[test]
        fn generated_valid_program_parses_clean(body in arb_expr()) {
            let src = format!("fn f() -> i32 {{ {body} }}");
            let (tokens, lex_diags) = Lexer::new(&src).tokenize();
            prop_assert!(lex_diags.is_empty(), "lex errors on {}", src);
            let (_ast, parse_diags) = Parser::new(&src, tokens).parse();
            prop_assert!(parse_diags.is_empty(), "parse errors on {}: {:?}", src, parse_diags);
        }

        /// A bare decimal integer always lexes as a single `Int` token.
        #[test]
        fn decimals_lex_as_one_int(n in 0u64..1_000_000) {
            let s = n.to_string();
            let (tokens, diags) = Lexer::new(&s).tokenize();
            prop_assert!(diags.is_empty());
            // tokens = [Int, Eof]
            prop_assert_eq!(tokens.len(), 2);
            prop_assert_eq!(tokens[0].kind, TokenKind::Int);
        }

        /// An `x`-prefixed identifier (no keyword starts with `x`) lexes as a
        /// single `Ident`. (NB: a `v` prefix would *not* be safe — `var` is a
        /// keyword.)
        #[test]
        fn prefixed_identifiers_are_idents(name in "x[a-zA-Z0-9_]{0,8}") {
            let (tokens, diags) = Lexer::new(&name).tokenize();
            prop_assert!(diags.is_empty());
            prop_assert_eq!(tokens.len(), 2);
            prop_assert_eq!(tokens[0].kind, TokenKind::Ident);
        }

        // ── the stricter layer ────────────────────────────────────────────────

        /// **Determinism (the headline).** A deterministic language needs a
        /// deterministic compiler: the same source must emit byte-identical C and
        /// the same diagnostic count every run — no `HashMap`/`HashSet` iteration
        /// order may leak into the output.
        #[test]
        fn compilation_is_deterministic(s in ".{0,400}") {
            prop_assert_eq!(compile(&s), compile(&s));
        }

        /// Determinism on *structurally rich* valid programs (structs/enums/fns),
        /// which exercise the monomorphization/struct/enum collectors where ordering
        /// bugs would hide.
        #[test]
        fn valid_programs_compile_deterministically(p in super::prop::arb_program()) {
            prop_assert_eq!(compile(&p), compile(&p));
        }

        /// A generated valid program lowers to *some* C built on the runtime prelude
        /// (cgen never silently produces nothing for a well-formed program).
        #[test]
        fn valid_program_emits_prelude(p in super::prop::arb_program()) {
            let (c, _) = compile(&p);
            prop_assert!(c.contains("#include"), "no prelude in: {}", c);
        }

        /// **Metamorphic — whitespace insensitivity.** The lexer discards layout, so
        /// doubling every space leaves the token-kind sequence unchanged.
        #[test]
        fn whitespace_does_not_change_tokens(p in super::prop::arb_program()) {
            prop_assert_eq!(token_kinds(&p), token_kinds(&p.replace(' ', "   ")));
        }

        /// **Metamorphic — comment insensitivity.** Comments are trivia (no tokens,
        /// no AST nodes), so prepending one cannot change the emitted C.
        #[test]
        fn comments_do_not_change_codegen(p in super::prop::arb_program()) {
            let plain = compile(&p).0;
            let commented = compile(&format!("// a comment\n{p}")).0;
            prop_assert_eq!(plain, commented);
        }

        /// The AST printer is total and **idempotent** (printing twice is stable) on
        /// any generated program.
        #[test]
        fn printer_is_total_and_stable(p in super::prop::arb_program()) {
            let (tokens, _) = Lexer::new(&p).tokenize();
            let (ast, _) = Parser::new(&p, tokens).parse();
            let a = crate::printer::print_ast(&ast);
            let b = crate::printer::print_ast(&ast);
            prop_assert_eq!(a, b);
        }

        // ── function-pointer types (TESTING.md §5, per-feature property layer) ──

        /// A generated program that *names* function-pointer types — in a vtable
        /// struct and in a signature, over varied parameter conventions, with and
        /// without a return — always parses with no diagnostics.
        #[test]
        fn fn_pointer_programs_parse_clean(p in arb_fn_type_program()) {
            let (tokens, lex_diags) = Lexer::new(&p).tokenize();
            prop_assert!(lex_diags.is_empty(), "lex errors on {}", p);
            let (_ast, parse_diags) = Parser::new(&p, tokens).parse();
            prop_assert!(parse_diags.is_empty(), "parse errors on {}: {:?}", p, parse_diags);
        }

        /// Every such program lowers to C that contains a `JestyrFn_` typedef —
        /// the lowering actually fires (the type isn't silently dropped) for every
        /// shape the generator produces.
        #[test]
        fn fn_pointer_programs_emit_a_typedef(p in arb_fn_type_program()) {
            let (c, _) = compile(&p);
            prop_assert!(c.contains("JestyrFn_"), "no fn-ptr typedef for {}:\n{}", p, c);
        }

        /// Determinism still holds on fn-pointer-bearing programs: byte-identical C
        /// and the same diagnostic count every run (no ordering leak via the new
        /// `fn_type_instances` collection).
        #[test]
        fn fn_pointer_programs_compile_deterministically(p in arb_fn_type_program()) {
            prop_assert_eq!(compile(&p), compile(&p));
        }

        // ── generic vtables (fn-pointer field on a generic struct) ────────────

        /// **The generic-vtable typing invariant** (the Main Objective): a
        /// fn-pointer field called method-style on a *generic-struct* receiver
        /// infers its result by the field's **substituted return type** — never
        /// `Unknown` from the generic fallthrough. The oracle is the construction
        /// type `t`, varied across primitives by the generator. Teeth: reverting
        /// `fn_ptr_field`'s `Ty::GenStruct` arm to `None` makes `b.op(n)` infer
        /// `Unknown`, and this fails for every `t`.
        #[test]
        fn generic_vtable_field_call_types_by_substituted_return(
            (p, t) in arb_gen_vtable_program(),
        ) {
            let (ast, info) = typeck_full(&p);
            let call = ast
                .exprs
                .iter()
                .enumerate()
                .find_map(|(i, e)| {
                    matches!(e.kind, crate::ast::ExprKind::Call { .. })
                        .then_some(crate::ast::ExprId(i as u32))
                })
                .expect("the b.op(n) call");
            prop_assert_eq!(info.type_of(call), &crate::types::Ty::Prim(t), "program: {}", p);
        }

        /// Determinism still holds on generic-vtable programs — byte-identical C
        /// and the same diagnostic count every run (the new generic field-call
        /// path adds no iteration-order leak).
        #[test]
        fn generic_vtable_programs_compile_deterministically(
            (p, _t) in arb_gen_vtable_program(),
        ) {
            prop_assert_eq!(compile(&p), compile(&p));
        }

        /// **The bare generic-struct field-read invariant**: reading a fn-pointer
        /// field off a generic-struct value (`let f = b.op`) resolves the field's
        /// type under substitution, so the later `f(n)` infers `t` rather than
        /// `Unknown`. Teeth: reverting `field_type`'s `Ty::GenStruct` arm makes
        /// the read — and the call through it — type as `Unknown`.
        #[test]
        fn generic_vtable_bare_field_read_types_under_substitution(
            (p, t) in arb_gen_vtable_read_program(),
        ) {
            let (ast, info) = typeck_full(&p);
            let call = ast
                .exprs
                .iter()
                .enumerate()
                .find_map(|(i, e)| {
                    matches!(e.kind, crate::ast::ExprKind::Call { .. })
                        .then_some(crate::ast::ExprId(i as u32))
                })
                .expect("the f(n) call");
            prop_assert_eq!(info.type_of(call), &crate::types::Ty::Prim(t), "program: {}", p);
        }

        // ── traits / interfaces, Stage A (parse + represent) ──────────────────

        /// A generated trait program — a trait (required + default methods), an
        /// `impl` for it, a bounded generic `[T: Tr]` use, and optionally a `dyn`
        /// parameter — always lexes and parses with no diagnostics.
        #[test]
        fn trait_programs_parse_clean(p in arb_trait_program()) {
            let (tokens, lex_diags) = Lexer::new(&p).tokenize();
            prop_assert!(lex_diags.is_empty(), "lex errors on {}", p);
            let (_ast, parse_diags) = Parser::new(&p, tokens).parse();
            prop_assert!(parse_diags.is_empty(), "parse errors on {}: {:?}", p, parse_diags);
        }

        /// The whole pipeline stays **total and deterministic** on trait programs —
        /// Stage A adds no semantics, so this also guards that the new AST nodes
        /// don't make any later stage panic or leak iteration order.
        #[test]
        fn trait_programs_are_total_and_deterministic(p in arb_trait_program()) {
            run_pipeline(&p);
            prop_assert_eq!(compile(&p), compile(&p));
        }

        /// `print_ast` is total and idempotent on trait/impl/`dyn`/bound nodes.
        #[test]
        fn trait_programs_print_stably(p in arb_trait_program()) {
            let (tokens, _) = Lexer::new(&p).tokenize();
            let (ast, _) = Parser::new(&p, tokens).parse();
            let a = crate::printer::print_ast(&ast);
            let b = crate::printer::print_ast(&ast);
            prop_assert_eq!(a, b);
        }

        // ── traits, Stage B: coherence (a single-program differential) ────────

        /// **Coherence soundness.** Two `impl`s of the same `(trait, type)` are
        /// *always* a conflict — for every concrete type the generator picks.
        #[test]
        fn duplicate_impl_is_always_a_coherence_error(t in arb_prim_ty()) {
            let p = format!(
                "trait xT {{ fn xm(read self) -> i32 }} \
                 impl xT for {t} {{ fn xm(read self) -> i32 {{ return 1 }} }} \
                 impl xT for {t} {{ fn xm(read self) -> i32 {{ return 2 }} }}"
            );
            let diags = typeck_diags(&p);
            prop_assert!(
                diags.iter().any(|d| d.contains("conflicting implementations")),
                "expected a coherence error: {:?}", diags
            );
        }

        /// **Coherence completeness.** `impl`s of a trait for *distinct* types are
        /// never a conflict (the oracle: the two types differ by construction).
        #[test]
        fn distinct_type_impls_are_accepted((a, b) in (arb_prim_ty(), arb_prim_ty())) {
            prop_assume!(a != b);
            let p = format!(
                "trait xT {{ fn xm(read self) -> i32 }} \
                 impl xT for {a} {{ fn xm(read self) -> i32 {{ return 1 }} }} \
                 impl xT for {b} {{ fn xm(read self) -> i32 {{ return 2 }} }}"
            );
            let diags = typeck_diags(&p);
            prop_assert!(
                !diags.iter().any(|d| d.contains("conflicting implementations")),
                "distinct-type impls must be accepted: {:?}", diags
            );
        }

        /// **Order independence.** The coherence verdict for a duplicate pair is the
        /// same whichever source order the two `impl`s appear in (no iteration-order
        /// leak in the single-pass `impl_index` check).
        #[test]
        fn coherence_verdict_is_order_independent(t in arb_prim_ty()) {
            let tr = "trait xT { fn xm(read self) -> i32 }";
            let i1 = format!("impl xT for {t} {{ fn xm(read self) -> i32 {{ return 1 }} }}");
            let i2 = format!("impl xT for {t} {{ fn xm(read self) -> i32 {{ return 2 }} }}");
            let n_ab = typeck_diags(&format!("{tr} {i1} {i2}"))
                .iter().filter(|d| d.contains("conflicting implementations")).count();
            let n_ba = typeck_diags(&format!("{tr} {i2} {i1}"))
                .iter().filter(|d| d.contains("conflicting implementations")).count();
            prop_assert_eq!(n_ab, 1);
            prop_assert_eq!(n_ab, n_ba);
        }

        // ── traits, Stage C: static dispatch ──────────────────────────────────

        /// **Static-dispatch lowering.** A `recv.m()` call resolved through
        /// `impl xShow for <t>` lowers to a *direct* call of the mangled
        /// impl-method symbol — for every concrete receiver type the generator
        /// picks. Teeth: with Stage C disabled the call falls through to a
        /// receiver-less form and this exact symbol-with-receiver never appears.
        #[test]
        fn trait_call_lowers_to_a_direct_impl_method_call((p, t) in arb_trait_call_program()) {
            let (c, _) = compile(&p);
            let call = format!("jestyr_impl_xShow__{t}__xm(j_s)");
            prop_assert!(c.contains(&call), "expected a direct static call `{}`:\n{}", call, c);
        }

        /// Determinism holds on trait-dispatch programs — byte-identical C and the
        /// same diagnostic count every run (impl-method emission iterates the item
        /// list in source order; the mangle is a pure function of the resolution).
        #[test]
        fn trait_call_programs_compile_deterministically((p, _t) in arb_trait_call_program()) {
            prop_assert_eq!(compile(&p), compile(&p));
        }

        // ── traits, Stage D: definition-site bounds ───────────────────────────

        /// **Bound soundness.** A bracket-generic `xuse[T: xB]` instantiated at a
        /// type with *no* matching `impl` always errors at the call — for every
        /// distinct (impl-type, call-type) pair. Teeth: removing the call-site
        /// bound check makes the unsatisfied instantiation type-check clean.
        #[test]
        fn unsatisfied_bound_always_errors_at_the_call((a, b) in (arb_prim_ty(), arb_prim_ty())) {
            prop_assume!(a != b);
            let p = format!(
                "trait xB {{ fn xm(read self) -> i32 }} \
                 impl xB for {a} {{ fn xm(read self) -> i32 {{ return 0 }} }} \
                 fn xuse[T: xB](read x: T) -> i32 {{ return 0 }} \
                 fn xc(read y: {b}) -> i32 {{ return xuse(y) }}"
            );
            let diags = typeck_diags(&p);
            prop_assert!(
                diags.iter().any(|d| d.contains("does not implement trait `xB`")),
                "expected an unsatisfied-bound error for `{}`: {:?}", b, diags
            );
        }

        /// **Bound completeness.** A bracket-generic instantiated at a type that
        /// *does* `impl` the bound never raises a bound error (the oracle: the
        /// impl is for that exact type).
        #[test]
        fn satisfied_bound_never_errors_at_the_call(t in arb_prim_ty()) {
            let p = format!(
                "trait xB {{ fn xm(read self) -> i32 }} \
                 impl xB for {t} {{ fn xm(read self) -> i32 {{ return 0 }} }} \
                 fn xuse[T: xB](read x: T) -> i32 {{ return 0 }} \
                 fn xc(read y: {t}) -> i32 {{ return xuse(y) }}"
            );
            let diags = typeck_diags(&p);
            prop_assert!(
                !diags.iter().any(|d| d.contains("does not implement")),
                "a satisfied bound must not error: {:?}", diags
            );
        }

        // ── traits, Stage E: operator traits ──────────────────────────────────

        /// **Operator dispatch.** `a OP b` on a user type with the matching
        /// operator `impl` lowers to a direct call of the impl method — for every
        /// trait-backed operator (`+`/`*`/`==`/`<`). Teeth: without operator
        /// resolution the op stays a native `(j_a OP j_b)` and this symbol is absent.
        #[test]
        fn an_operator_on_a_user_type_lowers_to_its_impl_call((p, sym) in arb_operator_program()) {
            let (c, _) = compile(&p);
            prop_assert!(c.contains(&sym), "expected operator dispatch `{}`:\n{}", sym, c);
        }

        /// Determinism holds on operator-trait programs — byte-identical C every run.
        #[test]
        fn operator_programs_compile_deterministically((p, _s) in arb_operator_program()) {
            prop_assert_eq!(compile(&p), compile(&p));
        }

        // ── bracket-generic monomorphization ──────────────────────────────────

        /// **Monomorphization.** A bracket generic `dup[T]` called at a concrete
        /// type emits a mangled instance `jestyr_dup__<t>` — `T` recovered from the
        /// argument type, for every primitive. Teeth: treating bracket generics as
        /// non-generic emits a single `jestyr_dup` with an unlowerable `T` instead.
        #[test]
        fn a_bracket_generic_is_monomorphized_per_instantiation(
            (p, sym) in arb_bracket_generic_program(),
        ) {
            let (c, _) = compile(&p);
            prop_assert!(c.contains(&sym), "expected a monomorphized instance `{}`:\n{}", sym, c);
        }

        /// Determinism holds on bracket-generic programs — byte-identical C every run.
        #[test]
        fn bracket_generic_programs_compile_deterministically(
            (p, _s) in arb_bracket_generic_program(),
        ) {
            prop_assert_eq!(compile(&p), compile(&p));
        }

        // ── body-side bound enforcement (the "Zig fix") ───────────────────────

        /// **Per-instance bound dispatch.** Inside `g[T: xS]`, `x.xm()` lowers to
        /// the concrete `impl xS for <t>` method at each instantiation — for every
        /// primitive `t`. Teeth: dropping the bound-method resolution emits an
        /// unresolved `j_x.j_xm(...)` instead of the mangled impl symbol.
        #[test]
        fn a_bound_method_dispatches_to_the_concrete_impl(
            (p, sym) in arb_bound_method_program(),
        ) {
            let (c, _) = compile(&p);
            prop_assert!(c.contains(&sym), "expected bound-method dispatch `{}`:\n{}", sym, c);
        }

        /// Determinism holds on bound-method programs — byte-identical C every run.
        #[test]
        fn bound_method_programs_compile_deterministically(
            (p, _s) in arb_bound_method_program(),
        ) {
            prop_assert_eq!(compile(&p), compile(&p));
        }

        // ── `dyn Trait` dynamic dispatch (Stage F) ────────────────────────────

        /// **Vtable coercion + dispatch.** Passing a concrete `t` (which `impl`s
        /// `xS`) as `dyn xS` emits a fat pointer carrying *that type's* vtable —
        /// `&jestyr_vt_xS__<t>` — for every primitive. Teeth: without the coercion
        /// the concrete value is passed raw and the vtable address is absent.
        #[test]
        fn a_concrete_value_coerced_to_dyn_carries_its_vtable(
            (p, sym) in arb_dyn_program(),
        ) {
            let (c, _) = compile(&p);
            prop_assert!(c.contains(&sym), "expected the per-type vtable `{}`:\n{}", sym, c);
        }

        /// Determinism holds on `dyn` programs — byte-identical C every run.
        #[test]
        fn dyn_programs_compile_deterministically((p, _s) in arb_dyn_program()) {
            prop_assert_eq!(compile(&p), compile(&p));
        }
    }

    /// A concrete primitive type to `impl` a trait for.
    fn arb_prim_ty() -> impl Strategy<Value = &'static str> {
        prop_oneof![Just("i32"), Just("i64"), Just("u8"), Just("u16"), Just("bool")]
    }

    /// A small *valid* trait program: a trait `xShow` with `n` required methods, an
    /// `impl xShow for i32` providing them, a bounded generic `xuse[T: xShow]`, and
    /// (optionally) a `dyn xShow` parameter. All names are `x`-prefixed so they
    /// never collide with a keyword — the program always lexes and parses clean.
    fn arb_trait_program() -> impl Strategy<Value = String> {
        (1usize..4, any::<bool>()).prop_map(|(n, use_dyn)| {
            let sigs: Vec<String> =
                (0..n).map(|i| format!("fn xm{i}(read self) -> i32")).collect();
            let impls: Vec<String> =
                (0..n).map(|i| format!("fn xm{i}(read self) -> i32 {{ return {i} }}")).collect();
            let dyn_fn = if use_dyn { "fn xr(read s: dyn xShow) -> i32 { return 0 }" } else { "" };
            format!(
                "trait xShow {{ {} }} impl xShow for i32 {{ {} }} \
                 fn xuse[T: xShow](read x: T) -> i32 {{ return 0 }} {dyn_fn}",
                sigs.join("  "),
                impls.join("  "),
            )
        })
    }

    /// A small *valid* program that actually *calls* a trait method method-style
    /// (`s.xm()`) on a concrete receiver `t`, so the call resolves through
    /// `impl xShow for <t>` and exercises Stage C static-dispatch lowering. Paired
    /// with `t` — the receiver type whose mangled symbol the call must target.
    fn arb_trait_call_program() -> impl Strategy<Value = (String, &'static str)> {
        arb_prim_ty().prop_map(|t| {
            let prog = format!(
                "trait xShow {{ fn xm(read self) -> i32 }} \
                 impl xShow for {t} {{ fn xm(read self) -> i32 {{ return 7 }} }} \
                 fn xuse(read s: {t}) -> i32 {{ return s.xm() }}"
            );
            (prog, t)
        })
    }

    /// A *valid* program that uses one of the four trait-backed operators on a
    /// user type `V` with the matching `impl`. Paired with the dispatch symbol the
    /// emitted C must contain (`jestyr_impl_<Trait>__V__<method>(j_a, j_b)`).
    fn arb_operator_program() -> impl Strategy<Value = (String, String)> {
        // The full operator surface: a `V` that `impl`s all six primitive operator
        // traits, then a function using one operator. Each case is paired with the
        // exact dispatch call the C must contain — including the *swapped* operand
        // order for the derived `>`/`<=`, which has teeth for the derivation.
        const IMPLS: &str = "struct V { n: i32 } \
            impl Add for V { fn add(read self, read rhs: V) -> V { return V{ n: self.n + rhs.n } } } \
            impl Sub for V { fn sub(read self, read rhs: V) -> V { return V{ n: self.n - rhs.n } } } \
            impl Mul for V { fn mul(read self, read rhs: V) -> V { return V{ n: self.n * rhs.n } } } \
            impl Div for V { fn div(read self, read rhs: V) -> V { return V{ n: self.n / rhs.n } } } \
            impl Eq for V { fn eq(read self, read rhs: V) -> bool { return self.n == rhs.n } } \
            impl Ord for V { fn lt(read self, read rhs: V) -> bool { return self.n < rhs.n } }";
        prop_oneof![
            Just(("+", "V", "jestyr_impl_Add__V__add(j_a, j_b)")),
            Just(("-", "V", "jestyr_impl_Sub__V__sub(j_a, j_b)")),
            Just(("*", "V", "jestyr_impl_Mul__V__mul(j_a, j_b)")),
            Just(("/", "V", "jestyr_impl_Div__V__div(j_a, j_b)")),
            Just(("==", "bool", "jestyr_impl_Eq__V__eq(j_a, j_b)")),
            Just(("!=", "bool", "jestyr_impl_Eq__V__eq(j_a, j_b)")),
            Just(("<", "bool", "jestyr_impl_Ord__V__lt(j_a, j_b)")),
            Just((">", "bool", "jestyr_impl_Ord__V__lt(j_b, j_a)")),
            Just(("<=", "bool", "jestyr_impl_Ord__V__lt(j_b, j_a)")),
            Just((">=", "bool", "jestyr_impl_Ord__V__lt(j_a, j_b)")),
        ]
        .prop_map(|(op, ret, sym)| {
            let prog =
                format!("{IMPLS} fn use_it(read a: V, read b: V) -> {ret} {{ return a {op} b }}");
            (prog, sym.to_string())
        })
    }

    /// A *valid* program that calls a bracket generic `dup[T]` at a concrete type
    /// `t` (inferred from the argument). Paired with the mangled instance prefix
    /// the emitted C must contain (`jestyr_dup__<t>(`).
    fn arb_bracket_generic_program() -> impl Strategy<Value = (String, String)> {
        arb_prim_ty().prop_map(|t| {
            let prog = format!(
                "fn dup[T](take x: T) -> T {{ return x }} \
                 fn use_it(take y: {t}) -> {t} {{ return dup(y) }}"
            );
            (prog, format!("jestyr_dup__{t}("))
        })
    }

    /// A *valid* program that coerces a concrete `t` (which `impl`s `xS`) to
    /// `dyn xS` and dispatches through it. Paired with the per-type vtable address
    /// the emitted C must contain.
    fn arb_dyn_program() -> impl Strategy<Value = (String, String)> {
        arb_prim_ty().prop_map(|t| {
            let prog = format!(
                "trait xS {{ fn xm(read self) -> i32 }} \
                 impl xS for {t} {{ fn xm(read self) -> i32 {{ return 0 }} }} \
                 fn describe(read s: dyn xS) -> i32 {{ return s.xm() }} \
                 fn use_it(read y: {t}) -> i32 {{ return describe(y) }}"
            );
            (prog, format!("&jestyr_vt_xS__{t}"))
        })
    }

    /// A *valid* program where a bound generic `g[T: xS]` calls the bound method
    /// `x.xm()`, instantiated at a concrete `t` that `impl`s `xS`. Paired with the
    /// per-instance dispatch symbol the emitted C must contain.
    fn arb_bound_method_program() -> impl Strategy<Value = (String, String)> {
        arb_prim_ty().prop_map(|t| {
            let prog = format!(
                "trait xS {{ fn xm(read self) -> i32 }} \
                 impl xS for {t} {{ fn xm(read self) -> i32 {{ return 0 }} }} \
                 fn g[T: xS](read x: T) -> i32 {{ return x.xm() }} \
                 fn use_it(read y: {t}) -> i32 {{ return g(y) }}"
            );
            // The dispatch *call* (receiver `j_x`), not the impl method definition
            // (`j_self`) — so the property has teeth for the bound-method lowering.
            (prog, format!("jestyr_impl_xS__{t}__xm(j_x)"))
        })
    }

    /// A base type to appear inside a generated function-pointer signature —
    /// primitives plus a thin pointer (so the niche/pointer path is exercised).
    fn arb_base_ty() -> impl Strategy<Value = &'static str> {
        prop_oneof![
            Just("i32"), Just("i64"), Just("u8"), Just("usize"), Just("bool"), Just("*mut u8"),
        ]
    }

    /// A single function-pointer *type* string, e.g. `fn(read i32, *mut u8) -> u8`
    /// or `fn(mut i64)` (no return). Conventions vary across `read`/`take`/`mut`
    /// and the bare default — the convention-in-the-type surface.
    fn arb_fn_sig() -> impl Strategy<Value = String> {
        let conv = prop_oneof![Just(""), Just("read "), Just("take "), Just("mut ")];
        let param = (conv, arb_base_ty()).prop_map(|(c, t)| format!("{c}{t}"));
        let params = proptest::collection::vec(param, 0..4).prop_map(|ps| ps.join(", "));
        let ret = prop_oneof![
            arb_base_ty().prop_map(|t| format!(" -> {t}")),
            Just(String::new()),
        ];
        (params, ret).prop_map(|(p, r)| format!("fn({p}){r}"))
    }

    /// A small *valid* program that names function-pointer types in the two
    /// canonical positions — a vtable struct field and a function parameter — with
    /// a trivial body so it also type-checks and lowers cleanly.
    fn arb_fn_type_program() -> impl Strategy<Value = String> {
        (arb_fn_sig(), arb_fn_sig(), arb_fn_sig()).prop_map(|(a, b, c)| {
            format!(
                "struct V {{ f0: {a}, f1: {b} }} fn use_v(g: {c}) -> i32 {{ return 0 }}"
            )
        })
    }

    /// A small *valid* program with a **generic** vtable: a generic struct `xBox`
    /// whose fn-pointer field `op: fn(T) -> T` ranges over the struct's own type
    /// parameter, built at a concrete type `t` and called *method-style*
    /// (`b.op(n)`). Paired with `t` — the oracle the call's result type must equal
    /// under substitution. The identity closure `|x| x` coerces for every `t`.
    fn arb_gen_vtable_program() -> impl Strategy<Value = (String, &'static str)> {
        arb_prim_ty().prop_map(|t| {
            let prog = format!(
                "fn xBox(comptime T: type) -> type {{ return struct {{ op: fn(T) -> T }} }} \
                 fn xuse(n: {t}) -> {t} {{ let b = xBox({t}){{ op: |x| x }} return b.op(n) }}"
            );
            (prog, t)
        })
    }

    /// Like [`arb_gen_vtable_program`], but the fn-pointer field is *read* into a
    /// local before being called (`let f = b.op  return f(n)`) — the bare
    /// field-read path through `field_type`'s generic-struct arm. The call's
    /// result type must still resolve to `t` under substitution.
    fn arb_gen_vtable_read_program() -> impl Strategy<Value = (String, &'static str)> {
        arb_prim_ty().prop_map(|t| {
            let prog = format!(
                "fn xBox(comptime T: type) -> type {{ return struct {{ op: fn(T) -> T }} }} \
                 fn xuse(n: {t}) -> {t} {{ let b = xBox({t}){{ op: |x| x }} let f = b.op return f(n) }}"
            );
            (prog, t)
        })
    }

    /// A small but structurally-rich *valid-parsing* program: a struct, an enum, and
    /// a function whose body is an arbitrary arithmetic expression. Names are
    /// `x`-prefixed + index-suffixed, so they are unique and never collide with a
    /// keyword — the program always lexes and parses clean.
    pub(super) fn arb_program() -> impl Strategy<Value = String> {
        (
            proptest::collection::vec("x[a-z0-9]{0,4}", 1..4),
            proptest::collection::vec("x[a-z0-9]{0,4}", 1..4),
            arb_expr(),
        )
            .prop_map(|(fields, variants, body)| {
                let fs: Vec<String> =
                    fields.iter().enumerate().map(|(i, f)| format!("{f}{i}: i32")).collect();
                let vs: Vec<String> =
                    variants.iter().enumerate().map(|(i, v)| format!("{v}{i}")).collect();
                format!(
                    "struct S {{ {} }} enum E {{ {} }} fn f() -> i32 {{ {body} }}",
                    fs.join(", "),
                    vs.join(", ")
                )
            })
    }

    /// A recursively-built, always-valid arithmetic expression. Identifiers are
    /// `x`-prefixed so they can never collide with a keyword — no Jestyr keyword
    /// begins with `x` (whereas `v` would admit `var`).
    pub(super) fn arb_expr() -> impl Strategy<Value = String> {
        let leaf = prop_oneof![
            (0u32..1000).prop_map(|n| n.to_string()),
            "x[a-z0-9]{0,4}".prop_map(|s| s),
        ];
        leaf.prop_recursive(4, 48, 4, |inner| {
            prop_oneof![
                (inner.clone(), prop_oneof![Just("+"), Just("-"), Just("*"), Just("/")], inner.clone())
                    .prop_map(|(a, op, b)| format!("({a} {op} {b})")),
            ]
        })
    }
}

/// Property tests for `#line` debug-info emission (workstream: debug info).
///
/// The on-thesis invariants are **behavioral invariance** (debug info must never
/// change results — `#line` is purely additive, so stripping the directives
/// recovers the no-debug C byte-for-byte), **bounded line numbers** (every
/// emitted line is a real `1..=file_line_count`), and **determinism** (the
/// debug-enabled emit is byte-identical across runs). All pure-Rust — they scan
/// the emitted C, so they run under the toolchain-free default `cargo test`.
mod debuginfo_props {
    use super::*;
    use proptest::prelude::*;

    /// Emit C with single-file debug info populated (the loader path's behavior).
    fn emit_with_debug(src: &str) -> String {
        let (tokens, _) = Lexer::new(src).tokenize();
        let (ast, _) = Parser::new(src, tokens).parse();
        let (mut info, _td) = typeck::check(&ast);
        info.debug = crate::types::DebugInfo::new(
            vec!["p.jtr".to_string()],
            vec![src.to_string()],
            vec![0],
        );
        cgen::emit(&ast, &info).0
    }

    /// Emit C with no debug info (empty tables) — no `#line` is produced.
    fn emit_plain(src: &str) -> String {
        let (tokens, _) = Lexer::new(src).tokenize();
        let (ast, _) = Parser::new(src, tokens).parse();
        let (info, _td) = typeck::check(&ast);
        cgen::emit(&ast, &info).0
    }

    /// Drop every `#line` directive, line-normalized — the canonical "what the C
    /// would be without debug info" form, applied identically to both builds so a
    /// trailing-newline difference can't masquerade as a real divergence.
    fn without_line_directives(c: &str) -> String {
        c.lines().filter(|l| !l.starts_with("#line ")).map(|l| format!("{l}\n")).collect()
    }

    proptest! {
        /// **Behavioral invariance (the star).** Emitting `#line` only *adds*
        /// `#line` lines: strip them and the result equals the no-debug build.
        /// Debug info can never change the program the compiler produces.
        #[test]
        fn debug_info_is_purely_additive(p in super::prop::arb_program()) {
            let dbg = emit_with_debug(&p);
            let plain = emit_plain(&p);
            prop_assert_eq!(without_line_directives(&dbg), without_line_directives(&plain));
        }

        /// **Bounded line numbers.** Every emitted `#line N` names a real source
        /// line: `1 <= N <= file_line_count`. An off-by-one in `span_to_file_line`
        /// that walked past the file end would trip the upper bound.
        #[test]
        fn line_numbers_are_in_range(p in super::prop::arb_program()) {
            let c = emit_with_debug(&p);
            // A real token's offset is always < src.len(), so the largest line a
            // span can resolve to is `newline_count + 1`.
            let max_line = p.matches('\n').count() as u32 + 1;
            for l in c.lines() {
                if let Some(rest) = l.strip_prefix("#line ") {
                    let n: u32 = rest.split_whitespace().next().unwrap().parse().unwrap();
                    prop_assert!(n >= 1, "line number must be >= 1: {l}");
                    prop_assert!(n <= max_line, "line {n} exceeds file line count {max_line}: {l}");
                }
            }
        }

        /// **Determinism preserved.** Debug-enabled emission is byte-identical
        /// across runs — no `HashMap`/`HashSet` order leaks via the `#line` path.
        #[test]
        fn debug_emit_is_deterministic(p in super::prop::arb_program()) {
            prop_assert_eq!(emit_with_debug(&p), emit_with_debug(&p));
        }

        /// Every emitted directive is *well-formed*: a quoted path, no embedded
        /// newline inside the quotes, and no raw backslash (paths are normalized).
        #[test]
        fn emitted_directives_are_well_formed(p in super::prop::arb_program()) {
            for l in emit_with_debug(&p).lines() {
                if l.starts_with("#line ") {
                    prop_assert!(l.matches('"').count() == 2, "path must be quoted: {l}");
                    prop_assert!(!l.contains('\\'), "path must be backslash-free: {l}");
                }
            }
        }
    }
}

/// Property tests for B4 — `unsafe`/block as a value (a `let`/`var` initializer).
///
/// The invariant is **transparency**: `unsafe { E }` (and a plain `{ E }`) in
/// value position is byte-identical to bare `E`, because `unsafe` is a
/// compile-time permission marker with no runtime effect. Plus determinism and
/// totality. All pure-Rust (scan emitted C) — toolchain-free.
mod unsafe_init_props {
    use super::*;
    use proptest::prelude::*;

    // Each property builds `fn f() { let y = <init> return 0 }` — `return 0`
    // (not `y`) so the body type-checks for any expression shape, and an unused
    // `y` warns identically on the wrapped and bare sides (so the metamorphic
    // equality still holds).
    proptest! {
        /// `unsafe { E }` as an initializer ≡ bare `E` (the transparency invariant).
        #[test]
        fn unsafe_initializer_is_transparent(e in super::prop::arb_expr()) {
            let wrapped = format!("fn f() -> i32 {{ let y = unsafe {{ {e} }} return 0 }}");
            let bare = format!("fn f() -> i32 {{ let y = {e} return 0 }}");
            prop_assert_eq!(compile(&wrapped), compile(&bare));
        }

        /// A plain `{ E }` block as an initializer ≡ bare `E`.
        #[test]
        fn block_initializer_is_transparent(e in super::prop::arb_expr()) {
            let block = format!("fn f() -> i32 {{ let y = {{ {e} }} return 0 }}");
            let bare = format!("fn f() -> i32 {{ let y = {e} return 0 }}");
            prop_assert_eq!(compile(&block), compile(&bare));
        }

        /// Determinism: the unsafe-initializer form compiles byte-identically twice.
        #[test]
        fn unsafe_initializer_is_deterministic(e in super::prop::arb_expr()) {
            let s = format!("fn f() -> i32 {{ let y = unsafe {{ {e} }} return 0 }}");
            prop_assert_eq!(compile(&s), compile(&s));
        }
    }
}

/// Property tests for B3 — recoverable `try_read_file -> String !IoError`.
///
/// Invariants: over any path string the call lowers to the tagged result (never
/// panics, never the plain read), it is deterministic, and — the additive gate —
/// a program that doesn't use it emits no `try_read` runtime/typedef. Pure-Rust.
mod try_read_props {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        /// For any path literal, `try_read_file` lowers to the recoverable result
        /// (its runtime helper + `JestyrResult_String`), and compiles deterministically.
        #[test]
        fn try_read_lowers_and_is_deterministic(p in "[a-zA-Z0-9_./-]{0,24}") {
            let src = format!("fn main() -> i32 {{ let r = try_read_file(\"{p}\") if is_err(r) {{ return 1 }} return 0 }}");
            let (c, _) = compile(&src);
            prop_assert!(c.contains("jestyr_rt_try_read_file"), "runtime helper present");
            prop_assert!(c.contains("JestyrResult_String"), "result typedef present");
            prop_assert_eq!(compile(&src), compile(&src));
        }

        /// The additive gate holds for arbitrary programs that never mention it:
        /// no `try_read` runtime/typedef leaks in.
        #[test]
        fn unrelated_programs_have_no_try_read(body in super::prop::arb_expr()) {
            let src = format!("fn f() -> i32 {{ let y = {body} return 0 }}");
            let (c, _) = compile(&src);
            prop_assert!(!c.contains("jestyr_rt_try_read_file"));
            prop_assert!(!c.contains("JestyrResult_String"));
        }
    }
}

/// Property tests for B5 — inline `slice(T, …)` typing in argument position.
///
/// The invariant: an *unannotated* `slice(u8, b, n)` fed straight into `from_utf8`
/// lowers byte-identically to the annotated-`let` workaround, for any buffer size,
/// and never falls back to the old `int _u` temp (which failed to compile). Plus
/// determinism. Pure-Rust (scan emitted C) — toolchain-free.
mod slice_typing_props {
    use super::*;
    use proptest::prelude::*;

    fn inline(n: usize) -> String {
        format!(
            "fn f() -> i64 {{ var b: *mut u8 = alloc(u8, {n}) \
             let s: str = from_utf8(slice(u8, b, {n})) return s.len as i64 }}"
        )
    }

    proptest! {
        /// The inline slice temp is always typed `JestyrSlice_u8`, never `int`.
        #[test]
        fn inline_slice_temp_is_typed(n in 1usize..32) {
            let (c, _) = compile(&inline(n));
            prop_assert!(c.contains("JestyrSlice_u8 _u ="), "slice temp typed:\n{}", c);
            prop_assert!(!c.contains("int _u ="), "no int fallback:\n{}", c);
        }

        /// Determinism: the inline form compiles byte-identically twice.
        #[test]
        fn inline_slice_is_deterministic(n in 1usize..32) {
            let s = inline(n);
            prop_assert_eq!(compile(&s), compile(&s));
        }
    }
}

/// Property tests for deterministic Drop/RAII scope-exit glue (design Phase 3).
///
/// The oracle is *known by construction*: a generator builds a program with a
/// known number of owned, non-moved droppables, and the property asserts the
/// emitted C contains exactly that many drop calls — a missed drop (`0`) is a
/// leak, a doubled one (`≥2`) is a double-free. All pure-Rust (scans emitted C),
/// so they run under the toolchain-free default `cargo test`.
mod drop_props {
    use super::compile;
    use proptest::prelude::*;

    const PRELUDE: &str = "trait Drop { fn drop(mut self) } struct R { id: i32 } \
        impl Drop for R { fn drop(mut self) { print_int(self.id) } } ";

    /// Non-overlapping occurrences of `needle` in `hay` (the in-test analogue of
    /// the handoff's `count_c_definitions` — here counting drop *call sites*).
    fn count(hay: &str, needle: &str) -> usize {
        hay.matches(needle).count()
    }

    /// A function with `n` owned `R` locals (used only by borrow, so all live to
    /// scope exit), optionally *returning the first* — which then moves to the
    /// caller and must not be dropped here. Returns `(source, expected_drops)`.
    fn drop_program(n: usize, move_first: bool) -> (String, usize) {
        let mut body = String::new();
        for i in 0..n {
            body.push_str(&format!("let x{i} = R{{ id: {i} }} "));
        }
        let (sig, tail, expected) = if move_first {
            ("fn f() -> R", "return x0", n - 1)
        } else {
            ("fn f()", "", n)
        };
        let src = format!("{PRELUDE} {sig} {{ {body}{tail} }} fn main() -> i32 {{ return 0 }}");
        (src, expected)
    }

    proptest! {
        /// Soundness **and** completeness of drop insertion: a function with `n`
        /// owned, non-moved droppables emits *exactly* `n` drop calls — never
        /// fewer (a leak) and never more (a double free).
        #[test]
        fn drops_each_owned_local_exactly_once(n in 1usize..6) {
            let (src, expected) = drop_program(n, false);
            let (c, diags) = compile(&src);
            prop_assert_eq!(diags, 0, "should compile clean:\n{}", src);
            prop_assert_eq!(
                count(&c, "jestyr_impl_Drop__R__drop(&j_x"), expected,
                "drop-call count mismatch:\n{}", c
            );
        }

        /// Drop-after-move elision (known-by-construction): returning a droppable
        /// moves it, so the origin drops `n-1` values and never the returned one.
        #[test]
        fn moved_out_value_is_not_dropped(n in 1usize..6) {
            let (src, expected) = drop_program(n, true);
            let (c, _diags) = compile(&src);
            prop_assert_eq!(
                count(&c, "jestyr_impl_Drop__R__drop(&j_x"), expected,
                "moved value dropped at origin:\n{}", c
            );
            prop_assert_eq!(
                count(&c, "jestyr_impl_Drop__R__drop(&j_x0)"), 0,
                "the moved-out local x0 must not be dropped:\n{}", c
            );
        }

        /// No-double-free: for every owned local, its drop call appears at most
        /// once on the straight-line path (`≤ 1`, never `2`).
        #[test]
        fn no_local_is_dropped_twice(n in 1usize..6) {
            let (src, _e) = drop_program(n, false);
            let (c, _d) = compile(&src);
            for i in 0..n {
                let needle = format!("jestyr_impl_Drop__R__drop(&j_x{i})");
                prop_assert!(count(&c, &needle) <= 1, "x{} dropped twice:\n{}", i, c);
            }
        }

        /// Drop-glue determinism: compiling the same Drop-heavy program twice is
        /// byte-identical (no iteration-order leak in drop lowering).
        #[test]
        fn drop_lowering_is_deterministic(n in 1usize..6) {
            let (src, _e) = drop_program(n, false);
            prop_assert_eq!(compile(&src), compile(&src));
        }

        /// The take-vs-borrow seam (the RAII-Vec enabler): a droppable passed to
        /// any number of `mut`-borrow calls still drops *exactly once* at scope
        /// exit — only a `take` argument would move it.
        #[test]
        fn borrow_passed_droppable_still_drops_once(k in 0usize..5) {
            let calls: String = (0..k).map(|_| "bump(x) ").collect();
            let src = format!(
                "{PRELUDE} fn bump(mut r: R) {{ r.id = r.id + 1 }} \
                 fn f() {{ var x = R{{ id: 1 }} {calls} }} fn main() -> i32 {{ return 0 }}"
            );
            let (c, _d) = compile(&src);
            prop_assert_eq!(
                count(&c, "jestyr_impl_Drop__R__drop(&j_x)"), 1,
                "a mut-borrowed droppable must drop exactly once:\n{}", c
            );
        }

        /// The generic-call seam (the generic-`Vec(T)` enabler): a droppable passed
        /// to a *generic* `mut`-borrow fn — where a leading `comptime` type argument
        /// occupies an arg slot — still drops exactly once, for any call count.
        #[test]
        fn generic_borrow_call_still_drops_once(k in 0usize..4) {
            let calls: String = (0..k).map(|_| "bump(R, x) ").collect();
            let src = format!(
                "{PRELUDE} fn bump(comptime T: type, mut r: T) {{ }} \
                 fn f() {{ var x = R{{ id: 1 }} {calls} }} fn main() -> i32 {{ return 0 }}"
            );
            let (c, _d) = compile(&src);
            prop_assert_eq!(
                count(&c, "jestyr_impl_Drop__R__drop(&j_x)"), 1,
                "a generic mut-borrow arg must not move the droppable:\n{}", c
            );
        }

        /// Region-integrated bulk drop (metamorphic): the same droppable emits one
        /// drop call in a plain block and *zero* inside a `region` (the arena
        /// reclaims it in bulk), for any small id.
        #[test]
        fn region_owned_value_emits_no_drop_glue(id in 0i32..50) {
            let outside = format!(
                "{PRELUDE} fn f() {{ let a = R{{ id: {id} }} }} fn main() -> i32 {{ return 0 }}"
            );
            let inside = format!(
                "{PRELUDE} fn f() {{ region r {{ let a = R{{ id: {id} }} }} }} \
                 fn main() -> i32 {{ return 0 }}"
            );
            let (co, _) = compile(&outside);
            let (ci, _) = compile(&inside);
            prop_assert_eq!(co.matches("__drop(&j_a)").count(), 1, "outside:\n{}", co);
            prop_assert_eq!(ci.matches("__drop(&j_a)").count(), 0, "region:\n{}", ci);
        }

        // --- B1: recursive field/payload drop (design §2.8) ---

        /// Field-drop completeness (known-by-construction): a container struct with
        /// no `Drop` of its own but `w` owned `R` *fields* drops every field exactly
        /// once — `w` leaf destructor calls, never fewer (a leak) nor more (a double
        /// free). The container itself has no own-drop symbol, so all `R` drops here
        /// are field recursion.
        #[test]
        fn struct_field_count_drops_each_field_once(w in 1usize..6) {
            let (src, _) = nested_drop_program(w, 1);
            let (c, diags) = compile(&src);
            prop_assert_eq!(diags, 0, "should compile clean:\n{}", src);
            prop_assert_eq!(
                count(&c, "jestyr_impl_Drop__R__drop(&j_h.j_f"), w,
                "expected {} field drops:\n{}", w, c
            );
        }

        /// Depth-invariance: however deeply a single droppable is nested inside
        /// chained field owners, the leaf destructor fires *exactly once* — the
        /// recursion follows the chain to the bottom and no further.
        #[test]
        fn arbitrary_nesting_drops_the_leaf_exactly_once(d in 1usize..6) {
            let (src, _) = nested_drop_program(1, d);
            let (c, diags) = compile(&src);
            prop_assert_eq!(diags, 0, "should compile clean:\n{}", src);
            prop_assert_eq!(
                count(&c, "jestyr_impl_Drop__R__drop(&"), 1,
                "a single nested leaf must drop exactly once at depth {}:\n{}", d, c
            );
        }

        /// Width×depth: a `w`-field container nested `d` levels deep drops exactly
        /// `w` leaves — the structural recursion visits every owned leaf once,
        /// independent of how the aggregates are shaped.
        #[test]
        fn nested_aggregate_drops_every_leaf_once(w in 1usize..5, d in 1usize..5) {
            let (src, expected_leaves) = nested_drop_program(w, d);
            let (c, diags) = compile(&src);
            prop_assert_eq!(diags, 0, "should compile clean:\n{}", src);
            prop_assert_eq!(
                count(&c, "jestyr_impl_Drop__R__drop(&"), expected_leaves,
                "expected {} leaf drops:\n{}", expected_leaves, c
            );
        }

        /// An enum payload drops once under a tag switch, for the live variant — and
        /// the inactive nullary variant contributes no spurious drop.
        #[test]
        fn enum_payload_drops_once_for_live_variant(id in 0i32..50) {
            let src = format!(
                "{PRELUDE} enum N {{ leaf, wrap(r: R) }} \
                 fn f() {{ let n = wrap(R{{ id: {id} }}) }} fn main() -> i32 {{ return 0 }}"
            );
            let (c, diags) = compile(&src);
            prop_assert_eq!(diags, 0, "should compile clean:\n{}", src);
            prop_assert_eq!(
                count(&c, "jestyr_impl_Drop__R__drop(&j_n.u.wrap.j_r)"), 1,
                "live enum payload must drop exactly once:\n{}", c
            );
        }

        /// Field/payload-drop determinism: compiling a nested-drop program twice is
        /// byte-identical — no iteration-order leak in the recursive walk.
        #[test]
        fn nested_drop_lowering_is_deterministic(w in 1usize..5, d in 1usize..5) {
            let (src, _) = nested_drop_program(w, d);
            prop_assert_eq!(compile(&src), compile(&src));
        }
    }

    /// Build a program whose local `h` is a `w`-field struct nested `d` levels deep,
    /// each leaf an owned `R`. Returns `(source, leaf_count)` where `leaf_count` is
    /// the number of `R` destructor calls auto-drop must emit. At `d == 1` the
    /// container directly holds `w` `R` fields; deeper levels wrap the previous
    /// level in a single-field struct, so the leaf count stays `w` (one chain of
    /// containers around the `w`-wide base). The container types have no `Drop` of
    /// their own — every emitted `R` drop is field recursion.
    fn nested_drop_program(w: usize, d: usize) -> (String, usize) {
        // Level 0: the wide base struct `L0 { f0: R, f1: R, … }` and its literal.
        let base_fields: Vec<String> = (0..w).map(|i| format!("f{i}: R")).collect();
        let base_lit: Vec<String> = (0..w).map(|i| format!("f{i}: R{{ id: {i} }}")).collect();
        let mut decls = format!("struct L0 {{ {} }} ", base_fields.join(", "));
        // Levels 1..d: each wraps the previous in a single field `f0`.
        for lvl in 1..d {
            decls.push_str(&format!("struct L{lvl} {{ f0: L{} }} ", lvl - 1));
        }
        // Build the literal from the inside out (its top type is `L{d-1}`).
        let mut lit = format!("L0{{ {} }}", base_lit.join(", "));
        for lvl in 1..d {
            lit = format!("L{lvl}{{ f0: {lit} }}");
        }
        let src = format!(
            "{PRELUDE} {decls} fn f() {{ let h = {lit} }} fn main() -> i32 {{ return 0 }}",
        );
        (src, w)
    }
}

/// Property tests for the `@no_alloc` enforced contract (design Phase 3).
///
/// Soundness **and** completeness against an independent oracle: the generator
/// knows by construction whether the body it built allocates, so the property
/// asserts the escape checker rejects a `@no_alloc` body *iff* it allocates — no
/// false negatives (a missed allocation) and no false positives (a rejected
/// allocation-free body).
mod alloc_props {
    use super::escape_diags;
    use proptest::prelude::*;

    /// The allocating intrinsics a `@no_alloc` body must not call, with a
    /// well-typed call form for each.
    const ALLOC_CALLS: &[&str] = &[
        "let p = alloc(i32, 4) free_ptr(p)",
        "let p = realloc(i32, null, 8) free_ptr(p)",
        "let h = arena_open(64)",
    ];

    /// Benign, allocation-free statements that must never trip the checker.
    const BENIGN: &[&str] = &["let s = n + 1", "let t = n * 2", "print_int(n)", ""];

    proptest! {
        /// A `@no_alloc` body that calls an allocating intrinsic is *always*
        /// rejected, whichever intrinsic and wherever in the body.
        #[test]
        fn allocating_body_is_always_rejected(
            ai in 0usize..ALLOC_CALLS.len(),
            bi in 0usize..BENIGN.len(),
        ) {
            let src = format!(
                "@no_alloc fn f(n: i32) -> i32 {{ {} {} return n }}",
                BENIGN[bi], ALLOC_CALLS[ai]
            );
            let diags = escape_diags(&src);
            prop_assert!(
                diags.iter().any(|m| m.contains("@no_alloc")),
                "an allocating @no_alloc body must be rejected: {}\n{:?}", src, diags
            );
        }

        /// A `@no_alloc` body built only from benign statements is *always*
        /// accepted — no false positives.
        #[test]
        fn allocation_free_body_is_always_accepted(
            b0 in 0usize..BENIGN.len(),
            b1 in 0usize..BENIGN.len(),
        ) {
            let src = format!(
                "@no_alloc fn f(n: i32) -> i32 {{ {} {} return n }}",
                BENIGN[b0], BENIGN[b1]
            );
            let diags = escape_diags(&src);
            prop_assert!(
                !diags.iter().any(|m| m.contains("@no_alloc")),
                "an allocation-free @no_alloc body must be accepted: {}\n{:?}", src, diags
            );
        }
    }
}

/// Tests for the `jestyrc test` runner polish (workstream O, increment 1): the
/// `@test`/`@bench` discovery (`cgen::list_tests`), the codegen-time name filter
/// (`cgen::emit_tests_filtered`), and their plumbing through the real pipeline.
///
/// The oracle is known *by construction*: a generator emits a program with a known
/// set of `@test`/`@bench` names plus decoys (a plain helper, a generic `@test`),
/// and the properties assert discovery and filtering match that set exactly —
/// soundness (never name a non-runnable item) and completeness (never drop a
/// runnable one). All pure-Rust (it inspects the emitted C string), so it runs
/// under the toolchain-free default `cargo test`; an end-to-end gcc check lives in
/// the `c_oracle` module behind `--features c-oracle`.
mod test_runner {
    use super::*;
    use crate::cgen::{self, TestKind};

    /// Type-check `src` and emit its `@test`/`@bench` harness narrowed by `filter`.
    fn harness(src: &str, filter: Option<&str>) -> String {
        let (ast, info) = typeck_full(src);
        cgen::emit_tests_filtered(&ast, &info, filter).0
    }

    /// The `N` of the harness's `running N test(s)\n` banner — the count of tests
    /// actually baked in. Panics if absent, so a harness that loses the banner
    /// fails loudly rather than silently reporting zero.
    fn baked_test_count(c: &str) -> usize {
        let at = c.find("running ").expect("harness must print the running banner");
        c[at + "running ".len()..]
            .split_whitespace()
            .next()
            .and_then(|n| n.parse().ok())
            .expect("running banner must carry a count")
    }

    /// `cgen::list_tests` over a parsed `src` (the data behind `--list`).
    fn list(src: &str) -> Vec<(String, TestKind)> {
        let (tokens, _) = Lexer::new(src).tokenize();
        let (ast, _) = Parser::new(src, tokens).parse();
        cgen::list_tests(&ast)
    }

    // ── unit: discovery (`list_tests`) ────────────────────────────────────────

    #[test]
    fn list_tests_finds_tests_and_benches_in_source_order() {
        let src = "@test fn a() -> bool { return true } \
                   fn helper() -> i32 { return 0 } \
                   @bench fn z() { } \
                   @test fn m() -> bool { return true }";
        assert_eq!(
            list(src),
            vec![
                ("a".to_string(), TestKind::Test),
                ("z".to_string(), TestKind::Bench),
                ("m".to_string(), TestKind::Test),
            ],
            "discovery must list tests+benches in source order and skip plain fns"
        );
    }

    #[test]
    fn list_tests_skips_a_generic_test() {
        // A `comptime T: type` test is a monomorphization template — never emitted
        // directly, so the harness can't run it; `--list` must not name it either.
        let src = "@test fn good() -> bool { return true } \
                   @test fn gen(comptime T: type) -> bool { return true }";
        assert_eq!(list(src), vec![("good".to_string(), TestKind::Test)]);
    }

    #[test]
    fn list_tests_is_empty_without_attributes() {
        assert!(list("fn main() -> i32 { return 0 }").is_empty());
    }

    // ── unit/golden: the codegen-time name filter ─────────────────────────────

    const THREE: &str = "@test fn add_one() -> bool { return true } \
                         @test fn add_two() -> bool { return true } \
                         @test fn sub_one() -> bool { return true }";

    #[test]
    fn unfiltered_harness_bakes_every_test() {
        let c = harness(THREE, None);
        assert_eq!(baked_test_count(&c), 3);
        assert!(c.contains("jestyr_add_one()") && c.contains("jestyr_sub_one()"));
    }

    #[test]
    fn a_substring_filter_bakes_only_matching_tests() {
        // `add` matches the two `add_*` tests; `sub_one` is excluded from both the
        // count and the call sites — the codegen-time filter, not a runtime skip.
        let c = harness(THREE, Some("add"));
        assert_eq!(baked_test_count(&c), 2, "filtered count: {c}");
        assert!(c.contains("jestyr_add_one()"), "kept add_one: {c}");
        assert!(c.contains("jestyr_add_two()"), "kept add_two: {c}");
        assert!(!c.contains("jestyr_sub_one()"), "sub_one must be filtered out: {c}");
    }

    #[test]
    fn an_empty_match_bakes_a_zero_test_harness() {
        let c = harness(THREE, Some("nomatch"));
        assert_eq!(baked_test_count(&c), 0);
        assert!(c.contains("result: %d passed; %d failed"), "still a well-formed harness: {c}");
    }

    #[test]
    fn the_filter_also_narrows_benches() {
        let src = "@test fn keep_t() -> bool { return true } \
                   @bench fn keep_b() { } \
                   @bench fn drop_b() { }";
        let c = harness(src, Some("keep"));
        assert!(c.contains("jestyr_keep_b()"), "kept bench: {c}");
        assert!(!c.contains("jestyr_drop_b()"), "dropped bench must be filtered: {c}");
    }

    #[test]
    fn unfiltered_equals_empty_filter_byte_for_byte() {
        // The whole point of codegen-time (not argv-time) filtering: `None` is the
        // ordinary harness, so `jestyrc test <file>` is unchanged.
        assert_eq!(harness(THREE, None), harness(THREE, Some("")));
    }

    // ── wiring: plumbed through the real loader, on the shipped demo ───────────

    #[test]
    fn filter_is_plumbed_through_the_pipeline_on_the_demo() {
        // Drive the same `module::load → typeck → escape` path `jestyrc test` uses,
        // then the filtered emit, on the real `examples/tests_demo.jtr`.
        let prog = crate::module::load("examples/tests_demo.jtr");
        assert!(prog.diags.is_empty(), "load diags: {:?}", prog.diags);
        let (info, td) = crate::typeck::check_program(&prog.ast, &prog.modules);
        assert!(td.is_empty(), "typeck: {:?}", td);
        assert!(crate::escape::check(&prog.ast, &info).is_empty());

        // The demo has two tests: `add_is_commutative`, `doubling_works`.
        let all = cgen::emit_tests_filtered(&prog.ast, &info, None).0;
        assert_eq!(baked_test_count(&all), 2, "demo has two tests");
        let only_double = cgen::emit_tests_filtered(&prog.ast, &info, Some("doub")).0;
        assert_eq!(baked_test_count(&only_double), 1, "`doub` selects one");
        assert!(only_double.contains("jestyr_doubling_works()"));
        assert!(!only_double.contains("jestyr_add_is_commutative()"));

        // `--list` discovery on the same AST (one greppable line per item upstream).
        assert_eq!(
            cgen::list_tests(&prog.ast),
            vec![
                ("add_is_commutative".to_string(), TestKind::Test),
                ("doubling_works".to_string(), TestKind::Test),
                ("sum_to_1000".to_string(), TestKind::Bench),
            ]
        );
    }
}

/// Property + fuzz tests for the `jestyrc test` runner (workstream O). The
/// generator builds a program with a *known* roster of `@test`/`@bench` names plus
/// decoys, so discovery and filtering have a by-construction oracle.
mod test_runner_props {
    use super::*;
    use crate::cgen::{self, TestKind};
    use proptest::prelude::*;

    /// Discovery + filtered emit over a parsed source.
    fn list(src: &str) -> Vec<(String, TestKind)> {
        let (tokens, _) = Lexer::new(src).tokenize();
        let (ast, _) = Parser::new(src, tokens).parse();
        cgen::list_tests(&ast)
    }
    fn baked_count(src: &str, filter: Option<&str>) -> usize {
        let (ast, info) = typeck_full(src);
        let c = cgen::emit_tests_filtered(&ast, &info, filter).0;
        let at = c.find("running ").expect("running banner");
        c[at + 8..].split_whitespace().next().unwrap().parse().unwrap()
    }

    /// A *valid* program with `t` tests `xtest{i}` and `b` benches `xbench{i}`, plus
    /// two decoys that must never be discovered: a plain `xhelper` and a *generic*
    /// `@test xgen` (a monomorphization template). Returns `(src, t, b)`.
    fn arb_test_program() -> impl Strategy<Value = (String, usize, usize)> {
        (1usize..5, 0usize..4).prop_map(|(t, b)| {
            let tests: String = (0..t)
                .map(|i| format!("@test fn xtest{i}() -> bool {{ return true }} "))
                .collect();
            let benches: String =
                (0..b).map(|i| format!("@bench fn xbench{i}() {{ }} ")).collect();
            let decoys = "fn xhelper() -> i32 { return 0 } \
                          @test fn xgen(comptime T: type) -> bool { return true }";
            (format!("{tests}{benches}{decoys}"), t, b)
        })
    }

    proptest! {
        /// **Discovery soundness + completeness.** `list_tests` names *exactly* the
        /// `t` tests and `b` benches — never the plain helper, never the generic
        /// `@test` decoy. Teeth: dropping the `is_generic_ast` guard in `list_tests`
        /// makes `xgen` appear and the count overshoots.
        #[test]
        fn discovery_matches_the_known_roster((src, t, b) in arb_test_program()) {
            let found = list(&src);
            prop_assert_eq!(found.len(), t + b, "roster size in {}", src);
            let n_tests = found.iter().filter(|(_, k)| *k == TestKind::Test).count();
            let n_bench = found.iter().filter(|(_, k)| *k == TestKind::Bench).count();
            prop_assert_eq!(n_tests, t);
            prop_assert_eq!(n_bench, b);
            prop_assert!(!found.iter().any(|(n, _)| n == "xhelper" || n == "xgen"),
                "a decoy was discovered: {:?}", found);
        }

        /// **Filter completeness.** The empty/`None` filter bakes every test — the
        /// banner count equals `t`. (And `None` ≡ `Some("")`.)
        #[test]
        fn the_unfiltered_count_is_the_test_count((src, t, _b) in arb_test_program()) {
            prop_assert_eq!(baked_count(&src, None), t, "in {}", src);
            prop_assert_eq!(baked_count(&src, Some("")), t);
        }

        /// **Filter soundness.** A substring shared by *all* test names (`xtest`)
        /// keeps every test; a substring matching *none* (`zzz`) bakes zero. Teeth:
        /// making the filter a no-op keeps the count at `t` for the `zzz` case.
        #[test]
        fn a_filter_selects_by_substring((src, t, _b) in arb_test_program()) {
            prop_assert_eq!(baked_count(&src, Some("xtest")), t, "shared substring keeps all");
            prop_assert_eq!(baked_count(&src, Some("zzz")), 0, "no-match bakes zero");
        }

        /// **Single-name selection.** Filtering by one exact test name `xtest{i}`
        /// bakes exactly one — the names are distinct by construction, so a
        /// substring equal to a full name matches only itself.
        #[test]
        fn an_exact_name_filter_bakes_exactly_one((src, t, _b) in arb_test_program()) {
            for i in 0..t {
                prop_assert_eq!(baked_count(&src, Some(&format!("xtest{i}"))), 1,
                    "exact name xtest{} in {}", i, src);
            }
        }

        /// **Determinism.** The filtered harness is byte-identical across runs — no
        /// `HashMap`/`HashSet` iteration order leaks through discovery or emission.
        #[test]
        fn filtered_harness_is_deterministic((src, _t, _b) in arb_test_program()) {
            let (ast, info) = typeck_full(&src);
            let a = cgen::emit_tests_filtered(&ast, &info, Some("xtest")).0;
            let b = cgen::emit_tests_filtered(&ast, &info, Some("xtest")).0;
            prop_assert_eq!(a, b);
        }
    }
}

/// Unit, wiring, and golden tests for `jestyrc attest` (workstream O, the headline).
/// The manifest is a pure function of the (checked) AST + emitted C, so these run
/// under the toolchain-free default `cargo test` — no C compiler needed (the C is
/// *hashed*, not built).
mod attest {
    use super::*;
    use crate::attest;

    /// Type-check a single-file source and build its manifest. (Single-file: no
    /// imports, so `src` *is* the span buffer the AST indexes into.)
    fn manifest(source_id: &str, src: &str) -> String {
        let (ast, info) = typeck_full(src);
        attest::manifest(source_id, src, &ast, &info)
    }

    /// The value of a `key ` header line (e.g. `c-sha256`), or `None` if absent.
    fn header<'a>(m: &'a str, key: &str) -> Option<&'a str> {
        m.lines().find_map(|l| l.strip_prefix(key).map(|r| r.trim()))
    }

    // ── unit: the manifest's shape and content ────────────────────────────────

    #[test]
    fn manifest_has_the_locked_header() {
        let m = manifest("t", "fn main() -> i32 { return 0 }");
        let mut lines = m.lines();
        assert_eq!(lines.next(), Some("jestyr-attest/v1"));
        assert_eq!(lines.next(), Some("source t"));
        let sha = lines.next().unwrap();
        assert!(sha.starts_with("c-sha256 "), "third line is the C hash: {sha}");
        // The locked compile command is recorded verbatim — the determinism seam.
        assert_eq!(
            lines.next(),
            Some("cc-flags -O2 -std=c11 -ffp-contract=off -fno-fast-math")
        );
    }

    #[test]
    fn the_c_hash_is_64_lowercase_hex_and_is_the_real_emitted_c() {
        let src = "fn main() -> i32 { return 0 }";
        let (ast, info) = typeck_full(src);
        let m = attest::manifest("t", src, &ast, &info);
        let sha = header(&m, "c-sha256").expect("a c-sha256 line");
        assert_eq!(sha.len(), 64, "sha is 64 hex chars: {sha}");
        assert!(sha.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        // It is exactly the SHA-256 of the C `build`/`run` would emit — the
        // attestation, cross-checked against an independent hash of the same bytes.
        let (c, _) = crate::cgen::emit(&ast, &info);
        assert_eq!(sha, crate::sha256::hex(c.as_bytes()));
    }

    #[test]
    fn a_functions_guarantees_are_reconstructed_from_the_ast() {
        // ensures + error set + @no_panic + a refined parameter — each must surface
        // as a `guarantee:` line, drawn from the same extractor the doc generator uses.
        let src = "@no_panic fn f(n: i32 in 0..10) -> i32 \
                   requires n >= 0 \
                   ensures result >= 0 \
                   { return n }";
        let m = manifest("t", src);
        let gs: Vec<&str> = m.lines().filter_map(|l| l.strip_prefix("  guarantee: ")).collect();
        assert!(gs.iter().any(|g| g.contains("@no_panic")), "no_panic: {gs:?}");
        assert!(gs.iter().any(|g| g.contains("requires n >= 0")), "requires: {gs:?}");
        assert!(gs.iter().any(|g| g.contains("ensures result >= 0")), "ensures: {gs:?}");
        assert!(gs.iter().any(|g| g.contains("constrained to `0..10`")), "refine: {gs:?}");
    }

    #[test]
    fn visibility_is_recorded_per_item() {
        let src = "pub fn shown() -> i32 { return 1 } fn hidden() -> i32 { return 2 }";
        let m = manifest("t", src);
        // The block order is sorted by (kind, name): hidden before shown.
        let blocks: Vec<&str> = m.split("\n\n").collect();
        let hidden = blocks.iter().find(|b| b.starts_with("fn hidden")).unwrap();
        let shown = blocks.iter().find(|b| b.starts_with("fn shown")).unwrap();
        assert!(hidden.contains("vis: priv"), "hidden is priv: {hidden}");
        assert!(shown.contains("vis: pub"), "shown is pub: {shown}");
    }

    #[test]
    fn items_are_sorted_by_kind_then_name() {
        let src = "fn zebra() -> i32 { return 0 } \
                   const APPLE: i32 = 1 \
                   fn alpha() -> i32 { return 0 } \
                   struct Box { n: i32 }";
        let m = manifest("t", src);
        let keys: Vec<&str> = m
            .lines()
            .filter(|l| !l.starts_with(' ') && !l.is_empty() && !l.contains(' ') == false)
            .filter(|l| {
                l.starts_with("fn ") || l.starts_with("const ") || l.starts_with("struct ")
            })
            .collect();
        assert_eq!(keys, ["const APPLE", "fn alpha", "fn zebra", "struct Box"], "{m}");
    }

    // ── golden: the full manifest on the shipped doc demo ─────────────────────

    #[test]
    fn docs_demo_manifest_is_pinned() {
        // The golden: every byte of `attest examples/docs.jtr` is fixed except the
        // hash, which is spliced from the live emitted C (so the structure, the
        // guarantee phrasing, the sort order, and the per-item visibility are all
        // locked, while the assertion stays robust to a deliberate codegen change —
        // which the separate hash cross-check above would catch).
        let src = std::fs::read_to_string("examples/docs.jtr").expect("read docs.jtr");
        let (ast, info) = typeck_full(&src);
        let (c, _) = crate::cgen::emit(&ast, &info);
        let hash = crate::sha256::hex(c.as_bytes());
        let expected = format!(
            "jestyr-attest/v1\n\
             source examples/docs.jtr\n\
             c-sha256 {hash}\n\
             cc-flags -O2 -std=c11 -ffp-contract=off -fno-fast-math\n\
             \n\
             const ANSWER\n  vis: priv\n  sig: const ANSWER: i32 = 42\n\
             \n\
             fn abs\n  vis: priv\n  sig: fn abs(x: i32) -> i32\n  \
             guarantee: `ensures result >= 0`\n\
             \n\
             fn add\n  vis: priv\n  sig: @no_panic fn add(a: i32, b: i32) -> i32\n  \
             guarantee: `@no_panic` — proven free of faulting operations\n\
             \n\
             fn at\n  vis: priv\n  sig: fn at(xs: []i32, i: usize in 0..xs.len) -> i32\n  \
             guarantee: parameter `i` is constrained to `0..xs.len`\n\
             \n\
             fn main\n  vis: priv\n  sig: fn main() -> i32\n\
             \n\
             fn set\n  vis: priv\n  sig: fn set(p: *mut i32, i: i32, v: i32)\n\
             \n\
             struct Counter\n  vis: priv\n  sig: struct Counter\n"
        );
        let got = attest::manifest("examples/docs.jtr", &src, &ast, &info);
        assert_eq!(got, expected, "manifest drift:\n{got}");
    }

    // ── wiring: plumbed through the loader (the path `jestyrc attest` runs) ────

    #[test]
    fn attest_runs_through_the_loader_pipeline() {
        // Drive the same `module::load → typeck → escape → manifest` chain the
        // `Mode::Attest` arm uses, on the real demo — confirming the subcommand is
        // wired to the live pipeline, not a parallel reimplementation.
        let prog = crate::module::load("examples/docs.jtr");
        assert!(prog.diags.is_empty(), "load diags: {:?}", prog.diags);
        let (info, td) = crate::typeck::check_program(&prog.ast, &prog.modules);
        assert!(td.is_empty(), "typeck: {:?}", td);
        assert!(crate::escape::check(&prog.ast, &info).is_empty());
        let src = attest::global_src(&prog.modules);
        let m = attest::manifest("examples/docs.jtr", &src, &prog.ast, &info);
        assert!(m.starts_with("jestyr-attest/v1\n"));
        assert!(m.contains("guarantee: `ensures result >= 0`"), "known guarantee present: {m}");
        assert_eq!(header(&m, "c-sha256").unwrap().len(), 64);
    }
}

/// Property + fuzz tests for `jestyrc attest`. The on-thesis invariants:
/// **determinism** (the manifest, hash and all, is byte-identical every run),
/// **soundness of the hash** (it is exactly the emitted-C digest), and
/// **completeness** (every top-level item appears, with its full guarantee set).
mod attest_props {
    use super::*;
    use crate::attest;
    use proptest::prelude::*;

    proptest! {
        /// **Determinism (the headline).** `attest` of the same source twice is
        /// byte-identical — manifest *and* the C hash inside it. A search for any
        /// `HashMap`/`HashSet` iteration-order leak in record collection or codegen.
        #[test]
        fn attest_is_deterministic(p in super::prop::arb_program()) {
            let (ast, info) = typeck_full(&p);
            let a = attest::manifest("p", &p, &ast, &info);
            let b = attest::manifest("p", &p, &ast, &info);
            prop_assert_eq!(a, b);
        }

        /// **Hash soundness.** The manifest's `c-sha256` is exactly the SHA-256 of
        /// the C `build` emits — never a stale or unrelated digest. Teeth: hashing
        /// anything but `cgen::emit(ast,info)` makes this fail.
        #[test]
        fn the_manifest_hash_is_the_emitted_c_digest(p in super::prop::arb_program()) {
            let (ast, info) = typeck_full(&p);
            let m = attest::manifest("p", &p, &ast, &info);
            let sha = m.lines().find_map(|l| l.strip_prefix("c-sha256 ")).unwrap();
            let (c, _) = cgen::emit(&ast, &info);
            prop_assert_eq!(sha, crate::sha256::hex(c.as_bytes()));
        }

        /// **Completeness.** Every top-level `fn`/`struct`/`enum` the generator
        /// emitted appears as a manifest record (`<kind> <name>` key line). The
        /// generator's `arb_program` builds `struct S`, `enum E`, and `fn f`.
        #[test]
        fn every_item_is_attested(p in super::prop::arb_program()) {
            let (ast, info) = typeck_full(&p);
            let m = attest::manifest("p", &p, &ast, &info);
            prop_assert!(m.contains("\nstruct S\n"), "struct S missing:\n{}", m);
            prop_assert!(m.contains("\nenum E\n"), "enum E missing:\n{}", m);
            prop_assert!(m.contains("\nfn f\n"), "fn f missing:\n{}", m);
        }

        /// **Guarantee fidelity.** Each fn's `guarantee:` lines are exactly
        /// `doc::fn_guarantees` — the attested ABI cannot drift from the doc
        /// generator. (Oracle: the shared extractor, over a contract-rich program.)
        #[test]
        fn guarantee_count_matches_the_doc_extractor(
            (np, req, ens) in (any::<bool>(), any::<bool>(), any::<bool>()),
        ) {
            let np_s = if np { "@no_panic " } else { "" };
            let req_s = if req { "requires n >= 0 " } else { "" };
            let ens_s = if ens { "ensures result >= 0 " } else { "" };
            let src = format!("{np_s}fn f(n: i32) -> i32 {req_s}{ens_s}{{ return n }}");
            let (ast, info) = typeck_full(&src);
            let m = attest::manifest("p", &src, &ast, &info);
            let got = m.lines().filter(|l| l.starts_with("  guarantee: ")).count();
            prop_assert_eq!(got, np as usize + req as usize + ens as usize, "in {}", src);
        }
    }
}

/// Unit, golden, wiring, and property tests for `jestyrc attest --diff` (workstream
/// O, increment 3 — the sound breaking-change detector). The oracle is known by
/// construction: a base program is mutated by one contract edit whose verdict we
/// know, and the diff must report exactly that change with that verdict. All
/// pure-Rust (it diffs manifest *text*), so toolchain-free.
mod attest_diff {
    use super::*;
    use crate::attest::{self, Verdict};

    /// Attest a single-file source to its manifest text (the `jestyrc attest` output).
    fn attest_src(id: &str, src: &str) -> String {
        let (ast, info) = typeck_full(src);
        attest::manifest(id, src, &ast, &info)
    }

    /// Diff two sources by attesting each and classifying — the full `--diff` path.
    fn diff(old_src: &str, new_src: &str) -> attest::DiffReport {
        let om = attest::parse_manifest(&attest_src("old", old_src)).expect("old parses");
        let nm = attest::parse_manifest(&attest_src("new", new_src)).expect("new parses");
        attest::diff(&om, &nm)
    }

    /// The single change's `(verdict, detail)` — panics unless there is exactly one,
    /// which is the point: a one-edit mutation must produce a one-change diff.
    /// Returns owned strings so callers can pass a temporary `diff(...)`.
    fn sole(report: &attest::DiffReport) -> (Verdict, String) {
        assert_eq!(report.changes.len(), 1, "expected exactly one change: {}", report.render());
        let c = &report.changes[0];
        (c.verdict, c.detail.clone())
    }

    // ── unit: round-trip the manifest parser ──────────────────────────────────

    #[test]
    fn parse_round_trips_guarantees() {
        let src = "@no_panic pub fn f(n: i32 in 0..10) -> i32 !{ Bad } \
                   requires n >= 0 ensures result >= 0 { return n }";
        let m = attest::parse_manifest(&attest_src("t", src)).unwrap();
        let it = m.items.get("fn f").expect("fn f present");
        assert!(it.is_pub);
        assert!(it.no_panic);
        assert!(it.requires.contains("n >= 0"), "{:?}", it.requires);
        assert!(it.ensures.contains("result >= 0"), "{:?}", it.ensures);
        assert!(it.errors.contains("Bad"), "{:?}", it.errors);
        assert_eq!(it.refines.get("n").map(String::as_str), Some("0..10"));
    }

    // ── unit: each classification rule, one edit at a time ────────────────────

    const BASE: &str = "pub fn f(n: i32) -> i32 { return n }";

    #[test]
    fn an_added_error_is_breaking() {
        let new = "pub fn f(n: i32) -> i32 !{ Overflow } { return n }";
        let (v, d) = sole(&diff(BASE, new));
        assert_eq!(v, Verdict::Breaking);
        assert!(d.contains("error added `Overflow`"), "{d}");
    }

    #[test]
    fn a_removed_error_is_compatible() {
        let old = "pub fn f(n: i32) -> i32 !{ Overflow } { return n }";
        let (v, d) = sole(&diff(old, BASE));
        assert_eq!(v, Verdict::Compatible);
        assert!(d.contains("error removed `Overflow`"), "{d}");
    }

    #[test]
    fn a_strengthened_requires_is_breaking() {
        let new = "pub fn f(n: i32) -> i32 requires n >= 0 { return n }";
        let (v, d) = sole(&diff(BASE, new));
        assert_eq!(v, Verdict::Breaking);
        assert!(d.contains("`requires n >= 0` added"), "{d}");
    }

    #[test]
    fn a_dropped_requires_is_compatible() {
        let old = "pub fn f(n: i32) -> i32 requires n >= 0 { return n }";
        let (v, _d) = sole(&diff(old, BASE));
        assert_eq!(v, Verdict::Compatible);
    }

    #[test]
    fn an_added_ensures_is_compatible() {
        let new = "pub fn f(n: i32) -> i32 ensures result >= 0 { return n }";
        let (v, d) = sole(&diff(BASE, new));
        assert_eq!(v, Verdict::Compatible);
        assert!(d.contains("`ensures result >= 0` added"), "{d}");
    }

    #[test]
    fn a_dropped_ensures_is_breaking() {
        let old = "pub fn f(n: i32) -> i32 ensures result >= 0 { return n }";
        let (v, _d) = sole(&diff(old, BASE));
        assert_eq!(v, Verdict::Breaking);
    }

    #[test]
    fn losing_no_panic_is_breaking() {
        let old = "@no_panic pub fn f(n: i32) -> i32 { return n }";
        let (v, d) = sole(&diff(old, BASE));
        assert_eq!(v, Verdict::Breaking);
        assert!(d.contains("lost `@no_panic`"), "{d}");
    }

    #[test]
    fn gaining_no_panic_is_compatible() {
        let new = "@no_panic pub fn f(n: i32) -> i32 { return n }";
        let (v, _d) = sole(&diff(BASE, new));
        assert_eq!(v, Verdict::Compatible);
    }

    #[test]
    fn narrowing_a_refinement_is_breaking() {
        let old = "pub fn f(n: i32 in 0..100) -> i32 { return n }";
        let new = "pub fn f(n: i32 in 1..100) -> i32 { return n }";
        let (v, d) = sole(&diff(old, new));
        assert_eq!(v, Verdict::Breaking);
        assert!(d.contains("narrowed `0..100` → `1..100`"), "{d}");
    }

    #[test]
    fn widening_a_refinement_is_compatible() {
        let old = "pub fn f(n: i32 in 1..100) -> i32 { return n }";
        let new = "pub fn f(n: i32 in 0..1000) -> i32 { return n }";
        let (v, d) = sole(&diff(old, new));
        assert_eq!(v, Verdict::Compatible);
        assert!(d.contains("widened `1..100` → `0..1000`"), "{d}");
    }

    #[test]
    fn a_non_literal_refinement_change_is_conservatively_breaking() {
        // Can't prove `0..xs.len ⊇ 0..n`, so the sound verdict is breaking.
        let old = "pub fn f(xs: []i32, i: usize in 0..xs.len) -> i32 { return xs[i] }";
        let new = "pub fn f(xs: []i32, i: usize in 0..i) -> i32 { return xs[i] }";
        let report = diff(old, new);
        assert!(
            report.changes.iter().any(|c| c.verdict == Verdict::Breaking && c.detail.contains("narrowed")),
            "non-literal refinement change must be breaking: {}", report.render()
        );
    }

    #[test]
    fn removing_a_pub_item_is_breaking_but_a_private_one_is_not() {
        let old = "pub fn a() -> i32 { return 0 } fn b() -> i32 { return 0 }";
        // remove both a (pub) and b (priv); add nothing.
        let new = "fn keep() -> i32 { return 0 }";
        let report = diff(old, new);
        let a = report.changes.iter().find(|c| c.item == "fn a").expect("fn a change");
        let b = report.changes.iter().find(|c| c.item == "fn b").expect("fn b change");
        assert_eq!(a.verdict, Verdict::Breaking, "pub removal: {}", a.detail);
        assert_eq!(b.verdict, Verdict::Compatible, "priv removal: {}", b.detail);
    }

    #[test]
    fn a_return_type_change_is_breaking() {
        let old = "pub fn f(n: i32) -> i32 { return n }";
        let new = "pub fn f(n: i32) -> i64 { return n }";
        let (v, d) = sole(&diff(old, new));
        assert_eq!(v, Verdict::Breaking);
        assert!(d.contains("signature changed"), "{d}");
    }

    #[test]
    fn losing_no_panic_reports_once_not_also_as_a_sig_change() {
        // Regression: `@no_panic` is in the `sig:` line too; `sig_core` must strip it
        // so the loss is reported exactly once (not also as "signature changed").
        let old = "@no_panic pub fn f(n: i32) -> i32 { return n }";
        let (v, d) = sole(&diff(old, BASE));
        assert_eq!(v, Verdict::Breaking);
        assert!(d.contains("@no_panic"), "the single change names @no_panic: {d}");
    }

    // ── golden + exit-gate: a multi-edit report, pinned ───────────────────────

    #[test]
    fn a_mixed_diff_report_is_pinned_and_gates() {
        let old = "pub fn parse(s: i32) -> i32 requires s >= 0 { return s } \
                   @no_panic pub fn push(x: i32) -> i32 { return x }";
        // parse: +error (breaking) and -requires (compatible); push: -@no_panic (breaking).
        let new = "pub fn parse(s: i32) -> i32 !{ Overflow } { return s } \
                   pub fn push(x: i32) -> i32 { return x }";
        let report = diff(old, new);
        assert!(report.has_breaking(), "must gate (exit non-zero)");
        assert_eq!((report.breaking(), report.compatible()), (2, 1));
        // The body lines, sorted by (item, verdict, detail) — deterministic.
        let rendered = report.render();
        let body: Vec<&str> = rendered
            .lines()
            .filter(|l| l.starts_with("BREAKING") || l.starts_with("compatible"))
            .collect();
        assert_eq!(
            body,
            [
                "BREAKING    fn parse  error added `Overflow`",
                "compatible  fn parse  `requires s >= 0` removed",
                "BREAKING    fn push  lost `@no_panic`",
            ]
        );
    }

    // ── wiring: identical manifests → zero changes ────────────────────────────

    #[test]
    fn a_manifest_diffed_against_itself_has_no_changes() {
        let m = attest::parse_manifest(&attest_src("t", BASE)).unwrap();
        let report = attest::diff(&m, &m);
        assert!(report.changes.is_empty(), "self-diff must be empty: {}", report.render());
        assert!(!report.has_breaking());
    }
}

/// Property + fuzz tests for `attest --diff`. The on-thesis invariants: a manifest
/// vs itself yields **zero** changes (reflexivity), and any single contract mutation
/// yields **exactly one** correctly-classified change (soundness + sharpness).
mod attest_diff_props {
    use super::*;
    use crate::attest::{self, Verdict};
    use proptest::prelude::*;

    fn parsed(src: &str) -> attest::ParsedManifest {
        let (ast, info) = typeck_full(src);
        attest::parse_manifest(&attest::manifest("p", src, &ast, &info)).expect("parses")
    }

    /// A base program (`f` over `t`) plus the same program after one contract edit,
    /// paired with that edit's known verdict. Each arm changes exactly one clause.
    fn arb_one_edit() -> impl Strategy<Value = (String, String, Verdict)> {
        let prim = prop_oneof![Just("i32"), Just("i64"), Just("u8")];
        (prim, 0usize..6).prop_map(|(t, which)| {
            let base = format!("pub fn f(n: {t}) -> {t} {{ return n }}");
            let (new, verdict) = match which {
                0 => (format!("pub fn f(n: {t}) -> {t} !{{ E }} {{ return n }}"), Verdict::Breaking),
                1 => (format!("pub fn f(n: {t}) -> {t} requires n >= 0 {{ return n }}"), Verdict::Breaking),
                2 => (format!("@no_panic pub fn f(n: {t}) -> {t} {{ return n }}"), Verdict::Compatible),
                3 => (format!("pub fn f(n: {t}) -> {t} ensures result >= 0 {{ return n }}"), Verdict::Compatible),
                4 => (format!("pub fn f(n: {t} in 0..10) -> {t} {{ return n }}"), Verdict::Breaking),
                _ => (format!("fn f(n: {t}) -> {t} {{ return n }}"), Verdict::Breaking), // pub -> priv
            };
            (base, new, verdict)
        })
    }

    proptest! {
        /// **Reflexivity.** A manifest diffed against itself has zero changes — for
        /// every generated program. Teeth: any spurious change (e.g. comparing
        /// unordered sets by sequence) breaks this.
        #[test]
        fn self_diff_is_empty(p in super::prop::arb_program()) {
            let m = parsed(&p);
            let report = attest::diff(&m, &m);
            prop_assert!(report.changes.is_empty(), "self-diff not empty:\n{}", report.render());
        }

        /// **Sharpness + soundness.** One contract edit yields exactly one change,
        /// with the verdict the generator knows by construction. Teeth: flipping any
        /// rule's verdict (e.g. error-add → compatible) fails for that arm.
        #[test]
        fn one_edit_yields_one_correctly_classified_change(
            (old, new, want) in arb_one_edit(),
        ) {
            let report = attest::diff(&parsed(&old), &parsed(&new));
            prop_assert_eq!(report.changes.len(), 1, "edit:\n{} ->\n{}\n{}", old, new, report.render());
            prop_assert_eq!(report.changes[0].verdict, want, "{}", report.render());
        }

        /// **Direction asymmetry.** Swapping old/new flips every verdict — a change
        /// breaking forwards is compatible backwards and vice-versa (no rule is
        /// accidentally symmetric). True for the contract rules the generator covers.
        #[test]
        fn swapping_old_and_new_flips_the_verdict((old, new, want) in arb_one_edit()) {
            let rev = attest::diff(&parsed(&new), &parsed(&old));
            prop_assert_eq!(rev.changes.len(), 1);
            let flipped = match want {
                Verdict::Breaking => Verdict::Compatible,
                Verdict::Compatible => Verdict::Breaking,
            };
            prop_assert_eq!(rev.changes[0].verdict, flipped, "{}", rev.render());
        }
    }
}

/// Experiment (feature `dharht-experiment`): the strongest "can D-HARHT replace a
/// `HashMap`?" check — a sealed D-HARHT (Memory profile) must agree with a HashMap
/// on every key, across random key sets.
#[cfg(feature = "dharht-experiment")]
mod dharht_experiment {
    use crate::dharht::{DHarht, LookupProfile};
    use proptest::prelude::*;
    use std::collections::HashMap;

    proptest! {
        #[test]
        fn dharht_memory_matches_hashmap(keys in proptest::collection::vec(any::<u64>(), 0..300)) {
            let mut hm: HashMap<u64, u64> = HashMap::new();
            let mut dh: DHarht<u64> = DHarht::new(16);
            dh.set_lookup_profile(LookupProfile::Memory);
            for (i, &k) in keys.iter().enumerate() {
                hm.insert(k, i as u64); // last write wins, both
                dh.insert(k, i as u64);
            }
            dh.seal_for_lookup();
            for &k in &keys {
                prop_assert_eq!(dh.get(k).copied(), hm.get(&k).copied());
            }
            for probe in [u64::MAX, u64::MAX / 2, 0xdead_beef_u64] {
                if !hm.contains_key(&probe) {
                    prop_assert_eq!(dh.get(probe), None);
                }
            }
        }
    }
}

/// Per-module namespaces (modules-v2, increment 1): the property layer over
/// *multi-module* programs — resolution soundness, namespace isolation, and
/// determinism. Unlike the single-source `mod prop` helpers, these drive the real
/// loader (`module::load`) over a small temp directory per case.
mod modules_props {
    use super::*;
    use proptest::prelude::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static CASE: AtomicU64 = AtomicU64::new(0);

    /// Run load → type-check → escape → lower over a materialized dir, returning
    /// (all diagnostic messages, emitted C). Pulled out of [`pipeline_multi`] so a
    /// determinism check can build *twice from the same directory*: the emitted C
    /// now carries `#line` directives naming the loaded file paths (debug info), so
    /// two builds are byte-identical only when they share a path — exactly as a C
    /// toolchain bakes the path you pass it. The per-call unique dir of
    /// `pipeline_multi` is the right default for a single build; determinism
    /// comparisons use [`pipeline_multi_twice`] to hold the path fixed.
    fn compile_dir(dir: &std::path::Path) -> (Vec<String>, String) {
        let prog = crate::module::load(dir.join("main.jtr").to_str().unwrap());
        let mut diags: Vec<String> = prog.diags.iter().map(|d| d.message.clone()).collect();
        let (info, td) = typeck::check_program(&prog.ast, &prog.modules);
        diags.extend(td.iter().map(|d| d.message.clone()));
        diags.extend(escape::check(&prog.ast, &info).iter().map(|d| d.message.clone()));
        let (c, cd) = cgen::emit(&prog.ast, &info);
        diags.extend(cd.iter().map(|d| d.message.clone()));
        (diags, c)
    }

    /// Materialize `files` into a fresh, uniquely-named temp dir.
    fn materialize(files: &[(String, String)]) -> std::path::PathBuf {
        let id = CASE.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("jestyr_modprop_{id:016x}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for (name, src) in files {
            let path = dir.join(name);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(path, src).unwrap();
        }
        dir
    }

    /// Write `files` to a fresh, uniquely-named temp dir, compile, and return
    /// (all diagnostic messages, emitted C). The dir is removed afterwards.
    fn pipeline_multi(files: &[(String, String)]) -> (Vec<String>, String) {
        let dir = materialize(files);
        let out = compile_dir(&dir);
        let _ = std::fs::remove_dir_all(&dir);
        out
    }

    /// Compile the same materialized program **twice from one directory**, so the
    /// path baked into `#line` is held fixed — the apples-to-apples determinism
    /// comparison now that the emitted C is a function of the loaded path. Returns
    /// (first build's diagnostics, first C, second C).
    fn pipeline_multi_twice(files: &[(String, String)]) -> (Vec<String>, String, String) {
        let dir = materialize(files);
        let (diags, a) = compile_dir(&dir);
        let b = compile_dir(&dir).1;
        let _ = std::fs::remove_dir_all(&dir);
        (diags, a, b)
    }

    /// `n` (2..=3) sibling modules, each defining `pub fn f` and a private
    /// `helper` returning a distinct constant, plus a root that calls each `f`
    /// **qualified**. The names collide across modules by construction — exactly
    /// what v1's flat pool forbade.
    fn arb_multimod() -> impl Strategy<Value = Vec<(String, String)>> {
        proptest::collection::vec(0i32..1000, 2..=3).prop_map(|vals| {
            let mut files = Vec::new();
            let mut imports = String::new();
            let mut sum = String::new();
            for (k, v) in vals.iter().enumerate() {
                let name = format!("m{k}");
                files.push((
                    format!("{name}.jtr"),
                    format!("pub fn f() -> i32 {{ return helper() }}\nfn helper() -> i32 {{ return {v} }}"),
                ));
                imports.push_str(&format!("import \"{name}\"\n"));
                if k > 0 {
                    sum.push_str(" + ");
                }
                sum.push_str(&format!("{name}.f()"));
            }
            files.insert(0, ("main.jtr".to_string(), format!("{imports}fn main() -> i32 {{ return {sum} }}")));
            files
        })
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 48, ..ProptestConfig::default() })]

        /// **Namespace isolation + soundness.** Several modules each defining `f`
        /// (and a private `helper`) compile cleanly, and every module's `f`/`helper`
        /// gets its own C symbol — never a silent cross-module collision.
        #[test]
        fn same_named_items_across_modules_get_distinct_symbols(files in arb_multimod()) {
            let (diags, c) = pipeline_multi(&files);
            prop_assert!(diags.is_empty(), "clean multi-module compile: {:?}", diags);
            // Root is module 0; the k-th sibling (1-indexed) is module k.
            let n = files.len() - 1;
            for k in 1..=n {
                prop_assert!(c.contains(&format!("jestyr_f__m{k}(void)")), "f__m{k} missing:\n{c}");
                prop_assert!(c.contains(&format!("jestyr_helper__m{k}(void)")), "helper__m{k} missing:\n{c}");
            }
        }

        /// **Determinism.** The same multi-module program lowers to byte-identical C
        /// (built twice from one directory, so the `#line` paths are held fixed).
        #[test]
        fn multimodule_compilation_is_deterministic(files in arb_multimod()) {
            let (_diags, a, b) = pipeline_multi_twice(&files);
            prop_assert_eq!(a, b);
        }

        /// **Negative soundness.** Calling a sibling's `f` *unqualified* from the
        /// root never resolves silently — it is an unresolved-name error.
        #[test]
        fn unqualified_sibling_call_never_resolves(files in arb_multimod()) {
            let mut files = files;
            let imports: String = files[1..]
                .iter()
                .map(|(n, _)| format!("import \"{}\"\n", n.trim_end_matches(".jtr")))
                .collect();
            files[0].1 = format!("{imports}fn main() -> i32 {{ return f() }}");
            let (diags, _c) = pipeline_multi(&files);
            prop_assert!(
                diags.iter().any(|d| d.contains("cannot find `f` in this module")),
                "an unqualified cross-module `f` must error: {:?}",
                diags
            );
        }

        /// **Qualified type paths (`mod.Type`) resolve + lower + are deterministic.**
        /// `n` modules each export a distinct `pub struct T<k>`; the root references
        /// each via `m<k>.T<k>` in a signature. Compiles cleanly, every type lowers
        /// to its C struct, and the emitted C is reproducible.
        #[test]
        fn qualified_type_paths_resolve_and_lower(k in 2usize..=4) {
            let mut files = Vec::new();
            let mut imports = String::new();
            let mut fns = String::new();
            for j in 0..k {
                files.push((format!("m{j}.jtr"), format!("pub struct T{j} {{ pub a: i32 }}")));
                imports.push_str(&format!("import \"m{j}\"\n"));
                fns.push_str(&format!("fn use{j}(p: m{j}.T{j}) -> i32 {{ return p.a }}\n"));
            }
            files.insert(0, ("main.jtr".to_string(), format!("{imports}{fns}fn main() -> i32 {{ return 0 }}")));
            let (diags, c, c2) = pipeline_multi_twice(&files);
            prop_assert!(diags.is_empty(), "qualified type paths compile cleanly: {:?}", diags);
            for j in 0..k {
                prop_assert!(c.contains(&format!("jestyr_use{j}(Jestyr_T{j}")), "T{j} lowered:\n{c}");
            }
            prop_assert_eq!(c2, c);
        }

        /// **Directory-as-module is one shared namespace + deterministic.** A `pkg/`
        /// of `k` files, each defining a `pub fn f<j>` that the next references
        /// *unqualified*, plus a root importing `pkg`. They share a namespace (no
        /// qualification needed between files), compile cleanly, and the merged
        /// module is order-independent — the same program lowers to identical C.
        #[test]
        fn directory_is_a_deterministic_shared_namespace(k in 2usize..=4) {
            let mut files = Vec::new();
            for j in 0..k {
                // f0 returns a constant; f<j> (j>0) calls f<j-1> — a cross-file
                // unqualified call, only valid because the package shares a namespace.
                let body = if j == 0 {
                    format!("pub fn f0() -> i32 {{ return 1 }}")
                } else {
                    format!("pub fn f{j}() -> i32 {{ return f{} () + 1 }}", j - 1)
                };
                files.push((format!("pkg/g{j}.jtr"), body));
            }
            files.push(("main.jtr".to_string(), format!("import \"pkg\"\nfn main() -> i32 {{ return pkg.f{}() }}", k - 1)));
            let (diags, c, c2) = pipeline_multi_twice(&files);
            prop_assert!(diags.is_empty(), "package files share a namespace and compile: {:?}", diags);
            // One module for the whole package: every f<j> emits a bare symbol.
            for j in 0..k {
                prop_assert!(c.contains(&format!("jestyr_f{j}(void)")), "f{j} emitted:\n{c}");
            }
            prop_assert_eq!(c2, c);
        }

        /// **Module content-hash: comment/whitespace-insensitive, deterministic, and
        /// semantics-sensitive.** The hash is over the normalized post-parse form, so
        /// a comment/whitespace-only edit leaves it unchanged and is reproducible,
        /// while a changed literal changes it.
        #[test]
        fn module_hash_is_normalized_deterministic_and_semantic(v in 0i32..10_000) {
            let plain = format!("fn main() -> i32 {{ return {v} }}");
            let noisy = format!("// lead\nfn   main ()  -> i32 {{\n    return   {v}  // trail\n}}");
            let other = format!("fn main() -> i32 {{ return {} }}", v + 1);
            let hp = single_hash(&plain);
            prop_assert_eq!(&hp, &single_hash(&noisy), "comment/whitespace edit must not change the hash");
            prop_assert_eq!(&hp, &single_hash(&plain), "the hash is deterministic");
            prop_assert_ne!(&hp, &single_hash(&other), "a semantic edit must change the hash");
            prop_assert_eq!(hp.len(), 64, "a sha256 hex digest");
        }

        /// **Same-named types across modules get distinct C symbols.** `k` modules
        /// each define `struct T` (+ a constructor and accessor); the root uses each
        /// via `m<j>.T`. They compile cleanly, each lowers to a distinct
        /// `Jestyr_T__m<id>` (never the bare `Jestyr_T`), and output is deterministic.
        #[test]
        fn same_named_types_across_modules_get_distinct_symbols(k in 2usize..=4) {
            let mut files = Vec::new();
            let mut imports = String::new();
            let mut uses = String::new();
            let mut body = String::from("fn main() -> i32 { return 0");
            for j in 0..k {
                files.push((format!("m{j}.jtr"),
                    format!("pub struct T {{ pub v: i32 }}\npub fn mk() -> T {{ return T {{ v: {j} }} }}\npub fn val(s: T) -> i32 {{ return s.v }}")));
                imports.push_str(&format!("import \"m{j}\"\n"));
                uses.push_str(&format!("fn u{j}(s: m{j}.T) -> i32 {{ return m{j}.val(s) }}\n"));
                body.push_str(&format!(" + u{j}(m{j}.mk())"));
            }
            body.push_str(" }");
            files.insert(0, ("main.jtr".to_string(), format!("{imports}{uses}{body}")));
            let (diags, c, c2) = pipeline_multi_twice(&files);
            prop_assert!(diags.is_empty(), "collidable `T` compiles: {:?}", diags);
            // Module ids are 1..=k (main is 0); each `T` is disambiguated by id.
            for id in 1..=k {
                prop_assert!(c.contains(&format!("Jestyr_T__m{id}")), "T__m{id} present:\n{c}");
            }
            prop_assert!(!c.contains("struct Jestyr_T "), "the bare `Jestyr_T` must not appear:\n{c}");
            prop_assert_eq!(c2, c);
        }

        /// **Pinned-hash verification round-trips.** Pinning an import to the
        /// dependency's *computed* hash verifies clean; any other pin errors — the
        /// lockfile-lite reproducibility guarantee, over varied dependencies.
        #[test]
        fn pinning_the_computed_hash_verifies_else_errors(v in 0i32..10_000) {
            let lib = format!("pub fn f() -> i32 {{ return {v} }}");
            // Learn lib's hash from an unpinned load (module order: main=0, lib=1).
            let probe = vec![
                ("main.jtr".to_string(), "import \"lib\"\nfn main() -> i32 { return lib.f() }".to_string()),
                ("lib.jtr".to_string(), lib.clone()),
            ];
            let h = pipeline_multi_load(&probe).hashes[1].clone();
            let good = vec![
                ("main.jtr".to_string(), format!("import \"lib\" = \"{h}\"\nfn main() -> i32 {{ return lib.f() }}")),
                ("lib.jtr".to_string(), lib.clone()),
            ];
            let (gd, _) = pipeline_multi(&good);
            prop_assert!(!gd.iter().any(|d| d.contains("hash mismatch")), "correct pin verifies: {:?}", gd);
            let wrong = format!("{:0>64}", v + 1); // a 64-char string that won't be the real hash
            let bad = vec![
                ("main.jtr".to_string(), format!("import \"lib\" = \"{wrong}\"\nfn main() -> i32 {{ return lib.f() }}")),
                ("lib.jtr".to_string(), lib),
            ];
            let (bd, _) = pipeline_multi(&bad);
            prop_assert!(bd.iter().any(|d| d.contains("hash mismatch")), "wrong pin errors: {:?}", bd);
        }
    }

    /// The single-module content hash of a source string (in-memory; no loader).
    fn single_hash(src: &str) -> String {
        let (tokens, _) = Lexer::new(src).tokenize();
        let (ast, _) = Parser::new(src, tokens).parse();
        crate::module::Modules::single(&ast).hashes.first().cloned().unwrap_or_default()
    }

    /// Like `pipeline_multi` but returns the loaded `Modules` (for its hashes).
    fn pipeline_multi_load(files: &[(String, String)]) -> crate::module::Modules {
        let id = CASE.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("jestyr_modprobe_{id:016x}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for (name, src) in files {
            let path = dir.join(name);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(path, src).unwrap();
        }
        let m = crate::module::load(dir.join("main.jtr").to_str().unwrap()).modules;
        let _ = std::fs::remove_dir_all(&dir);
        m
    }
}

/// Concurrency sync primitives (workstream N, increment 1): the Mutex protected
/// object. Two layers here — a toolchain-free wiring check that the shipped demo
/// lowers cleanly through the *real* module pipeline, and a pure-Rust model of the
/// test-and-set spinlock proving mutual exclusion holds under **any** interleaving
/// (the on-thesis property: the result is schedule-independent). The actual
/// thread-run proof lives in `c_oracle::mutex_demo` (gated behind `c-oracle`).
mod sync_props {
    use super::*;
    use proptest::prelude::*;

    /// Load → typeck → escape → lower a real example file through the module
    /// loader, returning every diagnostic message (mirrors `module::pipeline_is_clean`
    /// without touching that file).
    fn example_diags(rel: &str) -> Vec<String> {
        let prog = crate::module::load(rel);
        let mut diags: Vec<String> = prog.diags.iter().map(|d| d.message.clone()).collect();
        let (info, td) = typeck::check_program(&prog.ast, &prog.modules);
        diags.extend(td.iter().map(|d| d.message.clone()));
        diags.extend(escape::check(&prog.ast, &info).iter().map(|d| d.message.clone()));
        let (_c, cd) = cgen::emit(&prog.ast, &info);
        diags.extend(cd.iter().map(|d| d.message.clone()));
        diags
    }

    /// **Wiring (toolchain-free).** The shipped Mutex demo — which `import`s
    /// `sync.jtr`, builds a `Mutex(i64)` protected object, and shares it across a
    /// `concurrent { spawn … }` nursery — lowers with zero diagnostics. This proves
    /// a `read Mutex(T)` spawn argument is *accepted* by the escape checker (the
    /// protected object is the sanctioned sharing path), while the existing
    /// `mut`-slice spawn rule stays in force (see `escape` tests).
    #[test]
    fn mutex_example_compiles_clean() {
        let diags = example_diags("examples/std/mutex.jtr");
        assert!(diags.is_empty(), "examples/std/mutex.jtr: {diags:?}");
    }

    /// A model of the emitted test-and-set spinlock + guarded counter. `n` threads
    /// each perform `k` increments; each increment is the four-step critical region
    /// the lowering produces — acquire (TAS the lock word), read the counter, add
    /// one, write it back — then release. A generated `schedule` drives an arbitrary
    /// interleaving of *ready* steps; spinning threads make no progress until the
    /// holder releases. After replaying the schedule we deterministically drain to
    /// completion. The invariant: the final counter is **exactly `n*k`**, for every
    /// interleaving — no lost updates, the mutual-exclusion guarantee.
    fn run_locked_model(n: usize, k: usize, schedule: &[usize], lock_enabled: bool) -> i64 {
        // Per-thread critical-section progress: 0 = need lock, 1 = loaded, 2 = added,
        // 3 = holding, ready to store+release. `reg` is the thread's private copy.
        let mut phase = vec![0u8; n];
        let mut reg = vec![0i64; n];
        let mut iters = vec![k; n];
        let mut lock: i64 = 0; // 0 = free, 1 = held
        let mut counter: i64 = 0;

        // Step thread `t` if it can make progress; return true if it did.
        let step = |t: usize,
                    phase: &mut [u8],
                    reg: &mut [i64],
                    iters: &mut [usize],
                    lock: &mut i64,
                    counter: &mut i64|
         -> bool {
            if iters[t] == 0 {
                return false;
            }
            match phase[t] {
                0 => {
                    // Test-and-set acquire. With the lock disabled (teeth), always
                    // "acquire" so critical sections can interleave and race.
                    if lock_enabled {
                        if *lock != 0 {
                            return false; // spinning — no progress
                        }
                        *lock = 1;
                    }
                    reg[t] = *counter; // read under the lock
                    phase[t] = 1;
                    true
                }
                1 => {
                    reg[t] += 1; // compute
                    phase[t] = 2;
                    true
                }
                2 => {
                    *counter = reg[t]; // write back
                    phase[t] = 3;
                    true
                }
                _ => {
                    if lock_enabled {
                        *lock = 0; // release
                    }
                    phase[t] = 0;
                    iters[t] -= 1;
                    true
                }
            }
        };

        for &raw in schedule {
            let t = raw % n;
            let _ = step(t, &mut phase, &mut reg, &mut iters, &mut lock, &mut counter);
        }
        // Drain any unfinished work to completion (the schedule may be short).
        let mut guard = 0;
        loop {
            let mut progressed = false;
            for t in 0..n {
                if step(t, &mut phase, &mut reg, &mut iters, &mut lock, &mut counter) {
                    progressed = true;
                }
            }
            if iters.iter().all(|&i| i == 0) {
                break;
            }
            guard += 1;
            assert!(guard < 1_000_000, "model failed to converge");
            let _ = progressed;
        }
        counter
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]

        /// **Mutual exclusion is schedule-independent.** For any thread count,
        /// per-thread increment count, and interleaving, the lock serializes the
        /// read-modify-writes so the guarded counter ends at exactly `n*k`.
        #[test]
        fn tas_lock_serializes_increments(
            n in 2usize..6,
            k in 1usize..16,
            schedule in proptest::collection::vec(0usize..6, 0..400),
        ) {
            let got = run_locked_model(n, k, &schedule, true);
            prop_assert_eq!(got, (n * k) as i64, "lock must lose no updates");
        }
    }

    /// **Teeth.** Disable the lock and there exists an interleaving that loses
    /// updates — proving the property above is enforced by the lock, not by the
    /// model's structure. The adversarial schedule reads every thread, then adds,
    /// then writes: all writes clobber to 1 though two increments were intended.
    #[test]
    fn unlocked_increments_lose_updates() {
        // 2 threads, 1 increment each: load both (reg=0,0), add both (1,1), store
        // both (counter=1) — one update lost. With the lock the result would be 2.
        let schedule = [0usize, 1, 0, 1, 0, 1];
        let got = run_locked_model(2, 1, &schedule, false);
        assert!(got < 2, "without the lock an interleaving must lose an update, got {got}");
        // Sanity: the *same* schedule under the lock is exact.
        assert_eq!(run_locked_model(2, 1, &schedule, true), 2);
    }

    // ── Channels (increment 2) ────────────────────────────────────────────────

    /// **Wiring (toolchain-free).** The shipped channel demo — which builds a
    /// `Channel(i64)`, fills it from a `concurrent { spawn … }` nursery (move-only
    /// `channel_send`), and drains it — lowers with zero diagnostics through the real
    /// module pipeline.
    #[test]
    fn channel_example_compiles_clean() {
        let diags = example_diags("examples/std/channel.jtr");
        assert!(diags.is_empty(), "examples/std/channel.jtr: {diags:?}");
    }

    /// Write `files` to a fresh temp dir, run load → typeck → escape → lower, and
    /// return every diagnostic message. Used to drive a *qualified* call across a
    /// real module boundary (single-source `compile` can't model `mod.f(…)`).
    fn multi_diags(files: &[(&str, &str)]) -> Vec<String> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static CASE: AtomicU64 = AtomicU64::new(0);
        let id = CASE.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("jestyr_syncprop_{id:016x}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for (name, src) in files {
            std::fs::write(dir.join(name), src).unwrap();
        }
        let prog = crate::module::load(dir.join("main.jtr").to_str().unwrap());
        let mut diags: Vec<String> = prog.diags.iter().map(|d| d.message.clone()).collect();
        let (info, td) = typeck::check_program(&prog.ast, &prog.modules);
        diags.extend(td.iter().map(|d| d.message.clone()));
        diags.extend(escape::check(&prog.ast, &info).iter().map(|d| d.message.clone()));
        let _ = std::fs::remove_dir_all(&dir);
        diags
    }

    /// **Move-on-send soundness (the channel safety guarantee).** Sending a value
    /// through a move-only `take` parameter reached by a *qualified, generic* call
    /// (`q.sink(T, take v)`) must reject a **borrow** — you can only send something
    /// you own. This pins the give-away route's qualified-call arm; without it a
    /// borrow could be handed across a channel send and aliased after the move.
    #[test]
    fn qualified_take_of_borrow_is_rejected() {
        // `lib.sink` takes its value by `take` (the channel-send shape). `relay`
        // only *borrows* `b`, so handing it to `sink` is a give-away error.
        let lib = "pub fn sink(comptime T: type, take v: T) {}";
        let main = "import \"lib\"\n\
                    struct Box { p: *mut i64 }\n\
                    fn relay(read b: Box) { lib.sink(Box, b) }\n\
                    fn main() -> i32 { return 0 }";
        let diags = multi_diags(&[("lib.jtr", lib), ("main.jtr", main)]);
        assert!(
            diags.iter().any(|d| d.contains("give") || d.contains("borrow") || d.contains("own")),
            "sending a borrow through a qualified `take` must be rejected, got: {diags:?}"
        );
    }

    /// **Teeth for the rejection.** The *same* call with an **owned** value compiles
    /// clean — the check rejects only borrows, never legitimate moves.
    #[test]
    fn qualified_take_of_owned_is_accepted() {
        let lib = "pub fn sink(comptime T: type, take v: T) {}";
        let main = "import \"lib\"\n\
                    struct Box { p: *mut i64 }\n\
                    fn give() { var b: Box = Box{ p: null } lib.sink(Box, b) }\n\
                    fn main() -> i32 { return 0 }";
        let diags = multi_diags(&[("lib.jtr", lib), ("main.jtr", main)]);
        assert!(diags.is_empty(), "moving an owned value must be fine, got: {diags:?}");
    }

    /// A model of the channel's ring buffer + the send/recv index math. `n` producers
    /// each enqueue a block of distinct values; an interleaved consumer dequeues. The
    /// invariant: **every value sent is received exactly once, in FIFO order per
    /// producer block** — the buffer never drops, duplicates, or corrupts an item,
    /// for any capacity and any interleaving. (The cross-thread *result* sum is also
    /// order-independent, mirroring the live demo.)
    fn ring_roundtrip(cap: usize, blocks: &[Vec<i64>], sched: &[bool]) -> Vec<i64> {
        let mut buf = vec![0i64; cap.max(1)];
        let (mut head, mut tail, mut count) = (0usize, 0usize, 0usize);
        // Flatten producers into a single FIFO of pending sends (the demo's producers
        // are independent; per-producer order is preserved by sending in block order).
        let pending: Vec<i64> = blocks.iter().flatten().copied().collect();
        let mut pi = 0usize;
        let mut out = Vec::new();
        // `sched` chooses send (true) vs recv (false) at each step; blocked ops are
        // skipped. Drain to completion afterwards.
        let send = |buf: &mut [i64], _head: &mut usize, tail: &mut usize, count: &mut usize, pi: &mut usize| {
            if *pi < pending.len() && *count < buf.len() {
                buf[*tail] = pending[*pi];
                *tail = (*tail + 1) % buf.len();
                *count += 1;
                *pi += 1;
            }
        };
        let recv = |buf: &mut [i64], head: &mut usize, _tail: &mut usize, count: &mut usize, out: &mut Vec<i64>| {
            if *count > 0 {
                out.push(buf[*head]);
                *head = (*head + 1) % buf.len();
                *count -= 1;
            }
        };
        for &is_send in sched {
            if is_send {
                send(&mut buf, &mut head, &mut tail, &mut count, &mut pi);
            } else {
                recv(&mut buf, &mut head, &mut tail, &mut count, &mut out);
            }
        }
        while pi < pending.len() {
            send(&mut buf, &mut head, &mut tail, &mut count, &mut pi);
            recv(&mut buf, &mut head, &mut tail, &mut count, &mut out);
        }
        while count > 0 {
            recv(&mut buf, &mut head, &mut tail, &mut count, &mut out);
        }
        out
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]

        /// The ring buffer transfers every value exactly once (multiset-preserving),
        /// for any capacity and any send/recv interleaving.
        #[test]
        fn channel_ring_preserves_every_value(
            cap in 1usize..8,
            blocks in proptest::collection::vec(
                proptest::collection::vec(0i64..1000, 0..6), 1..5),
            sched in proptest::collection::vec(any::<bool>(), 0..200),
        ) {
            let out = ring_roundtrip(cap, &blocks, &sched);
            let mut sent: Vec<i64> = blocks.iter().flatten().copied().collect();
            let mut got = out.clone();
            sent.sort_unstable();
            got.sort_unstable();
            prop_assert_eq!(sent, got, "every sent value received exactly once");
        }
    }
}

/// **CTFE properties (workstream G).** A comptime interpreter has three obligations
/// a unit test cannot really pin: it must be *total* (no panic, no hang on any
/// input), *deterministic* (a compiler that folds differently between runs breaks
/// reproducible builds and every attest hash), and *correct* (the folded number is
/// the number the arithmetic says). These check all three against an oracle.
#[cfg(test)]
mod comptime_props {
    use super::*;
    use crate::comptime::{Interp, Value};
    use proptest::prelude::*;

    /// Evaluate the first `comptime { … }` block in `src`.
    fn eval_block(src: &str) -> Result<Value, String> {
        let (tokens, _) = Lexer::new(src).tokenize();
        let (ast, _) = Parser::new(src, tokens).parse();
        let id = ast
            .exprs
            .iter()
            .position(|e| matches!(e.kind, crate::ast::ExprKind::Comptime(_)))
            .map(|i| crate::ast::ExprId(i as u32))
            .ok_or_else(|| "no comptime block".to_string())?;
        Interp::new(&ast).eval(id).map_err(|e| e.message)
    }

    /// A left-folded, fully parenthesised integer expression plus its Rust oracle.
    /// Parenthesising every step removes precedence from the equation, so a
    /// divergence can only mean the *arithmetic* disagrees. `None` means the oracle
    /// says this expression has no value (overflow or division by zero) — which the
    /// interpreter must report as an error rather than wrap or crash.
    fn build(ops: &[(u8, i64)]) -> (String, Option<i64>) {
        let mut text = String::from("7");
        let mut oracle = Some(7i64);
        for &(op, v) in ops {
            let sym = match op % 4 {
                0 => "+",
                1 => "-",
                2 => "*",
                _ => "/",
            };
            text = format!("({text} {sym} {v})");
            oracle = oracle.and_then(|a| match op % 4 {
                0 => a.checked_add(v),
                1 => a.checked_sub(v),
                2 => a.checked_mul(v),
                _ => {
                    if v == 0 {
                        None
                    } else {
                        a.checked_div(v)
                    }
                }
            });
        }
        (text, oracle)
    }

    fn arb_ops() -> impl Strategy<Value = Vec<(u8, i64)>> {
        proptest::collection::vec((0u8..4u8, 0i64..1_000_000i64), 0..14)
    }

    proptest! {
        /// **Correctness against an oracle.** Whatever the interpreter folds is what
        /// checked `i64` arithmetic says — and where the arithmetic has no answer
        /// (overflow, division by zero) the interpreter says so rather than wrapping.
        /// Teeth: swapping any `checked_*` in `int_binop` for a wrapping op fails here.
        #[test]
        fn ctfe_arithmetic_matches_a_checked_oracle(ops in arb_ops()) {
            let (text, oracle) = build(&ops);
            let src = format!("const A: i64 = comptime {{ {text} }}\n");
            match (eval_block(&src), oracle) {
                (Ok(Value::Int(got)), Some(want)) => prop_assert_eq!(got, want, "{}", text),
                (Err(_), None) => {}
                (got, want) => prop_assert!(false, "{text}: got {got:?}, oracle {want:?}"),
            }
        }

        /// **Determinism.** The same source folds to the same value — or fails with
        /// the same message — every time. Any iteration-order or cross-run state leak
        /// in the interpreter shows up here, and it would corrupt every attest hash.
        #[test]
        fn ctfe_expr_is_deterministic(ops in arb_ops()) {
            let (text, _) = build(&ops);
            let src = format!("const A: i64 = comptime {{ {text} }}\n");
            let first = eval_block(&src);
            for _ in 0..4 {
                prop_assert_eq!(format!("{:?}", eval_block(&src)), format!("{first:?}"));
            }
        }

        /// **Trivia cannot change a folded value.** Comments and whitespace are not
        /// part of the value domain; if they were, formatting a file would change the
        /// program it compiles to.
        #[test]
        fn ctfe_ignores_whitespace_and_comments(ops in arb_ops()) {
            let (text, _) = build(&ops);
            let plain = format!("const A: i64 = comptime {{ {text} }}\n");
            let noisy = format!(
                "const A: i64 = comptime {{\n  // a line comment\n  /* and a block one */ {}\n}}\n",
                text.replace(" + ", "  +  ")
            );
            prop_assert_eq!(format!("{:?}", eval_block(&plain)), format!("{:?}", eval_block(&noisy)));
        }

        /// **Reflection order is deterministic and is declaration order.** Generated
        /// output (a serializer, a table) is only reproducible if the compiler answers
        /// "what are this type's fields" the same way every time — so this builds a
        /// struct from a generated field list and checks the answer back, repeatedly.
        /// A `HashMap` anywhere in the reflection path would fail it.
        #[test]
        fn reflection_order_is_deterministic(n in 1usize..8) {
            let names: Vec<String> = (0..n).map(|i| format!("f{i}")).collect();
            let decl: String =
                names.iter().map(|f| format!("{f}: i32")).collect::<Vec<_>>().join(", ");
            for i in 0..n {
                let src = format!(
                    "struct S {{ {decl} }}\nconst A: str = comptime {{ @field_name(S, {i}) }}\n"
                );
                // Same query, several runs: same answer, and it is the i-th DECLARED field.
                for _ in 0..3 {
                    prop_assert_eq!(eval_block(&src), Ok(Value::Str(names[i].clone())));
                }
            }
            let cnt = format!("struct S {{ {decl} }}\nconst A: i64 = comptime {{ @field_count(S) }}\n");
            prop_assert_eq!(eval_block(&cnt), Ok(Value::Int(n as i64)));
        }

        /// **Aggregates round-trip.** For any generated list, `.len` is its length and
        /// `xs[i]` is the i-th element written — the two operations that make a
        /// comptime table useful must agree with construction for every shape and size.
        #[test]
        fn ctfe_aggregates_round_trip(vals in proptest::collection::vec(-9999i64..9999, 1..12)) {
            let lit: String =
                vals.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(", ");
            let n = vals.len();
            let len_src = format!("const A: i64 = comptime {{ [{lit}].len }}\n");
            prop_assert_eq!(eval_block(&len_src), Ok(Value::Int(n as i64)));
            for (i, want) in vals.iter().enumerate() {
                let src = format!("const A: i64 = comptime {{ [{lit}][{i}] }}\n");
                prop_assert_eq!(eval_block(&src), Ok(Value::Int(*want)));
            }
            // One past the end is an error, never a wrap or a zero.
            let oob = format!("const A: i64 = comptime {{ [{lit}][{n}] }}\n");
            prop_assert!(eval_block(&oob).is_err());
        }

        /// **A repeat count can never outrun the budget.** The fuel is spent per
        /// element, so any count is either produced or diagnosed — never allocated
        /// speculatively. Teeth: removing the per-element `spend` hangs this test.
        #[test]
        fn ctfe_repeat_counts_are_bounded(n in 0i64..50_000_000) {
            let src = format!("const A: i64 = comptime {{ [7; {n}].len }}\n");
            match eval_block(&src) {
                Ok(Value::Int(got)) => prop_assert_eq!(got, n),
                Ok(other) => prop_assert!(false, "unexpected {:?}", other),
                Err(e) => prop_assert!(e.contains("step budget"), "{}", e),
            }
        }

        /// **A loop-built table equals the same table written out.** The tier-7 claim is
        /// that computing a table's *shape* changes nothing about its contents — so a
        /// `for` filling a `var` must agree, element for element, with the literal it
        /// replaces.
        #[test]
        fn ctfe_loop_built_tables_match_the_literal(n in 1usize..12, k in -50i64..50) {
            let looped = format!(
                "const A: [{n}]i64 = comptime {{\n    var t = [0; {n}]\n    \
                 for i in 0..{n} {{ t[i] = i * {k} }}\n    t\n}}\n"
            );
            let lit: String =
                (0..n).map(|i| (i as i64 * k).to_string()).collect::<Vec<_>>().join(", ");
            let written = format!("const A: [{n}]i64 = comptime {{ [{lit}] }}\n");
            prop_assert_eq!(eval_block(&looped), eval_block(&written));
        }

        /// **Every loop terminates.** Any bound, any step, any direction: the result is
        /// a value or a diagnostic, never a hang. Teeth: dropping the per-iteration
        /// `spend` makes this run forever instead of failing.
        #[test]
        fn ctfe_loops_always_terminate(lo in -1000i64..1000, hi in -1000i64..1000, step in -8i64..8) {
            let src = format!(
                "const A: i64 = comptime {{\n    var s = 0\n    \
                 for i in {lo}..{hi} step {step} {{ s += 1 }}\n    s\n}}\n"
            );
            match eval_block(&src) {
                Ok(Value::Int(count)) => {
                    // A run that finished must have counted exactly the elements the
                    // range contains.
                    let want = if step > 0 && hi > lo {
                        ((hi - lo) as f64 / step as f64).ceil() as i64
                    } else if step < 0 && lo > hi {
                        ((lo - hi) as f64 / (-step) as f64).ceil() as i64
                    } else {
                        0
                    };
                    prop_assert_eq!(count, want, "lo={} hi={} step={}", lo, hi, step);
                }
                Ok(other) => prop_assert!(false, "unexpected {:?}", other),
                // A zero step never advances, and an over-long run runs out of budget.
                Err(e) => prop_assert!(
                    e.contains("never advances") || e.contains("step budget"),
                    "{}", e
                ),
            }
        }

        /// **Totality on nonsense.** An arbitrary body is refused with a diagnostic or
        /// folded — never a panic and never a hang. The three bounds (fuel, call depth,
        /// const-cycle detection) are what make this true for *every* input, not just
        /// the well-formed ones.
        #[test]
        fn ctfe_invalid_programs_refuse_not_panic(body in ".{0,64}") {
            let src = format!("const A: i64 = comptime {{ {body} }}\n");
            let _ = eval_block(&src);
        }
    }

    /// Coverage-guided fuzzing of the comptime surface: arbitrary text inside a
    /// `comptime` block, driven through the *whole* pipeline (parse → typeck → cgen),
    /// so a body that survives folding still cannot panic emission. Replays the corpus
    /// under `cargo test`; a real campaign is `cargo bolero test fuzz_comptime_eval`.
    #[test]
    fn fuzz_comptime_eval() {
        bolero::check!().with_type::<String>().for_each(|s: &String| {
            let src = format!("fn main() -> i32 {{ let a = comptime {{ {s} }}\n return 0 }}");
            let (tokens, _) = Lexer::new(&src).tokenize();
            let (ast, _) = Parser::new(&src, tokens).parse();
            let (info, _td) = typeck::check(&ast);
            let _ = cgen::emit(&ast, &info);
        });
    }
}

mod fuzz {
    use super::*;

    /// Coverage-guided fuzzing of the whole pipeline. Under `cargo test` this
    /// replays the corpus and a bounded number of generated inputs; under
    /// `cargo bolero test fuzz_pipeline` it runs a real fuzzing engine.
    #[test]
    fn fuzz_pipeline() {
        bolero::check!().with_type::<String>().for_each(|s: &String| {
            run_pipeline(s);
        });
    }

    /// **Deep expression nesting never panics the parser.** Derive a depth from the
    /// fuzz input and feed a deeply-nested expression to the parser; the depth guard
    /// must resolve *any* depth to a bounded tree plus a clean diagnostic, never a
    /// stack overflow. Uses the two *iterative*-to-parse shapes — the left fold and
    /// the postfix chain — which stay O(1) on the parser stack at any depth, so this
    /// is safe on the fuzz thread's stack; the recursive shapes (which need the
    /// worker stack) are covered by `recursive_deep_shapes_report_on_the_worker_stack`.
    #[test]
    fn fuzz_deep_nesting() {
        bolero::check!().with_type::<Vec<u8>>().for_each(|bytes: &Vec<u8>| {
            let b0 = bytes.first().copied().unwrap_or(0);
            let b1 = bytes.get(1).copied().unwrap_or(0);
            // Two bytes reach depths on both sides of the cap from a tiny input.
            let depth = u16::from_le_bytes([b0, b1]) as usize % 4096;
            let body = if b0 & 1 == 0 {
                let mut e = String::from("1");
                for _ in 0..depth {
                    e.push_str("+1");
                }
                e
            } else {
                let mut e = String::from("x");
                for _ in 0..depth {
                    e.push_str(".f");
                }
                e
            };
            let src = format!("fn main() -> i64 {{ return {body} }}");
            let (tokens, _) = Lexer::new(&src).tokenize();
            let _ = Parser::new(&src, tokens).parse();
        });
    }

    /// **Recoverable `try_read_file` lowering never panics (B3, totality).** Feed
    /// an arbitrary path expression to `try_read_file` through the full pipeline; the
    /// intrinsic arm and the result-struct emission must be total.
    #[test]
    fn fuzz_fs_try() {
        bolero::check!().with_type::<String>().for_each(|s: &String| {
            let src = format!("fn main() -> i32 {{ let r = try_read_file({s}) return 0 }}");
            let (tokens, _) = Lexer::new(&src).tokenize();
            let (ast, _) = Parser::new(&src, tokens).parse();
            let (info, _td) = typeck::check(&ast);
            let _ = cgen::emit(&ast, &info);
        });
    }

    /// **Inline `slice(…)` arg-position typing never panics (B5, totality).** Feed
    /// an arbitrary first (type) argument to `slice(…)` in `from_utf8` position;
    /// the typeck arm that reads it as the element type must be total.
    #[test]
    fn fuzz_slice_arg_typing() {
        bolero::check!().with_type::<String>().for_each(|s: &String| {
            let src = format!(
                "fn f() -> i64 {{ var b: *mut u8 = alloc(u8, 4) \
                 let v = from_utf8(slice({s}, b, 4)) return 0 }}"
            );
            let (tokens, _) = Lexer::new(&src).tokenize();
            let (ast, _) = Parser::new(&src, tokens).parse();
            let (info, _td) = typeck::check(&ast);
            let _ = cgen::emit(&ast, &info);
        });
    }

    /// **Value-position `unsafe`/block lowering never panics (B4, totality).**
    /// Drive arbitrary source as the body of an `unsafe`-block `let` initializer
    /// through the full parse→typeck→cgen pipeline; the new value-position lowering
    /// must be total on any input.
    #[test]
    fn fuzz_unsafe_blocks() {
        bolero::check!().with_type::<String>().for_each(|s: &String| {
            let src = format!("fn f() -> i32 {{ let y = unsafe {{ {s} }} return 0 }}");
            let (tokens, _) = Lexer::new(&src).tokenize();
            let (ast, _) = Parser::new(&src, tokens).parse();
            let (info, _td) = typeck::check(&ast);
            let _ = cgen::emit(&ast, &info);
        });
    }

    /// **`#line` debug-info emission never panics and never malforms a directive.**
    /// Wrap arbitrary source in a function (so the backend reaches the per-function
    /// `#line` site) with single-file debug info populated, then assert every
    /// emitted directive is well-formed: a line number ≥ 1, a quoted path, no
    /// embedded newline, no raw backslash. `span_to_file_line` must be total over
    /// any spans the arbitrary body produces.
    #[test]
    fn fuzz_line_directives() {
        bolero::check!().with_type::<String>().for_each(|s: &String| {
            let src = format!("fn f() {{ {s} }}");
            let (tokens, _) = Lexer::new(&src).tokenize();
            let (ast, _) = Parser::new(&src, tokens).parse();
            let (mut info, _td) = typeck::check(&ast);
            info.debug = crate::types::DebugInfo::new(
                vec!["f.jtr".to_string()],
                vec![src.to_string()],
                vec![0],
            );
            let (c, _) = cgen::emit(&ast, &info);
            for l in c.lines() {
                if let Some(rest) = l.strip_prefix("#line ") {
                    let n: u32 = rest.split_whitespace().next().unwrap().parse().unwrap();
                    assert!(n >= 1, "line >= 1: {l}");
                    assert_eq!(l.matches('"').count(), 2, "path quoted: {l}");
                    assert!(!l.contains('\\'), "no raw backslash: {l}");
                }
            }
        });
    }

    /// **The recursive drop-glue synthesizer never panics (B1, totality).** Drive
    /// arbitrary source through the full parse→typeck→escape→cgen pipeline while it
    /// is biased toward the field/payload-drop path: a `Drop`-having `R`, a struct
    /// that *owns* an `R`, and an enum with an `R` payload are all in scope, so the
    /// adversarial body can nest, move, and match droppable aggregates. The
    /// recursion over aggregates must terminate and never panic on any input.
    #[test]
    fn fuzz_drop_glue() {
        bolero::check!().with_type::<String>().for_each(|s: &String| {
            let src = format!(
                "trait Drop {{ fn drop(mut self) }} struct R {{ id: i32 }} \
                 impl Drop for R {{ fn drop(mut self) {{ print_int(self.id) }} }} \
                 struct Holder {{ r: R }} enum Node {{ leaf, wrap(r: R) }} \
                 fn f() {{ {s} }}"
            );
            let (tokens, _) = Lexer::new(&src).tokenize();
            let (ast, _) = Parser::new(&src, tokens).parse();
            let (info, _td) = typeck::check(&ast);
            let _ed = escape::check(&ast, &info);
            let _ = cgen::emit(&ast, &info);
        });
    }

    /// Build a synthetic two-module `Modules` over a single parsed AST by
    /// assigning items alternately to module 0/1 (each importing the other). Lets
    /// the multi-module resolver be fuzzed in-memory — no filesystem.
    fn split_two_modules(ast: &crate::ast::Ast) -> crate::module::Modules {
        use std::collections::HashMap;
        let n = ast.items.len();
        let mut imports = vec![HashMap::new(), HashMap::new()];
        imports[0].insert("m1".to_string(), 1usize);
        imports[1].insert("m0".to_string(), 0usize);
        crate::module::Modules {
            names: vec!["m0".to_string(), "m1".to_string()],
            paths: vec!["<m0>".to_string(), "<m1>".to_string()],
            srcs: vec![String::new(), String::new()],
            bases: vec![0, 0],
            item_mod: (0..n).map(|i| i % 2).collect(),
            item_pub: vec![true; n],
            imports,
            hashes: vec![String::new(), String::new()],
        }
    }

    /// **Multi-module resolution never panics.** Parse arbitrary source, split its
    /// items across two synthetic modules, then run name resolution + lowering —
    /// exercising the `(module, name)` owner keying, collision detection, and
    /// `canon` on adversarial input without touching disk. `canon` itself is also
    /// hit directly on the raw bytes as a name.
    #[test]
    fn fuzz_multimodule_resolution() {
        bolero::check!().with_type::<String>().for_each(|s: &String| {
            let (tokens, _) = Lexer::new(s).tokenize();
            let (ast, _) = Parser::new(s, tokens).parse();
            let modules = split_two_modules(&ast);
            let (info, _td) = typeck::check_program(&ast, &modules);
            let _ = cgen::emit(&ast, &info);
            let mut dup = std::collections::HashSet::new();
            dup.insert(s.clone());
            let _ = crate::types::canon(1, s, &dup);
        });
    }

    /// **Module content-hashing never panics and is deterministic.** Computing the
    /// hash over an arbitrary parsed program (via `Modules::single`, which runs the
    /// hash path) must not panic and must reproduce — on any adversarial input.
    #[test]
    fn fuzz_module_hash() {
        bolero::check!().with_type::<String>().for_each(|s: &String| {
            let (tokens, _) = Lexer::new(s).tokenize();
            let (ast, _) = Parser::new(s, tokens).parse();
            let h1 = crate::module::Modules::single(&ast).hashes;
            let h2 = crate::module::Modules::single(&ast).hashes;
            assert_eq!(h1, h2, "module hashing is deterministic");
        });
    }

    /// Fuzz the parser directly on arbitrary token-shaped byte input.
    #[test]
    fn fuzz_lexer_spans() {
        bolero::check!().with_type::<Vec<u8>>().for_each(|bytes: &Vec<u8>| {
            let s = String::from_utf8_lossy(bytes);
            let (tokens, _diags) = Lexer::new(&s).tokenize();
            assert_eq!(tokens.last().unwrap().kind, TokenKind::Eof);
        });
    }

    /// **Determinism under fuzzing** — the strongest invariant: the same adversarial
    /// input must compile to byte-identical C and the same diagnostic count, every
    /// run. A coverage-guided search for any `HashMap`/`HashSet` ordering leak.
    #[test]
    fn fuzz_determinism() {
        bolero::check!().with_type::<String>().for_each(|s: &String| {
            assert_eq!(compile(s), compile(s));
        });
    }

    /// The doc generator never panics on arbitrary bytes, in either output format.
    #[test]
    fn fuzz_doc_generator() {
        bolero::check!().with_type::<Vec<u8>>().for_each(|bytes: &Vec<u8>| {
            let s = String::from_utf8_lossy(bytes);
            let _ = crate::doc::generate(&s, "t", false);
            let _ = crate::doc::generate(&s, "t", true);
        });
    }

    /// Coverage-guided fuzzing of the **function-pointer** parse/typeck/cgen
    /// paths: the fuzzer's bytes are dropped into a fn-pointer type slot (and a
    /// call slot), so `parse_type`'s `Fn` arm, its recovery guard, and the
    /// typedef lowering are all hammered. The pipeline must stay total *and*
    /// deterministic on whatever soup lands inside the parentheses.
    #[test]
    fn fuzz_fn_pointer_pipeline() {
        bolero::check!().with_type::<String>().for_each(|s: &String| {
            let prog = format!(
                "struct V {{ f: fn({s}) -> i32 }} fn use_v(g: fn({s})) -> i32 {{ return f(0) }}"
            );
            run_pipeline(&prog);
            assert_eq!(compile(&prog), compile(&prog));
        });
    }

    /// Coverage-guided fuzzing of the **generic-vtable field-call** path: the
    /// fuzzer's bytes land in the generic struct's fn-pointer field type and in
    /// the method-style call's argument slot, so the new `Ty::GenStruct` arm of
    /// `fn_ptr_field` (and the substitution behind it) is hammered. The pipeline
    /// must stay total *and* deterministic on whatever lands there.
    #[test]
    fn fuzz_generic_vtable_pipeline() {
        bolero::check!().with_type::<String>().for_each(|s: &String| {
            let prog = format!(
                "fn xBox(comptime T: type) -> type {{ return struct {{ op: fn({s}) -> T }} }} \
                 fn xuse(n: i32) -> i32 {{ let b = xBox(i32){{ op: |x| x }} return b.op({s}) }}"
            );
            run_pipeline(&prog);
            assert_eq!(compile(&prog), compile(&prog));
        });
    }

    /// Coverage-guided fuzzing of the **concurrency** lowering: the fuzzer's bytes
    /// land in a spawn-target's body and a `concurrent { spawn … }` argument slot,
    /// alongside the atomics + `atomic_xchg` (the spinlock atom) the Mutex is built
    /// on. The `concurrent`/`spawn` desugaring, the spawn-site arg-struct emission,
    /// and the escape data-race check must stay total *and* deterministic — and the
    /// escape checker must never silently accept a `mut`-slice spawn — on whatever
    /// soup lands inside.
    #[test]
    fn fuzz_concurrency_pipeline() {
        bolero::check!().with_type::<String>().for_each(|s: &String| {
            let prog = format!(
                "fn wk(p: *mut i64) {{ atomic_xchg(p, 1) {s} }} \
                 fn main() -> i32 {{ var c: *mut i64 = alloc(i64, 1) atomic_store(c, 0) \
                     concurrent {{ spawn wk(c) spawn wk({s}) }} free_ptr(c) return 0 }}"
            );
            run_pipeline(&prog);
            assert_eq!(compile(&prog), compile(&prog));
        });
    }

    /// Coverage-guided fuzzing of the **traits** parse/recovery paths: the
    /// fuzzer's bytes land inside an `impl` body and a bounded-generic body, so
    /// the new `trait`/`impl`/`dyn`/`[T: Bound]` grammar and its recovery guards
    /// are hammered. The pipeline must stay total *and* deterministic.
    #[test]
    fn fuzz_traits_pipeline() {
        bolero::check!().with_type::<String>().for_each(|s: &String| {
            let prog = format!(
                "trait T {{ fn m(read self) -> i32 }} \
                 impl T for i32 {{ {s} }} fn u[X: T](read x: X) -> i32 {{ {s} }}"
            );
            run_pipeline(&prog);
            assert_eq!(compile(&prog), compile(&prog));
        });
    }

    /// Coverage-guided fuzzing of **Stage C static dispatch**: the fuzzer's bytes
    /// fill the impl-method body — now actually *emitted* — while a real
    /// `x.m()` call drives the dispatch lowering. Exercises `emit_impl_call`,
    /// `emit_impl_method_decl`, and the mangle on arbitrary body soup; the
    /// pipeline must stay total *and* deterministic.
    #[test]
    fn fuzz_trait_static_dispatch() {
        bolero::check!().with_type::<String>().for_each(|s: &String| {
            let prog = format!(
                "trait T {{ fn m(read self) -> i32 }} \
                 impl T for i32 {{ fn m(read self) -> i32 {{ {s} }} }} \
                 fn u(read x: i32) -> i32 {{ return x.m() }}"
            );
            run_pipeline(&prog);
            assert_eq!(compile(&prog), compile(&prog));
        });
    }

    /// Coverage-guided fuzzing of **Stage D bound checking**: fuzz bytes fill a
    /// bracket-generic body while a real call `g(y)` drives the call-site bound
    /// check (`check_call_bounds` / `unify_tp`). Totality + determinism on
    /// arbitrary body soup — the bound machinery must never panic or leak order.
    #[test]
    fn fuzz_definition_site_bounds() {
        bolero::check!().with_type::<String>().for_each(|s: &String| {
            let prog = format!(
                "trait B {{ fn m(read self) -> i32 }} \
                 impl B for i32 {{ fn m(read self) -> i32 {{ return 0 }} }} \
                 fn g[T: B](read x: T) -> i32 {{ {s} }} \
                 fn c(read y: i32) -> i32 {{ return g(y) }}"
            );
            run_pipeline(&prog);
            assert_eq!(compile(&prog), compile(&prog));
        });
    }

    /// Coverage-guided fuzzing of **`dyn` dispatch** (Stage F): fuzz bytes fill a
    /// `dyn`-taking function's body while a coercion + `d.xm()` drive the vtable
    /// construction and dispatch. Total + deterministic on arbitrary body soup.
    #[test]
    fn fuzz_dyn_dispatch() {
        bolero::check!().with_type::<String>().for_each(|s: &String| {
            let prog = format!(
                "trait xS {{ fn xm(read self) -> i32 }} \
                 impl xS for i32 {{ fn xm(read self) -> i32 {{ return 0 }} }} \
                 fn describe(read d: dyn xS) -> i32 {{ {s} return d.xm() }} \
                 fn use_it(read y: i32) -> i32 {{ return describe(y) }}"
            );
            run_pipeline(&prog);
            assert_eq!(compile(&prog), compile(&prog));
        });
    }

    /// Coverage-guided fuzzing of the **body-side bound check** (the "Zig fix"):
    /// fuzz bytes fill a bound generic's body while `x.xm()` drives the
    /// bound-method resolution (typeck) and per-instance dispatch (cgen). Total +
    /// deterministic on arbitrary body soup.
    #[test]
    fn fuzz_bound_method_calls() {
        bolero::check!().with_type::<String>().for_each(|s: &String| {
            let prog = format!(
                "trait xS {{ fn xm(read self) -> i32 }} \
                 impl xS for i32 {{ fn xm(read self) -> i32 {{ return 0 }} }} \
                 fn g[T: xS](read x: T) -> i32 {{ {s} return x.xm() }} \
                 fn use_it(read y: i32) -> i32 {{ return g(y) }}"
            );
            run_pipeline(&prog);
            assert_eq!(compile(&prog), compile(&prog));
        });
    }

    /// Coverage-guided fuzzing of **operator traits** (Stage E): fuzz bytes fill an
    /// `Add` impl body while `a + b` drives the operator resolution + dispatch
    /// lowering (`resolve_operator_trait` / `emit_operator_call`). Total +
    /// deterministic on arbitrary body soup.
    #[test]
    fn fuzz_operator_traits() {
        bolero::check!().with_type::<String>().for_each(|s: &String| {
            let prog = format!(
                "struct V {{ n: i32 }} \
                 impl Add for V {{ fn add(read self, read rhs: V) -> V {{ {s} }} }} \
                 fn use_it(read a: V, read b: V) -> V {{ return a + b }}"
            );
            run_pipeline(&prog);
            assert_eq!(compile(&prog), compile(&prog));
        });
    }

    /// Coverage-guided fuzzing of the **`jestyrc test` runner** (workstream O): the
    /// fuzzer's bytes are used both as a `@test` name slot *and* as the filter
    /// substring, so `cgen::list_tests` and `emit_tests_filtered` run on adversarial
    /// names/filters. Invariants: the harness emit never panics and always prints a
    /// well-formed `running …` banner, the baked count never exceeds the discovered
    /// test count, and emission stays deterministic. `list_tests` and the filter
    /// agree (filtered count ≤ unfiltered).
    #[test]
    fn fuzz_test_runner() {
        bolero::check!().with_type::<(String, String)>().for_each(|(name, filt): &(String, String)| {
            // Drop the name into a `@test` slot and a second fixed test, so there is
            // always at least one discoverable test regardless of the fuzzer's bytes.
            let prog = format!(
                "@test fn {name}() -> bool {{ return true }} \
                 @test fn xfixed() -> bool {{ return true }}"
            );
            let (tokens, _) = Lexer::new(&prog).tokenize();
            let (ast, _) = Parser::new(&prog, tokens).parse();
            let (info, _) = crate::typeck::check(&ast);

            let discovered = cgen::list_tests(&ast).len();
            let count = |c: &str| -> usize {
                c.find("running ")
                    .and_then(|at| c[at + 8..].split_whitespace().next())
                    .and_then(|n| n.parse().ok())
                    .expect("a well-formed running banner")
            };
            let (full, _) = cgen::emit_tests_filtered(&ast, &info, None);
            let (filtered, _) = cgen::emit_tests_filtered(&ast, &info, Some(filt));
            let nfull = count(&full);
            let nfilt = count(&filtered);
            assert!(nfull <= discovered, "baked > discovered: {nfull} > {discovered}");
            assert!(nfilt <= nfull, "filter grew the roster: {nfilt} > {nfull}");
            // Determinism: identical bytes on a re-emit.
            assert_eq!(filtered, cgen::emit_tests_filtered(&ast, &info, Some(filt)).0);
        });
    }

    /// Coverage-guided fuzzing of **`jestyrc attest`** (workstream O): fuzz bytes
    /// fill a function body and a `requires` clause, so record collection, signature
    /// reconstruction, guarantee extraction, codegen, and the SHA all run on
    /// adversarial input. Invariants on whatever soup lands: `manifest` never panics,
    /// always emits the locked 4-line header with a 64-hex C hash, that hash is
    /// exactly the emitted-C digest, and the whole manifest is deterministic.
    #[test]
    fn fuzz_attest() {
        bolero::check!().with_type::<String>().for_each(|s: &String| {
            let prog = format!(
                "fn f(n: i32) -> i32 requires {s} {{ {s} return n }} \
                 fn main() -> i32 {{ return 0 }}"
            );
            let (tokens, _) = Lexer::new(&prog).tokenize();
            let (ast, _) = Parser::new(&prog, tokens).parse();
            let (info, _) = crate::typeck::check(&ast);
            let m = crate::attest::manifest("fuzz", &prog, &ast, &info);
            assert!(m.starts_with("jestyr-attest/v1\nsource fuzz\nc-sha256 "), "header: {m}");
            let sha = m.lines().find_map(|l| l.strip_prefix("c-sha256 ")).expect("a hash line");
            assert_eq!(sha.len(), 64, "64-hex hash: {sha}");
            let (c, _) = cgen::emit(&ast, &info);
            assert_eq!(sha, crate::sha256::hex(c.as_bytes()));
            assert_eq!(m, crate::attest::manifest("fuzz", &prog, &ast, &info), "deterministic");
        });
    }

    /// Coverage-guided fuzzing of **`attest --diff`** (workstream O): two layers.
    /// First, `parse_manifest` must never panic on arbitrary bytes (it parses
    /// untrusted manifest files). Second, `diff` of two *real* manifests built from
    /// fuzzed contract clauses must never panic and stays reflexive (a manifest vs
    /// itself is empty) — exercising the classifier on adversarial guarantee text.
    #[test]
    fn fuzz_attest_diff() {
        bolero::check!().with_type::<(String, String)>().for_each(|(a, b): &(String, String)| {
            // Layer 1: the parser is total on arbitrary text.
            let _ = crate::attest::parse_manifest(a);
            let _ = crate::attest::parse_manifest(b);

            // Layer 2: build two genuine manifests with fuzzed contract bodies and
            // diff them; the classifier never panics, and self-diff is empty.
            let prog = |s: &str| {
                format!("pub fn f(n: i32) -> i32 requires {s} {{ {s} return n }}")
            };
            let mk = |s: &str| {
                let src = prog(s);
                let (tokens, _) = Lexer::new(&src).tokenize();
                let (ast, _) = Parser::new(&src, tokens).parse();
                let (info, _) = crate::typeck::check(&ast);
                crate::attest::parse_manifest(&crate::attest::manifest("f", &src, &ast, &info)).unwrap()
            };
            let (ma, mb) = (mk(a), mk(b));
            let _ = crate::attest::diff(&ma, &mb).render();
            assert!(crate::attest::diff(&ma, &ma).changes.is_empty(), "self-diff must be empty");
        });
    }

    /// Coverage-guided fuzzing of the **Drop/RAII** path (design Phase 3): fuzz
    /// bytes fill a `Drop` impl body and a function body holding owned droppable
    /// locals, so the scope-exit drop-glue insertion + move analysis run on
    /// adversarial input. Never panics; deterministic.
    #[test]
    fn fuzz_drop_alloc_pipeline() {
        bolero::check!().with_type::<String>().for_each(|s: &String| {
            let prog = format!(
                "trait Drop {{ fn drop(mut self) }} struct R {{ id: i32 }} \
                 impl Drop for R {{ fn drop(mut self) {{ {s} }} }} \
                 fn use_it() {{ let a = R{{ id: 1 }} let b = R{{ id: 2 }} {s} }}"
            );
            run_pipeline(&prog);
            assert_eq!(compile(&prog), compile(&prog));
        });
    }

    /// Coverage-guided fuzzing of the **blanket generic `Drop` impl** path: fuzz
    /// bytes fill the body of an `impl[T] Drop for Box(T)` while two distinct
    /// instantiations drive per-instance monomorphization. Never panics;
    /// deterministic.
    #[test]
    fn fuzz_blanket_drop_impl() {
        bolero::check!().with_type::<String>().for_each(|s: &String| {
            let prog = format!(
                "trait Drop {{ fn drop(mut self) }} \
                 fn Box(comptime T: type) -> type {{ return struct {{ v: T }} }} \
                 impl[T] Drop for Box(T) {{ fn drop(mut self) {{ {s} }} }} \
                 fn f() {{ var a = Box(i32){{ v: 1 }} var b = Box(f64){{ v: 2.0 }} }}"
            );
            run_pipeline(&prog);
            assert_eq!(compile(&prog), compile(&prog));
        });
    }

    /// Determinism of Drop/RAII lowering under fuzzing: the same Drop-heavy source
    /// compiles byte-identically (search for an iteration-order leak in drop/move
    /// analysis).
    #[test]
    fn fuzz_drop_alloc_determinism() {
        bolero::check!().with_type::<String>().for_each(|s: &String| {
            let prog = format!(
                "trait Drop {{ fn drop(mut self) }} struct R {{ id: i32 }} \
                 impl Drop for R {{ fn drop(mut self) {{ print_int(self.id) }} }} \
                 fn use_it() -> i32 {{ let a = R{{ id: 1 }} {s} return 0 }}"
            );
            assert_eq!(compile(&prog), compile(&prog));
        });
    }

    /// Every AST node's span stays in-bounds and on char boundaries even on
    /// adversarial input (parser span integrity, not just the lexer's).
    #[test]
    fn fuzz_ast_spans_in_bounds() {
        bolero::check!().with_type::<Vec<u8>>().for_each(|bytes: &Vec<u8>| {
            let s = String::from_utf8_lossy(bytes);
            let (tokens, _) = Lexer::new(&s).tokenize();
            let (ast, _) = Parser::new(&s, tokens).parse();
            let len = s.len() as u32;
            for e in &ast.exprs {
                assert!(e.span.start <= e.span.end && e.span.end <= len, "expr span OOB");
            }
        });
    }
}

/// Property tests for the no-alloc `core` Option/Result combinators
/// (`examples/std/core.jtr`). Two layers:
///   * **codegen** — a generic `Option(T)` combinator (constructed *and* matched
///     inside generic functions) compiles clean and deterministically for *any*
///     element type. This exercises the real compiler path the combinators ride.
///   * **laws as oracles** — the functor/monad laws asserted against a faithful
///     Rust mirror of the combinators' `match` structure (the contract the Jestyr
///     code must satisfy; the runtime side is pinned by
///     `examples/std/combinators.jtr` and the `cgen` goldens).
mod core_props {
    use super::*;
    use proptest::prelude::*;

    /// A self-contained program mirroring `core`'s Option combinators at element
    /// type `prim`: `opt_map` (construct + match inside a generic fn) composed with
    /// `opt_unwrap_or`. Concrete `idf`/literals keep it valid for any integer type.
    fn option_combinator_source(prim: &str) -> String {
        format!(
            "enum Option(T) {{ none, some(v: T) }}\n\
             fn idf(x: {p}) -> {p} {{ return x }}\n\
             fn omap(comptime T: type, take o: Option(T), f: fn(T) -> T) -> Option(T) {{ match o {{ some(v) => some(f(v)), none => none }} }}\n\
             fn ouw(comptime T: type, take o: Option(T), take d: T) -> T {{ match o {{ some(v) => v, none => d }} }}\n\
             fn main() -> i32 {{ var a: Option({p}) = some(1) var b: Option({p}) = none \
                return (ouw({p}, omap({p}, a, &idf), 1) as i32) + (ouw({p}, omap({p}, b, &idf), 0) as i32) }}",
            p = prim
        )
    }

    fn int_prim() -> impl Strategy<Value = &'static str> {
        prop_oneof![Just("i32"), Just("i64"), Just("u8"), Just("u16"), Just("usize")]
    }

    // ── A faithful Rust mirror of core's combinators (same `match` structure) ────
    fn m_map<T, U>(o: Option<T>, f: impl FnOnce(T) -> U) -> Option<U> {
        match o { Some(v) => Some(f(v)), None => None }
    }
    fn m_unwrap_or<T>(o: Option<T>, d: T) -> T {
        match o { Some(v) => v, None => d }
    }
    fn m_ok_or<T, E>(o: Option<T>, e: E) -> Result<T, E> {
        match o { Some(v) => Ok(v), None => Err(e) }
    }
    fn m_res_ok<T, E>(r: Result<T, E>) -> Option<T> {
        match r { Ok(v) => Some(v), Err(_) => None }
    }
    fn m_res_map<T, U, E>(r: Result<T, E>, f: impl FnOnce(T) -> U) -> Result<U, E> {
        match r { Ok(v) => Ok(f(v)), Err(e) => Err(e) }
    }
    fn m_and_then<T, U>(o: Option<T>, f: impl FnOnce(T) -> Option<U>) -> Option<U> {
        match o { Some(v) => f(v), None => None }
    }
    fn m_filter<T>(o: Option<T>, pred: impl FnOnce(&T) -> bool) -> Option<T> {
        match o { Some(v) => if pred(&v) { Some(v) } else { None }, None => None }
    }

    // ── Rust mirrors of core's slice algorithms (same structure as core.jtr) ────
    fn m_sl_find<T>(s: &[T], pred: impl Fn(&T) -> bool) -> Option<usize> {
        let mut i = 0;
        for x in s {
            if pred(x) {
                return Some(i);
            }
            i += 1;
        }
        None
    }
    /// Insertion sort by `less` — the exact structure of `core.sl_sort`.
    fn m_sl_sort_by<T: Clone>(s: &mut [T], less: impl Fn(&T, &T) -> bool) {
        let mut i = 1;
        while i < s.len() {
            let mut j = i;
            while j > 0 && less(&s[j], &s[j - 1]) {
                s.swap(j, j - 1);
                j -= 1;
            }
            i += 1;
        }
    }

    // ── Rust mirrors of core's integer parse/format (same structure as core.jtr) ─
    /// Mirror of `core.parse_i64`: optional sign, ASCII digits, **negative
    /// accumulation** (so `i64::MIN` fits) with overflow checked before each step.
    fn m_parse_i64(s: &str) -> Option<i64> {
        let b = s.as_bytes();
        if b.is_empty() {
            return None;
        }
        let (neg, mut i) = match b[0] {
            b'-' => (true, 1usize),
            b'+' => (false, 1usize),
            _ => (false, 0usize),
        };
        if i == b.len() {
            return None;
        }
        let mut acc: i64 = 0;
        while i < b.len() {
            let c = b[i];
            if !c.is_ascii_digit() {
                return None;
            }
            let d = (c - b'0') as i64;
            if neg {
                if acc < (i64::MIN + d) / 10 {
                    return None;
                }
                acc = acc * 10 - d;
            } else {
                if acc > (i64::MAX - d) / 10 {
                    return None;
                }
                acc = acc * 10 + d;
            }
            i += 1;
        }
        Some(acc)
    }

    /// Mirror of `core.format_i64`: sign + magnitude (`0u64.wrapping_sub` handles
    /// `i64::MIN`), digits emitted back-to-front.
    fn m_format_i64(n: i64) -> String {
        if n == 0 {
            return "0".to_string();
        }
        let (sign, mag) = if n < 0 {
            ("-", 0u64.wrapping_sub(n as u64))
        } else {
            ("", n as u64)
        };
        let mut digits: Vec<u8> = Vec::new();
        let mut x = mag;
        while x > 0 {
            digits.push(b'0' + (x % 10) as u8);
            x /= 10;
        }
        digits.reverse();
        format!("{sign}{}", std::str::from_utf8(&digits).unwrap())
    }

    // ── Rust mirrors of core's float-support primitives (same structure) ────────
    /// Mirror of `core.mul64`: the 64×64→128 product synthesized from 32-bit halves
    /// (`core` has no `u128`). Validated against Rust's native `u128` below.
    fn m_mul64(a: u64, b: u64) -> (u64, u64) {
        let mask = 0xFFFF_FFFFu64;
        let (al, ah, bl, bh) = (a & mask, a >> 32, b & mask, b >> 32);
        let ll = al * bl;
        let lh = al * bh;
        let hl = ah * bl;
        let hh = ah * bh;
        let mid = (ll >> 32) + (lh & mask) + (hl & mask);
        let lo = (ll & mask) | (mid << 32);
        let hi = hh + (lh >> 32) + (hl >> 32) + (mid >> 32);
        (hi, lo)
    }
    /// Mirror of `core.clz64`: shift-until-top-bit.
    fn m_clz64(x: u64) -> u32 {
        if x == 0 {
            return 64;
        }
        let mut v = x;
        let mut n = 0u32;
        while v & (1u64 << 63) == 0 {
            v <<= 1;
            n += 1;
        }
        n
    }

    // ── Rust mirrors of core's deterministic reductions (same structure) ────────
    fn m_f64_sum(s: &[f64]) -> f64 {
        let mut acc = 0.0;
        for &x in s {
            acc += x;
        }
        acc
    }
    /// Mirror of `core.f64_kahan_sum` — Neumaier compensated summation.
    fn m_f64_kahan(s: &[f64]) -> f64 {
        let mut sum = 0.0f64;
        let mut c = 0.0f64;
        for &x in s {
            let t = sum + x;
            if sum.abs() >= x.abs() {
                c += (sum - t) + x;
            } else {
                c += (x - t) + sum;
            }
            sum = t;
        }
        sum + c
    }
    /// Mirror of `core.f64_pairwise_sum` — fixed `len/2` split, leaves ≤ 8.
    fn m_f64_pairwise(s: &[f64]) -> f64 {
        let n = s.len();
        if n == 0 {
            return 0.0;
        }
        if n <= 8 {
            let mut acc = 0.0;
            for &x in s {
                acc += x;
            }
            return acc;
        }
        let mid = n / 2;
        m_f64_pairwise(&s[..mid]) + m_f64_pairwise(&s[mid..])
    }

    // ── Rust mirror of core's binned superaccumulator (same structure) ──────────
    /// Deposit `v`'s integer significand into its exponent bin (mirror of
    /// `core.binned_add`). Integer addition → order-independent bins.
    fn m_binned_add(bins: &mut [i64; 2048], v: f64) {
        let bits = v.to_bits();
        let sign = bits >> 63;
        let be = ((bits >> 52) & 0x7FF) as usize;
        let mant = bits & 0xFFFF_FFFF_FFFFF;
        let mut sigu = mant;
        if be != 0 {
            sigu = (1u64 << 52) | mant;
        }
        let mut sig = sigu as i64;
        if sign == 1 {
            sig = -sig;
        }
        // Cascade a carry up the exponents so no bin can overflow its i64 (mirror of
        // the carry in `core.binned_add`). Exact — preserves the accumulator value —
        // so it changes only the bin layout, never the rounded result.
        const THRESH: i64 = 1 << 53;
        let mut e = be;
        bins[e] = bins[e].wrapping_add(sig);
        while e < 2047 {
            let v = bins[e];
            if v.unsigned_abs() < THRESH as u64 {
                break;
            }
            let c = if e == 0 {
                bins[0] = 0; // bin 0 → bin 1 is 1:1 (shared ULP)
                v
            } else {
                let c = v / 2; // bin e → bin e+1 is 2:1
                bins[e] = v - c * 2;
                c
            };
            bins[e + 1] = bins[e + 1].wrapping_add(c);
            e += 1;
        }
    }
    // ── Correctly-rounded finalize (mirror of `core.binned_sum`) ────────────────
    // The bins are an *exact* big fixed-point number: X = Σ bins[e]·2^(e-1075), with
    // bin 0 sharing bin 1's ULP (2^-1074). Scaling by 2^1074 makes it the integer
    // Y = bins[0] + Σ_{e≥1} bins[e]·2^(e-1), so X = Y·2^-1074. We reconstruct |Y| as
    // a fixed-width unsigned bignum (split into non-negative Pos/Neg halves, then
    // subtract — no two's-complement sign-extension needed) and round once to
    // nearest-even. Max bit ≈ 2046 (top bitpos) + 63 (bin width) ≈ 2109, so 36
    // 64-bit limbs (2304 bits) clears it with margin.
    const M_NL: usize = 36;
    /// Add `u << shift` into an unsigned little-endian limb array, carrying upward.
    fn m_mag_add_shifted(acc: &mut [u64; M_NL], u: u64, shift: usize) {
        let li = shift / 64;
        let off = shift % 64;
        let lo = u << off;
        let hi = if off == 0 { 0 } else { u >> (64 - off) };
        m_add_at(acc, li, lo);
        m_add_at(acc, li + 1, hi);
    }
    fn m_add_at(acc: &mut [u64; M_NL], idx: usize, val: u64) {
        let mut carry = val;
        let mut i = idx;
        while carry != 0 && i < M_NL {
            let (s, c) = acc[i].overflowing_add(carry);
            acc[i] = s;
            carry = c as u64;
            i += 1;
        }
    }
    /// Compare two magnitudes (top-down): -1 if a<b, 0 if equal, 1 if a>b.
    fn m_mag_cmp(a: &[u64; M_NL], b: &[u64; M_NL]) -> i32 {
        for i in (0..M_NL).rev() {
            if a[i] != b[i] {
                return if a[i] < b[i] { -1 } else { 1 };
            }
        }
        0
    }
    /// big - small (caller guarantees big ≥ small), borrow-propagated.
    fn m_mag_sub(big: &[u64; M_NL], small: &[u64; M_NL]) -> [u64; M_NL] {
        let mut out = [0u64; M_NL];
        let mut borrow = 0u64;
        for i in 0..M_NL {
            let (d1, b1) = big[i].overflowing_sub(small[i]);
            let (d2, b2) = d1.overflowing_sub(borrow);
            out[i] = d2;
            borrow = (b1 as u64) | (b2 as u64);
        }
        out
    }
    fn m_bit(mag: &[u64; M_NL], i: usize) -> u64 {
        (mag[i / 64] >> (i % 64)) & 1
    }
    /// Extract `n` (≤53) bits of `mag` starting at bit `start`.
    fn m_extract(mag: &[u64; M_NL], start: usize, n: usize) -> u64 {
        let li = start / 64;
        let off = start % 64;
        let mut v = mag[li] >> off;
        if off != 0 && li + 1 < M_NL {
            v |= mag[li + 1] << (64 - off);
        }
        v & ((1u64 << n) - 1)
    }
    /// Is any bit of `mag` strictly below index `pos` set? (the sticky bit)
    fn m_any_below(mag: &[u64; M_NL], pos: usize) -> bool {
        let lp = pos / 64;
        let op = pos % 64;
        for i in 0..lp {
            if mag[i] != 0 {
                return true;
            }
        }
        op > 0 && (mag[lp] & ((1u64 << op) - 1)) != 0
    }
    /// Round `mag · 2^-1074` (mag a non-negative integer) to the nearest f64.
    fn m_round_mag(mag: &[u64; M_NL], neg: bool) -> f64 {
        // highest set bit
        let mut top = M_NL;
        while top > 0 && mag[top - 1] == 0 {
            top -= 1;
        }
        if top == 0 {
            return 0.0;
        }
        let hl = top - 1;
        let msb = hl * 64 + (63 - mag[hl].leading_zeros() as usize);
        let sgn = if neg { 1u64 << 63 } else { 0 };
        // msb ≤ 52: value < 2^-1021 and exactly representable (subnormal / smallest
        // normals). Compose by exact scaling of the (≤53-bit) integer.
        if msb <= 52 {
            return f64::from_bits(sgn) + (if neg { -1.0 } else { 1.0 }) * (mag[0] as f64)
                * f64::from_bits(1); // 2^-1074
        }
        let e = msb as i64 - 1074; // unbiased exponent of the leading bit
        let shift = msb - 52; // keep the 53 bits [shift, msb]
        let sig = m_extract(mag, shift, 53);
        let round_bit = m_bit(mag, shift - 1);
        let sticky = m_any_below(mag, shift - 1);
        let mut m = sig;
        if round_bit == 1 && (sticky || (m & 1) == 1) {
            m += 1;
        }
        let mut biased = e + 1023;
        if m == (1u64 << 53) {
            m >>= 1; // mantissa carried out → renormalize
            biased += 1;
        }
        if biased >= 2047 {
            return f64::from_bits(sgn | (2047u64 << 52)); // ±inf
        }
        f64::from_bits(sgn | ((biased as u64) << 52) | (m - (1u64 << 52)))
    }
    /// Reconstruct the bins' exact value and round it once to nearest-even.
    fn m_binned_round(bins: &[i64; 2048]) -> f64 {
        let mut pos = [0u64; M_NL];
        let mut neg = [0u64; M_NL];
        for (e, &b) in bins.iter().enumerate() {
            if b == 0 {
                continue;
            }
            let shift = if e == 0 { 0 } else { e - 1 };
            let u = (b as i128).unsigned_abs() as u64;
            if b > 0 {
                m_mag_add_shifted(&mut pos, u, shift);
            } else {
                m_mag_add_shifted(&mut neg, u, shift);
            }
        }
        match m_mag_cmp(&pos, &neg) {
            0 => 0.0,
            1 => m_round_mag(&m_mag_sub(&pos, &neg), false),
            _ => m_round_mag(&m_mag_sub(&neg, &pos), true),
        }
    }

    /// A self-contained generic slice-fold program at element type `prim` — the real
    /// compiler path the slice algorithms ride (subst through the for-loop / index).
    fn slice_fold_source(prim: &str) -> String {
        format!(
            "fn sl_fold(comptime T: type, read s: []T, take z: T, f: fn(T, T) -> T) -> T {{ var acc: T = z for x in s {{ acc = f(acc, x) }} return acc }}\n\
             fn add(a: {p}, b: {p}) -> {p} {{ return a + b }}\n\
             fn main() -> i32 {{ var p: *mut {p} = alloc({p}, 1) unsafe {{ p.* = 1 }} var s: []{p} = slice({p}, p, 1) return sl_fold({p}, s, 0, &add) as i32 }}",
            p = prim
        )
    }

    proptest! {
        // ── codegen layer (the real Jestyr compiler path) ──────────────────────

        /// A generic Option combinator monomorphizes with **no diagnostics** for any
        /// element type — the generic-enum-in-generic-fn codegen this work landed.
        /// (Teeth: dropping the `apply_subst`/tail-expected fixes reintroduces the
        /// "cannot infer the type arguments of generic enum" diagnostic, failing this.)
        #[test]
        fn core_option_combinator_compiles_clean_for_any_int_type(p in int_prim()) {
            let (_c, diags) = compile(&option_combinator_source(p));
            prop_assert_eq!(diags, 0, "combinator over {} must compile clean", p);
        }

        /// The same combinator program emits **byte-identical C** twice — no
        /// iteration-order leak in the new fn-type / generic-enum collectors.
        #[test]
        fn core_option_combinator_is_deterministic(p in int_prim()) {
            let src = option_combinator_source(p);
            prop_assert_eq!(compile(&src), compile(&src));
        }

        // ── laws as oracles (the contract the Jestyr combinators satisfy) ───────

        /// Functor identity: `map(id) == id`.
        #[test]
        fn option_functor_identity(x in any::<i32>(), is_some in any::<bool>()) {
            let o = if is_some { Some(x) } else { None };
            prop_assert_eq!(m_map(o, |v| v), o);
        }

        /// Functor composition: `map(g∘f) == map(f) then map(g)`.
        #[test]
        fn option_functor_composition(x in any::<i32>(), is_some in any::<bool>()) {
            let o = if is_some { Some(x) } else { None };
            let f = |v: i32| v.wrapping_add(1);
            let g = |v: i32| v.wrapping_mul(2);
            prop_assert_eq!(m_map(m_map(o, f), g), m_map(o, |v| g(f(v))));
        }

        /// `unwrap_or` selects the value on `some`, the default on `none`.
        #[test]
        fn option_unwrap_or_selects(x in any::<i32>(), d in any::<i32>(), is_some in any::<bool>()) {
            let o = if is_some { Some(x) } else { None };
            let expect = if is_some { x } else { d };
            prop_assert_eq!(m_unwrap_or(o, d), expect);
        }

        /// Bridge round-trip: `res_ok(ok_or(o, e)) == o` for every `o` and `e`.
        #[test]
        fn option_ok_or_then_res_ok_roundtrips(x in any::<i32>(), e in any::<i32>(), is_some in any::<bool>()) {
            let o = if is_some { Some(x) } else { None };
            prop_assert_eq!(m_res_ok(m_ok_or(o, e)), o);
        }

        /// Result functor composition over the ok value (the err passes through).
        #[test]
        fn result_map_composition(x in any::<i32>(), e in any::<i32>(), is_ok in any::<bool>()) {
            let r: Result<i32, i32> = if is_ok { Ok(x) } else { Err(e) };
            let f = |v: i32| v.wrapping_add(1);
            let g = |v: i32| v.wrapping_mul(2);
            prop_assert_eq!(m_res_map(m_res_map(r, f), g), m_res_map(r, |v| g(f(v))));
        }

        // ── monad laws (now that `and_then` lowers — the typedef reorder) ───────

        /// Monad left identity: `and_then(some(x), f) == f(x)`.
        #[test]
        fn option_monad_left_identity(x in any::<i32>()) {
            let f = |v: i32| if v > 0 { Some(v.wrapping_add(1)) } else { None };
            prop_assert_eq!(m_and_then(Some(x), f), f(x));
        }

        /// Monad right identity: `m.and_then(some) == m`.
        #[test]
        fn option_monad_right_identity(x in any::<i32>(), is_some in any::<bool>()) {
            let o = if is_some { Some(x) } else { None };
            prop_assert_eq!(m_and_then(o, Some), o);
        }

        /// Monad associativity: `m.and_then(f).and_then(g) == m.and_then(|x| f(x).and_then(g))`.
        #[test]
        fn option_monad_associativity(x in any::<i32>(), is_some in any::<bool>()) {
            let o = if is_some { Some(x) } else { None };
            let f = |v: i32| if v % 2 == 0 { Some(v.wrapping_add(1)) } else { None };
            let g = |v: i32| if v > 0 { Some(v.wrapping_mul(2)) } else { None };
            prop_assert_eq!(m_and_then(m_and_then(o, f), g), m_and_then(o, |v| m_and_then(f(v), g)));
        }

        /// `filter` keeps the value iff the predicate holds; `none` stays `none`.
        #[test]
        fn option_filter_keeps_iff_predicate(x in any::<i32>(), is_some in any::<bool>()) {
            let o = if is_some { Some(x) } else { None };
            let pred = |v: &i32| *v > 10;
            let expect = if is_some && x > 10 { Some(x) } else { None };
            prop_assert_eq!(m_filter(o, pred), expect);
        }

        // ── slice / iterator algorithm laws ────────────────────────────────────

        /// A generic slice fold compiles clean (subst through the `for`-loop / index)
        /// for any integer element type — the real compiler path. (Teeth: dropping the
        /// slice-`for` subst fix reintroduces an undefined `JestyrSlice_T`.)
        #[test]
        fn slice_fold_compiles_clean_for_any_int_type(p in int_prim()) {
            let (_c, diags) = compile(&slice_fold_source(p));
            prop_assert_eq!(diags, 0, "slice fold over {} must compile clean", p);
        }

        /// `find` returns the first matching index — agrees with `iter().position()`.
        #[test]
        fn slice_find_matches_position(xs in proptest::collection::vec(any::<i32>(), 0..32)) {
            prop_assert_eq!(m_sl_find(&xs, |&x| x > 0), xs.iter().position(|&x| x > 0));
        }

        /// `any`/`all` (expressed via the short-circuiting `find`) agree with the
        /// reference iterators: `any p == find(p).is_some`, `all p == find(¬p).is_none`.
        #[test]
        fn slice_any_all_match_reference(xs in proptest::collection::vec(any::<i32>(), 0..32)) {
            prop_assert_eq!(m_sl_find(&xs, |&x| x > 0).is_some(), xs.iter().any(|&x| x > 0));
            prop_assert_eq!(m_sl_find(&xs, |&x| !(x > 0)).is_none(), xs.iter().all(|&x| x > 0));
        }

        /// `sort` yields a **sorted permutation** of the input, **deterministically**.
        #[test]
        fn slice_sort_is_a_sorted_permutation(xs in proptest::collection::vec(any::<i32>(), 0..32)) {
            let mut a = xs.clone();
            m_sl_sort_by(&mut a, |x, y| x < y);
            prop_assert!(a.windows(2).all(|w| w[0] <= w[1]), "sorted (non-decreasing)");
            let mut b = xs.clone();
            b.sort();
            prop_assert_eq!(&a, &b, "a permutation of the input");
            let mut c = xs.clone();
            m_sl_sort_by(&mut c, |x, y| x < y);
            prop_assert_eq!(&a, &c, "deterministic: same input → same output");
        }

        /// `sort` is **stable**: among equal keys, input order is preserved.
        #[test]
        fn slice_sort_is_stable(keys in proptest::collection::vec(0u8..4, 0..24)) {
            let mut tagged: Vec<(u8, usize)> = keys.iter().cloned().zip(0..).collect();
            m_sl_sort_by(&mut tagged, |x, y| x.0 < y.0);  // compare keys only
            prop_assert!(tagged.windows(2).all(|w| w[0].0 <= w[1].0), "sorted by key");
            for w in tagged.windows(2) {
                if w[0].0 == w[1].0 {
                    prop_assert!(w[0].1 < w[1].1, "equal keys keep input order");
                }
            }
        }

        // ── number parse / format laws (the determinism deliverable, int side) ──

        /// The fundamental round-trip: `parse(format(x)) == x` for every `i64`.
        #[test]
        fn int_parse_format_roundtrips(x in any::<i64>()) {
            prop_assert_eq!(m_parse_i64(&m_format_i64(x)), Some(x));
        }

        /// `format_i64` is byte-identical to Rust's shortest decimal (`Display`) —
        /// locale-free and deterministic by construction.
        #[test]
        fn format_i64_matches_display(x in any::<i64>()) {
            prop_assert_eq!(m_format_i64(x), format!("{x}"));
        }

        /// Differential parse: on arbitrary sign/digit/letter soup, `parse_i64`
        /// agrees with Rust's correctly-implemented `str::parse::<i64>()` — same
        /// successes (incl. overflow → error) and same values.
        #[test]
        fn parse_i64_matches_rust(s in "[-+0-9a-f]{0,22}") {
            prop_assert_eq!(m_parse_i64(&s), s.parse::<i64>().ok());
        }

        /// Overflow is a *defined* error, never a wrap: one past `i64::MAX`/`MIN` and
        /// the obviously-too-long string all fail to parse.
        #[test]
        fn parse_i64_overflow_is_defined(_ in 0u8..1) {
            prop_assert_eq!(m_parse_i64("9223372036854775807"), Some(i64::MAX));
            prop_assert_eq!(m_parse_i64("9223372036854775808"), None);
            prop_assert_eq!(m_parse_i64("-9223372036854775808"), Some(i64::MIN));
            prop_assert_eq!(m_parse_i64("-9223372036854775809"), None);
            prop_assert_eq!(m_parse_i64("99999999999999999999"), None);
        }

        // ── float-support primitives (toward correctly-rounded float parse/format) ─

        /// The synthesized 64×64→128 product (no `u128` in `core`) equals the true
        /// product computed with Rust's native `u128`, for any operands. This is the
        /// crux primitive Eisel–Lemire / Ryū multiply through.
        #[test]
        fn mul64_matches_u128(a in any::<u64>(), b in any::<u64>()) {
            let p = (a as u128) * (b as u128);
            let (hi, lo) = m_mul64(a, b);
            prop_assert_eq!(hi, (p >> 64) as u64);
            prop_assert_eq!(lo, p as u64);
        }

        /// `clz64` agrees with the hardware `leading_zeros` for any input.
        #[test]
        fn clz64_matches_builtin(x in any::<u64>()) {
            prop_assert_eq!(m_clz64(x), x.leading_zeros());
        }

        // ── deterministic reductions (CJC-inspired numerics, serial tier) ───────

        /// On **exactly-representable** inputs (small integers as `f64`, no rounding),
        /// all three reductions equal the true sum — they only diverge under rounding.
        #[test]
        fn reductions_agree_on_exact_inputs(xs in proptest::collection::vec(-1000i32..1000, 0..64)) {
            let fs: Vec<f64> = xs.iter().map(|&x| x as f64).collect();
            let exact: f64 = xs.iter().map(|&x| x as i64).sum::<i64>() as f64;
            prop_assert_eq!(m_f64_sum(&fs), exact);
            prop_assert_eq!(m_f64_kahan(&fs), exact);
            prop_assert_eq!(m_f64_pairwise(&fs), exact);
        }

        /// Each reduction is a pure, **run-deterministic** function — same input,
        /// bit-identical output every call (no FMA/reassociation; the FP flags are
        /// locked, and the algorithm fixes the order).
        #[test]
        fn reductions_are_run_deterministic(xs in proptest::collection::vec(-1e6f64..1e6, 0..64)) {
            prop_assert_eq!(m_f64_kahan(&xs).to_bits(), m_f64_kahan(&xs).to_bits());
            prop_assert_eq!(m_f64_pairwise(&xs).to_bits(), m_f64_pairwise(&xs).to_bits());
        }

        /// Compensated summation recovers precision naive summation throws away under
        /// catastrophic cancellation: `[1, 1e100, 1, -1e100]` sums to `2`, but naive
        /// loses the small terms and returns `0`.
        #[test]
        fn kahan_recovers_cancellation(_ in 0u8..1) {
            let xs = [1.0, 1e100, 1.0, -1e100];
            prop_assert_eq!(m_f64_kahan(&xs), 2.0);
            prop_assert_eq!(m_f64_sum(&xs), 0.0);
        }

        // ── binned superaccumulator — the chunk-count-independent reduction ─────

        /// **The headline determinism property:** the binned sum is **bit-identical**
        /// however the data is split across chunks (accumulate per chunk, merge bins,
        /// finalize == accumulate the whole). Integer bins make this true *by
        /// construction* — the property naive/Kahan/pairwise summation cannot give.
        #[test]
        fn binned_sum_is_chunk_independent(
            xs in proptest::collection::vec(-1e6f64..1e6, 0..96),
            split in 0usize..96,
        ) {
            let mut whole = [0i64; 2048];
            for &x in &xs {
                m_binned_add(&mut whole, x);
            }
            let s = split.min(xs.len());
            let mut a = [0i64; 2048];
            let mut b = [0i64; 2048];
            for &x in &xs[..s] {
                m_binned_add(&mut a, x);
            }
            for &x in &xs[s..] {
                m_binned_add(&mut b, x);
            }
            for i in 0..2048 {
                a[i] = a[i].wrapping_add(b[i]); // merge
            }
            prop_assert_eq!(m_binned_round(&whole).to_bits(), m_binned_round(&a).to_bits());
        }

        /// **The `par_reduce` determinism property:** for each deterministic integer
        /// reduction (sum/min/max/xor), folding the whole slice equals folding
        /// arbitrary disjoint chunks and merging them with the same op — for ANY
        /// split into `nchunks` pieces. This is exactly what `core.par_reduce` does
        /// (each worker folds a chunk, `combine` merges), so its result is
        /// bit-identical to `serial_reduce` regardless of the chunk split or thread
        /// schedule. True by construction: integer +/min/max/xor are associative AND
        /// commutative. Inputs are bounded so the sum cannot overflow (where i64 `+`
        /// would otherwise leave the exact, associative regime).
        #[test]
        fn par_reduce_is_split_independent(
            xs in proptest::collection::vec(-1_000_000i64..1_000_000, 0..200),
            nchunks in 1usize..8,
        ) {
            // (identity, op) for each built-in. `wrapping_add` models i64 `+`; inputs
            // are bounded so no wrap actually occurs (the exact regime).
            let ops: [(i64, fn(i64, i64) -> i64); 4] = [
                (0, |a, b| a.wrapping_add(b)),
                (i64::MAX, |a, b| a.min(b)),
                (i64::MIN, |a, b| a.max(b)),
                (0, |a, b| a ^ b),
            ];
            for (ident, op) in ops {
                let serial = xs.iter().fold(ident, |acc, &x| op(acc, x));
                // Split xs into `nchunks` near-equal pieces, fold each, merge.
                let chunk = xs.len().div_ceil(nchunks).max(1);
                let merged = xs
                    .chunks(chunk)
                    .map(|c| c.iter().fold(ident, |acc, &x| op(acc, x)))
                    .fold(ident, |acc, part| op(acc, part));
                prop_assert_eq!(serial, merged, "split-dependent result for a deterministic reduction");
            }
        }

        /// On exactly-representable inputs the binned sum equals the true sum.
        #[test]
        fn binned_sum_is_exact_on_representable_inputs(
            xs in proptest::collection::vec(-1000i32..1000, 0..96),
        ) {
            let mut bins = [0i64; 2048];
            for &x in &xs {
                m_binned_add(&mut bins, x as f64);
            }
            let exact = xs.iter().map(|&x| x as i64).sum::<i64>() as f64;
            prop_assert_eq!(m_binned_round(&bins), exact);
        }

        /// **The correctly-rounded finalize is correctly rounded.** Against an
        /// independent oracle — sum the inputs' significands exactly into an `i128`
        /// at the finest common scale, then let `i128 as f64` (a correctly-rounded
        /// conversion) plus exact power-of-two scaling produce the reference result.
        /// Inputs are `m·2^p` with bounded exponent so the exact sum fits in `i128`
        /// and the result stays in the normal range (so the scaling can't double-
        /// round). Full-width significands make summation genuinely round.
        /// (Teeth: changing the tie rule to truncation — drop the `(m & 1)` term —
        /// or off-by-one in `shift`/`msb` makes the bits mismatch the oracle.)
        #[test]
        fn binned_round_is_correctly_rounded(
            mps in proptest::collection::vec(
                ((-(1i64 << 53))..(1i64 << 53), -6i32..6i32), 0..64),
        ) {
            let xs: Vec<f64> = mps.iter().map(|&(m, p)| (m as f64) * 2f64.powi(p)).collect();
            // independent oracle: exact i128 sum at the finest scale, then a
            // correctly-rounded i128→f64 conversion times an exact power of two.
            // Zeros contribute nothing and are dropped (they would otherwise pin
            // kmin to the subnormal floor and force the whole case to be skipped).
            let mut acc: i128 = 0;
            let mut kmin = i64::MAX;
            let parts: Vec<(i128, i64)> = xs.iter().filter(|&&x| x != 0.0).map(|&x| {
                let bits = x.to_bits();
                let be = ((bits >> 52) & 0x7FF) as i64;
                let mant = (bits & 0xFFFF_FFFF_FFFFF) as i128;
                let sigu = if be != 0 { (1i128 << 52) | mant } else { mant };
                let sig = if (bits >> 63) == 1 { -sigu } else { sigu };
                let k = if be == 0 { -1074 } else { be - 1075 };
                (sig, k)
            }).collect();
            for &(_, k) in &parts { kmin = kmin.min(k); }
            let mut ok = true;
            for &(sig, k) in &parts {
                let sh = (k - kmin) as u32;
                match sig.checked_shl(sh).and_then(|t| acc.checked_add(t)) {
                    Some(v) => acc = v,
                    None => { ok = false; break; }
                }
            }
            // skip pathological dynamic ranges that overflow the i128 oracle
            prop_assume!(ok && kmin != i64::MAX);
            let oracle = (acc as f64) * 2f64.powi(kmin as i32);
            prop_assume!(oracle.is_finite() && (oracle == 0.0 || oracle.abs() >= f64::MIN_POSITIVE));

            let mut bins = [0i64; 2048];
            for &x in &xs {
                m_binned_add(&mut bins, x);
            }
            prop_assert_eq!(m_binned_round(&bins).to_bits(), oracle.to_bits());
        }

        /// **The add-time carry lifts the per-bin overflow bound.** Depositing the
        /// same value `n > 2^10` times would wrap a single `i64` bin (≈2^10 max-
        /// significand adds reach 2^63) — the documented old limitation. With the
        /// cascading carry the accumulator stays exact, so (a) the sum is still
        /// **bit-identical** however it is chunked/merged, and (b) it equals the
        /// correctly-rounded true total `n·base`. (Teeth: dropping the carry from
        /// `m_binned_add` wraps the bin — both assertions then fail, since whole and
        /// chunked wrap differently and neither matches the oracle.)
        #[test]
        fn binned_handles_per_bin_overflow(
            m in 1i64..(1i64 << 53),
            p in -6i32..6,
            n in 1100usize..4000,
            split in 0usize..4000,
        ) {
            let base = (m as f64) * 2f64.powi(p);
            let mut whole = [0i64; 2048];
            for _ in 0..n {
                m_binned_add(&mut whole, base);
            }
            let s = split.min(n);
            let mut a = [0i64; 2048];
            let mut b = [0i64; 2048];
            for _ in 0..s {
                m_binned_add(&mut a, base);
            }
            for _ in s..n {
                m_binned_add(&mut b, base);
            }
            for i in 0..2048 {
                a[i] = a[i].wrapping_add(b[i]); // merge
            }
            // (a) chunk-independent even though a single bin overflowed
            prop_assert_eq!(m_binned_round(&whole).to_bits(), m_binned_round(&a).to_bits());
            // (b) correct: the exact total n·base rounded once to nearest
            let bits = base.to_bits();
            let be = ((bits >> 52) & 0x7FF) as i64;
            let mant = (bits & 0xFFFF_FFFF_FFFFF) as i128;
            let sig0 = if be != 0 { (1i128 << 52) | mant } else { mant };
            let k0 = if be == 0 { -1074 } else { be - 1075 };
            let oracle = (sig0 * n as i128) as f64 * 2f64.powi(k0 as i32);
            prop_assume!(oracle.is_finite() && (oracle == 0.0 || oracle.abs() >= f64::MIN_POSITIVE));
            prop_assert_eq!(m_binned_round(&whole).to_bits(), oracle.to_bits());
        }
    }
}

/// **Workstream Q (data parallelism), tier 1 — `parallel.par_map` / `par_scan`.**
/// The other two SOACs beside `core.par_reduce`. These pin the determinism guarantee
/// for each: `par_map` is embarrassingly parallel (output[i] depends only on input[i]),
/// so any split is identical to serial *by construction*; `par_scan` is the subtle one
/// — a left-to-right prefix scan that parallelizes via the two-pass algorithm, whose
/// split-independence rests entirely on the operator being **associative**. Both
/// mirrors are toolchain-free (the gcc/thread proof is `c_oracle::par_soac_demo`).
#[cfg(test)]
mod parallel_props {
    use proptest::prelude::*;

    /// The four associative built-in ops, with their identities (matching
    /// `parallel.jtr`): 0 sum/xor, i64::MAX min, i64::MIN max.
    fn op(tag: u8, a: i64, b: i64) -> i64 {
        match tag {
            1 => a.min(b),
            2 => a.max(b),
            3 => a ^ b,
            _ => a.wrapping_add(b),
        }
    }
    fn identity(tag: u8) -> i64 {
        match tag {
            1 => i64::MAX,
            2 => i64::MIN,
            _ => 0,
        }
    }

    /// Serial inclusive scan: `out[i] = identity ⊕ s[0] ⊕ … ⊕ s[i]` (the oracle).
    fn serial_scan(xs: &[i64], tag: u8) -> Vec<i64> {
        let mut run = identity(tag);
        let mut out = Vec::with_capacity(xs.len());
        for &x in xs {
            run = op(tag, run, x);
            out.push(run);
        }
        out
    }

    /// The two-pass parallel scan, mirroring `parallel.par_scan` over an *arbitrary*
    /// partition: (1) reduce each chunk to its total; (2) exclusive-prefix the totals
    /// into per-chunk seeds; (3) scan each chunk seeded with its prefix.
    fn two_pass_scan(xs: &[i64], bounds: &[usize], tag: u8) -> Vec<i64> {
        // Pass 1: chunk totals.
        let mut totals = Vec::new();
        for w in bounds.windows(2) {
            let mut acc = identity(tag);
            for &x in &xs[w[0]..w[1]] {
                acc = op(tag, acc, x);
            }
            totals.push(acc);
        }
        // Exclusive prefix → seeds.
        let mut seeds = Vec::with_capacity(totals.len());
        let mut pre = identity(tag);
        for &t in &totals {
            seeds.push(pre);
            pre = op(tag, pre, t);
        }
        // Pass 2: scan each chunk seeded with its prefix.
        let mut out = vec![0i64; xs.len()];
        for (ci, w) in bounds.windows(2).enumerate() {
            let mut run = seeds[ci];
            for i in w[0]..w[1] {
                run = op(tag, run, xs[i]);
                out[i] = run;
            }
        }
        out
    }

    /// Build an arbitrary ordered partition of `0..len` from cut points.
    fn partition(cuts: &[usize], len: usize) -> Vec<usize> {
        let mut b: Vec<usize> = cuts.iter().map(|&c| c.min(len)).collect();
        b.push(0);
        b.push(len);
        b.sort_unstable();
        b.dedup();
        b
    }

    proptest! {
        /// **par_scan determinism:** the two-pass chunked scan equals the serial
        /// inclusive scan for *every* partition and every associative built-in op —
        /// the bit-identical-across-worker-counts guarantee. This holds only because
        /// the op is associative; it is why a non-associative float `+` scan is not
        /// offered. (Teeth: a non-associative op — e.g. saturating-then-wrapping mix —
        /// breaks this immediately.)
        #[test]
        fn par_scan_is_split_independent(
            xs in proptest::collection::vec(any::<i64>(), 0..96),
            cuts in proptest::collection::vec(0usize..96, 0..6),
            tag in 0u8..4,
        ) {
            let bounds = partition(&cuts, xs.len());
            prop_assert_eq!(serial_scan(&xs, tag), two_pass_scan(&xs, &bounds, tag));
        }

        /// **par_map determinism:** a chunked element-wise map equals the whole-slice
        /// map for any partition — trivially, since each output element depends only on
        /// its own input. (`f(x) = x*x` as a representative pure map.)
        #[test]
        fn par_map_is_split_independent(
            xs in proptest::collection::vec(-3_000_000_000i64..3_000_000_000, 0..96),
            cuts in proptest::collection::vec(0usize..96, 0..6),
        ) {
            let f = |x: i64| x.wrapping_mul(x);
            let whole: Vec<i64> = xs.iter().map(|&x| f(x)).collect();
            let bounds = partition(&cuts, xs.len());
            let mut chunked = vec![0i64; xs.len()];
            for w in bounds.windows(2) {
                for i in w[0]..w[1] {
                    chunked[i] = f(xs[i]);
                }
            }
            prop_assert_eq!(whole, chunked);
        }
    }
}

/// **Workstream Q — `examples/std/parallel.jtr` + `par_soac.jtr` compile clean.**
/// Toolchain-free guard (no gcc): the SOAC module and its demo load, type-check, and
/// pass the ownership/escape checker — in particular the spawned workers (a `read`
/// slice + raw `*mut i64` + an `fn` pointer) are accepted while the disjoint-region
/// writes stay race-free. The end-to-end thread run is `c_oracle::par_soac_demo`.
#[cfg(test)]
#[test]
fn par_soac_example_compiles_clean() {
    let prog = crate::module::load("examples/std/par_soac.jtr");
    assert!(!prog.diags.iter().any(|d| d.is_error()), "load errors: {:?}", prog.diags);
    let (info, td) = crate::typeck::check_program(&prog.ast, &prog.modules);
    assert!(!td.iter().any(|d| d.is_error()), "typeck errors: {:?}", td);
    let ed = crate::escape::check(&prog.ast, &info);
    assert!(!ed.iter().any(|d| d.is_error()), "escape errors: {:?}", ed);
}

/// **Workstream Q — the work-span cost model (`@span`).** The compiler computes a
/// function's parallel *span* (depth) from its loop structure and checks it against
/// the declared `@span(...)`: a sequential loop is O(n), a deterministic `par for …
/// reduce(r)` is O(log n). The headline property is **rejection soundness** — a
/// serialized reduction (a `par for` rewritten as `for`) overshoots its declared span
/// and becomes a compile error. The check runs in the parser (`attrs::validate_fn`),
/// so these assert over the parse diagnostics — toolchain-free.
#[cfg(test)]
mod cost_model {
    use super::*;

    fn span_diags(src: &str) -> Vec<String> {
        let (tokens, _) = Lexer::new(src).tokenize();
        let (_ast, pd) = Parser::new(src, tokens).parse();
        pd.iter().map(|d| d.message.clone()).collect()
    }
    fn has_span_violation(src: &str) -> bool {
        span_diags(src).iter().any(|m| m.contains("is violated"))
    }
    fn clean(src: &str) -> bool {
        span_diags(src).iter().all(|m| !m.contains("@span") && !m.contains("span class"))
    }

    /// `@span(log)` accepts a `par for` reduction (O(log n)) but **rejects** the same
    /// reduction written as a sequential `for` (O(n)) — the serialization guard.
    #[test]
    fn span_log_accepts_par_for_rejects_serial() {
        assert!(
            clean("@span(log) fn f(s: []i64) -> i64 { par for x in s reduce(red()) { x } }"),
            "a par-for reduction has span O(log n) → satisfies @span(log)"
        );
        assert!(
            has_span_violation(
                "@span(log) fn f(s: []i64) -> i64 { var a: i64 = 0 for x in s { a = a + x } return a }"
            ),
            "the same fold written sequentially is O(n) → must violate @span(log)"
        );
    }

    /// Span classes compose: a single loop is linear, nested loops are quadratic, a
    /// loop-free body is constant. The declared class must not be undershot.
    #[test]
    fn span_classes_compose_and_check() {
        assert!(clean("@span(linear) fn f(s: []i64) { for x in s { } }"), "one loop is O(n)");
        assert!(
            span_diags("@span(linear) fn f(s: []i64) { for x in s { for y in s { } } }")
                .iter()
                .any(|m| m.contains("is violated") && m.contains("quadratic")),
            "nested loops are O(n²) → violate @span(linear)"
        );
        assert!(clean("@span(quadratic) fn f(s: []i64) { for x in s { for y in s { } } }"), "and satisfy @span(quadratic)");
        assert!(clean("@span(constant) fn f() -> i64 { return 0 }"), "a loop-free body is O(1)");
        assert!(
            has_span_violation("@span(constant) fn f(s: []i64) { for x in s { } }"),
            "a loop overshoots @span(constant)"
        );
    }

    /// A declaration *looser* than the actual span is allowed (like weakening a
    /// `requires`); an unknown class name is a clean error.
    #[test]
    fn span_looser_ok_unknown_class_errors() {
        // par-for is O(log n); declaring the looser O(n) is fine.
        assert!(clean("@span(linear) fn f(s: []i64) -> i64 { par for x in s reduce(red()) { x } }"));
        assert!(
            span_diags("@span(bogus) fn f() -> i64 { return 0 }")
                .iter()
                .any(|m| m.contains("unknown span class")),
            "an unrecognized class is rejected"
        );
    }

    /// The shipped demo compiles clean: both `@span(log)` (the `par for`) and
    /// `@span(linear)` (its serial reference) are satisfied.
    #[test]
    fn par_cost_example_compiles_clean() {
        let prog = crate::module::load("examples/std/par_cost.jtr");
        assert!(!prog.diags.iter().any(|d| d.is_error()), "load errors: {:?}", prog.diags);
        let (info, td) = crate::typeck::check_program(&prog.ast, &prog.modules);
        assert!(!td.iter().any(|d| d.is_error()), "typeck errors: {:?}", td);
        let ed = crate::escape::check(&prog.ast, &info);
        assert!(!ed.iter().any(|d| d.is_error()), "escape errors: {:?}", ed);
    }
}

/// **Workstream Q — `par for` over any integer width.**
///
/// The reduction stays `i64` (the declared deterministic operators are exactly
/// associative there, which is the whole determinism argument), but the loop no longer
/// has to *iterate* `i64`. That is possible because `emit_par_for` is map-then-reduce:
/// it fills an `int64_t` buffer by running the body per element and hands only that to
/// `core.par_reduce`, so the engine never sees the source slice and needs no generic
/// `spawn` — the constraint the handoff had assumed was blocking.
///
/// Why it matters beyond ergonomics: a later SIMD lowering fills lanes with the
/// *element* type, so a body over `i32` gets twice the lanes of one over `i64` and `u8`
/// eight times. This is Q-S2's prerequisite.
///
/// These are reference-side: type-level accept/reject plus the emitted shape. The
/// end-to-end gcc run arrives with the corpus file, which is also what triggers the
/// port mirror — the same ordering G1 and Q-S1 used.
#[cfg(test)]
mod par_for_width {
    use super::*;

    /// A whole program with a locally-declared reduction (the check is on the callee's
    /// NAME, so this satisfies it without pulling in `core`).
    fn prog(elem: &str, body: &str) -> String {
        format!(
            "fn sum_reduction() -> i64 {{ return 0 }}\n\
             fn f(read s: []{elem}) -> i64 {{\n    return par for x in s reduce(sum_reduction()) {{ {body} }}\n}}\n"
        )
    }

    fn errors(src: &str) -> Vec<String> {
        let (tokens, _) = Lexer::new(src).tokenize();
        let (ast, _) = Parser::new(src, tokens).parse();
        let (_info, d) = crate::typeck::check(&ast);
        d.iter().filter(|x| x.is_error()).map(|x| x.message.clone()).collect()
    }

    fn emitted(src: &str) -> String {
        let (tokens, _) = Lexer::new(src).tokenize();
        let (ast, _) = Parser::new(src, tokens).parse();
        let (info, _) = crate::typeck::check(&ast);
        crate::cgen::emit(&ast, &info).0
    }

    /// Every integer width is accepted as the element type.
    #[test]
    fn every_integer_element_type_is_accepted() {
        for t in ["i8", "i16", "i32", "i64", "isize", "u8", "u16", "u32", "u64", "usize"] {
            let e = errors(&prog(t, "x as i64"));
            assert!(e.is_empty(), "`[]{t}` should be iterable by `par for`: {e:?}");
        }
    }

    /// The loop variable carries the ELEMENT's type, so the body computes in that width
    /// — which is exactly what gives a later lowering its lane count.
    #[test]
    fn the_loop_variable_has_the_element_type() {
        let c = emitted(&prog("i32", "x * x"));
        assert!(c.contains("int32_t j_x = _pf0.ptr[_pi0];"), "loop var should be int32_t:\n{c}");
        // …and the contribution is widened exactly once, on the way into the buffer.
        assert!(c.contains("_pm0[_pi0] = (int64_t)((j_x * j_x));"), "contribution should widen once:\n{c}");
    }

    /// The `i64` case must emit what it always did — that byte-identity is what keeps
    /// this increment off the corpus, the concat, the seed and every attested hash.
    #[test]
    fn the_i64_lowering_is_unchanged() {
        let c = emitted(&prog("i64", "x * x"));
        assert!(c.contains("int64_t j_x = _pf0.ptr[_pi0];"), "{c}");
        // No cast: the body is already the reduction's type.
        assert!(c.contains("_pm0[_pi0] = (j_x * j_x);"), "an i64 body must not gain a cast:\n{c}");
        assert!(!c.contains("(int64_t)((j_x * j_x))"), "the i64 path must be byte-identical:\n{c}");
    }

    /// The source slice keeps its own type while the reduce call takes the `i64` buffer
    /// — two different slice types in one lowering, which is the thing that makes the
    /// widening free.
    #[test]
    fn the_source_and_the_reduced_buffer_have_different_slice_types() {
        let c = emitted(&prog("u8", "x as i64"));
        assert!(c.contains("JestyrSlice_u8 _pf0 = "), "source keeps its element type:\n{c}");
        assert!(c.contains("(JestyrSlice_i64){ _pm0, _pf0.len }"), "the engine still takes []i64:\n{c}");
    }

    /// A non-integer element or contribution is refused, not coerced. The reduction is
    /// defined on integers; inventing a conversion is what this compiler does not do —
    /// and a float would take the determinism argument with it.
    #[test]
    fn non_integer_elements_and_bodies_are_refused() {
        let e = errors(&prog("f64", "x as i64"));
        assert!(
            e.iter().any(|m| m.contains("slice of any integer type")),
            "a float element type must be refused: {e:?}"
        );
        let e2 = errors(&prog("i32", "\"nope\""));
        assert!(
            e2.iter().any(|m| m.contains("must produce an integer")),
            "a non-integer contribution must be refused: {e2:?}"
        );
    }

    /// **Q-S2: `@simd` now LOWERS.** A certified `par for` inside an `@simd` function
    /// emits a vector head plus a scalar remainder; everything else is untouched.
    #[test]
    fn simd_lowering_emits_a_vector_head_and_a_scalar_remainder() {
        let src = "fn sum_reduction() -> i64 { return 0 }\n\
                   @simd fn f(read s: []i32) -> i64 { return par for x in s reduce(sum_reduction()) { x * x } }\n";
        let c = emitted(src);
        assert!(
            c.contains("typedef int32_t JestyrVec_i32 __attribute__((vector_size(32)));"),
            "a vector typedef must be emitted:\n{c}"
        );
        // 32 bytes / 4 = 8 lanes for i32 — the width Q-W1 unlocked.
        assert!(c.contains("_pi0 + 8 <= _pf0.len"), "i32 should give 8 lanes:\n{c}");
        assert!(c.contains("memcpy(&_pv0, _pf0.ptr + _pi0, sizeof(JestyrVec_i32))"), "{c}");
        // The remainder is SCALAR — the whole point of Q-S1's harness bug.
        assert!(
            c.contains("for (; _pi0 < _pf0.len; _pi0++) { int32_t j_x = _pf0.ptr[_pi0];"),
            "the remainder must stay scalar:\n{c}"
        );
    }

    /// Opt-in: without `@simd` the C is exactly what it was, which is what keeps this
    /// increment off the corpus, the concat, the seed and every attested hash.
    #[test]
    fn without_the_attribute_nothing_changes() {
        let base = "fn sum_reduction() -> i64 { return 0 }\n\
                    fn f(read s: []i32) -> i64 { return par for x in s reduce(sum_reduction()) { x * x } }\n";
        let c = emitted(base);
        assert!(!c.contains("JestyrVec_"), "an unannotated program must emit no vectors:\n{c}");
        assert!(!c.contains("memcpy(&_pv0"), "{c}");
    }

    /// A lane width per element type — 4 for `i64`, 8 for `i32`, 32 for `u8`. This is
    /// what Q-W1 was for.
    #[test]
    fn lane_count_follows_the_element_type() {
        for (t, w) in [("i64", 4), ("i32", 8), ("u8", 32)] {
            let src = format!(
                "fn sum_reduction() -> i64 {{ return 0 }}\n\
                 @simd fn f(read s: []{t}) -> i64 {{ return par for x in s reduce(sum_reduction()) {{ x }} }}\n"
            );
            let c = emitted(&src);
            assert!(c.contains(&format!("_pi0 + {w} <= _pf0.len")), "`{t}` should give {w} lanes:\n{c}");
        }
    }

    /// A select DOES lower to a mask blend in the vector half — the form GNU vector
    /// semantics force, since `?:` is not defined on vectors.
    #[test]
    fn a_select_becomes_a_mask_blend_in_the_vector_half() {
        let src = "fn sum_reduction() -> i64 { return 0 }\n\
                   @simd fn f(read s: []i32) -> i64 { return par for x in s reduce(sum_reduction()) { if x > 0 { x } else { 0 - x } } }\n";
        let c = emitted(src);
        assert!(c.contains("& ((_pv0 > 0))"), "the vector half must blend on a mask:\n{c}");
        assert!(c.contains("~((_pv0 > 0))"), "the else side must be masked by the complement:
{c}");
    }

    /// **The pass and the backend now agree about the select — Q-S2b.**
    ///
    /// `simd::classify` certifies `if c { a } else { b }`, and until this increment cgen
    /// refused it in value position, so the vector head compiled and the scalar remainder
    /// did not. That made the pass *optimistic relative to the backend* in exactly one
    /// place, contradicting its own documented "conservative, never optimistic" claim.
    ///
    /// Closed the better way: cgen now lowers a value-position `if` whose arms are single
    /// tail expressions to C's conditional operator. The two halves therefore agree, and
    /// the fix reaches well past SIMD — `let a = if c { x } else { y }` works generally.
    #[test]
    fn a_value_position_if_now_lowers_in_both_halves() {
        let src = "fn sum_reduction() -> i64 { return 0 }\n\
                   @simd fn f(read s: []i32) -> i64 { return par for x in s reduce(sum_reduction()) { if x > 0 { x } else { 0 - x } } }\n";
        let (tokens, _) = Lexer::new(src).tokenize();
        let (ast, _) = Parser::new(src, tokens).parse();
        assert!(crate::simd::analyze(&ast)[0].verdict.is_legal(), "classify certifies a select");
        let (info, _) = crate::typeck::check(&ast);
        let (c, cd) = crate::cgen::emit(&ast, &info);
        assert!(
            !cd.iter().any(|d| d.is_error()),
            "cgen must now lower it: {:?}",
            cd.iter().map(|d| d.message.clone()).collect::<Vec<_>>()
        );
        // The vector half blends on a mask; the scalar remainder uses the conditional.
        assert!(c.contains("& ((_pv0 > 0))"), "vector half must blend:\n{c}");
        assert!(c.contains("((j_x > 0)) ? (j_x) : ((0 - j_x))"), "remainder must use `?:`:\n{c}");
    }

    /// The general win that came with it: an `if` used as a value outside any `par for`,
    /// including an else-if chain, which lowers as nested conditionals.
    #[test]
    fn a_value_position_if_works_outside_simd_too() {
        let src = "fn g(x: i32) -> i32 { let a: i32 = if x > 0 { x } else { 0 - x }\n return a }\n\
                   fn h(x: i32) -> i32 { let b: i32 = if x > 10 { 1 } else if x > 5 { 2 } else { 3 }\n return b }\n";
        let (tokens, _) = Lexer::new(src).tokenize();
        let (ast, _) = Parser::new(src, tokens).parse();
        let (info, _) = crate::typeck::check(&ast);
        let (c, cd) = crate::cgen::emit(&ast, &info);
        assert!(!cd.iter().any(|d| d.is_error()), "{:?}", cd);
        assert!(
            c.contains("int32_t j_a = ") && c.contains("? (j_x) : ((0 - j_x))"),
            "a value-position if must lower:\n{c}"
        );
        assert!(
            c.contains("? (1) : (") && c.contains("? (2) : (3)"),
            "an else-if chain nests as conditionals:\n{c}"
        );
    }

    /// An arm carrying statements still gets the old diagnostic — that is the case the
    /// deferred "statement-expression with drop-safe spilling" is for, and pretending
    /// otherwise would be the unsafe half of the fix.
    #[test]
    fn an_arm_with_statements_is_still_refused() {
        let src = "fn g(x: i32) -> i32 { let a: i32 = if x > 0 { let t: i32 = x * 2\n t } else { 0 }\n return a }\n";
        let (tokens, _) = Lexer::new(src).tokenize();
        let (ast, _) = Parser::new(src, tokens).parse();
        let (info, _) = crate::typeck::check(&ast);
        let (_c, cd) = crate::cgen::emit(&ast, &info);
        assert!(
            cd.iter().any(|d| d.message.contains("only supported in statement or return position")),
            "a multi-statement arm must still be diagnosed: {:?}",
            cd.iter().map(|d| d.message.clone()).collect::<Vec<_>>()
        );
    }

    /// The shipped demo type-checks and passes the escape checker — the narrow-width
    /// `par for` in a real multi-module program, not a fixture string.
    #[test]
    fn par_for_width_example_compiles_clean() {
        let prog = crate::module::load("examples/std/par_for_width.jtr");
        assert!(!prog.diags.iter().any(|d| d.is_error()), "load errors: {:?}", prog.diags);
        let (info, td) = crate::typeck::check_program(&prog.ast, &prog.modules);
        assert!(!td.iter().any(|d| d.is_error()), "typeck errors: {:?}", td);
        let ed = crate::escape::check(&prog.ast, &info);
        assert!(!ed.iter().any(|d| d.is_error()), "escape errors: {:?}", ed);
    }

    /// Widening the element type does not widen what may reduce: the declared
    /// deterministic set is still the only thing accepted.
    #[test]
    fn a_non_declared_reduction_is_still_rejected() {
        let src = "fn my_reduction() -> i64 { return 0 }\n\
                   fn f(read s: []i32) -> i64 { return par for x in s reduce(my_reduction()) { x as i64 } }\n";
        assert!(
            errors(src).iter().any(|m| m.contains("declared deterministic reduction")),
            "the checked guarantee must survive the widening"
        );
    }
}

/// **Workstream Q — SIMD legality (`@simd`).** The compiler decides whether a
/// `par for` body may be evaluated a SIMD lane at a time without changing a bit, and
/// `@simd` is the *checked declaration* that it can be. Like `@span`, the check runs
/// in the parser (`attrs::validate_fn` → `simd::classify`), so these assert over parse
/// diagnostics and stay toolchain-free. The soundness half — that a certified body
/// really does compute the same bits at every lane width — is
/// `simd_lanes_match_scalar_bit_for_bit` under `--features c-oracle`.
#[cfg(test)]
mod simd_legality {
    use super::*;

    fn simd_diags(src: &str) -> Vec<String> {
        let (tokens, _) = Lexer::new(src).tokenize();
        let (_ast, pd) = Parser::new(src, tokens).parse();
        pd.iter().map(|d| d.message.clone()).collect()
    }
    fn violated(src: &str) -> bool {
        simd_diags(src).iter().any(|m| m.contains("`@simd` is violated"))
    }
    fn clean(src: &str) -> bool {
        simd_diags(src).iter().all(|m| !m.contains("@simd"))
    }

    /// The headline: an integer/bitwise body is certified, and the *same reduction*
    /// with a division in it is rejected — the guard that keeps a body from silently
    /// falling off the vector path.
    #[test]
    fn simd_accepts_integer_bodies_rejects_faulting_ones() {
        assert!(
            clean("@simd fn f(s: []i64) -> i64 { par for x in s reduce(red()) { x * x + (x & 7) } }"),
            "a total elementwise integer body vectorizes"
        );
        assert!(
            violated("@simd fn f(s: []i64) -> i64 { par for x in s reduce(red()) { x / 2 } }"),
            "a division can fault in a lane a scalar run would have skipped"
        );
    }

    /// Each rejection names its own cause, so the message says what to change. A call
    /// is not a lane operation; indexing is a gather; a cast changes the lane width.
    #[test]
    fn simd_rejections_name_their_cause() {
        let of = |body: &str| {
            simd_diags(&format!("@simd fn f(s: []i64) -> i64 {{ par for x in s reduce(red()) {{ {body} }} }}"))
                .join(" | ")
        };
        assert!(of("g(x)").contains("calls a function"), "{}", of("g(x)"));
        assert!(of("s[0]").contains("accesses memory"), "{}", of("s[0]"));
        assert!(of("x as i32 as i64").contains("lane count"), "{}", of("x as i32 as i64"));
        assert!(of("if 1.5 > 0.0 { x } else { 0 }").contains("floating point"));
    }

    /// A lane select is legal (both arms are blended), and short-circuit `and` is legal
    /// *because* nothing reachable in the subset can fault — the invariant the whole
    /// whitelist exists to maintain. Pinned so a later relaxation has to face it.
    #[test]
    fn simd_admits_selects_and_short_circuits() {
        assert!(clean(
            "@simd fn f(s: []i64) -> i64 { par for x in s reduce(red()) { if x > 0 and x < 9 { x * x } else { 0 } } }"
        ));
    }

    /// An attribute that quietly means nothing is worse than no attribute: `@simd` on a
    /// function with no `par for` is an error, not a silent pass.
    #[test]
    fn simd_on_a_function_with_no_par_for_is_an_error() {
        assert!(
            simd_diags("@simd fn f() -> i64 { return 1 }")
                .iter()
                .any(|m| m.contains("no `par for` loop")),
            "an empty promise is refused"
        );
    }

    /// `@simd` is a *contract*, not a lowering switch: it must change no emitted C at
    /// all. This is the property that keeps the increment off the two-sided tax — the
    /// corpus, the concatenated build and the bootstrap seed cannot move if the
    /// attribute is invisible to the backend.
    #[test]
    fn simd_changes_no_emitted_c() {
        let body = "fn r() -> i64 { return 0 }\nfn f(read s: []i64) -> i64 {\n    return par for x in s reduce(core.sum_reduction()) { x * x }\n}\nfn main() -> i32 { return 0 }\n";
        let plain = emit_c(body);
        let annotated = emit_c(&format!("@simd {body}"));
        assert_eq!(plain, annotated, "`@simd` must be invisible to the backend");
    }

    fn emit_c(src: &str) -> String {
        let (tokens, _) = Lexer::new(src).tokenize();
        let (ast, _) = Parser::new(src, tokens).parse();
        let (info, _) = crate::typeck::check(&ast);
        crate::cgen::emit(&ast, &info).0
    }

    /// The shipped `par for` demo, run through the report: its bodies (`x * x` and the
    /// identity) are exactly the subset this pass certifies, so the corpus itself
    /// carries a positive case without a new file.
    #[test]
    fn the_par_for_demo_is_vectorizable() {
        let src = std::fs::read_to_string("examples/std/par_for.jtr").unwrap();
        let (tokens, _) = Lexer::new(&src).tokenize();
        let (ast, _) = Parser::new(&src, tokens).parse();
        let sites = crate::simd::analyze(&ast);
        assert_eq!(sites.len(), 2, "the demo has two `par for` loops");
        assert!(sites.iter().all(|s| s.verdict.is_legal()), "{sites:?}");
        let report = crate::simd::render(&src, &sites);
        assert!(report.contains("par-for 2 vectorizable 2"), "{report}");
    }
}

/// Array *list* literals `[e0, e1, …]` — the lookup-table enabler (TESTING.md §5,
/// per-feature unit layer). The repeat form `[v; N]` already existed; these pin the
/// list form, in particular that a `const` table lowers to a C **brace initializer**
/// (a statement-expression cannot initialize a `static const`).
#[cfg(test)]
mod array_literals {
    use super::compile;

    const PROG: &str = "\
const T: [4]u64 = [0x8000000000000000, 1, 2, 3]\n\
fn pick(read t: [4]u64, i: usize) -> u64 { return t[i] }\n\
fn main() -> i32 {\n\
    let local: [3]i64 = [10, 20, 30]\n\
    var s: i64 = 0\n\
    for x in local { s = s + x }\n\
    return (pick(T, 0) as i64 + s) as i32\n\
}\n";

    #[test]
    fn array_list_literal_compiles_clean() {
        let (_c, diags) = compile(PROG);
        assert_eq!(diags, 0, "array list literal program must compile clean");
    }

    #[test]
    fn const_array_is_a_brace_initializer() {
        let (c, _) = compile(PROG);
        // The const table is a static brace initializer carrying every element —
        // NOT a statement-expression (`= ({`), which C rejects for a static const.
        assert!(c.contains("j_T = { {"), "const table not brace-initialized:\n{c}");
        assert!(
            c.contains("0x8000000000000000"),
            "table element missing from initializer:\n{c}"
        );
        assert!(
            !c.contains("j_T = ({"),
            "const table wrongly emitted as a statement-expression:\n{c}"
        );
    }

    #[test]
    fn const_array_element_type_is_adopted() {
        // The `[4]u64` annotation makes the array a u64 array (not i32-by-default),
        // so the emitted struct holds `uint64_t a[4]`.
        let (c, _) = compile(PROG);
        assert!(
            c.contains("uint64_t a[4]"),
            "u64 element type not adopted:\n{c}"
        );
    }

    #[test]
    fn array_list_literal_is_deterministic() {
        assert_eq!(compile(PROG), compile(PROG));
    }
}

/// Reference implementation of correctly-rounded decimal→`f64` parsing via
/// **Eisel–Lemire** — the algorithm and power-of-ten table that the Jestyr
/// `core.parse_float` will mirror. Validated end-to-end against Rust's own
/// (correctly-rounded) `str::parse::<f64>()`; the same generator emits the Jestyr
/// table, so the two share their constants by construction.
#[cfg(test)]
mod lemire {
    use std::cmp::Ordering;

    // ── A minimal big unsigned integer (little-endian u64 limbs) ────────────────
    // Just enough to build the table: ×small, <<1, compare, subtract, bit length,
    // and exact division for the negative-power reciprocals.
    #[derive(Clone)]
    struct Big {
        l: Vec<u64>,
    }
    impl Big {
        fn zero() -> Big {
            Big { l: vec![0] }
        }
        fn one() -> Big {
            Big { l: vec![1] }
        }
        fn trim(&mut self) {
            while self.l.len() > 1 && *self.l.last().unwrap() == 0 {
                self.l.pop();
            }
        }
        fn is_zero(&self) -> bool {
            self.l.iter().all(|&x| x == 0)
        }
        fn mul_small(&mut self, m: u64) {
            let mut carry: u128 = 0;
            for d in self.l.iter_mut() {
                let p = (*d as u128) * (m as u128) + carry;
                *d = p as u64;
                carry = p >> 64;
            }
            while carry != 0 {
                self.l.push(carry as u64);
                carry >>= 64;
            }
        }
        fn shl1(&mut self) {
            let mut carry = 0u64;
            for d in self.l.iter_mut() {
                let nc = *d >> 63;
                *d = (*d << 1) | carry;
                carry = nc;
            }
            if carry != 0 {
                self.l.push(carry);
            }
        }
        fn bit_len(&self) -> usize {
            for i in (0..self.l.len()).rev() {
                if self.l[i] != 0 {
                    return i * 64 + (64 - self.l[i].leading_zeros() as usize);
                }
            }
            0
        }
        fn bit(&self, i: usize) -> u64 {
            let w = i / 64;
            if w >= self.l.len() {
                0
            } else {
                (self.l[w] >> (i % 64)) & 1
            }
        }
        fn set_bit(&mut self, i: usize) {
            let w = i / 64;
            while self.l.len() <= w {
                self.l.push(0);
            }
            self.l[w] |= 1 << (i % 64);
        }
        fn cmp(&self, o: &Big) -> Ordering {
            let n = self.l.len().max(o.l.len());
            for i in (0..n).rev() {
                let a = *self.l.get(i).unwrap_or(&0);
                let b = *o.l.get(i).unwrap_or(&0);
                if a != b {
                    return a.cmp(&b);
                }
            }
            Ordering::Equal
        }
        fn sub_assign(&mut self, o: &Big) {
            // self -= o, assuming self >= o
            let mut borrow = 0i128;
            for i in 0..self.l.len() {
                let b = *o.l.get(i).unwrap_or(&0);
                let v = self.l[i] as i128 - b as i128 - borrow;
                if v < 0 {
                    self.l[i] = (v + (1i128 << 64)) as u64;
                    borrow = 1;
                } else {
                    self.l[i] = v as u64;
                    borrow = 0;
                }
            }
            self.trim();
        }
        fn pow(base: u64, exp: usize) -> Big {
            let mut r = Big::one();
            for _ in 0..exp {
                r.mul_small(base);
            }
            r
        }
        /// `floor(2^p / self)` and whether it divided evenly (remainder == 0).
        fn pow2_div(p: usize, den: &Big) -> (Big, bool) {
            // long division of 2^p by den, MSB-first
            let mut rem = Big::zero();
            let mut q = Big::zero();
            for i in (0..=p).rev() {
                rem.shl1();
                if i == p {
                    rem.l[0] |= 1; // the single set bit of 2^p
                }
                q.shl1();
                if rem.cmp(den) != Ordering::Less {
                    rem.sub_assign(den);
                    q.l[0] |= 1;
                }
            }
            q.trim();
            (q, rem.is_zero())
        }
        /// The top 128 bits as (hi, lo), given bit_len >= 128.
        fn top128(&self) -> (u64, u64) {
            let bl = self.bit_len();
            let sh = bl - 128;
            let mut hi = 0u64;
            let mut lo = 0u64;
            for k in 0..64 {
                if self.bit(sh + 64 + k) == 1 {
                    hi |= 1 << k;
                }
                if self.bit(sh + k) == 1 {
                    lo |= 1 << k;
                }
            }
            (hi, lo)
        }
        // ── extra ops for the slow path ─────────────────────────────────────────
        fn from_u64(x: u64) -> Big {
            Big { l: vec![x] }
        }
        fn add_small(&mut self, x: u64) {
            let mut carry = x as u128;
            let mut i = 0;
            while carry != 0 {
                if i == self.l.len() {
                    self.l.push(0);
                }
                let s = self.l[i] as u128 + carry;
                self.l[i] = s as u64;
                carry = s >> 64;
                i += 1;
            }
        }
        fn shl_bits(&mut self, n: usize) {
            let words = n / 64;
            let bits = n % 64;
            if bits != 0 {
                let mut carry = 0u64;
                for d in self.l.iter_mut() {
                    let nc = *d >> (64 - bits);
                    *d = (*d << bits) | carry;
                    carry = nc;
                }
                if carry != 0 {
                    self.l.push(carry);
                }
            }
            if words != 0 {
                let mut nl = vec![0u64; words];
                nl.extend_from_slice(&self.l);
                self.l = nl;
            }
        }
        fn mul_pow5(&mut self, e: usize) {
            for _ in 0..e {
                self.mul_small(5);
            }
        }
        /// In-place divide by a small divisor; returns the remainder. (Test helper
        /// for building exact midpoint decimals.)
        fn div_small(&mut self, d: u64) -> u64 {
            let mut rem: u128 = 0;
            for i in (0..self.l.len()).rev() {
                let cur = (rem << 64) | self.l[i] as u128;
                self.l[i] = (cur / d as u128) as u64;
                rem = cur % d as u128;
            }
            self.trim();
            rem as u64
        }
        fn to_decimal(&self) -> String {
            if self.is_zero() {
                return "0".to_string();
            }
            let mut n = self.clone();
            let mut ds = Vec::new();
            while !n.is_zero() {
                ds.push(b'0' + n.div_small(10) as u8);
            }
            ds.reverse();
            String::from_utf8(ds).unwrap()
        }
        /// The big integer of the decimal `digits` (each a byte b'0'..=b'9').
        fn from_digits(digits: &[u8]) -> Big {
            let mut b = Big::zero();
            for &d in digits {
                b.mul_small(10);
                b.add_small((d - b'0') as u64);
            }
            b
        }
    }

    // ── Algorithm constants for binary64 ────────────────────────────────────────
    const SMALLEST_POWER: i32 = -342;
    const LARGEST_POWER: i32 = 308;
    const MANTISSA_BITS: i32 = 52;
    const INFINITE_POWER: i32 = 0x7FF;
    const MIN_EXP_ROUND_EVEN: i32 = -4;
    const MAX_EXP_ROUND_EVEN: i32 = 23;

    /// `power(q) = ⌊q·log₂10⌋ + 63` via Lemire's integer magic.
    fn power(q: i32) -> i32 {
        (((152170 + 65536) * q) >> 16) + 63
    }

    /// The 128-bit significand of 10^q, normalized so bit 127 is set, with positive
    /// powers truncated and negative powers rounded up — the alignment `power(q)`
    /// assumes. Returns (hi, lo).
    fn table_entry(q: i32) -> (u64, u64) {
        let trueexp = power(q) - 63; // = ⌊log₂(10^q)⌋
        let shift = 127 - trueexp; // scale 10^q into [2^127, 2^128)
        if q >= 0 {
            let mut v = Big::pow(10, q as usize);
            if shift >= 0 {
                for _ in 0..shift {
                    v.shl1();
                }
                // v now exactly 128-bit (bit 127 set), lower bits zero
                let (hi, lo) = pad_to_128(&v);
                (hi, lo)
            } else {
                v.top128() // truncate to top 128 bits
            }
        } else {
            // 10^q = 2^q / 5^|q|; M = ceil(2^(shift+q) / 5^|q|)
            let p = (shift + q) as usize; // shift - |q|
            let den = Big::pow(5, (-q) as usize);
            let (mut m, exact) = Big::pow2_div(p, &den);
            if !exact {
                add_one(&mut m); // round up
            }
            // m is ~128 bits; normalize if a carry pushed it to 129
            if m.bit_len() > 128 {
                // shift right by 1 (drop the lsb) — only on the rare carry overflow
                let mut nm = Big::zero();
                for k in 0..m.bit_len() {
                    if m.bit(k + 1) == 1 {
                        nm.set_bit(k);
                    }
                }
                m = nm;
            }
            pad_to_128(&m)
        }
    }
    fn add_one(b: &mut Big) {
        let mut i = 0;
        loop {
            if i >= b.l.len() {
                b.l.push(1);
                break;
            }
            let (s, c) = b.l[i].overflowing_add(1);
            b.l[i] = s;
            if !c {
                break;
            }
            i += 1;
        }
    }
    fn pad_to_128(b: &Big) -> (u64, u64) {
        let lo = *b.l.first().unwrap_or(&0);
        let hi = *b.l.get(1).unwrap_or(&0);
        (hi, lo)
    }

    /// The full table of (hi, lo) pairs for q in [-342, 308].
    fn gen_table() -> Vec<(u64, u64)> {
        (SMALLEST_POWER..=LARGEST_POWER).map(table_entry).collect()
    }

    fn full_mul(a: u64, b: u64) -> (u64, u64) {
        let p = (a as u128) * (b as u128);
        ((p >> 64) as u64, p as u64)
    }

    /// (hi, lo) ≈ the top 128 bits of w × 10^q, two-step for precision.
    fn compute_product(q: i32, w: u64, table: &[(u64, u64)]) -> (u64, u64) {
        let idx = (q - SMALLEST_POWER) as usize;
        let (thi, tlo) = table[idx];
        let (mut hi, mut lo) = full_mul(w, thi);
        let precision_mask: u64 = 0xFFFF_FFFF_FFFF_FFFF >> (MANTISSA_BITS + 3);
        if (hi & precision_mask) == precision_mask {
            let (shi, _slo) = full_mul(w, tlo);
            let (nlo, carry) = lo.overflowing_add(shi);
            lo = nlo;
            if carry {
                hi += 1;
            }
        }
        (hi, lo)
    }

    /// Eisel–Lemire fast path. Returns `Some(f64 bits)` when confident, `None` when
    /// it bails (the rare ambiguous case needing a slow path).
    fn lemire(neg: bool, mantissa: u64, q: i32, table: &[(u64, u64)]) -> Option<u64> {
        let sign = if neg { 1u64 << 63 } else { 0 };
        if mantissa == 0 || q < SMALLEST_POWER {
            return Some(sign); // ±0
        }
        if q > LARGEST_POWER {
            return Some(sign | ((INFINITE_POWER as u64) << 52)); // ±inf
        }
        let lz = mantissa.leading_zeros() as i32;
        let w = mantissa << lz;
        let (hi, lo) = compute_product(q, w, table);
        let upperbit = (hi >> 63) as i32;
        let mut m = hi >> (upperbit + 64 - MANTISSA_BITS - 3);
        let mut power2 = power(q) + upperbit - lz - (-1023);
        if power2 <= 0 {
            // subnormal
            if -power2 + 1 >= 64 {
                return Some(sign); // underflow to ±0
            }
            m >>= -power2 + 1;
            m += m & 1;
            m >>= 1;
            power2 = if m < (1u64 << MANTISSA_BITS) { 0 } else { 1 };
            // No masking: in the true-subnormal case bit 52 of `m` is clear; at the
            // boundary `m == 2^52` shares bit 52 with `power2 == 1`, which is exactly
            // the smallest-normal encoding (exponent 1, mantissa 0).
            return Some(sign | m | ((power2 as u64) << 52));
        }
        // round-to-even tie: only when 5^q fits exactly and we are in the safe range
        if lo <= 1
            && q >= MIN_EXP_ROUND_EVEN
            && q <= MAX_EXP_ROUND_EVEN
            && (m & 3) == 1
            && (m << (upperbit + 64 - MANTISSA_BITS - 3)) == hi
        {
            m &= !1u64; // exactly halfway → round down to even
        }
        m += m & 1;
        m >>= 1;
        if m >= (1u64 << (MANTISSA_BITS + 1)) {
            m = 1u64 << MANTISSA_BITS;
            power2 += 1;
        }
        m &= !(1u64 << MANTISSA_BITS);
        if power2 >= INFINITE_POWER {
            return Some(sign | ((INFINITE_POWER as u64) << 52)); // overflow → ±inf
        }
        Some(sign | m | ((power2 as u64) << 52))
    }
    /// Parse the decimal into (neg, 64-bit significand, power-of-ten q), or None if
    /// it doesn't fit the fast-path shape (too many digits, etc.).
    fn parse_decimal(s: &str) -> Option<(bool, u64, i32)> {
        let b = s.as_bytes();
        let mut i = 0;
        let mut neg = false;
        if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
            neg = b[i] == b'-';
            i += 1;
        }
        let mut mant: u64 = 0;
        let mut digits = 0i32;
        let mut dot_at: Option<i32> = None;
        let mut overflowed = false;
        let mut seen_digit = false;
        while i < b.len() {
            let c = b[i];
            if c.is_ascii_digit() {
                seen_digit = true;
                if mant.checked_mul(10).and_then(|v| v.checked_add((c - b'0') as u64)).is_some()
                    && digits < 19
                {
                    mant = mant * 10 + (c - b'0') as u64;
                    digits += 1;
                } else {
                    overflowed = true; // too many significant digits for the fast path
                }
                i += 1;
            } else if c == b'.' && dot_at.is_none() {
                dot_at = Some(digits);
                i += 1;
            } else {
                break;
            }
        }
        if !seen_digit || overflowed {
            return None;
        }
        let mut q: i32 = match dot_at {
            Some(d) => d - digits, // digits after the point lower the exponent
            None => 0,
        };
        if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
            i += 1;
            let mut esign = 1i32;
            if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
                if b[i] == b'-' {
                    esign = -1;
                }
                i += 1;
            }
            let mut e = 0i32;
            let mut any = false;
            while i < b.len() && b[i].is_ascii_digit() {
                any = true;
                e = e.saturating_mul(10).saturating_add((b[i] - b'0') as i32);
                i += 1;
            }
            if !any {
                return None;
            }
            q += esign * e;
        }
        if i != b.len() {
            return None; // trailing junk
        }
        Some((neg, mant, q))
    }

    /// Full fast-path parse: returns Some(f64) when both decimal-parse and Lemire
    /// succeed, else None (caller would use a slow path).
    fn parse_f64(s: &str, table: &[(u64, u64)]) -> Option<f64> {
        let (neg, mant, q) = parse_decimal(s)?;
        lemire(neg, mant, q, table).map(f64::from_bits)
    }

    // ── Slow path: arbitrary-precision, correctly rounded ───────────────────────
    // For inputs with > 19 significant digits, the fast path can't form the 64-bit
    // mantissa. The slow path takes the fast-path result on the first 19 digits as a
    // candidate `g` (within ~0.01 ULP of the true value), then decides between `g`
    // and its bit-pattern neighbours `g±1` by comparing the exact value `D·10^E`
    // against the relevant rounding midpoint — a cross-multiplied big-integer
    // comparison (multiply by 5ⁿ, shift, compare; never divide). Only the first
    // `SLOW_DIGITS` significant digits + a sticky bit are needed (more can't affect
    // an f64 except at an exact tie, which the sticky bit resolves).
    const SLOW_DIGITS: usize = 800;

    /// Parse a decimal string into (neg, significant digits with leading zeros
    /// stripped, decimal exponent E) such that value = digits·10^E. None on malformed.
    fn parse_decimal_full(s: &str) -> Option<(bool, Vec<u8>, i32)> {
        let b = s.as_bytes();
        let mut i = 0;
        let mut neg = false;
        if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
            neg = b[i] == b'-';
            i += 1;
        }
        let mut digits: Vec<u8> = Vec::new();
        let mut frac = 0i32;
        let mut dot = false;
        let mut seen = false;
        while i < b.len() {
            let c = b[i];
            if c.is_ascii_digit() {
                seen = true;
                digits.push(c);
                if dot {
                    frac += 1;
                }
                i += 1;
            } else if c == b'.' && !dot {
                dot = true;
                i += 1;
            } else {
                break;
            }
        }
        if !seen {
            return None;
        }
        let mut e = -frac;
        if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
            i += 1;
            let mut es = 1i32;
            if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
                if b[i] == b'-' {
                    es = -1;
                }
                i += 1;
            }
            let mut ev = 0i32;
            let mut any = false;
            while i < b.len() && b[i].is_ascii_digit() {
                any = true;
                ev = ev.saturating_mul(10).saturating_add((b[i] - b'0') as i32);
                i += 1;
            }
            if !any {
                return None;
            }
            e += es * ev;
        }
        if i != b.len() {
            return None;
        }
        // strip leading zeros (they don't change the integer value)
        let mut start = 0;
        while start + 1 < digits.len() && digits[start] == b'0' {
            start += 1;
        }
        Some((neg, digits[start..].to_vec(), e))
    }

    /// Compare `dt · 10^et` against `c · 2^p` (the boundary), with `sticky` meaning
    /// the true value is strictly larger than `dt·10^et`. Cross-multiplies to clear
    /// denominators — division-free.
    fn cmp_boundary(dt: &Big, et: i32, c: u64, p: i32, sticky: bool) -> Ordering {
        let mut l = dt.clone();
        let mut r = Big::from_u64(c);
        // fold the 5-powers onto the side whose 5-exponent is positive
        if et >= 0 {
            l.mul_pow5(et as usize);
        } else {
            r.mul_pow5((-et) as usize);
        }
        // remaining 2-powers: l has 2^et, r has 2^p; factor out 2^min and shift
        let m = et.min(p);
        l.shl_bits((et - m) as usize);
        r.shl_bits((p - m) as usize);
        match l.cmp(&r) {
            Ordering::Equal if sticky => Ordering::Greater,
            ord => ord,
        }
    }

    /// Correctly-rounded slow path for any number of significant digits.
    fn slow_parse(neg: bool, digits: &[u8], e: i32, table: &[(u64, u64)]) -> u64 {
        let sign = if neg { 1u64 << 63 } else { 0 };
        let nd = digits.len();
        if nd == 0 || digits.iter().all(|&d| d == b'0') {
            return sign;
        }
        // candidate g from the first ≤19 digits
        let take = nd.min(19);
        let mut w: u64 = 0;
        for &d in &digits[..take] {
            w = w * 10 + (d - b'0') as u64;
        }
        let q = e + (nd as i32 - take as i32);
        let gbits = lemire(neg, w, q, table).unwrap() & !(1u64 << 63); // magnitude pattern
        let be = (gbits >> 52) & 0x7FF;
        if be == 0x7FF {
            return sign | gbits; // already ±inf
        }
        // Keep up to SLOW_DIGITS significant digits exactly (more cannot change the
        // rounding except via the sticky bit — a boundary's exact decimal has < 768
        // significant digits, so beyond that no input can *equal* a boundary).
        let t = nd.min(SLOW_DIGITS);
        let dt = Big::from_digits(&digits[..t]);
        let sticky = digits[t..].iter().any(|&d| d != b'0');
        let et = e + (nd as i32 - t as i32);
        // g's significand m and binary exponent k
        let mant = gbits & 0xF_FFFF_FFFF_FFFF;
        let (m, k) = if be == 0 { (mant, -1074i64) } else { ((1u64 << 52) | mant, be as i64 - 1075) };
        // compare V to g's value (= 2m · 2^(k-1))
        let to_bv = |c: u64, p: i64| cmp_boundary(&dt, et, c, p as i32, sticky);
        let mut out = gbits;
        if m == 0 {
            // candidate is zero: boundary to the smallest subnormal is 2^-1075
            if to_bv(1, -1075) == Ordering::Greater {
                out = gbits + 1;
            }
        } else {
            match to_bv(2 * m, k - 1) {
                Ordering::Equal => {} // exact: g is correct
                Ordering::Greater => {
                    // V above g → g or succ; midpoint (2m+1)·2^(k-1)
                    match to_bv(2 * m + 1, k - 1) {
                        Ordering::Greater => out = gbits + 1,
                        Ordering::Less => {}
                        Ordering::Equal => {
                            if m & 1 == 1 {
                                out = gbits + 1; // tie → even
                            }
                        }
                    }
                }
                Ordering::Less => {
                    // V below g → g or pred; midpoint depends on the power-of-two gap
                    let (c, p) = if mant == 0 && be > 1 {
                        (4 * m - 1, k - 2) // asymmetric: pred is half-ulp below
                    } else {
                        (2 * m - 1, k - 1)
                    };
                    match to_bv(c, p) {
                        Ordering::Less => out = gbits - 1,
                        Ordering::Greater => {}
                        Ordering::Equal => {
                            if m & 1 == 1 {
                                out = gbits - 1; // tie → even
                            }
                        }
                    }
                }
            }
        }
        sign | out
    }

    /// Full correctly-rounded parser: fast path for ≤ 19 digits, slow path otherwise.
    fn parse_f64_full(s: &str, table: &[(u64, u64)]) -> Option<f64> {
        let (neg, digits, e) = parse_decimal_full(s)?;
        let nd = digits.len();
        if nd <= 19 {
            // route through the fast path (rebuild the u64 significand)
            let mut w: u64 = 0;
            for &d in &digits {
                w = w * 10 + (d - b'0') as u64;
            }
            return lemire(neg, w, e, table).map(f64::from_bits);
        }
        Some(f64::from_bits(slow_parse(neg, &digits, e, table)))
    }

    // ── tests ───────────────────────────────────────────────────────────────────

    /// `power(q)` equals the true `⌊log₂(10^q)⌋` across the whole table range — the
    /// invariant the table's normalization depends on.
    #[test]
    fn power_formula_matches_true_log2() {
        for q in SMALLEST_POWER..=LARGEST_POWER {
            // true exponent: bit_len(10^q)-1 for q>=0; for q<0 compute via 5^|q|.
            let true_exp = if q >= 0 {
                Big::pow(10, q as usize).bit_len() as i32 - 1
            } else {
                // 10^q = 2^q/5^|q|; ⌊log2⌋ = q - ceil(log2(5^|q|))... compute directly:
                // find e with 2^e <= 2^q/5^|q| < 2^(e+1) ⇔ 2^(e-q) <= 1/5^|q| ...
                // easier: ⌊log2(10^q)⌋ = -(bit_len(5^|q|)) - |q| + q + correction.
                // Compute by searching: smallest k with 5^|q| <= 2^(k) i.e. bit_len.
                let bl5 = Big::pow(5, (-q) as usize).bit_len() as i32;
                // 10^q = 2^q / 5^|q|. log2 = q - log2(5^|q|). ⌊⌋ = q - bl5 if 5^|q| is
                // not a power of two (always true for |q|>=1), since
                // 2^(bl5-1) < 5^|q| < 2^bl5 ⇒ bl5-1 < log2(5^|q|) < bl5 ⇒
                // ⌊q - log2(5^|q|)⌋ = q - bl5.
                q - bl5
            };
            assert_eq!(power(q) - 63, true_exp, "power({q}) mismatch");
        }
    }

    /// Hand-checked positive-power anchors validate the generator's normalization.
    #[test]
    fn table_anchor_values() {
        let t = gen_table();
        let at = |q: i32| t[(q - SMALLEST_POWER) as usize];
        assert_eq!(at(0), (0x8000_0000_0000_0000, 0)); // 10^0 = 1 → 2^127
        assert_eq!(at(1), (0xA000_0000_0000_0000, 0)); // 10  = 0b1010 → top bits
        assert_eq!(at(2), (0xC800_0000_0000_0000, 0)); // 100 = 0b1100100
    }

    /// **The headline: the fast path is correctly rounded.** For every accepted
    /// input the bits equal Rust's own correctly-rounded `str::parse::<f64>()`.
    /// (Teeth: corrupting any table entry, or flipping the round-to-even tie to
    /// round-up, makes some case's bits disagree with std.)
    #[test]
    fn lemire_matches_std_parse() {
        let t = gen_table();
        let mut state: u64 = 0x1234_5678_9abc_def1;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let mut checked = 0u64;
        let mut bailed = 0u64;
        for _ in 0..500_000 {
            let x = f64::from_bits(next());
            if !x.is_finite() {
                continue;
            }
            // Compact e-notation keeps the significand ≤ 18 digits regardless of how
            // extreme the exponent is — the fast-path shape. (Plain `Display` spells
            // out huge magnitudes to hundreds of digits, which the fast path bails on
            // by design; that is the slow path's job, tested elsewhere later.)
            for s in [format!("{x:e}"), format!("{:.17e}", x)] {
                match parse_f64(&s, &t) {
                    Some(v) => {
                        checked += 1;
                        let want: f64 = s.parse().unwrap();
                        assert_eq!(
                            v.to_bits(),
                            want.to_bits(),
                            "parse({s}) = {v:?} but std = {want:?}"
                        );
                    }
                    None => bailed += 1,
                }
            }
        }
        // e-notation is always within the fast-path digit budget → essentially no
        // bails; correctness (above) is the real assertion.
        assert!(checked > 800_000, "too few accepted: {checked}");
        assert!(bailed * 1000 < checked, "unexpected fast-path bail rate: {bailed}/{checked}");
    }

    /// Dump the verified table as a Jestyr `const` to the scratchpad — run on demand
    /// (`cargo test dump_pow10_table -- --ignored --nocapture`) to (re)generate the
    /// constant pasted into `core.jtr`. Interleaved hi, lo per power (idx = q + 342).
    #[test]
    #[ignore]
    fn dump_pow10_table() {
        let t = gen_table();
        let mut s = String::new();
        s.push_str("// Power-of-ten table for Eisel–Lemire parse_float: the 128-bit\n");
        s.push_str("// significand of 10^q (bit 127 set; +powers truncated, -powers rounded up)\n");
        s.push_str("// for q in [-342, 308], interleaved hi, lo. Index: i = (q + 342) * 2.\n");
        s.push_str("// GENERATED by `cargo test dump_pow10_table -- --ignored` — do not hand-edit.\n");
        s.push_str("const POW10_128: [1302]u64 = [\n");
        for (i, (hi, lo)) in t.iter().enumerate() {
            s.push_str(&format!("    0x{hi:016X}, 0x{lo:016X},"));
            s.push_str(&format!("   // q = {}\n", SMALLEST_POWER + i as i32));
        }
        s.push_str("]\n");
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../pow10_table.jtr.txt");
        // fall back to a temp path if the relative one fails
        if std::fs::write("pow10_table.jtr.txt", &s).is_err() {
            let _ = std::fs::write(path, &s);
        }
        eprintln!("wrote {} bytes ({} entries)", s.len(), t.len());
    }

    /// Hard, hand-picked cases (subnormals, powers of ten, halfway ties, the famous
    /// 2.2250738585072011e-308) all match std.
    #[test]
    fn lemire_matches_std_hard_cases() {
        let t = gen_table();
        let cases = [
            "0", "1", "10", "0.1", "0.5", "1.5", "100000000",
            "1e308", "1e-308", "5e-324", "2.2250738585072011e-308",
            "2.2250738585072014e-308", "4.9406564584124654e-324",
            "9007199254740992", "9007199254740993", "9007199254740994",
            "1.7976931348623157e308", "123456789012345678", "0.30000000000000004",
            "2.5", "3.5", "0.000244140625",
        ];
        for s in cases {
            if let Some(v) = parse_f64(s, &t) {
                let want: f64 = s.parse().unwrap();
                assert_eq!(v.to_bits(), want.to_bits(), "hard case {s}");
            }
        }
    }

    /// **The slow path is correctly rounded for arbitrarily many digits.** Format
    /// random doubles to full (and over-full) precision so the parser must take the
    /// > 19-digit slow path, and assert the bits match std. (Teeth: skipping the
    /// midpoint adjustment, or dropping the sticky bit, makes near-boundary cases
    /// disagree with std.)
    fn slow_sweep(n: usize, seed: u64) -> u64 {
        let t = gen_table();
        let mut state = seed;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let mut took_slow = 0u64;
        for _ in 0..n {
            let x = f64::from_bits(next());
            if !x.is_finite() {
                continue;
            }
            // {:.30e} / {:.45e} have 31 / 46 significant digits → force the slow path
            for s in [format!("{:.30e}", x), format!("{:.45e}", x)] {
                let v = parse_f64_full(&s, &t).unwrap();
                let want: f64 = s.parse().unwrap();
                assert_eq!(v.to_bits(), want.to_bits(), "slow parse({s})");
                took_slow += 1;
            }
        }
        took_slow
    }

    #[test]
    fn slow_parse_matches_std() {
        let n = slow_sweep(25_000, 0xda3e_39cb_94b9_5bdb);
        assert!(n > 40_000, "slow path under-exercised: {n}");
    }

    #[test]
    #[ignore]
    fn slow_parse_matches_std_thorough() {
        slow_sweep(1_000_000, 0x106e_b8a3_2f1d_77c9);
    }

    /// The exact decimal of the midpoint between `x` and `succ(x)` — a value that is
    /// genuinely halfway, so the digits past the 19-digit candidate decide the
    /// rounding. `x` must be a positive finite normal.
    fn succ_midpoint_decimal(x: f64) -> String {
        let bits = x.to_bits();
        let be = (bits >> 52) & 0x7FF;
        let mant = bits & 0xF_FFFF_FFFF_FFFF;
        let (m, k) = if be == 0 { (mant, -1074i64) } else { ((1u64 << 52) | mant, be as i64 - 1075) };
        let c = 2 * m + 1; // midpoint = c · 2^(k-1)
        let kk = k - 1;
        if kk >= 0 {
            let mut b = Big::from_u64(c);
            b.shl_bits(kk as usize);
            b.to_decimal()
        } else {
            // c / 2^(-kk) = c·5^(-kk) / 10^(-kk): integer digits with a point
            let f = (-kk) as usize;
            let mut b = Big::from_u64(c);
            b.mul_pow5(f);
            let mut s = b.to_decimal();
            while s.len() <= f {
                s.insert(0, '0'); // pad so the point has digits on its left
            }
            let dot = s.len() - f;
            s.insert(dot, '.');
            s
        }
    }

    /// **Teeth for the adjustment:** parse exact midpoints and midpoints nudged just
    /// above — the cases where the 19-digit candidate is on the wrong side and only
    /// the big-integer midpoint comparison gets the answer right. Each must match std.
    #[test]
    fn slow_parse_near_boundaries() {
        let t = gen_table();
        let xs = [
            1.0f64, 2.0, 0.5, 3.14159, 1e10, 1e-10, 123456.789,
            9007199254740992.0, 1e100, 1e-100, 2.5, 0.1, 100.0,
        ];
        for &x in &xs {
            let mid = succ_midpoint_decimal(x);
            // exact midpoint → ties to even; nudged up → rounds to succ(x). Both via std.
            for s in [mid.clone(), format!("{mid}1"), format!("{mid}00001"), format!("{mid}5")] {
                let v = parse_f64_full(&s, &t).unwrap();
                let want: f64 = s.parse().unwrap();
                assert_eq!(v.to_bits(), want.to_bits(), "near-boundary {s} (x={x:?})");
            }
        }
    }

    /// Long-digit hard cases: many digits straddling a rounding boundary, long zero
    /// tails (exact ties), subnormals with long tails, and the classic denormal
    /// boundary 2.2250738585072011e-308 written long.
    #[test]
    fn slow_parse_hard_cases() {
        let t = gen_table();
        let cases: &[&str] = &[
            "1.00000000000000000000000000000001",          // just above 1
            "0.99999999999999999999999999999999",          // just below 1
            "9007199254740993.0000000000000000",           // tie → 2^53 (even)
            "9007199254740995.0000000000000000",           // tie → 9007199254740996
            "2.22507385850720113605740979670913197593481954635164564e-308",
            "4.9406564584124654417656879286822137236505980e-324", // smallest subnormal
            "1.7976931348623158e308",                      // overflow → inf
            "0.0000000000000000000000000000000000000001",  // tiny → 1e-40
            "123456789012345678901234567890",              // 30-digit integer
            "1.000000000000000000000000000000000000000000000000005",
        ];
        for &s in cases {
            let v = parse_f64_full(s, &t).unwrap();
            let want: f64 = s.parse().unwrap();
            assert_eq!(v.to_bits(), want.to_bits(), "slow hard {s}");
        }
    }
}

/// Reference implementation of **shortest correctly-rounded `f64`→decimal** via
/// Dragon4 (Steele-White / Burger-Dubois) — the algorithm Jestyr `core.format_float`
/// will mirror. Table-free big-integer arithmetic; produces the byte-identical
/// shortest digits that Ryū would (Ryū is a later perf optimization of the same
/// output). Validated against Rust's own shortest formatter (`{:e}`).
#[cfg(test)]
mod dragon {
    use std::cmp::Ordering;

    // ── Minimal big unsigned (little-endian u64 limbs) ──────────────────────────
    #[derive(Clone)]
    struct Big {
        l: Vec<u64>,
    }
    impl Big {
        fn from_u64(x: u64) -> Big {
            Big { l: vec![x] }
        }
        fn trim(&mut self) {
            while self.l.len() > 1 && *self.l.last().unwrap() == 0 {
                self.l.pop();
            }
        }
        /// self <<= n bits
        fn shl_bits(&mut self, n: usize) {
            let words = n / 64;
            let bits = n % 64;
            if bits != 0 {
                let mut carry = 0u64;
                for d in self.l.iter_mut() {
                    let nc = *d >> (64 - bits);
                    *d = (*d << bits) | carry;
                    carry = nc;
                }
                if carry != 0 {
                    self.l.push(carry);
                }
            }
            if words != 0 {
                let mut nl = vec![0u64; words];
                nl.extend_from_slice(&self.l);
                self.l = nl;
            }
        }
        fn mul_small(&mut self, m: u64) {
            let mut carry: u128 = 0;
            for d in self.l.iter_mut() {
                let p = (*d as u128) * (m as u128) + carry;
                *d = p as u64;
                carry = p >> 64;
            }
            while carry != 0 {
                self.l.push(carry as u64);
                carry >>= 64;
            }
        }
        fn cmp(&self, o: &Big) -> Ordering {
            let n = self.l.len().max(o.l.len());
            for i in (0..n).rev() {
                let a = *self.l.get(i).unwrap_or(&0);
                let b = *o.l.get(i).unwrap_or(&0);
                if a != b {
                    return a.cmp(&b);
                }
            }
            Ordering::Equal
        }
        /// out = self + o
        fn add(&self, o: &Big) -> Big {
            let n = self.l.len().max(o.l.len());
            let mut out = Vec::with_capacity(n + 1);
            let mut carry = 0u128;
            for i in 0..n {
                let s = *self.l.get(i).unwrap_or(&0) as u128
                    + *o.l.get(i).unwrap_or(&0) as u128
                    + carry;
                out.push(s as u64);
                carry = s >> 64;
            }
            if carry != 0 {
                out.push(carry as u64);
            }
            Big { l: out }
        }
        /// self -= o, assuming self >= o
        fn sub_assign(&mut self, o: &Big) {
            let mut borrow = 0i128;
            for i in 0..self.l.len() {
                let v = self.l[i] as i128 - *o.l.get(i).unwrap_or(&0) as i128 - borrow;
                if v < 0 {
                    self.l[i] = (v + (1i128 << 64)) as u64;
                    borrow = 1;
                } else {
                    self.l[i] = v as u64;
                    borrow = 0;
                }
            }
            self.trim();
        }
    }

    /// The shortest decimal for a finite, nonzero, positive `x`: returns the digit
    /// bytes (first digit nonzero) and the decimal exponent `k` such that
    /// `x = 0.d₁d₂…dₙ × 10^k`.
    fn shortest_digits(x: f64) -> (Vec<u8>, i32) {
        let bits = x.to_bits();
        let ieee_exp = ((bits >> 52) & 0x7FF) as i32;
        let ieee_mant = bits & 0xF_FFFF_FFFF_FFFF;
        // f · 2^e with f the integer significand
        let (f, e): (u64, i32) = if ieee_exp == 0 {
            (ieee_mant, -1074) // subnormal
        } else {
            ((1u64 << 52) | ieee_mant, ieee_exp - 1075)
        };
        let even = (f & 1) == 0;
        let min_exp = -1074;

        // R/S/m+/m- (Burger-Dubois). The asymmetric gap at a power-of-two significand
        // (f == 2^52, and not at the minimum exponent) makes the low margin half.
        let (mut r, mut s, mut mp, mut mm);
        if e >= 0 {
            let be = {
                let mut b = Big::from_u64(1);
                b.shl_bits(e as usize);
                b
            };
            if f != (1u64 << 52) {
                r = Big::from_u64(f);
                r.mul_small(2);
                r = mul_big(&r, &be);
                s = Big::from_u64(2);
                mp = be.clone();
                mm = be;
            } else {
                r = Big::from_u64(f);
                r.mul_small(4);
                r = mul_big(&r, &be);
                s = Big::from_u64(4);
                mp = {
                    let mut t = be.clone();
                    t.mul_small(2);
                    t
                };
                mm = be;
            }
        } else if e == min_exp || f != (1u64 << 52) {
            r = Big::from_u64(f);
            r.mul_small(2);
            s = Big::from_u64(1);
            s.shl_bits((1 - e) as usize);
            mp = Big::from_u64(1);
            mm = Big::from_u64(1);
        } else {
            r = Big::from_u64(f);
            r.mul_small(4);
            s = Big::from_u64(1);
            s.shl_bits((2 - e) as usize);
            mp = Big::from_u64(2);
            mm = Big::from_u64(1);
        }

        // Scale into [0.1, 1): find k so that 0.1 ≤ value·10^-k < 1. Table-free
        // fixup (no log estimate): adjust by ±1 powers of ten until in range.
        let mut k = 0i32;
        // while value + m+ > 1 (i.e. R + mp > S): scale S up
        loop {
            let rp = r.add(&mp);
            let hi = if even { rp.cmp(&s) != Ordering::Less } else { rp.cmp(&s) == Ordering::Greater };
            if hi {
                s.mul_small(10);
                k += 1;
            } else {
                break;
            }
        }
        // while (value + m+) · 10 ≤ 1 (i.e. (R + mp)·10 ≤ S): scale R/m up
        loop {
            let mut rp = r.add(&mp);
            rp.mul_small(10);
            let low = if even { rp.cmp(&s) != Ordering::Greater } else { rp.cmp(&s) == Ordering::Less };
            if low {
                r.mul_small(10);
                mp.mul_small(10);
                mm.mul_small(10);
                k -= 1;
            } else {
                break;
            }
        }

        // Generate digits.
        let mut digits = Vec::new();
        loop {
            r.mul_small(10);
            mp.mul_small(10);
            mm.mul_small(10);
            // d = R / S (single digit) via ≤9 subtractions
            let mut d: u8 = 0;
            while r.cmp(&s) != Ordering::Less {
                r.sub_assign(&s);
                d += 1;
            }
            let low = if even { r.cmp(&mm) != Ordering::Greater } else { r.cmp(&mm) == Ordering::Less };
            let rp = r.add(&mp);
            let high = if even { rp.cmp(&s) != Ordering::Less } else { rp.cmp(&s) == Ordering::Greater };
            if !low && !high {
                digits.push(b'0' + d);
            } else {
                // terminate with the correctly-rounded last digit
                let up = if low && !high {
                    false
                } else if high && !low {
                    true
                } else {
                    // both: round to nearest by 2R vs S
                    let mut r2 = r.clone();
                    r2.mul_small(2);
                    match r2.cmp(&s) {
                        Ordering::Greater => true,
                        Ordering::Less => false,
                        Ordering::Equal => (d & 1) == 1, // tie → round to even
                    }
                };
                digits.push(if up { b'0' + d + 1 } else { b'0' + d });
                break;
            }
        }
        (digits, k)
    }

    fn mul_big(a: &Big, b: &Big) -> Big {
        let mut out = vec![0u64; a.l.len() + b.l.len()];
        for (i, &ad) in a.l.iter().enumerate() {
            let mut carry = 0u128;
            for (j, &bd) in b.l.iter().enumerate() {
                let cur = out[i + j] as u128 + (ad as u128) * (bd as u128) + carry;
                out[i + j] = cur as u64;
                carry = cur >> 64;
            }
            let mut k = i + b.l.len();
            while carry != 0 {
                let cur = out[k] as u128 + carry;
                out[k] = cur as u64;
                carry = cur >> 64;
                k += 1;
            }
        }
        let mut r = Big { l: out };
        r.trim();
        r
    }

    /// Assemble shortest scientific `[-]d.ddde±XX` (a canonical, round-trippable
    /// form). `x` finite. Mirrors what `format_float` will write into a `[]u8`.
    fn format_sci(x: f64) -> String {
        if x == 0.0 {
            return if x.is_sign_negative() { "-0e0".to_string() } else { "0e0".to_string() };
        }
        let neg = x < 0.0;
        let (digits, k) = shortest_digits(x.abs());
        let mut s = String::new();
        if neg {
            s.push('-');
        }
        s.push(digits[0] as char);
        if digits.len() > 1 {
            s.push('.');
            for &d in &digits[1..] {
                s.push(d as char);
            }
        }
        s.push('e');
        // value = 0.d… × 10^k ⇒ d.dd… × 10^(k-1)
        let exp = k - 1;
        s.push_str(&exp.to_string());
        s
    }

    // ── tests ───────────────────────────────────────────────────────────────────

    /// Assert the three properties that *define* a shortest correctly-rounded
    /// representation, against Rust's own shortest (`{:e}`): (1) it round-trips,
    /// (2) it has the same (minimal) number of significant digits, and (3) every
    /// digit but the last matches std. The last digit may differ **only** on an
    /// exact tie — where `…25`-style values are equidistant between two shortest
    /// decimals and we round to even (deterministic, IEEE-consistent) while std
    /// rounds the other way. Both are valid shortest round-trips.
    fn check_against_std(x: f64) {
        let s = format_sci(x);
        let back: f64 = s.parse().unwrap();
        assert_eq!(back.to_bits(), x.to_bits(), "round-trip {s} for {x:?}");
        let mine = significant_digits(&s);
        let theirs = significant_digits(&format!("{:e}", x));
        assert_eq!(mine.len(), theirs.len(), "length {mine} vs {theirs} for {x:?}");
        assert_eq!(
            mine[..mine.len() - 1],
            theirs[..theirs.len() - 1],
            "non-final digits {mine} vs {theirs} for {x:?}"
        );
    }

    fn xorshift_check(n: usize, seed: u64) {
        let mut state = seed;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for _ in 0..n {
            let x = f64::from_bits(next());
            if !x.is_finite() || x == 0.0 {
                continue;
            }
            check_against_std(x);
        }
    }

    /// **The headline:** random doubles satisfy the shortest-round-trip contract
    /// above. The default run is sized to stay fast (the Vec-based bignum is
    /// allocation-heavy); `dragon_matches_std_thorough` (#[ignore]) sweeps far more.
    /// (Teeth: dropping a digit breaks round-trip; an extra digit breaks the length
    /// check; a wrong interior digit breaks the prefix check.)
    #[test]
    fn dragon_matches_std_shortest() {
        xorshift_check(8_000, 0x2545_f491_4f6c_dd1d);
    }

    #[test]
    #[ignore]
    fn dragon_matches_std_thorough() {
        xorshift_check(2_000_000, 0x9e37_79b9_7f4a_7c15);
    }

    /// The shortest-digit string (no sign, no dot, no exponent, trailing form
    /// normalized) of a scientific-notation string — for comparing digit content.
    fn significant_digits(s: &str) -> String {
        let mant = s.split(['e', 'E']).next().unwrap();
        let mant = mant.trim_start_matches('-');
        let mut d: String = mant.chars().filter(|c| c.is_ascii_digit()).collect();
        // drop trailing zeros (1.50 and 1.5 are the same significand) and leading
        while d.len() > 1 && d.ends_with('0') {
            d.pop();
        }
        while d.len() > 1 && d.starts_with('0') {
            d.remove(0);
        }
        d
    }

    /// Hand-picked cases incl. powers of ten, the asymmetric power-of-two gap,
    /// subnormals, and values needing round-to-even.
    #[test]
    fn dragon_hard_cases() {
        for &x in &[
            1.0f64, 0.5, 0.1, 0.2, 0.3, 1.5, 2.0, 10.0, 100.0, 1e308, 5e-324,
            2.2250738585072014e-308, 9007199254740992.0, 1234567890.12345,
            0.30000000000000004, 1.7976931348623157e308, 123.456, 1e-300, 4503599627370497.0,
        ] {
            check_against_std(x);
        }
    }
}

/// **The two layout models must agree, over the whole corpus.**
///
/// `layout.rs` answers "what does this type cost?" twice, from two different front ends:
/// `layout_of` reads a resolved `Ty` from the checked table (what `jestyrc layout` and
/// the backend use), and `ast_layout_of` walks the AST directly (what the **comptime**
/// `@size_of`/`@align_of`/`@offset_of` use, because `Interp::new(ast)` runs during type
/// checking and so cannot depend on the table being built).
///
/// The *rules* are shared — `prim_layout`, `aggregate`, `align_to`, `auto_order` are one
/// copy each — but the traversals are not, and a rule added to one traversal and
/// forgotten in the other would make `@size_of(T)` and `size_of(T)` disagree **inside a
/// single program**. That is a silent-miscompile shape: the constant folds to one number
/// while the C compiler lays out another.
///
/// So this pins them against each other across every corpus file. A type the AST model
/// declines on (a generic instance, an imported type) is skipped rather than failed —
/// declining is a legitimate answer there, and `@size_of` reports it as an error. What
/// is *not* allowed is answering, and answering differently.
#[test]
fn the_two_layout_models_agree() {
    let mut compared = 0usize;
    for dir in ["examples", "examples/std"] {
        let Ok(rd) = std::fs::read_dir(dir) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().and_then(|s| s.to_str()) != Some("jtr") {
                continue;
            }
            let src = std::fs::read_to_string(&p).unwrap();
            let (tokens, _) = crate::lexer::Lexer::new(&src).tokenize();
            let (ast, _) = crate::parser::Parser::new(&src, tokens).parse();
            let (info, _) = crate::typeck::check(&ast);
            for t in crate::layout::compute(&ast, &info) {
                if t.incomplete {
                    continue; // the table model itself declined
                }
                let Some(l) = crate::layout::ast_layout_by_name(&ast, &t.name) else {
                    continue; // the AST model declined — a legitimate answer
                };
                assert_eq!(
                    (l.size, l.align),
                    (t.size, t.align),
                    "{}: `{}` — the AST layout model says {:?}, the table model says {:?}",
                    p.display(),
                    t.name,
                    (l.size, l.align),
                    (t.size, t.align)
                );
                // …and the field offsets, which is what `@offset_of` returns.
                for f in &t.fields {
                    if let Some(off) = crate::layout::ast_offset_of(&ast, &t.name, &f.name) {
                        assert_eq!(
                            off, f.offset,
                            "{}: `{}.{}` — offset {} vs {}",
                            p.display(), t.name, f.name, off, f.offset
                        );
                    }
                }
                compared += 1;
            }
        }
    }
    // A guard against the comparison quietly becoming vacuous: if a refactor made the
    // AST model decline on everything, every assertion above would pass.
    assert!(compared > 50, "only {compared} types compared — the models are not being exercised");
}

/// SHA-256 now lives in the shared, non-test `crate::sha256` module — both this
/// numerics-determinism canary and `jestyrc attest` hash with the same code, so the
/// vectors that vouch for one vouch for the other. Re-exported here so the existing
/// `super::sha256` reference in the `c_oracle` submodule keeps resolving unchanged.
/// (Gated to `c-oracle`, the only consumer, so the default build sees no dead import.)
#[cfg(feature = "c-oracle")]
use crate::sha256;

/// **The gcc-in-test oracle (`--features c-oracle`).** Compile + run each
/// `examples/std` demo through a real C compiler (exactly as `jestyrc run` does) and
/// assert its output — turning the demos into end-to-end regression tests — plus a
/// **locked SHA-256 over all numerics output**: the cross-OS/-compiler determinism
/// canary. If FP determinism ever slips (an FMA fusion, a reassociation, a rounding
/// change), the digest moves and this fails. Opt-in because it needs a C compiler.
#[cfg(all(test, feature = "c-oracle"))]
mod c_oracle {
    use super::sha256;
    use std::process::Command;

    /// Compile `rel` (a `.jtr` path, with imports) to C, build with the locked FP
    /// flags, run it, and return its stdout — the same pipeline as `jestyrc run`.
    fn build_and_run(rel: &str) -> String {
        let prog = crate::module::load(rel);
        assert!(!prog.diags.iter().any(|d| d.is_error()), "load/parse errors in {rel}: {:?}", prog.diags);
        let (info, td) = crate::typeck::check_program(&prog.ast, &prog.modules);
        assert!(!td.iter().any(|d| d.is_error()), "typeck errors in {rel}");
        let ed = crate::escape::check(&prog.ast, &info);
        assert!(!ed.iter().any(|d| d.is_error()), "escape errors in {rel}");
        let (c_src, _cd) = crate::cgen::emit(&prog.ast, &info);

        // Unique output names per call: cargo runs tests in parallel, and a demo built
        // by two tests (e.g. its per-demo test *and* the canary) would otherwise share
        // one `.c`/`.exe` and clobber each other mid-compile. Disjoint by construction.
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let uniq = SEQ.fetch_add(1, Ordering::Relaxed);
        let stem: String = rel.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
        let dir = std::env::temp_dir();
        let cfile = dir.join(format!("jestyr_oracle_{stem}_{uniq}.c"));
        let exe = dir.join(format!("jestyr_oracle_{stem}_{uniq}{}", std::env::consts::EXE_SUFFIX));
        std::fs::write(&cfile, &c_src).unwrap();

        let cc = crate::find_c_compiler().expect("c-oracle needs a C compiler on PATH");
        let mut cmd = Command::new(&cc);
        cmd.args(crate::CC_FLAGS);
        if c_src.contains("pthread") {
            cmd.arg("-pthread");
        }
        let st = cmd.arg("-o").arg(&exe).arg(&cfile).status().unwrap();
        assert!(st.success(), "gcc failed for {rel}");
        let out = Command::new(&exe).output().unwrap();
        assert!(out.status.success(), "run of {rel} failed");
        String::from_utf8(out.stdout).unwrap()
    }

    /// Whitespace-normalized tokens of a demo's output (robust to newline style).
    fn toks(rel: &str) -> Vec<String> {
        build_and_run(rel).split_whitespace().map(|s| s.to_string()).collect()
    }

    /// Compile `rel` to an executable and return its path (does NOT run it) — for
    /// programs that take command-line arguments, like the self-hosting lexer.
    fn build_exe(rel: &str) -> std::path::PathBuf {
        let prog = crate::module::load(rel);
        assert!(!prog.diags.iter().any(|d| d.is_error()), "load errors in {rel}: {:?}", prog.diags);
        let (info, td) = crate::typeck::check_program(&prog.ast, &prog.modules);
        assert!(!td.iter().any(|d| d.is_error()), "typeck errors in {rel}");
        assert!(!crate::escape::check(&prog.ast, &info).iter().any(|d| d.is_error()), "escape errors in {rel}");
        let (c_src, _cd) = crate::cgen::emit(&prog.ast, &info);
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let uniq = SEQ.fetch_add(1, Ordering::Relaxed);
        let stem: String = rel.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
        let dir = std::env::temp_dir();
        let cfile = dir.join(format!("jestyr_exe_{stem}_{uniq}.c"));
        let exe = dir.join(format!("jestyr_exe_{stem}_{uniq}{}", std::env::consts::EXE_SUFFIX));
        std::fs::write(&cfile, &c_src).unwrap();
        let cc = crate::find_c_compiler().expect("c-oracle needs a C compiler on PATH");
        let mut cmd = Command::new(&cc);
        cmd.args(crate::CC_FLAGS);
        // The Jestyr-written compiler recurses per expression-nesting level; give the exe the
        // same headroom the Rust reference gets from its 8MB main-thread stack (Windows
        // defaults to 1MB, which the deepest corpus files overflow). Harness-only — the locked
        // `CC_FLAGS`/attest command is untouched.
        #[cfg(windows)]
        cmd.arg("-Wl,--stack,67108864");
        if c_src.contains("pthread") {
            cmd.arg("-pthread");
        }
        assert!(cmd.arg("-o").arg(&exe).arg(&cfile).status().unwrap().success(), "gcc failed for {rel}");
        exe
    }

    /// The Rust *reference* lexer's lexeme stream for `src`: each non-`Eof` token's
    /// exact source text. The oracle the Jestyr-written lexer must reproduce.
    fn rust_lexemes(src: &str) -> Vec<String> {
        use crate::token::TokenKind;
        crate::lexer::Lexer::new(src)
            .tokenize()
            .0
            .into_iter()
            .filter(|t| t.kind != TokenKind::Eof)
            .map(|t| src[t.span.range()].to_string())
            .collect()
    }

    /// Run the built Jestyr lexer exe on `file` with `extra` args, take its stdout
    /// lines, and drop the trailing 6 summary numbers. With no extra args it dumps
    /// lexemes; with a second arg it dumps kind labels. (Token output never contains
    /// a raw newline — strings are single-line, comments skipped — so one-per-line holds.)
    fn run_jestyr_lexer(lexer_exe: &std::path::Path, file: &str, extra: &[&str]) -> Vec<String> {
        let out = Command::new(lexer_exe).arg(file).args(extra).output().unwrap();
        assert!(out.status.success(), "jestyr lexer failed on {file}");
        let text = String::from_utf8(out.stdout).unwrap();
        let mut lines: Vec<String> = text.lines().map(|s| s.to_string()).collect();
        let n = lines.len();
        assert!(n >= 6, "lexer output too short for {file}: {text}");
        lines.truncate(n - 6); // the summary: total, kw, id, num, punct, distinct
        lines
    }

    /// The Jestyr lexer's lexeme stream for a file (P1 golden).
    fn jestyr_lexemes(lexer_exe: &std::path::Path, file: &str) -> Vec<String> {
        run_jestyr_lexer(lexer_exe, file, &[])
    }

    /// The Jestyr lexer's *kind-label* stream for a file (the parser's input, P2 golden).
    fn jestyr_kinds(lexer_exe: &std::path::Path, file: &str) -> Vec<String> {
        run_jestyr_lexer(lexer_exe, file, &["kinds"])
    }

    /// The Rust *reference* lexer's kind-label stream for `src`: each non-`Eof`
    /// token's `TokenKind::describe()`. The oracle the Jestyr kind dump must match.
    fn rust_kinds(src: &str) -> Vec<String> {
        use crate::token::TokenKind;
        crate::lexer::Lexer::new(src)
            .tokenize()
            .0
            .into_iter()
            .filter(|t| t.kind != TokenKind::Eof)
            .map(|t| t.kind.describe().to_string())
            .collect()
    }

    /// The Jestyr lexer's integer *kind-tag* stream for a file (P2a, strongest form).
    fn jestyr_kind_ids(lexer_exe: &std::path::Path, file: &str) -> Vec<String> {
        run_jestyr_lexer(lexer_exe, file, &["nums"])
    }

    /// The Rust *reference* lexer's integer kind-tag stream for `src`: each non-`Eof`
    /// token's `TokenKind` discriminant (`kind as u32`). `TokenKind` is a unit-only
    /// enum, so the cast yields the enum's declaration order — exactly the numbering
    /// the Jestyr lexer assigns (Ident=0 … Unknown=111). Unlike `rust_kinds`, this pins
    /// the operator/keyword tags themselves, not just their (span-derived) labels.
    fn rust_kind_ids(src: &str) -> Vec<String> {
        use crate::token::TokenKind;
        crate::lexer::Lexer::new(src)
            .tokenize()
            .0
            .into_iter()
            .filter(|t| t.kind != TokenKind::Eof)
            .map(|t| (t.kind as u32).to_string())
            .collect()
    }

    // --- P2 expression-parser cross-check (parser AST-dump golden) ---

    /// Canonical `UnOp` label for the AST dump — matches `unop_label` in
    /// `examples/std/parser.jtr`.
    fn ref_unop_label(op: crate::ast::UnOp) -> &'static str {
        use crate::ast::UnOp::*;
        match op {
            Neg => "neg",
            Not => "not",
            BitNot => "bitnot",
            Ref => "ref",
        }
    }

    /// Canonical `BinOp` label for the AST dump — matches `binop_label` in
    /// `examples/std/parser.jtr`.
    fn ref_binop_label(op: crate::ast::BinOp) -> &'static str {
        use crate::ast::BinOp::*;
        match op {
            Add => "add",
            Sub => "sub",
            Mul => "mul",
            Div => "div",
            Rem => "rem",
            Eq => "eq",
            Ne => "ne",
            Lt => "lt",
            Le => "le",
            Gt => "gt",
            Ge => "ge",
            And => "and",
            Or => "or",
            BitAnd => "bitand",
            BitOr => "bitor",
            BitXor => "bitxor",
            Shl => "shl",
            Shr => "shr",
        }
    }

    /// Canonical `AssignOp` label for the AST dump — matches `assign_label` in
    /// `examples/std/parser.jtr`.
    fn ref_assign_label(op: crate::ast::AssignOp) -> &'static str {
        use crate::ast::AssignOp::*;
        match op {
            Assign => "set",
            Add => "add",
            Sub => "sub",
            Mul => "mul",
            Div => "div",
            Rem => "rem",
            BitAnd => "bitand",
            BitOr => "bitor",
            BitXor => "bitxor",
        }
    }

    /// Dump an optional child expression: the node if present, else a `(none)` marker —
    /// matches `dump_opt` in `examples/std/parser.jtr`.
    fn ref_dump_opt(ast: &crate::ast::Ast, id: Option<crate::ast::ExprId>, out: &mut Vec<String>) {
        match id {
            Some(e) => ref_dump_expr(ast, e, out),
            None => {
                out.push("(".to_string());
                out.push("none".to_string());
                out.push(")".to_string());
            }
        }
    }

    /// Dump an optional loop identifier (a `for` label or `region` name), matching the Jestyr
    /// `dump_loopname`: `(loopname <s> <e>)` when present, else `(none)`.
    fn ref_dump_loopname(opt: &Option<crate::ast::Ident>, out: &mut Vec<String>) {
        match opt {
            Some(id) => {
                out.push("(".to_string());
                out.push("loopname".to_string());
                out.push(id.span.start.to_string());
                out.push(id.span.end.to_string());
                out.push(")".to_string());
            }
            None => {
                out.push("(".to_string());
                out.push("none".to_string());
                out.push(")".to_string());
            }
        }
    }

    /// Dump one statement (matching the Jestyr `dump_stmt`): `(let <mutbl> <name span>
    /// <stmt span> <type-opt> <init-opt>)`, `(return <stmt span> <value-opt>)`, or
    /// `(exprstmt <expr>)`.
    fn ref_dump_stmt(ast: &crate::ast::Ast, st: &crate::ast::Stmt, out: &mut Vec<String>) {
        use crate::ast::Stmt;
        out.push("(".to_string());
        match st {
            Stmt::Let { mutbl, name, ty, init, span } => {
                out.push("let".to_string());
                out.push(if *mutbl { "1" } else { "0" }.to_string());
                out.push(name.span.start.to_string());
                out.push(name.span.end.to_string());
                out.push(span.start.to_string());
                out.push(span.end.to_string());
                match ty {
                    Some(tid) => ref_dump_type(ast, *tid, out),
                    None => {
                        out.push("(".to_string());
                        out.push("none".to_string());
                        out.push(")".to_string());
                    }
                }
                ref_dump_opt(ast, *init, out);
            }
            Stmt::Return { value, span } => {
                out.push("return".to_string());
                out.push(span.start.to_string());
                out.push(span.end.to_string());
                ref_dump_opt(ast, *value, out);
            }
            Stmt::Expr(id) => {
                out.push("exprstmt".to_string());
                ref_dump_expr(ast, *id, out);
            }
        }
        out.push(")".to_string());
    }

    /// The atoms of a block body — `block`, statement count, span, each statement — with NO
    /// surrounding parens (the caller wraps: `ref_dump_expr`'s outer parens for a `Block`
    /// expr, or `ref_dump_block` for a block nested in `if`/`unsafe`).
    fn ref_dump_block_body(ast: &crate::ast::Ast, b: &crate::ast::Block, out: &mut Vec<String>) {
        out.push("block".to_string());
        out.push(b.stmts.len().to_string());
        out.push(b.span.start.to_string());
        out.push(b.span.end.to_string());
        for st in &b.stmts {
            ref_dump_stmt(ast, st, out);
        }
    }

    /// A parenthesized block, for blocks that are a *child* of another node (`if`'s `then`,
    /// `unsafe`'s body) rather than an `ExprKind::Block` reached through `ref_dump_expr`.
    fn ref_dump_block(ast: &crate::ast::Ast, b: &crate::ast::Block, out: &mut Vec<String>) {
        out.push("(".to_string());
        ref_dump_block_body(ast, b, out);
        out.push(")".to_string());
    }

    fn ref_conv_code(c: crate::ast::Conv) -> i32 {
        use crate::ast::Conv;
        match c {
            Conv::Default => 0,
            Conv::Read => 1,
            Conv::Mut => 2,
            Conv::Take => 3,
            Conv::Out => 4,
        }
    }

    /// Dump one type structurally (matching the Jestyr `dump_type`). Names/`type`/errors carry
    /// a span; pointers/slices/refs recurse into their inner; App/Path emit their name spans +
    /// arg count + args; Fn emits `(tyfnparam <conv> <ty>)` params and an optional return type.
    fn ref_dump_type(ast: &crate::ast::Ast, tid: crate::ast::TypeId, out: &mut Vec<String>) {
        use crate::ast::{PtrMut, TypeKind};
        let t = ast.type_at(tid);
        let s = t.span.start.to_string();
        let en = t.span.end.to_string();
        out.push("(".to_string());
        match &t.kind {
            TypeKind::Name(_) => {
                out.push("tyname".to_string());
                out.push(s);
                out.push(en);
            }
            TypeKind::TypeKw => {
                out.push("tykw".to_string());
                out.push(s);
                out.push(en);
            }
            TypeKind::Ptr { mutbl, inner } => {
                let m = match mutbl {
                    PtrMut::Default => 0,
                    PtrMut::Mut => 1,
                    PtrMut::Const => 2,
                };
                out.push("typtr".to_string());
                out.push(m.to_string());
                out.push(s);
                out.push(en);
                ref_dump_type(ast, *inner, out);
            }
            TypeKind::Slice(inner) => {
                out.push("tyslice".to_string());
                out.push(s);
                out.push(en);
                ref_dump_type(ast, *inner, out);
            }
            TypeKind::Array { len, elem } => {
                out.push("tyarray".to_string());
                out.push(s);
                out.push(en);
                ref_dump_expr(ast, *len, out);
                ref_dump_type(ast, *elem, out);
            }
            TypeKind::GenRef(inner) => {
                out.push("tygenref".to_string());
                out.push(s);
                out.push(en);
                ref_dump_type(ast, *inner, out);
            }
            TypeKind::RegionRef { region, inner } => {
                out.push("tyregionref".to_string());
                out.push(region.span.start.to_string());
                out.push(region.span.end.to_string());
                out.push(s);
                out.push(en);
                ref_dump_type(ast, *inner, out);
            }
            TypeKind::App { ctor, args } => {
                out.push("tyapp".to_string());
                out.push(ctor.span.start.to_string());
                out.push(ctor.span.end.to_string());
                out.push(args.len().to_string());
                out.push(s);
                out.push(en);
                for a in args {
                    ref_dump_type(ast, *a, out);
                }
            }
            TypeKind::Path { module, name, args } => {
                out.push("typath".to_string());
                out.push(module.span.start.to_string());
                out.push(module.span.end.to_string());
                out.push(name.span.start.to_string());
                out.push(name.span.end.to_string());
                out.push(args.len().to_string());
                out.push(s);
                out.push(en);
                for a in args {
                    ref_dump_type(ast, *a, out);
                }
            }
            TypeKind::Fn { params, ret_conv, ret } => {
                out.push("tyfn".to_string());
                out.push(params.len().to_string());
                out.push(ref_conv_code(*ret_conv).to_string());
                out.push(s);
                out.push(en);
                for pm in params {
                    out.push("(".to_string());
                    out.push("tyfnparam".to_string());
                    out.push(ref_conv_code(pm.conv).to_string());
                    ref_dump_type(ast, pm.ty, out);
                    out.push(")".to_string());
                }
                match ret {
                    Some(r) => ref_dump_type(ast, *r, out),
                    None => {
                        out.push("(".to_string());
                        out.push("none".to_string());
                        out.push(")".to_string());
                    }
                }
            }
            TypeKind::Dyn(name) => {
                out.push("tydyn".to_string());
                out.push(name.span.start.to_string());
                out.push(name.span.end.to_string());
                out.push(s);
                out.push(en);
            }
            TypeKind::Error => {
                out.push("tyerr".to_string());
                out.push(s);
                out.push(en);
            }
        }
        out.push(")".to_string());
    }

    /// Dump a generic-parameter slice: the count, then each `(generic <name span> <bound-opt>)`.
    fn ref_dump_generics(generics: &[crate::ast::GenericParam], out: &mut Vec<String>) {
        out.push(generics.len().to_string());
        for g in generics {
            out.push("(".to_string());
            out.push("generic".to_string());
            out.push(g.name.span.start.to_string());
            out.push(g.name.span.end.to_string());
            match &g.bound {
                Some(b) => {
                    out.push("(".to_string());
                    out.push("bound".to_string());
                    out.push(b.span.start.to_string());
                    out.push(b.span.end.to_string());
                    out.push(")".to_string());
                }
                None => {
                    out.push("(".to_string());
                    out.push("none".to_string());
                    out.push(")".to_string());
                }
            }
            out.push(")".to_string());
        }
    }

    /// Dump an attribute slice: the count, then each `(attr <name text> <argcount> <args…>)`.
    fn ref_dump_attrs(ast: &crate::ast::Ast, attrs: &[crate::ast::Attribute], out: &mut Vec<String>) {
        out.push(attrs.len().to_string());
        for a in attrs {
            out.push("(".to_string());
            out.push("attr".to_string());
            out.push(a.name.clone());
            out.push(a.args.len().to_string());
            for arg in &a.args {
                ref_dump_expr(ast, *arg, out);
            }
            out.push(")".to_string());
        }
    }

    /// Dump a parameter slice as a run of `(param <comptime> <conv> <is_self> <name span>
    /// <ty-opt> <refine-opt>)`. Shared by the fn item and trait-method dumps.
    fn ref_dump_params(ast: &crate::ast::Ast, params: &[crate::ast::Param], out: &mut Vec<String>) {
        for pm in params {
            out.push("(".to_string());
            out.push("param".to_string());
            out.push(if pm.comptime { "1" } else { "0" }.to_string());
            out.push(ref_conv_code(pm.conv).to_string());
            out.push(if pm.is_self { "1" } else { "0" }.to_string());
            out.push(pm.name.span.start.to_string());
            out.push(pm.name.span.end.to_string());
            match pm.ty {
                Some(t) => ref_dump_type(ast, t, out),
                None => {
                    out.push("(".to_string());
                    out.push("none".to_string());
                    out.push(")".to_string());
                }
            }
            match pm.refine {
                Some(e) => ref_dump_expr(ast, e, out),
                None => {
                    out.push("(".to_string());
                    out.push("none".to_string());
                    out.push(")".to_string());
                }
            }
            out.push(")".to_string());
        }
    }

    /// Dump a function declaration's atoms (no surrounding parens) — shared by the `Item::Fn`
    /// arm and struct-method members: `fn`, is_pub, name span, param count, each `(param …)`,
    /// then ret_conv, ret-opt, and the body block.
    fn ref_dump_fn(ast: &crate::ast::Ast, f: &crate::ast::FnDecl, out: &mut Vec<String>) {
        out.push("fn".to_string());
        ref_dump_attrs(ast, &f.attrs, out);
        ref_dump_generics(&f.generics, out);
        out.push(if f.is_pub { "1" } else { "0" }.to_string());
        out.push(f.name.span.start.to_string());
        out.push(f.name.span.end.to_string());
        out.push(f.params.len().to_string());
        ref_dump_params(ast, &f.params, out);
        out.push(ref_conv_code(f.ret_conv).to_string());
        match f.ret_ty {
            Some(t) => ref_dump_type(ast, t, out),
            None => {
                out.push("(".to_string());
                out.push("none".to_string());
                out.push(")".to_string());
            }
        }
        // fn extras: error-set names, then requires, then ensures.
        match &f.errors {
            Some(es) => {
                out.push(es.names.len().to_string());
                for n in &es.names {
                    out.push("(".to_string());
                    out.push("errname".to_string());
                    out.push(n.span.start.to_string());
                    out.push(n.span.end.to_string());
                    out.push(")".to_string());
                }
            }
            None => out.push("0".to_string()),
        }
        out.push(f.requires.len().to_string());
        for r in &f.requires {
            ref_dump_expr(ast, *r, out);
        }
        out.push(f.ensures.len().to_string());
        for e in &f.ensures {
            ref_dump_expr(ast, *e, out);
        }
        ref_dump_block(ast, &f.body, out);
    }

    /// Dump a struct/record/union member list (matching the Jestyr `dump_members`): each member
    /// is a method (the fn dump) or a field `(sfield <is_pub> <volatile> <name span> <type>
    /// <bits-opt> <default-opt>)`. Shared by the struct *item* dump and the `struct { … }` value.
    fn ref_dump_members(ast: &crate::ast::Ast, members: &[crate::ast::StructMember], out: &mut Vec<String>) {
        use crate::ast::StructMember;
        for m in members {
            match m {
                StructMember::Field { name, ty, is_pub, default, volatile, bits, .. } => {
                    out.push("(".to_string());
                    out.push("sfield".to_string());
                    out.push(if *is_pub { "1" } else { "0" }.to_string());
                    out.push(if *volatile { "1" } else { "0" }.to_string());
                    out.push(name.span.start.to_string());
                    out.push(name.span.end.to_string());
                    ref_dump_type(ast, *ty, out);
                    match bits {
                        Some(b) => {
                            out.push("(".to_string());
                            out.push("bits".to_string());
                            out.push(b.to_string());
                            out.push(")".to_string());
                        }
                        None => {
                            out.push("(".to_string());
                            out.push("none".to_string());
                            out.push(")".to_string());
                        }
                    }
                    match default {
                        Some(e) => ref_dump_expr(ast, *e, out),
                        None => {
                            out.push("(".to_string());
                            out.push("none".to_string());
                            out.push(")".to_string());
                        }
                    }
                    out.push(")".to_string());
                }
                StructMember::Method(f) => {
                    out.push("(".to_string());
                    ref_dump_fn(ast, f, out);
                    out.push(")".to_string());
                }
            }
        }
    }

    /// Dump one top-level item (matching the Jestyr `dump_item`). `import`: the path text,
    /// an alias name span or `(none)`, a pinned-hash text or `(none)`. `distinct`: is_pub,
    /// name span, base type. `const`: is_pub, name span, an optional type, the value.
    fn ref_dump_item(ast: &crate::ast::Ast, item: &crate::ast::Item, out: &mut Vec<String>) {
        use crate::ast::Item;
        out.push("(".to_string());
        match item {
            Item::Import(imp) => {
                out.push("import".to_string());
                out.push("(".to_string());
                out.push("path".to_string());
                out.push(imp.path.clone());
                out.push(")".to_string());
                match &imp.alias {
                    Some(a) => {
                        out.push("(".to_string());
                        out.push("alias".to_string());
                        out.push(a.span.start.to_string());
                        out.push(a.span.end.to_string());
                        out.push(")".to_string());
                    }
                    None => {
                        out.push("(".to_string());
                        out.push("none".to_string());
                        out.push(")".to_string());
                    }
                }
                match &imp.expected_hash {
                    Some(h) => {
                        out.push("(".to_string());
                        out.push("hash".to_string());
                        out.push(h.clone());
                        out.push(")".to_string());
                    }
                    None => {
                        out.push("(".to_string());
                        out.push("none".to_string());
                        out.push(")".to_string());
                    }
                }
            }
            Item::Distinct(d) => {
                out.push("distinct".to_string());
                out.push(if d.is_pub { "1" } else { "0" }.to_string());
                out.push(d.name.span.start.to_string());
                out.push(d.name.span.end.to_string());
                ref_dump_type(ast, d.base, out);
            }
            Item::Const(c) => {
                out.push("const".to_string());
                ref_dump_attrs(ast, &c.attrs, out);
                out.push(if c.is_pub { "1" } else { "0" }.to_string());
                out.push(c.name.span.start.to_string());
                out.push(c.name.span.end.to_string());
                match c.ty {
                    Some(t) => ref_dump_type(ast, t, out),
                    None => {
                        out.push("(".to_string());
                        out.push("none".to_string());
                        out.push(")".to_string());
                    }
                }
                ref_dump_expr(ast, c.value, out);
            }
            // fn: is_pub, name span, param count, each `(param …)`, then ret_conv, ret-opt,
            // and the body. (Generics/attrs/errors/contracts are omitted until the parser
            // handles them — the fn-core corpus has none, so they'd be empty anyway.)
            Item::Fn(f) => ref_dump_fn(ast, f, out),
            // struct/record/union: kind code (0/1/2), is_pub, name span, member count, then
            // each member — a field `(sfield <is_pub> <name span> <type> <default-opt>)` or a
            // method fn dump. (Field `@volatile`/`: bits` are omitted until parsed.)
            Item::Struct { is_pub, is_record, is_union, name, body, attrs, .. } => {
                let kindcode = if *is_record { 1 } else if *is_union { 2 } else { 0 };
                out.push("struct".to_string());
                ref_dump_attrs(ast, attrs, out);
                out.push(kindcode.to_string());
                out.push(if *is_pub { "1" } else { "0" }.to_string());
                out.push(name.span.start.to_string());
                out.push(name.span.end.to_string());
                out.push(body.members.len().to_string());
                ref_dump_members(ast, &body.members, out);
            }
            // enum: is_pub, name span, tparam count, `(tparam <span>)`s, variant count, then
            // each `(variant <name span> <fieldcount> <(vfield <fname span> <type>)>s
            // <disc-opt>)`.
            Item::Enum(e) => {
                out.push("enum".to_string());
                out.push(if e.is_pub { "1" } else { "0" }.to_string());
                out.push(e.name.span.start.to_string());
                out.push(e.name.span.end.to_string());
                out.push(e.type_params.len().to_string());
                for tp in &e.type_params {
                    out.push("(".to_string());
                    out.push("tparam".to_string());
                    out.push(tp.span.start.to_string());
                    out.push(tp.span.end.to_string());
                    out.push(")".to_string());
                }
                out.push(e.variants.len().to_string());
                for v in &e.variants {
                    out.push("(".to_string());
                    out.push("variant".to_string());
                    out.push(v.name.span.start.to_string());
                    out.push(v.name.span.end.to_string());
                    out.push(v.fields.len().to_string());
                    for (fname, fty) in &v.fields {
                        out.push("(".to_string());
                        out.push("vfield".to_string());
                        out.push(fname.span.start.to_string());
                        out.push(fname.span.end.to_string());
                        ref_dump_type(ast, *fty, out);
                        out.push(")".to_string());
                    }
                    match &v.discriminant {
                        Some(d) => ref_dump_expr(ast, *d, out),
                        None => {
                            out.push("(".to_string());
                            out.push("none".to_string());
                            out.push(")".to_string());
                        }
                    }
                    out.push(")".to_string());
                }
            }
            // trait: is_pub, name span, method count, then each `(tmethod <name span>
            // <paramcount> <(param …)>s <ret_conv> <ret-opt> <default-body-opt>)`.
            Item::Trait(t) => {
                out.push("trait".to_string());
                out.push(if t.is_pub { "1" } else { "0" }.to_string());
                out.push(t.name.span.start.to_string());
                out.push(t.name.span.end.to_string());
                out.push(t.methods.len().to_string());
                for m in &t.methods {
                    out.push("(".to_string());
                    out.push("tmethod".to_string());
                    out.push(m.name.span.start.to_string());
                    out.push(m.name.span.end.to_string());
                    out.push(m.params.len().to_string());
                    ref_dump_params(ast, &m.params, out);
                    out.push(ref_conv_code(m.ret_conv).to_string());
                    match m.ret_ty {
                        Some(t) => ref_dump_type(ast, t, out),
                        None => {
                            out.push("(".to_string());
                            out.push("none".to_string());
                            out.push(")".to_string());
                        }
                    }
                    match &m.default_body {
                        Some(b) => ref_dump_block(ast, b, out),
                        None => {
                            out.push("(".to_string());
                            out.push("none".to_string());
                            out.push(")".to_string());
                        }
                    }
                    out.push(")".to_string());
                }
            }
            // impl: trait-name span, target type, method count, then each method fn dump.
            Item::Impl(im) => {
                out.push("impl".to_string());
                ref_dump_generics(&im.generics, out);
                out.push(im.trait_name.span.start.to_string());
                out.push(im.trait_name.span.end.to_string());
                ref_dump_type(ast, im.ty, out);
                out.push(im.methods.len().to_string());
                for f in &im.methods {
                    out.push("(".to_string());
                    ref_dump_fn(ast, f, out);
                    out.push(")".to_string());
                }
            }
            // extern: is_pub, name span, param count, each `(param …)`, ret_conv, ret-opt.
            // (The abi string is omitted, deferred like fn generics — extern-core.)
            Item::Extern(e) => {
                out.push("extern".to_string());
                out.push(e.abi.clone());
                out.push(if e.is_pub { "1" } else { "0" }.to_string());
                out.push(e.name.span.start.to_string());
                out.push(e.name.span.end.to_string());
                out.push(e.params.len().to_string());
                ref_dump_params(ast, &e.params, out);
                out.push(ref_conv_code(e.ret_conv).to_string());
                match e.ret_ty {
                    Some(t) => ref_dump_type(ast, t, out),
                    None => {
                        out.push("(".to_string());
                        out.push("none".to_string());
                        out.push(")".to_string());
                    }
                }
            }
        }
        out.push(")".to_string());
    }

    /// Dump one pattern (matching the Jestyr `dump_pat`): `patwild`/`patident`/`patrest`/
    /// `paterr` carry a span; `patvariant` its name span + subpat count + span + subpatterns;
    /// `patstruct` adds has_rest and `(patfield <fname span> <subpat>)` fields; `patlit` /
    /// `patrange` dump their literal expressions; `pator` its alternative count + alternatives.
    fn ref_dump_pat(ast: &crate::ast::Ast, pid: crate::ast::PatId, out: &mut Vec<String>) {
        use crate::ast::PatKind;
        let p = ast.pat_at(pid);
        let s = p.span.start.to_string();
        let en = p.span.end.to_string();
        out.push("(".to_string());
        match &p.kind {
            PatKind::Wildcard => {
                out.push("patwild".to_string());
                out.push(s);
                out.push(en);
            }
            PatKind::Ident(_) => {
                out.push("patident".to_string());
                out.push(s);
                out.push(en);
            }
            PatKind::Variant { name, subpats } => {
                out.push("patvariant".to_string());
                out.push(name.span.start.to_string());
                out.push(name.span.end.to_string());
                out.push(subpats.len().to_string());
                out.push(s);
                out.push(en);
                for sp in subpats {
                    ref_dump_pat(ast, *sp, out);
                }
            }
            PatKind::StructVariant { name, fields, has_rest } => {
                out.push("patstruct".to_string());
                out.push(name.span.start.to_string());
                out.push(name.span.end.to_string());
                out.push(if *has_rest { "1" } else { "0" }.to_string());
                out.push(fields.len().to_string());
                out.push(s);
                out.push(en);
                for (fname, sub) in fields {
                    out.push("(".to_string());
                    out.push("patfield".to_string());
                    out.push(fname.span.start.to_string());
                    out.push(fname.span.end.to_string());
                    ref_dump_pat(ast, *sub, out);
                    out.push(")".to_string());
                }
            }
            PatKind::Lit(e) => {
                out.push("patlit".to_string());
                ref_dump_expr(ast, *e, out);
            }
            PatKind::Range { lo, hi, inclusive } => {
                out.push("patrange".to_string());
                out.push(if *inclusive { "1" } else { "0" }.to_string());
                out.push(s);
                out.push(en);
                ref_dump_expr(ast, *lo, out);
                ref_dump_expr(ast, *hi, out);
            }
            PatKind::Or(alts) => {
                out.push("pator".to_string());
                out.push(alts.len().to_string());
                out.push(s);
                out.push(en);
                for a in alts {
                    ref_dump_pat(ast, *a, out);
                }
            }
            PatKind::Rest => {
                out.push("patrest".to_string());
                out.push(s);
                out.push(en);
            }
            PatKind::Error => {
                out.push("paterr".to_string());
                out.push(s);
                out.push(en);
            }
        }
        out.push(")".to_string());
    }

    /// The Rust *reference* canonical AST dump for one expression node: a flattened
    /// S-expression, one atom per line — `(`, the kind label, (operator label,) the
    /// span `start`/`end`, then each child's dump in order, then `)`. A **pure function
    /// of the AST**: no source text, no HashMap iteration, fixed field/child order — so
    /// the Jestyr-written `dump` in `parser.jtr` can reproduce it atom-for-atom. This is
    /// the oracle the P2 parser golden diffs against.
    fn ref_dump_expr(ast: &crate::ast::Ast, id: crate::ast::ExprId, out: &mut Vec<String>) {
        use crate::ast::ExprKind;
        let e = ast.expr_at(id);
        let s = e.span.start.to_string();
        let en = e.span.end.to_string();
        out.push("(".to_string());
        match &e.kind {
            ExprKind::Int(_) => {
                out.push("int".to_string());
                out.push(s);
                out.push(en);
            }
            ExprKind::Float(_) => {
                out.push("float".to_string());
                out.push(s);
                out.push(en);
            }
            ExprKind::Name(_) => {
                out.push("name".to_string());
                out.push(s);
                out.push(en);
            }
            ExprKind::Char(_) => {
                out.push("char".to_string());
                out.push(s);
                out.push(en);
            }
            ExprKind::Str(_) => {
                out.push("str".to_string());
                out.push(s);
                out.push(en);
            }
            ExprKind::Null => {
                out.push("null".to_string());
                out.push(s);
                out.push(en);
            }
            ExprKind::Bool(b) => {
                out.push("bool".to_string());
                out.push(if *b { "1" } else { "0" }.to_string());
                out.push(s);
                out.push(en);
            }
            ExprKind::Unary { op, rhs } => {
                out.push("unary".to_string());
                out.push(ref_unop_label(*op).to_string());
                out.push(s);
                out.push(en);
                ref_dump_expr(ast, *rhs, out);
            }
            ExprKind::Binary { op, lhs, rhs } => {
                out.push("binary".to_string());
                out.push(ref_binop_label(*op).to_string());
                out.push(s);
                out.push(en);
                ref_dump_expr(ast, *lhs, out);
                ref_dump_expr(ast, *rhs, out);
            }
            // Postfix: `.field` carries the field name's span (which field), `[index]`
            // has base + index children, `.*`/`?` wrap a single base.
            ExprKind::Field { base, name } => {
                out.push("field".to_string());
                out.push(name.span.start.to_string());
                out.push(name.span.end.to_string());
                out.push(s);
                out.push(en);
                ref_dump_expr(ast, *base, out);
            }
            ExprKind::Index { base, index } => {
                out.push("index".to_string());
                out.push(s);
                out.push(en);
                ref_dump_expr(ast, *base, out);
                ref_dump_expr(ast, *index, out);
            }
            ExprKind::Deref { base } => {
                out.push("deref".to_string());
                out.push(s);
                out.push(en);
                ref_dump_expr(ast, *base, out);
            }
            ExprKind::Try { base } => {
                out.push("try".to_string());
                out.push(s);
                out.push(en);
                ref_dump_expr(ast, *base, out);
            }
            // Call: arg count, then the callee, then each argument in order.
            ExprKind::Call { callee, args } => {
                out.push("call".to_string());
                out.push(args.len().to_string());
                out.push(s);
                out.push(en);
                ref_dump_expr(ast, *callee, out);
                for arg in args {
                    ref_dump_expr(ast, *arg, out);
                }
            }
            // Cast: the operand, then the target type (structural).
            ExprKind::Cast { expr, ty } => {
                out.push("cast".to_string());
                out.push(s);
                out.push(en);
                ref_dump_expr(ast, *expr, out);
                ref_dump_type(ast, *ty, out);
            }
            // Assign: the compound operator, then target and value.
            ExprKind::Assign { op, target, value } => {
                out.push("assign".to_string());
                out.push(ref_assign_label(*op).to_string());
                out.push(s);
                out.push(en);
                ref_dump_expr(ast, *target, out);
                ref_dump_expr(ast, *value, out);
            }
            // Range: inclusive flag, then the optional lo/hi bounds (`(none)` when absent).
            ExprKind::Range { lo, hi, inclusive } => {
                out.push("range".to_string());
                out.push(if *inclusive { "1" } else { "0" }.to_string());
                out.push(s);
                out.push(en);
                ref_dump_opt(ast, *lo, out);
                ref_dump_opt(ast, *hi, out);
            }
            // Array literals: `[e0, …]` (count + elements) and `[value; count]`.
            ExprKind::ArrayLit { elems } => {
                out.push("array".to_string());
                out.push(elems.len().to_string());
                out.push(s);
                out.push(en);
                for e in elems {
                    ref_dump_expr(ast, *e, out);
                }
            }
            ExprKind::ArrayRepeat { value, count } => {
                out.push("arrayrepeat".to_string());
                out.push(s);
                out.push(en);
                ref_dump_expr(ast, *value, out);
                ref_dump_expr(ast, *count, out);
            }
            // StructLit: field count, the path (as a synthetic `(name span)` node matching
            // the Jestyr parser), the optional spread, then each field as
            // `(fieldinit <name span> <value>)`.
            ExprKind::StructLit { path, fields, spread } => {
                out.push("structlit".to_string());
                out.push(fields.len().to_string());
                out.push(s);
                out.push(en);
                out.push("(".to_string());
                out.push("name".to_string());
                out.push(path.span.start.to_string());
                out.push(path.span.end.to_string());
                out.push(")".to_string());
                ref_dump_opt(ast, *spread, out);
                for f in fields {
                    out.push("(".to_string());
                    out.push("fieldinit".to_string());
                    out.push(f.name.span.start.to_string());
                    out.push(f.name.span.end.to_string());
                    ref_dump_expr(ast, f.value, out);
                    out.push(")".to_string());
                }
            }
            // GenStructLit: type-arg count, field count, the ctor (as a synthetic `(name
            // span)` node), then each type-arg expr, then each `(fieldinit <name span>
            // <value>)`. No spread (generic struct literals have none).
            ExprKind::GenStructLit { ctor, type_args, fields } => {
                out.push("genstructlit".to_string());
                out.push(type_args.len().to_string());
                out.push(fields.len().to_string());
                out.push(s);
                out.push(en);
                out.push("(".to_string());
                out.push("name".to_string());
                out.push(ctor.span.start.to_string());
                out.push(ctor.span.end.to_string());
                out.push(")".to_string());
                for ta in type_args {
                    ref_dump_expr(ast, *ta, out);
                }
                for f in fields {
                    out.push("(".to_string());
                    out.push("fieldinit".to_string());
                    out.push(f.name.span.start.to_string());
                    out.push(f.name.span.end.to_string());
                    ref_dump_expr(ast, f.value, out);
                    out.push(")".to_string());
                }
            }
            // `self` / `Self` value-and-type keywords: just their span.
            ExprKind::SelfValue => {
                out.push("selfval".to_string());
                out.push(s);
                out.push(en);
            }
            ExprKind::SelfType => {
                out.push("selftype".to_string());
                out.push(s);
                out.push(en);
            }
            // Attr `@name`: the name's source span (which attribute), then the node span. A
            // call postfix on top (`@address(0x10)`) dumps as an ordinary `call` over it.
            ExprKind::Attr(name) => {
                out.push("attr".to_string());
                out.push(name.span.start.to_string());
                out.push(name.span.end.to_string());
                out.push(s);
                out.push(en);
            }
            // FString: part count, expr count, span; then each literal part's *text*, then
            // each interpolation's *name text*. Text (not spans) is the canonical form because
            // the interpolation `Name` nodes all carry the whole f-string span — see
            // parse_fstring. The Jestyr side reproduces the same text by slicing `src`.
            ExprKind::FString { parts, exprs } => {
                out.push("fstring".to_string());
                out.push(parts.len().to_string());
                out.push(exprs.len().to_string());
                out.push(s);
                out.push(en);
                for part in parts {
                    out.push(part.clone());
                }
                for e in exprs {
                    match &ast.expr_at(*e).kind {
                        ExprKind::Name(id) => out.push(id.name.clone()),
                        _ => out.push(String::new()),
                    }
                }
            }
            // Block: `block`, statement count, span, then each statement. (ref_dump_expr's
            // own outer parens wrap this — so we emit the body atoms only.)
            ExprKind::Block(b) => {
                ref_dump_block_body(ast, b, out);
            }
            // If: the condition, the then-block (wrapped), then the optional else (a Block
            // expr or a chained If).
            ExprKind::If { cond, then, els } => {
                out.push("if".to_string());
                out.push(s);
                out.push(en);
                ref_dump_expr(ast, *cond, out);
                ref_dump_block(ast, then, out);
                ref_dump_opt(ast, *els, out);
            }
            // Unsafe: just its block (wrapped).
            ExprKind::Unsafe(b) => {
                out.push("unsafe".to_string());
                out.push(s);
                out.push(en);
                ref_dump_block(ast, b, out);
            }
            // Comptime: the block as WRITTEN. The dump is the parser's golden, so it
            // must show the parse, not the folded value.
            ExprKind::Comptime(b) => {
                out.push("comptime".to_string());
                out.push(s);
                out.push(en);
                ref_dump_block(ast, b, out);
            }
            // Match: arm count, span, the scrutinee, then each arm as `(arm <pat> <guard-opt>
            // <body>)`.
            ExprKind::Match { scrut, arms } => {
                out.push("match".to_string());
                out.push(arms.len().to_string());
                out.push(s);
                out.push(en);
                ref_dump_expr(ast, *scrut, out);
                for arm in arms {
                    out.push("(".to_string());
                    out.push("arm".to_string());
                    ref_dump_pat(ast, arm.pat, out);
                    ref_dump_opt(ast, arm.guard, out);
                    ref_dump_expr(ast, arm.body, out);
                    out.push(")".to_string());
                }
            }
            // For: label, head (infinite / conditional / iterating), region, body block, and
            // the optional `else` block. The head's iterating form emits its bind count, source
            // count, each `(bind <conv> <name span>)`, each source, then the optional step.
            ExprKind::For { label, head, region, body, els } => {
                use crate::ast::ForHead;
                out.push("for".to_string());
                out.push(s);
                out.push(en);
                ref_dump_loopname(label, out);
                match head {
                    ForHead::Infinite => {
                        out.push("(".to_string());
                        out.push("headinf".to_string());
                        out.push(")".to_string());
                    }
                    ForHead::While(cond) => {
                        out.push("(".to_string());
                        out.push("headwhile".to_string());
                        ref_dump_expr(ast, *cond, out);
                        out.push(")".to_string());
                    }
                    ForHead::Iter { binds, sources, step } => {
                        out.push("(".to_string());
                        out.push("headiter".to_string());
                        out.push(binds.len().to_string());
                        out.push(sources.len().to_string());
                        for b in binds {
                            out.push("(".to_string());
                            out.push("bind".to_string());
                            out.push(ref_conv_code(b.conv).to_string());
                            out.push(b.name.span.start.to_string());
                            out.push(b.name.span.end.to_string());
                            out.push(")".to_string());
                        }
                        for src in sources {
                            ref_dump_expr(ast, *src, out);
                        }
                        ref_dump_opt(ast, *step, out);
                        out.push(")".to_string());
                    }
                }
                ref_dump_loopname(region, out);
                ref_dump_block(ast, body, out);
                match els {
                    Some(b) => ref_dump_block(ast, b, out),
                    None => {
                        out.push("(".to_string());
                        out.push("none".to_string());
                        out.push(")".to_string());
                    }
                }
            }
            ExprKind::Break(l) => {
                out.push("break".to_string());
                out.push(s);
                out.push(en);
                ref_dump_loopname(l, out);
            }
            ExprKind::Continue(l) => {
                out.push("continue".to_string());
                out.push(s);
                out.push(en);
                ref_dump_loopname(l, out);
            }
            ExprKind::Invariant(e) => {
                out.push("invariant".to_string());
                out.push(s);
                out.push(en);
                ref_dump_expr(ast, *e, out);
            }
            ExprKind::Variant(e) => {
                out.push("variant".to_string());
                out.push(s);
                out.push(en);
                ref_dump_expr(ast, *e, out);
            }
            // StructType: an anonymous `struct { … }` value — member count, span, then the
            // shared member dump (same `(sfield …)`/fn format as a struct item).
            ExprKind::StructType(body) => {
                out.push("structtype".to_string());
                out.push(body.members.len().to_string());
                out.push(s);
                out.push(en);
                ref_dump_members(ast, &body.members, out);
            }
            // Closure: param count, span, each `(cparam <name span> <type-opt>)`, then the body.
            ExprKind::Closure { params, body } => {
                out.push("closure".to_string());
                out.push(params.len().to_string());
                out.push(s);
                out.push(en);
                for cp in params {
                    out.push("(".to_string());
                    out.push("cparam".to_string());
                    out.push(cp.name.span.start.to_string());
                    out.push(cp.name.span.end.to_string());
                    match cp.ty {
                        Some(t) => ref_dump_type(ast, t, out),
                        None => {
                            out.push("(".to_string());
                            out.push("none".to_string());
                            out.push(")".to_string());
                        }
                    }
                    out.push(")".to_string());
                }
                ref_dump_expr(ast, *body, out);
            }
            // Concurrency: `concurrent { block }`, `spawn <e>`, `await <e>`.
            ExprKind::Concurrent(b) => {
                out.push("concurrent".to_string());
                out.push(s);
                out.push(en);
                ref_dump_block(ast, b, out);
            }
            ExprKind::Spawn(e) => {
                out.push("spawn".to_string());
                out.push(s);
                out.push(en);
                ref_dump_expr(ast, *e, out);
            }
            ExprKind::Await(e) => {
                out.push("await".to_string());
                out.push(s);
                out.push(en);
                ref_dump_expr(ast, *e, out);
            }
            // ParFor: the loop-var name span, span, then the iterable, reduction, and body.
            ExprKind::ParFor { var, iter, reduction, body } => {
                out.push("parfor".to_string());
                out.push(var.span.start.to_string());
                out.push(var.span.end.to_string());
                out.push(s);
                out.push(en);
                ref_dump_expr(ast, *iter, out);
                ref_dump_expr(ast, *reduction, out);
                ref_dump_expr(ast, *body, out);
            }
            // Select: arm count, span, then each `(selectarm <chan> <bind span> <body block>)`.
            ExprKind::Select(arms) => {
                out.push("select".to_string());
                out.push(arms.len().to_string());
                out.push(s);
                out.push(en);
                for arm in arms {
                    out.push("(".to_string());
                    out.push("selectarm".to_string());
                    ref_dump_expr(ast, arm.chan, out);
                    out.push(arm.bind.span.start.to_string());
                    out.push(arm.bind.span.end.to_string());
                    ref_dump_block(ast, &arm.body, out);
                    out.push(")".to_string());
                }
            }
            // Region: the region name span, span, then the body block.
            ExprKind::Region { name, body } => {
                out.push("region".to_string());
                out.push(name.span.start.to_string());
                out.push(name.span.end.to_string());
                out.push(s);
                out.push(en);
                ref_dump_block(ast, body, out);
            }
            // Any construct the P2 slice does not yet build dumps as `error`; the golden
            // corpus is curated to the handled constructs, so this arm stays unexercised.
            _ => {
                out.push("error".to_string());
                out.push(s);
                out.push(en);
            }
        }
        out.push(")".to_string());
    }

    /// The reference expression-dump line stream for `src`: lex, parse a single
    /// expression, and dump it canonically.
    fn rust_expr_dump(src: &str) -> Vec<String> {
        let (tokens, _) = crate::lexer::Lexer::new(src).tokenize();
        let (ast, root, _diags) = crate::parser::Parser::new(src, tokens).parse_single_expr();
        let mut out = Vec::new();
        ref_dump_expr(&ast, root, &mut out);
        out
    }

    /// Run the built Jestyr parser exe on `file` and return its stdout lines (the
    /// flattened AST dump — no trailing summary, so every line is compared).
    fn jestyr_expr_dump(parser_exe: &std::path::Path, file: &str) -> Vec<String> {
        let out = Command::new(parser_exe).arg(file).output().unwrap();
        assert!(out.status.success(), "jestyr parser failed on {file}");
        String::from_utf8(out.stdout).unwrap().lines().map(|s| s.to_string()).collect()
    }

    /// **P2 expression-parser cross-implementation golden.** The Jestyr-written Pratt
    /// parser (`examples/std/parser.jtr`) must build the *same expression AST* as the
    /// Rust reference on a curated corpus that exercises operator precedence,
    /// left-associativity, prefix unary, `( … )` grouping, and int/float/name leaves —
    /// verified by diffing the canonical AST dump (node kind + operator + exact span +
    /// child order) atom-for-atom. This is the acceptance test for the P2 expression
    /// slice; the corpus grows as the parser gains constructs.
    #[test]
    fn jestyr_parser_expr_dump_matches_reference() {
        let parser = build_exe("examples/std/parser_cli.jtr");
        let snippets = [
            "1 + 2 * 3",       // multiplicative binds tighter than additive
            "1 * 2 + 3",       // …either side
            "(1 + 2) * 3",     // grouping overrides precedence
            "a + b + c",       // additive is left-associative
            "a - b - c",       // …and subtraction
            "x % y / z * w",   // same-precedence multiplicative chain, left-assoc
            "- - x",           // nested prefix negation
            "!a and b",        // `!` binds tighter than `and`
            "not a or b and c", // `and` binds tighter than `or`; `not` tightest
            "a == b + c",      // additive binds tighter than comparison
            "a < b == c",      // comparisons share precedence (left-assoc here)
            "a | b ^ c & d",   // bitwise: & > ^ > |
            "a << b + c",      // additive binds tighter than shift
            "~x + 1",          // bitwise-not prefix
            "&a == &b",        // reference prefix vs comparison
            "1.5 * x - 2.0",   // float + name leaves
            "((a))",           // redundant nested grouping collapses
            "a and b or c",    // `and` before `or`
            // postfix: field / index / deref / try, and chains + mixes
            "a.b",             // field access
            "a.b.c",           // field chain is left-deep: field(field(a,b),c)
            "a[0]",            // index with a literal
            "a[i][j]",         // index chain
            "p.*",             // pointer deref
            "x?",              // try
            "a.b[c].d",        // mixed field/index/field chain
            "- a.b",           // prefix binds looser than postfix: neg(field(a,b))
            "!a.b",            // …and `not`
            "p.*.x",           // deref then field: field(deref(p),x)
            "a.b?",            // field then try: try(field(a,b))
            "a.len + b.len",   // fields as binary operands
            "arr[i + 1]",      // a binary expression inside an index
            "a[i] < b[j]",     // indexed operands in a comparison
            // calls: zero/one/many args, chaining, and mixing with other postfix
            "f()",             // no-arg call
            "f(x)",            // one arg
            "f(x, y, z)",      // several args
            "g(a + b)",        // an expression argument
            "f(g(x))",         // nested call
            "f(x)(y)",         // curried: call(call(f,x),y)
            "a.f(x)",          // method-style: call(field(a,f),x)
            "f(x) + 1",        // a call as a binary operand
            "arr[f(i)]",       // a call inside an index
            // casts: named + pointer types, chaining, precedence vs unary/binary/postfix
            "x as i32",        // named-type cast
            "x as i32 + 1",    // cast binds tighter than `+`: (x as i32) + 1
            "a + b as u8",     // …either side: a + (b as u8)
            "p as usize",      //
            "x as i32 as i64", // left-chained cast
            "- x as u8",       // prefix vs cast
            "a.b as usize",    // postfix field then cast
            "arr[i] as u8",    // postfix index then cast
            "q as *mut u8",    // pointer-type cast
            // assignment (lowest precedence, right-associative) + compound assigns
            "a = b",           // plain assignment
            "a += 1",          // compound: add-assign
            "a -= b * c",      // value is a binary expression
            "x = y = z",       // right-associative: assign(x, assign(y, z))
            "a.b = c",         // a field as the target
            "arr[i] = v",      // an index as the target
            "n *= 2",          //
            "f &= g | h",      // bit-and-assign of a bitwise value
            // ranges (infix `..` / `..=`, with an optional upper bound)
            "0..n",            // exclusive range
            "0..=len",         // inclusive range
            "a..b",            //
            "i..j + 1",        // the upper bound is a binary expression: i..(j+1)
            "lo..=hi",         //
            "arr[i..]",        // open-ended range (no upper bound) inside an index
            "arr[1..n]",       // a bounded range as an index
            "x = 0..count",    // a range as an assignment value
            // array literals: list form (with trailing comma + nesting) and repeat form
            "[1, 2, 3]",       // list of literals
            "[a, b]",          //
            "[x]",             // single element
            "[f(x), g(y)]",    // calls as elements (arg arena coexists with elem arena)
            "[1, 2, 3,]",      // trailing comma tolerated
            "[[1], [2, 3]]",   // nested arrays
            "[0; 10]",         // `[value; count]` repeat
            "[a + b; n]",      // repeat with expression value/count
            "[x][0]",          // an array literal then an index: index(array, 0)
            // struct literals: fields, spread, nesting, empty
            "Point{ x: 1, y: 2 }",       // named fields
            "P{ a: f(x) }",              // a call as a field value
            "Config{ x: 1, ..base }",    // functional-update spread
            "Wrap{ inner: Point{ x: 1, y: 2 } }", // nested struct literal
            "Empty{}",                   // no fields
            "Line{ a: p, b: q, }",       // trailing comma
            // generic struct literals: `Ctor(type_args){ fields }` — the parenthesized args
            // are reinterpreted as type arguments once the `{` is seen
            "List(i32){ len: 0 }",       // one type arg, one field
            "Map(K, V){ n: 0 }",         // several type args
            "Vec(T){}",                  // type arg, no fields
            "Pair(A, B){ a: x, b: y }",  // several type args + several fields
            "Box(i32){ v: f(x) }",       // a call as a field value (arg vs type-arg arenas coexist)
            "Wrap(T){ inner: List(i32){ len: 1 } }", // nested generic struct literal
            // self / Self keywords, Self struct literal, and @attr callables
            "self",                      // the self value
            "self.x",                    // postfix field on self: field(selfval, x)
            "self.f(x)",                 // method call on self
            "Self",                      // the Self type
            "Self{ x: 1 }",              // `Self { … }` struct literal (path span = Self)
            "Self{ p: self }",           // self value as a field value
            "@inline",                   // a bare attribute
            "@align(16)",                // a callable attribute: call(attr, 16)
            "@repr(C)",                  // callable attr with a Name argument
            "@address(0x10)",            // callable attr with a hex-literal argument
            // f-strings: literal parts + `{ident}` interpolations (dumped as text, since the
            // reference's interpolation Name nodes all carry the whole f-string span)
            "f\"hello\"",                // one literal part, no interpolation
            "f\"x = {x}\"",              // one interpolation with surrounding text
            "f\"a {x} b\"",              // parts carry leading/trailing spaces
            "f\"{x}\"",                  // empty parts on both sides of the interpolation
            "f\"{x}{y}\"",               // adjacent interpolations (three empty parts)
            "f\"sum={a}+{b}\"",          // two interpolations with a literal between
            "f\"{ x }\"",                // whitespace inside the braces is trimmed
            "f\"\"",                     // empty f-string (one empty part)
            // blocks + statements: let/var/return/expr-stmt, tail expr, nesting, empty
            "{ 1 }",                     // a block whose value is a tail expression
            "{ }",                       // an empty block
            "{ a  b }",                  // two expression statements
            "{ a; b; c }",               // explicit `;` separators
            "{ let x = 1  x }",          // a `let` then a tail expression
            "{ var y = 0  y = y + 1  y }", // `var`, an assignment statement, a tail
            "{ let x: i32 = 5  return x }", // a typed `let` and a `return` with a value
            "{ return }",                // a bare `return` (no value)
            "{ let p = f(a, b)  p.field }", // a call initializer, a field tail
            "{ { 1 } }",                 // a nested block as the tail
            "{ let q: *mut u8 = p  q.* }", // pointer-type annotation + deref tail
            // if / else / else-if chains, and unsafe blocks
            "if a { 1 }",                // if with no else
            "if a { 1 } else { 2 }",     // if / else
            "if a == b { x } else { y }", // a comparison condition (no_struct in the header)
            "if a { 1 } else if b { 2 } else { 3 }", // an else-if chain
            "if f(x) { g() }",           // a call condition, a call in the then-block
            "unsafe { p.* }",            // an unsafe block with a deref tail
            "unsafe { let v = q  v }",   // unsafe block with statements
            // comptime blocks (CTFE tier 2) — the same block-led shape as `unsafe`, so
            // the parse must agree on spans and nesting even though the block is later
            // evaluated rather than compiled.
            "comptime { 2 + 2 }",              // the minimal form
            "comptime { let x = 4  x * 2 }",   // statements then a tail expression
            "1 + comptime { 2 * 3 }",          // in value position inside a binary
            "comptime { 1 + comptime { 3 } }", // nested (inner in VALUE position)
            "comptime { if a { 1 } else { 2 } }", // a block-led form inside one
            "comptime { var t = [0; 4]  for i in 0..4 { t[i] = i }  t }", // tier 7
            "{ if a { 1 } else { 2 } }", // an if as a block statement (block-only position)
            "if a { let x = 1  x } else { 0 }", // then-block with statements + tail
            // char and bool literal leaves
            "'a'",                       // a char literal
            "true",                      // bool true
            "false",                     // bool false
            "c == 'x'",                  // char literal as a binary operand
            "flag or false",             // bool literal as a binary operand
            "if true { 1 } else { 0 }",  // bool literal as an if condition
            // match + patterns: literals, wildcard, bindings, variants, struct-variants,
            // or-patterns, ranges, guards, rest
            "match x { 0 => a, _ => b }", // a literal arm and a wildcard
            "match x { n => n }",         // an identifier binding
            "match e { circle(r) => r, rect(w, h) => w }", // positional variant patterns
            "match e { rect { w, h } => w }", // struct-variant with shorthand fields
            "match e { rect { w: 0, .. } => w }", // struct-variant with a subpattern + rest
            "match c { red | green | blue => 0, _ => 1 }", // an or-pattern
            "match n { 0..=9 => a, 10..99 => b, _ => c }", // inclusive + half-open ranges
            "match n { -1 => a, 0 => b }", // a negative literal pattern
            "match p { circle(r) if r > 0 => r, _ => 0 }", // a guard
            "match v { pair(a, ..) => a }", // `..` rest as the last variant field
            "match x { 'a' => 1, 'z' => 2, _ => 0 }", // char-literal patterns
            "match b { true => 1, false => 0 }", // bool-literal patterns
            // structural types (via casts and let annotations)
            "x as *mut u8",              // pointer, mut
            "x as *const T",             // pointer, const
            "x as List(i32)",            // generic application
            "x as Map(K, V)",            // several type args
            "x as []u8",                 // slice
            "x as [16]u8",               // fixed-size array (const length)
            "x as &T",                   // generational reference
            "x as &[r]T",                // region reference
            "x as dyn Show",             // dyn trait
            "x as mem.Alloc",            // module-qualified path (no args)
            "x as mod.Vec(i32)",         // module path with type args
            "x as fn(i32) -> i32",       // function-pointer type
            "x as fn(read i32, mut u8)", // fn params with conventions, no return
            "x as *mut List(i32)",       // nested: pointer to an application
            "{ let p: *mut u8 = q  p }", // pointer type in a let annotation
            "{ let xs: []i32 = ys  xs }", // slice type in a let annotation
            "{ let m: Map(K, V) = n  m }", // generic type in a let annotation
            // string and null literal leaves
            "\"hello\"",                 // a string literal
            "null",                      // the null literal
            "f(\"a\", \"b\")",           // string args in a call
            "x == null",                 // null as a binary operand
            "[\"one\", \"two\"]",        // strings in an array literal
            // loops: the unified `for` (infinite / conditional / iterating), labels, region,
            // else, step, zip/element+index binds, conv keywords; break/continue; invariant/variant
            "for { x }",                 // infinite loop
            "for i < n { i = i + 1 }",   // conditional (the "while" job); no_struct in the header
            "for x in xs { f(x) }",      // simple iteration over a slice
            "for i in 0..n { g(i) }",    // iteration over a range source
            "for i in 0..n step 2 { g(i) }", // a range with an explicit `step`
            "for x, i in xs { h(x, i) }", // element + index (two binds, one source)
            "for a, b in xs, ys { z(a, b) }", // lockstep zip (two binds, two sources)
            "for mut x in xs { x = 0 }",  // a `mut` iteration convention on the bind
            "for read x in xs { f(x) }",  // an explicit `read` convention
            "for _ in xs { tick() }",     // a wildcard bind
            "for outer: x in xs { break outer }", // a loop label + labeled break
            "for x in xs { continue }",   // a bare continue
            "for x in xs { if x { break } }", // break nested in the body
            "for x in xs { f(x) } else { g() }", // loop-`else` (runs if no break)
            "for i < n { i = i + 1 } else { done() }", // conditional loop with else
            "for region r { alloc(r) }",  // an infinite loop with a `region` scratch arena
            "for x in xs region r { use(r, x) }", // iterating loop with a region
            "for { invariant x > 0  variant n }", // invariant + variant inside a loop body
            "for x in 0..n { for y in 0..m { p(x, y) } }", // a nested `for` in the body
            // `struct { … }` value — an anonymous struct type in expression position
            "struct { }",                // empty struct value
            "struct { x: i32 }",         // one field
            "struct { x: i32, y: i32 }", // two fields, comma-separated
            "struct { ptr: *mut T, len: usize, cap: usize, a: Allocator }", // the List/Vec shape
            "struct { xs: []u8 }",       // a slice field type
            "struct { m: Map(K, V) }",   // a generic field type
            "struct { pub x: i32 }",     // a `pub` field
            "struct { x: i32 = 0 }",     // a field default
            "struct { d: i32 = 0, e: i32 = 1 }", // multiple defaults
            "struct { flags: @volatile u8 }",    // a `@volatile` field attribute
            "struct { bits: u8 : 3 }",   // a bit-field width
            "struct { fn area(self) -> i32 { 0 } }", // a method member
            "struct { x: i32  fn get(self) -> i32 { self.x } }", // a field + a method
            // closures: `|params| body` / `|| body`, params with optional `: T`, block bodies
            "|| 0",                      // no-parameter closure
            "|x| x",                     // one parameter, expression body
            "|n| n - 1",                 // the fn_ptr.jtr shape
            "|a, b| a + b",              // two parameters
            "|x: i32| x + 1",            // a typed parameter
            "|x: i32, y: i32| x * y",    // two typed parameters
            "|x| { let y = x  y }",      // a block body with statements
            "f(|n| n + 1)",              // a closure as a call argument
            "xs.map(|x| x * 2)",         // method-call with a closure argument
            "|x| |y| x + y",             // a closure returning a closure (nested)
            // concurrency: concurrent / spawn / await / par for / select
            "spawn f(x)",                // launch a task (operand at the unary level)
            "spawn compute(a, b)",       // a multi-arg spawned call
            "await t",                   // join a task handle
            "await a + await b",         // await binds tighter than `+`: (await a)+(await b)
            "await t as i32",            // await binds tighter than `as`: (await t) as i32
            "concurrent { spawn f() }",  // a concurrent scope with a spawn
            "concurrent { let h = spawn f(x)  await h }", // spawn a handle, await it
            "par for x in xs reduce(add) { x }", // a deterministic parallel reduction
            "par for i in 0..n reduce(sum) { i * 2 }", // par-for over a range, mapping body
            "select { recv(c) => v { use(v) } }", // a one-arm select
            "select { recv(a) => x { f(x) }  recv(b) => y { g(y) } }", // two select arms
            // standalone `region r { … }` — an arena scope
            "region r { alloc(r) }",     // a region scope with a body
            "region scratch { let p = make(scratch)  use(p) }", // region with statements
            "for x in xs { region r { f(r, x) } }", // a region nested in a loop body
        ];
        for src in snippets {
            let probe = std::env::temp_dir().join("jestyr_expr_probe.jtr");
            std::fs::write(&probe, src).unwrap();
            let got = jestyr_expr_dump(&parser, probe.to_str().unwrap());
            let want = rust_expr_dump(src);
            assert_eq!(got, want, "expression AST dump diverged on `{src}`");
        }
    }

    /// The reference item-dump for `src`: lex, parse a single item, dump it.
    fn rust_item_dump(src: &str) -> Vec<String> {
        let (tokens, _) = crate::lexer::Lexer::new(src).tokenize();
        let (ast, item, _diags) = crate::parser::Parser::new(src, tokens).parse_single_item();
        let mut out = Vec::new();
        match item {
            Some(it) => ref_dump_item(&ast, &it, &mut out),
            None => out.push("(none)".to_string()),
        }
        out
    }

    /// Run the Jestyr parser in **item mode** (a second CLI arg selects it) on `file`.
    fn jestyr_item_dump(parser_exe: &std::path::Path, file: &str) -> Vec<String> {
        let out = Command::new(parser_exe).arg(file).arg("item").output().unwrap();
        assert!(out.status.success(), "jestyr parser (item mode) failed on {file}");
        String::from_utf8(out.stdout).unwrap().lines().map(|s| s.to_string()).collect()
    }

    /// **P2 item-parser cross-implementation golden.** The Jestyr-written item parser must
    /// build the same item AST as the reference on a curated corpus — verified by diffing
    /// the canonical item dump atom-for-atom. Grows as the item parser gains kinds.
    #[test]
    fn jestyr_parser_item_dump_matches_reference() {
        let parser = build_exe("examples/std/parser_cli.jtr");
        let snippets = [
            // imports: bare, aliased, hash-pinned
            "import \"std/mem\"",           // a bare import
            "import \"list\" as lst",       // an aliased import
            "import \"core\" = \"abc123def\"", // a hash-pinned import
            // distinct nominal wrappers
            "distinct UserId = u64",        // a distinct over a primitive
            "pub distinct Meters = f64",    // a public distinct
            "distinct Handle = *mut u8",    // a distinct over a pointer type
            // consts: with/without a type annotation, public, structured value/type
            "const MAX = 100",              // an untyped const
            "pub const PI: f64 = 3.14",     // a public typed const
            "const ORIGIN: Point = Point{ x: 0, y: 0 }", // a struct-literal value
            "const SIZES: [3]i32 = [1, 2, 4]", // an array type + array literal
            // functions (fn-core: no generics/errors/contracts/attrs)
            "fn f() { }",                   // no params, no return, empty body
            "fn add(x: i32, y: i32) -> i32 { return x + y }", // typed params + return + body
            "pub fn get(read self) -> i32 { self.x }", // public, `self` receiver, convention
            "fn store(mut self, v: i32) { }", // a `mut self` receiver + a value param
            "fn g(comptime T: type, buf: []u8) { }", // a comptime param + a slice-typed param
            "fn h(take owned: List(i32)) { }", // a `take` convention + generic-type param
            "fn clamp(i: usize in 0..n) -> usize { i }", // a param refinement (`in <expr>`)
            "fn out_param(out result: i32) { }", // an `out` convention param
            "fn body_stmts() { let x = 1  x }", // a body with a let + tail expression
            // struct / record / union (struct-core: no @volatile/bit-fields)
            "struct Point { x: i32, y: i32 }", // a plain struct with two fields
            "pub record Rgb { r: u8, g: u8, b: u8 }", // a public record, three fields
            "union Bits { i: i32, f: f32 }", // an untagged union
            "struct Empty { }",             // no members
            "struct Node { value: i32, next: *mut Node }", // a self-referential field type
            "struct Config { retries: i32 = 3 }", // a field default
            "struct WithMethod { n: i32  fn get(read self) -> i32 { self.n } }", // a field + method
            "pub struct Vec2 { pub x: f64, pub y: f64 }", // public fields
            // enums: C-like, payload variants, generic, discriminants
            "enum Color { red, green, blue }", // nullary variants
            "enum Shape { circle(r: f64), rect(w: f64, h: f64) }", // named-field payloads
            "pub enum Option(T) { none, some(value: T) }", // a generic enum
            "enum Either(L, R) { left(v: L), right(v: R) }", // two type params
            "enum Status { ok = 0, err = 1 }", // explicit discriminants
            "enum Empty { }",               // no variants
            "enum Mixed { a, b(x: i32), c = 5 }", // nullary + payload + discriminant
            // traits: required signatures + default methods
            "trait Show { fn show(read self) -> str }", // one required method
            "trait Zero { fn zero() -> Self }",         // a static (no-self) method
            "pub trait Eq { fn eq(read self, read other: Self) -> bool }", // public, a param
            "trait Greet { fn hi(read self) { } }",     // a default-body method
            "trait Empty { }",                          // no methods
            // impls and externs
            "impl Show for Point { fn show(read self) -> str { self.name } }", // impl with a method
            "impl Zero for i32 { fn zero() -> i32 { 0 } }", // impl on a primitive
            "impl Drop for Buf { }",                    // an impl with no methods
            "extern \"c\" fn malloc(size: usize) -> *mut u8", // an extern with an abi
            "extern fn puts(s: *const u8) -> i32",      // an extern without an abi
            "pub extern \"c\" fn free(p: *mut u8)",     // a public extern, no return
            // attributes on fn / const / struct (incl. string args, enabled by Str leaves)
            "@inline fn fast() { }",                    // a bare attribute on a fn
            "@no_mangle const VERSION = 1",             // an attribute on a const
            "@packed struct Header { tag: u8 }",        // an attribute on a struct
            "@align(16) @packed struct Aligned { x: i64 }", // two attrs, one with an int arg
            "@section(\"data\") const BUF: i32 = 0",    // an attribute with a string argument
            "@deprecated(\"use bar\") pub fn foo() { }", // attr + pub, string arg
            // method attributes (on struct + impl methods)
            "struct S { n: i32  @inline fn get(read self) -> i32 { self.n } }", // struct method attr
            "impl T for U { @cold fn slow(read self) { } }", // impl method attr
            // field-level: @volatile and bit-fields
            "struct Mmio { status: @volatile u32 }",    // a volatile field
            "struct Flags { a: u8 : 3, b: u8 : 5 }",     // bit-field widths
            "struct Mixed { x: i32, ctrl: @volatile u16 : 4 }", // volatile + bits together
            // generics on fn + impl
            "fn id[T](x: T) -> T { x }",                 // one unbounded generic
            "fn sum[T: Add, U](x: T) -> T { x }",        // a bounded + an unbounded generic
            "impl[T] Drop for Vec(T) { fn drop(mut self) { } }", // a blanket impl generic
            "pub fn map[T: Show](x: T) { }",             // pub + a bounded generic
            "extern \"stdcall\" fn WinApi(h: i32) -> i32", // a non-default extern abi
            // fn error sets + contracts
            "fn load() -> i32 !{ NotFound, Timeout } { 0 }", // an error set
            "fn div(a: i32, b: i32) -> i32 requires b != 0 ensures result > 0 { a }", // contracts
            "fn one() -> i32 !{ Bad } requires true { 1 }", // error set + a require
        ];
        for src in snippets {
            let probe = std::env::temp_dir().join("jestyr_item_probe.jtr");
            std::fs::write(&probe, src).unwrap();
            let got = jestyr_item_dump(&parser, probe.to_str().unwrap());
            let want = rust_item_dump(src);
            assert_eq!(got, want, "item AST dump diverged on `{src}`");
        }
    }

    /// The reference module-dump for `src`: lex, parse every item, dump count + each item.
    fn rust_module_dump(src: &str) -> Vec<String> {
        let (tokens, _) = crate::lexer::Lexer::new(src).tokenize();
        let (ast, items, _diags) = crate::parser::Parser::new(src, tokens).parse_module();
        let mut out = Vec::new();
        out.push(items.len().to_string());
        for it in &items {
            ref_dump_item(&ast, it, &mut out);
        }
        out
    }

    /// Run the Jestyr parser in **module mode** (two extra CLI args) on `file`.
    fn jestyr_module_dump(parser_exe: &std::path::Path, file: &str) -> Vec<String> {
        let out = Command::new(parser_exe).arg(file).arg("x").arg("module").output().unwrap();
        assert!(out.status.success(), "jestyr parser (module mode) failed on {file}");
        String::from_utf8(out.stdout).unwrap().lines().map(|s| s.to_string()).collect()
    }

    /// Files excluded from the whole-corpus module golden. **Empty** — the P2 parser is complete
    /// and covers every expression form in the corpus (all 125 files match item-for-item). Kept
    /// as a hook in case a future corpus file needs staging. (Basename match.)
    const MODULE_GOLDEN_DENYLIST: &[&str] = &[];

    /// **P2 whole-corpus module golden** — the acceptance test for the item parser. For every
    /// real `.jtr` file *not* on the denylist, the Jestyr `parse_module` must build the same
    /// item stream as the reference. Currently 76 of the 125 corpus files (the rest await the
    /// remaining expression forms). Also asserts the denylisted files still *run* (parse to
    /// completion without crashing), so a form landing can only add coverage, never regress it.
    #[test]
    fn jestyr_parser_module_dump_matches_reference() {
        let parser = build_exe("examples/std/parser_cli.jtr");
        let mut files: Vec<std::path::PathBuf> = Vec::new();
        for dir in ["examples", "examples/std"] {
            if let Ok(rd) = std::fs::read_dir(dir) {
                for e in rd.flatten() {
                    let p = e.path();
                    if p.extension().and_then(|s| s.to_str()) == Some("jtr") {
                        files.push(p);
                    }
                }
            }
        }
        files.sort();
        assert!(files.len() > 100, "expected the whole corpus, found {}", files.len());
        let mut checked = 0;
        let mut diverged: Vec<String> = Vec::new();
        for p in &files {
            let f = p.to_str().unwrap();
            let base = p.file_name().and_then(|s| s.to_str()).unwrap();
            let src = std::fs::read_to_string(p).unwrap();
            let got = jestyr_module_dump(&parser, f); // must not crash on any file
            if MODULE_GOLDEN_DENYLIST.contains(&base) {
                continue;
            }
            let want = rust_module_dump(&src);
            if got != want {
                diverged.push(f.to_string());
                if std::env::var("DUMP_DIVERGE").is_ok() {
                    let first = got.iter().zip(want.iter()).position(|(a, b)| a != b).unwrap_or(0);
                    let lo = first.saturating_sub(4);
                    eprintln!("=== {f} (first diff at atom {first}) ===");
                    eprintln!("GOT : {:?}", &got[lo..(lo + 20).min(got.len())]);
                    eprintln!("WANT: {:?}", &want[lo..(lo + 20).min(want.len())]);
                }
            } else {
                checked += 1;
            }
        }
        assert!(diverged.is_empty(), "Jestyr module dump diverged from the reference on: {diverged:?}");
        eprintln!("whole-corpus module golden: {checked} files item-for-item identical");
    }

    // ---- P3 typeck: the resolved-type dump golden ----

    /// Does the P3 typeck pass resolve a type for this expr kind yet? Mirrors `typed_kind` in
    /// `examples/std/typeck.jtr` — the two MUST agree so the golden compares the same expression
    /// subset on both sides. Grow both together, one increment at a time.
    fn typeck_dump_kind(_k: &crate::ast::ExprKind) -> bool {
        // THE FULL STREAM: every expression is compared (mirrors `is_typed` in typeck.jtr).
        true
    }

    /// The reference resolved-type dump for `src`: parse the single file, type-check it
    /// (single-module `check`), then emit `Ty::display` for every expression whose kind the P3
    /// pass types — in ExprId order. The Jestyr `typeck.jtr` emits the identical stream.
    fn rust_typeck_dump(src: &str) -> Vec<String> {
        let (tokens, _) = crate::lexer::Lexer::new(src).tokenize();
        // `parse()` populates `ast.items` (which `check` iterates); `parse_module` returns items
        // in a separate vec, leaving `ast.items` empty ⇒ nothing inferred.
        let (ast, _diags) = crate::parser::Parser::new(src, tokens).parse();
        let (info, _d) = crate::typeck::check(&ast);
        // Representation shim: this AST materializes f-string interpolations as Name exprs; the
        // Jestyr parser stores them as text (no nodes). Skip them so both sides compare the same
        // stream. (The mirror case — the Jestyr parser materializes struct-literal paths /
        // generic-lit ctors as Name nodes — is skipped on the Jestyr side in `dump_types`.)
        let mut skip = vec![false; ast.exprs.len()];
        for ed in &ast.exprs {
            if let crate::ast::ExprKind::FString { exprs, .. } = &ed.kind {
                for e in exprs {
                    skip[e.0 as usize] = true;
                }
            }
        }
        let mut out = Vec::new();
        for (id, ed) in ast.exprs.iter().enumerate() {
            if typeck_dump_kind(&ed.kind) && !skip[id] {
                out.push(info.expr_types[id].display(&info.table));
            }
        }
        out
    }

    /// Run the Jestyr typeck exe on `file` and return its stdout lines (the resolved-type dump).
    fn jestyr_typeck_dump(exe: &std::path::Path, file: &str) -> Vec<String> {
        let out = Command::new(exe).arg(file).output().unwrap();
        assert!(out.status.success(), "jestyr typeck failed on {file}");
        String::from_utf8(out.stdout).unwrap().lines().map(|s| s.to_string()).collect()
    }

    /// Files excluded from the whole-corpus typeck golden. Each contains a literal in a position
    /// the reference's `infer` never visits — so the reference leaves it `Unknown` (`?`) while the
    /// current Jestyr pass (a context-free literal sweep) types it concretely. Those positions are
    /// *const-eval* slots (array sizes `[N]T`, enum discriminants `= 1`, attribute args, bit-field
    /// widths) and *impl/trait method bodies* (which `check_items` doesn't infer). The next
    /// increment adds the body-reachability walk that mirrors `infer`'s traversal, which types
    /// only body-reachable exprs and clears this list. (Basename match.)
    /// Files whose typed-Name stream still diverges — each needs machinery from a later P3
    /// increment, grouped by cause:
    /// - the GLOBAL TABLE (fn return types for call-bound lets, struct field types, variant
    ///   payloads for match binds, bare variant names, method resolution): compute, container,
    ///   copy_optin, discriminants, drop_nested, errors, fn_ptr, gen_vtable, guards, methods,
    ///   nested_match, niche, option, orpat, recursion, rest_pat, shapes, await, core, list,
    ///   sync, struct_variant, try_utf8, vec, vec_generic.
    /// - EXPECTED-TYPE adoption (`var xs: [5]i64 = [0; 5]` — the annotation's element type
    ///   flows onto the literal): array_lit.
    /// - comptime-generic files with a stream misalignment still to diagnose: genlist,
    ///   genmethods.
    /// Files whose typed streams still diverge — the remaining P3 machinery, by cause:
    /// - EXPECTED-TYPE propagation (`cur_expected`): a nullary generic variant adopts the
    ///   annotation's instantiation (`var m: Option(i32) = none`), and a closure adopts an
    ///   expected fn-pointer's param types: fn_ptr, gen_vtable, guards, option.
    /// - GENERIC METHOD returns (struct-value methods under the receiver's type args —
    ///   `resolve_struct_method`'s substitution — and bracket-generic unification in
    ///   `monomorphize_ret`): genmethods, core.
    const TYPECK_GOLDEN_DENYLIST: &[&str] = &[];

    /// **P3 whole-corpus resolved-type golden.** For every corpus `.jtr` file, the Jestyr typeck
    /// (`examples/std/typeck.jtr`) must resolve the *same* `Ty` for every expression whose kind
    /// the pass types as the Rust reference (`typeck::check` + `Ty::display`). Staged by kind via
    /// `typed_kind`/`typeck_dump_kind`: as the pass grows, more expression kinds enter the compared
    /// subset. Currently: the literal leaves (Int/Float/Str/Char/Bool/Null).
    #[test]
    fn jestyr_typeck_dump_matches_reference() {
        let tc = build_exe("examples/std/typeck_cli.jtr");
        let mut files: Vec<std::path::PathBuf> = Vec::new();
        for dir in ["examples", "examples/std"] {
            if let Ok(rd) = std::fs::read_dir(dir) {
                for e in rd.flatten() {
                    let p = e.path();
                    if p.extension().and_then(|s| s.to_str()) == Some("jtr") {
                        files.push(p);
                    }
                }
            }
        }
        files.sort();
        assert!(files.len() > 100, "expected the whole corpus, found {}", files.len());
        let mut checked = 0;
        let mut diverged: Vec<String> = Vec::new();
        for p in &files {
            let f = p.to_str().unwrap();
            let base = p.file_name().and_then(|s| s.to_str()).unwrap();
            if TYPECK_GOLDEN_DENYLIST.contains(&base) {
                continue;
            }
            let src = std::fs::read_to_string(p).unwrap();
            let got = jestyr_typeck_dump(&tc, f);
            let want = rust_typeck_dump(&src);
            if got != want {
                diverged.push(f.to_string());
                if std::env::var("DUMP_DIVERGE").is_ok() {
                    let first = got.iter().zip(want.iter()).position(|(a, b)| a != b).unwrap_or(0);
                    let lo = first.saturating_sub(4);
                    eprintln!("=== {f} (first diff at line {first}) ===");
                    eprintln!("GOT : {:?}", &got[lo..(lo + 20).min(got.len())]);
                    eprintln!("WANT: {:?}", &want[lo..(lo + 20).min(want.len())]);
                }
                // Deep-dive one file: TYPECK_FILE=<basename> prints the reference's compared
                // stream with each entry's ExprKind discriminant + span, to align against GOT.
                if std::env::var("TYPECK_FILE").map(|v| f.ends_with(&v)).unwrap_or(false) {
                    let (tokens, _) = crate::lexer::Lexer::new(&src).tokenize();
                    let (ast, _diags) = crate::parser::Parser::new(&src, tokens).parse();
                    let (info, _d) = crate::typeck::check(&ast);
                    let mut skip = vec![false; ast.exprs.len()];
                    for ed in &ast.exprs {
                        if let crate::ast::ExprKind::FString { exprs, .. } = &ed.kind {
                            for e in exprs {
                                skip[e.0 as usize] = true;
                            }
                        }
                    }
                    let mut k = 0usize;
                    for (id, ed) in ast.exprs.iter().enumerate() {
                        if typeck_dump_kind(&ed.kind) && !skip[id] {
                            let kindname = format!("{:?}", ed.kind);
                            let kindshort = kindname.split(['(', ' ', '{']).next().unwrap_or("?");
                            let g = got.get(k).map(String::as_str).unwrap_or("<end>");
                            let w = info.expr_types[id].display(&info.table);
                            let mark = if g == w { " " } else { "*" };
                            eprintln!(
                                "{mark} [{k:3}] id={id:3} {kindshort:12} span={}..{} want={w} got={g}",
                                ed.span.start, ed.span.end
                            );
                            k += 1;
                        }
                    }
                }
            } else {
                checked += 1;
            }
        }
        assert!(diverged.is_empty(), "Jestyr typeck dump diverged from the reference on: {diverged:?}");
        eprintln!("whole-corpus typeck golden: {checked} files' typed-expr streams identical");
    }

    // ---- P4 escape: the diagnostic-set dump golden ----

    /// The reference escape-diagnostic dump for `src`: parse, type-check, run `escape::check`,
    /// then emit each diagnostic as three lines — span start, span end, message — in emission
    /// order. The Jestyr `escape.jtr` emits the identical stream.
    fn rust_escape_dump(src: &str) -> Vec<String> {
        let (tokens, _) = crate::lexer::Lexer::new(src).tokenize();
        let (ast, _diags) = crate::parser::Parser::new(src, tokens).parse();
        let (info, _d) = crate::typeck::check(&ast);
        let diags = crate::escape::check(&ast, &info);
        let mut out = Vec::new();
        for d in &diags {
            out.push(d.span.start.to_string());
            out.push(d.span.end.to_string());
            out.push(d.message.clone());
        }
        out
    }

    /// Run the Jestyr escape checker on `file` and return its stdout lines.
    fn jestyr_escape_dump(exe: &std::path::Path, file: &str) -> Vec<String> {
        let out = Command::new(exe).arg(file).output().unwrap();
        assert!(out.status.success(), "jestyr escape checker failed on {file}");
        String::from_utf8(out.stdout).unwrap().lines().map(|s| s.to_string()).collect()
    }

    const ESCAPE_GOLDEN_DENYLIST: &[&str] = &[];

    /// **P4 whole-corpus escape golden.** For every corpus `.jtr` file, the Jestyr escape checker
    /// (`examples/std/escape.jtr`) must produce the *same set of diagnostics* (span + message, in
    /// emission order) as the Rust reference (`escape::check`). Most files are valid ⇒ empty; the
    /// escape example files carry the real diagnostics.
    #[test]
    fn jestyr_escape_dump_matches_reference() {
        let exe = build_exe("examples/std/escape_cli.jtr");
        let mut files: Vec<std::path::PathBuf> = Vec::new();
        for dir in ["examples", "examples/std"] {
            if let Ok(rd) = std::fs::read_dir(dir) {
                for e in rd.flatten() {
                    let p = e.path();
                    if p.extension().and_then(|s| s.to_str()) == Some("jtr") {
                        files.push(p);
                    }
                }
            }
        }
        files.sort();
        assert!(files.len() > 100, "expected the whole corpus, found {}", files.len());
        let mut checked = 0;
        let mut diverged: Vec<String> = Vec::new();
        for p in &files {
            let f = p.to_str().unwrap();
            let base = p.file_name().and_then(|s| s.to_str()).unwrap();
            if ESCAPE_GOLDEN_DENYLIST.contains(&base) {
                continue;
            }
            let src = std::fs::read_to_string(p).unwrap();
            let got = jestyr_escape_dump(&exe, f);
            let want = rust_escape_dump(&src);
            if got != want {
                diverged.push(f.to_string());
                if std::env::var("DUMP_DIVERGE").is_ok() {
                    let first = got.iter().zip(want.iter()).position(|(a, b)| a != b).unwrap_or(0);
                    let lo = first.saturating_sub(2);
                    eprintln!("=== {f} (first diff at line {first}) ===");
                    eprintln!("GOT : {:?}", &got[lo..(lo + 12).min(got.len())]);
                    eprintln!("WANT: {:?}", &want[lo..(lo + 12).min(want.len())]);
                }
            } else {
                checked += 1;
            }
        }
        assert!(diverged.is_empty(), "Jestyr escape dump diverged from the reference on: {diverged:?}");
        eprintln!("whole-corpus escape golden: {checked} files' diagnostic sets identical");
    }

    /// The Rust *reference* C for `src`, as lines. Uses the single-file `parse` + `typeck::check`
    /// path (not `module::load`), so `TypeInfo::debug` is empty and no `#line` directives are
    /// emitted — the target is the pure C text. `str::lines()` drops each line's trailing `\r`,
    /// so a Windows exe's CRLF stdout compares equal to this LF text.
    fn rust_cgen_dump(src: &str) -> Vec<String> {
        let (tokens, _) = crate::lexer::Lexer::new(src).tokenize();
        let (ast, _diags) = crate::parser::Parser::new(src, tokens).parse();
        let (info, _d) = crate::typeck::check(&ast);
        let (c, _cd) = crate::cgen::emit(&ast, &info);
        c.lines().map(|s| s.to_string()).collect()
    }

    /// Run the Jestyr C backend on `file` and return its stdout (the emitted C) as lines.
    fn jestyr_cgen_dump(exe: &std::path::Path, file: &str) -> Vec<String> {
        let out = Command::new(exe).arg(file).output().unwrap();
        assert!(out.status.success(), "jestyr cgen failed on {file}");
        String::from_utf8(out.stdout).unwrap().lines().map(|s| s.to_string()).collect()
    }

    /// **Driver-plumbing intrinsics** (`run_command` -> i32 via system(); `eprint_str` -> stderr):
    /// the demo's stdout must be exactly the exit code + `done` — the stderr line must NOT
    /// appear on stdout (the stream split is the point: compiler output stays clean C).
    #[test]
    fn run_command_and_eprint_demo() {
        assert_eq!(build_and_run("examples/proc_demo.jtr").replace("\r\n", "\n"), "0\ndone\n");
    }

    /// **The self-hosted DRIVER.** `jc <file> build` must gate on the ported escape checker
    /// (refusing with `path:line:col: error:` diagnostics on stderr and writing nothing),
    /// and on a clean file must write `<stem>.c`, drive gcc itself via `run_command`, and
    /// produce a runnable exe; `jc <file> run` additionally executes it, forwarding the
    /// program's exit code and stdout. The first end-to-end "daily-drive" check of the
    /// Jestyr-written compiler as a TOOL rather than a dump.
    #[test]
    fn jestyr_driver_builds_and_runs() {
        let jc = build_exe("examples/std/cgen.jtr");
        let dir = std::env::temp_dir().join("jestyr_driver_t");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // 1. A clean program builds and runs.
        let hello = dir.join("hello.jtr");
        std::fs::copy("examples/hello.jtr", &hello).unwrap();
        let out = Command::new(&jc).args([hello.to_str().unwrap(), "build"]).output().unwrap();
        assert!(out.status.success(), "driver build failed: {}", String::from_utf8_lossy(&out.stderr));
        assert!(dir.join("hello.c").exists(), "driver wrote no C");
        let exe = dir.join("hello.exe");
        assert!(exe.exists(), "driver produced no exe");
        let prog = Command::new(&exe).output().unwrap();
        let want = build_and_run("examples/hello.jtr");
        assert_eq!(String::from_utf8_lossy(&prog.stdout), want, "driver-built exe output diverged");
        // 2. `run` executes and forwards stdout + exit code.
        let out = Command::new(&jc).args([hello.to_str().unwrap(), "run"]).output().unwrap();
        assert!(out.status.success(), "driver run failed: {}", String::from_utf8_lossy(&out.stderr));
        assert_eq!(String::from_utf8_lossy(&out.stdout), want, "driver run stdout diverged");
        // 3. An escape-diagnostic file is REFUSED: non-zero exit, rendered stderr, no C written.
        let bad = dir.join("bad.jtr");
        std::fs::copy("examples/region_escape.jtr", &bad).unwrap();
        let out = Command::new(&jc).args([bad.to_str().unwrap(), "build"]).output().unwrap();
        assert!(!out.status.success(), "driver must refuse a file with escape diagnostics");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains(": error: "), "diagnostics not rendered: {stderr}");
        assert!(stderr.contains("bad.jtr:"), "diagnostic path missing: {stderr}");
        assert!(!dir.join("bad.c").exists(), "driver must not emit C for a refused file");
        // 4. A PARSE error refuses with a located syntax diagnostic (generic v1 message).
        // The scan keys on the parser's RECOVERY artifacts (Error nodes), which expression-
        // position breakage reliably produces; some item-level malformations recover into
        // plausible-but-broken items and still degrade to gcc (documented v1 partiality).
        let synbad = dir.join("synbad.jtr");
        std::fs::write(&synbad, "fn main() -> i32 { return 0 }\nfn broken() -> i32 { return ) }\n").unwrap();
        let out = Command::new(&jc).args([synbad.to_str().unwrap(), "build"]).output().unwrap();
        assert!(!out.status.success(), "driver must refuse a parse-error file");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains("syntax error"), "parse refusal not rendered: {stderr}");
        assert!(stderr.contains("synbad.jtr:2:"), "parse diagnostic location wrong: {stderr}");
        assert!(!dir.join("synbad.c").exists(), "driver must not emit C for a parse-error file");
        // 5. A TYPE error (an unknown field -> the Error type, typeerr.jtr's shape) refuses
        // likewise. (Prim-operator misuse recovers to the operand type, not Error — v1
        // catches what the checker actually marks Error.)
        let tybad = dir.join("tybad.jtr");
        std::fs::write(
            &tybad,
            "struct P { x: i32 }\nfn main() -> i32 {\n    let p: P = P { x: 1 }\n    return p.z\n}\n",
        )
        .unwrap();
        let out = Command::new(&jc).args([tybad.to_str().unwrap(), "build"]).output().unwrap();
        assert!(!out.status.success(), "driver must refuse a type-error file");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains("type error"), "type refusal not rendered: {stderr}");
        assert!(stderr.contains("tybad.jtr:4:"), "type diagnostic location wrong: {stderr}");
        eprintln!("driver: build + run + escape/parse/type refusal all green");
    }

    /// **In-language attest.** `jc <file> attest` emits the FULL attestation manifest —
    /// the header (version, source id, the sha256 of exactly the C `build` would emit
    /// via the Jestyr-written SHA-256, the locked compile flags) plus the per-item
    /// records (kind/name, vis, reconstructed Jestyr-surface signature, machine-checked
    /// guarantees) — byte-equal to the reference `attest::manifest`. Pins the ported
    /// `doc::{fn_sig, fn_guarantees, const_sig, extern_sig, ty_str}` family across
    /// contracts, error sets, refinements, `@no_panic`, records/methods, externs,
    /// enums, consts, and slice/ptr/array param types.
    #[test]
    fn jestyr_driver_attest_manifest_matches_reference() {
        let jc = build_exe("examples/std/cgen.jtr");
        for file in [
            "examples/hello.jtr",
            "examples/contracts.jtr",
            "examples/errors.jtr",
            "examples/refine.jtr",
            "examples/records.jtr",
            "examples/docs.jtr",
            "examples/extern_c.jtr",
            "examples/loops_advanced.jtr",
            "examples/shapes.jtr",
            "examples/array_lit.jtr",
        ] {
            let out = Command::new(&jc).args([file, "attest"]).output().unwrap();
            assert!(
                out.status.success(),
                "jc attest failed on {file}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let got: Vec<String> =
                String::from_utf8(out.stdout).unwrap().lines().map(|s| s.to_string()).collect();
            let src = std::fs::read_to_string(file).unwrap();
            let (tokens, _) = crate::lexer::Lexer::new(&src).tokenize();
            let (ast, _) = crate::parser::Parser::new(&src, tokens).parse();
            let (info, _) = crate::typeck::check(&ast);
            let want_full = crate::attest::manifest(file, &src, &ast, &info);
            let want: Vec<String> = want_full.lines().map(|s| s.to_string()).collect();
            if got != want {
                for (i, (g, w)) in got.iter().zip(want.iter()).enumerate() {
                    assert_eq!(g, w, "{file}: manifest line {} diverged", i + 1);
                }
                panic!("{file}: manifest line count diverged: got {} want {}", got.len(), want.len());
            }
            eprintln!("attest manifest: {file} byte-equal to the reference");
        }
    }

    /// **CTFE (workstream G, increment 1).** An array length is part of the type — of
    /// Jestyr's `Ty::Array { len }` and of the emitted C type name — so it must be known
    /// while checking, not left for the C compiler to fold. Before the comptime
    /// interpreter, only an integer *literal* was accepted and everything else silently
    /// became `0`: `[SIZE]i32` emitted a zero-length array, a type-mismatched
    /// initialization, and `assert(_ix < 0)` on every access, with no diagnostic. This
    /// pins the fix end-to-end through a real C compiler, for a const, an arithmetic
    /// expression, and a call of a pure function.
    #[test]
    fn comptime_folds_array_lengths_end_to_end() {
        let dir = std::env::temp_dir().join("jestyr_ctfe_t");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let src = "\
const SIZE: usize = 4
const DOUBLE: usize = SIZE * 2

fn width() -> usize { return 3 }

fn main() -> i32 {
    var a: [SIZE]i32 = [1, 2, 3, 4]
    var b: [SIZE + 1]i32 = [0; SIZE + 1]
    var c: [DOUBLE]i32 = [0; DOUBLE]
    var d: [width()]i32 = [7; width()]
    print_int(a[3] as i64)
    print_int(b.len as i64)
    print_int(c.len as i64)
    print_int(d[2] as i64)
    return 0
}
";
        let f = dir.join("lens.jtr");
        std::fs::write(&f, src).unwrap();
        let rel = f.to_str().unwrap();

        // No diagnostics, and the types carry the folded lengths.
        let prog = crate::module::load(rel);
        assert!(!prog.diags.iter().any(|d| d.is_error()), "load: {:?}", prog.diags);
        let (info, td) = crate::typeck::check_program(&prog.ast, &prog.modules);
        assert!(!td.iter().any(|d| d.is_error()), "typeck rejected folded lengths: {td:?}");
        let (c_src, _) = crate::cgen::emit(&prog.ast, &info);
        for want in ["JestyrArr_i32_4", "JestyrArr_i32_5", "JestyrArr_i32_8", "JestyrArr_i32_3"] {
            assert!(c_src.contains(want), "missing {want} in emitted C");
        }
        assert!(!c_src.contains("JestyrArr_i32_0"), "a zero-length ghost array survived:\n{c_src}");

        // And it runs: the C compiler agrees the types line up and the values are right.
        let exe = build_exe(rel);
        let out = Command::new(&exe).output().unwrap();
        assert!(out.status.success(), "run failed: {}", String::from_utf8_lossy(&out.stderr));
        assert_eq!(String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n"), "4\n5\n8\n7\n");
    }

    /// A length the compiler cannot evaluate is now a diagnostic rather than a silent
    /// zero — including the two ways an interpreter could otherwise fail to terminate.
    #[test]
    fn comptime_rejects_a_non_constant_array_length() {
        let dir = std::env::temp_dir().join("jestyr_ctfe_bad_t");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let cases: [(&str, &str); 4] = [
            ("runtime value", "fn main() -> i32 {\n    var n: usize = 4\n    var xs: [n]i32 = [0; 4]\n    return 0\n}\n"),
            ("cyclic const", "const A: usize = B\nconst B: usize = A\nfn main() -> i32 {\n    var xs: [A]i32 = [0; 1]\n    return 0\n}\n"),
            ("unbounded recursion", "fn f(n: usize) -> usize { return f(n + 1) }\nfn main() -> i32 {\n    var xs: [f(0)]i32 = [0; 1]\n    return 0\n}\n"),
            ("division by zero", "const Z: usize = 0\nfn main() -> i32 {\n    var xs: [4 / Z]i32 = [0; 1]\n    return 0\n}\n"),
        ];
        for (label, src) in cases {
            let f = dir.join(format!("{}.jtr", label.replace(' ', "_")));
            std::fs::write(&f, src).unwrap();
            let prog = crate::module::load(f.to_str().unwrap());
            let (_info, td) = crate::typeck::check_program(&prog.ast, &prog.modules);
            let msgs: Vec<&str> = td.iter().map(|d| d.message.as_str()).collect();
            assert!(
                msgs.iter().any(|m| m.contains("array length must be a compile-time constant")),
                "{label}: expected a length diagnostic, got {msgs:?}"
            );
        }
    }

    /// **CTFE (workstream G, increment 2) — `comptime { … }` end-to-end.** The tier-2
    /// contract is that a comptime block is indistinguishable from the literal it folds
    /// to: same type, same emitted C, same runtime behaviour. This drives a real C
    /// compiler over every value kind the interpreter produces (int, bool, string),
    /// over the two places a *constant* is structurally required (an array length and a
    /// repeat count, where the number becomes part of the C type name), and over a
    /// comptime block that calls a pure recursive function.
    #[test]
    fn comptime_blocks_fold_end_to_end() {
        let dir = std::env::temp_dir().join("jestyr_ctfe_block_t");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let src = "\
const N: usize = 3

fn tri(n: i64) -> i64 {
    if n <= 0 { return 0 }
    return n + tri(n - 1)
}

fn main() -> i32 {
    let a = comptime { 2 + 2 }
    let b = comptime { tri(4) }
    let flag = comptime { 10 > 3 }
    let s = comptime { \"ab\" + \"cd\" }
    var xs: [comptime { N * 2 }]i32 = [0; comptime { N * 2 }]
    print_int(a as i64)
    print_int(b)
    print_bool(flag)
    print_str(s)
    print_int(xs.len as i64)
    return 0
}
";
        let f = dir.join("blocks.jtr");
        std::fs::write(&f, src).unwrap();
        let rel = f.to_str().unwrap();

        let prog = crate::module::load(rel);
        assert!(!prog.diags.iter().any(|d| d.is_error()), "load: {:?}", prog.diags);
        let (info, td) = crate::typeck::check_program(&prog.ast, &prog.modules);
        assert!(!td.iter().any(|d| d.is_error()), "typeck rejected comptime blocks: {td:?}");
        let (c_src, _) = crate::cgen::emit(&prog.ast, &info);

        // What reaches C is the VALUE — the keyword and the block are gone entirely.
        assert!(!c_src.contains("comptime"), "a comptime block leaked into the C:\n{c_src}");
        assert!(c_src.contains("JestyrArr_i32_6"), "the folded length is not in the C type name");
        assert!(c_src.contains("JSTR(\"abcd\")"), "the folded string is not emitted");

        let exe = build_exe(rel);
        let out = Command::new(&exe).output().unwrap();
        assert!(out.status.success(), "run failed: {}", String::from_utf8_lossy(&out.stderr));
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n"),
            "4\n10\ntrue\nabcd\n6\n"
        );
    }

    /// A *computed* string has no source text to pass through, so it is re-encoded —
    /// and C has two rules that make the obvious encoder wrong. A hex escape is
    /// maximal-munch (`\x41` before a `1` swallows it), and `-std=c11` still honours
    /// trigraphs (`??/` means a backslash). This round-trips the awkward bytes through
    /// a real C compiler: what the interpreter computed is what the program prints.
    #[test]
    fn a_comptime_string_survives_c_escaping() {
        let dir = std::env::temp_dir().join("jestyr_ctfe_str_t");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // Quote, backslash, tab, a trigraph-shaped `??/`, and — the maximal-munch case
        // — a NUL immediately followed by digits, which a hex escape would swallow but
        // fixed-width octal cannot. Each is *concatenated* at comptime, so none of it
        // can be served by the source-literal passthrough path.
        let src = "\
fn main() -> i32 {
    print_str(comptime { \"q\\\"q\" + \"b\\\\b\" })
    print_str(comptime { \"t\\tt\" + \"??/\" })
    let z = comptime { \"\\0\" + \"1234\" }
    print_int(z.len as i64)
    return 0
}
";
        let f = dir.join("cstr.jtr");
        std::fs::write(&f, src).unwrap();
        let rel = f.to_str().unwrap();
        let prog = crate::module::load(rel);
        assert!(!prog.diags.iter().any(|d| d.is_error()), "load: {:?}", prog.diags);
        let (info, td) = crate::typeck::check_program(&prog.ast, &prog.modules);
        assert!(!td.iter().any(|d| d.is_error()), "typeck: {td:?}");

        // The escapes are in the C exactly as intended: `?` neutralised so `??/` cannot
        // become a backslash, and the NUL written as fixed-width octal.
        let (c_src, _) = crate::cgen::emit(&prog.ast, &info);
        assert!(c_src.contains(r#"JSTR("t\tt\?\?/")"#), "trigraph guard missing:\n{c_src}");
        assert!(c_src.contains(r#"JSTR("\0001234")"#), "NUL is not three-digit octal:\n{c_src}");

        let exe = build_exe(rel);
        let out = Command::new(&exe).output().unwrap();
        assert!(out.status.success(), "run failed: {}", String::from_utf8_lossy(&out.stderr));
        // The NUL case is checked by LENGTH, not by printing: `printf("%.*s")` stops at a
        // NUL whatever precision it is given, so only the compile-time length
        // (`sizeof(lit) - 1`) can show the four digits survived as their own bytes. Had
        // the encoder used a hex escape, `\x00` would have munched `1234` into one
        // (overlong) escape and the length would not be 5.
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n"),
            "q\"qb\\b\nt\tt??/\n5\n"
        );
    }

    /// **CTFE (workstream G, increment 3) — reflection end-to-end.** Tier 3 reflects the
    /// *declared shape* of a type: its name, how many fields it has, and each field's
    /// name and type, in declaration order. Every query is answered by the Jestyr
    /// compiler itself and reaches C as a literal — unlike `size_of`/`align_of`/
    /// `offset_of` beside it, which are deferred to the C compiler.
    ///
    /// Arguments must be **compile-time constants**, which is what this fixture uses.
    /// The evaluator can already walk a struct by recursion (see
    /// `comptime::tests::reflection_composes_with_the_rest_of_comptime`), but a helper
    /// that indexes by a *parameter* is not yet usable end-to-end: a top-level `fn` is
    /// also emitted as ordinary runtime code, where the parameter is not constant and
    /// the query cannot fold. Closing that needs comptime-only functions — the next
    /// slice — not the layout pass.
    #[test]
    fn comptime_reflection_folds_end_to_end() {
        let dir = std::env::temp_dir().join("jestyr_ctfe_reflect_t");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let src = "\
const LAST: i64 = 2

struct Point {
    x: i32,
    y: f64,
    label: str,
    fn shift(mut self) { self.x = self.x + 1 }
}

fn main() -> i32 {
    print_str(@type_name(Point))
    print_int(@field_count(Point))
    print_str(@field_name(Point, LAST))
    print_str(comptime { @field_name(Point, 0) + \":\" + @field_type(Point, 0) })
    print_int(comptime { @field_count(Point) * 10 })
    return 0
}
";
        let f = dir.join("reflect.jtr");
        std::fs::write(&f, src).unwrap();
        let rel = f.to_str().unwrap();

        let prog = crate::module::load(rel);
        assert!(!prog.diags.iter().any(|d| d.is_error()), "load: {:?}", prog.diags);
        let (info, td) = crate::typeck::check_program(&prog.ast, &prog.modules);
        assert!(!td.iter().any(|d| d.is_error()), "typeck rejected reflection: {td:?}");
        let (c_src, _) = crate::cgen::emit(&prog.ast, &info);

        // Answered by this compiler: the queries are literals in the C, not calls.
        assert!(c_src.contains(r#"JSTR("Point")"#), "type_name did not fold:\n{c_src}");
        assert!(!c_src.contains("@field_count("), "a reflection call leaked into the C");

        let exe = build_exe(rel);
        let out = Command::new(&exe).output().unwrap();
        assert!(out.status.success(), "run failed: {}", String::from_utf8_lossy(&out.stderr));
        // A method is not a field, so the count is 3 — and the order is the order written.
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n"),
            "Point\n3\nlabel\nx:i32\n30\n"
        );
    }

    /// **CTFE (workstream G, increment 4) — `build.jestyr` end-to-end.** A build
    /// description written in Jestyr, *evaluated* by the comptime interpreter into a
    /// plan, and then actually built: `jestyrc plan <script> --build` produces the
    /// named executables and they run.
    ///
    /// The plan is deliberately a pure function of an index rather than the imperative
    /// `exe(…)`/`test(…)` shape build systems usually take, because that shape needs
    /// compile-time *effects* — which is exactly what the tier ladder forbids. This
    /// checks both halves of the payoff: the plan is reproducible, and it builds.
    #[test]
    fn build_script_plans_and_builds_a_fixture_project() {
        let dir = std::env::temp_dir().join("jestyr_ctfe_build_t");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("greet.jtr"),
            "fn main() -> i32 { print_str(\"greetings\")\n return 0 }\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("count.jtr"),
            "fn main() -> i32 { print_int(7)\n return 0 }\n",
        )
        .unwrap();
        // The build description. Note `source`/`output` are *computed*, not literal
        // tables — the whole point of describing a build in Jestyr.
        std::fs::write(
            dir.join("build.jestyr"),
            "// The build, described in Jestyr and evaluated at compile time.\n\
             const targets: i64 = 2\n\
             fn stem(i: i64) -> str {\n\
             \x20   if i == 0 { return \"greet\" }\n\
             \x20   return \"count\"\n\
             }\n\
             fn source(i: i64) -> str { return stem(i) + \".jtr\" }\n\
             fn output(i: i64) -> str { return \"built_\" + stem(i) }\n",
        )
        .unwrap();

        // These tests live in the binary crate, so cargo does not set
        // `CARGO_BIN_EXE_*` (that is an integration-test variable). The test
        // executable sits in `target/<profile>/deps/`, so the compiler it was built
        // alongside is two directories up.
        let jestyrc = {
            let t = std::env::current_exe().unwrap();
            let profile_dir = t.parent().unwrap().parent().unwrap();
            let p = profile_dir.join(format!("jestyrc{}", std::env::consts::EXE_SUFFIX));
            assert!(p.exists(), "jestyrc binary not found at {}", p.display());
            p
        };

        // 1. The plan is what the script describes, and it is stable across runs.
        let plan_of = || {
            let out = Command::new(&jestyrc)
                .args(["plan", "build.jestyr"])
                .current_dir(&dir)
                .output()
                .unwrap();
            assert!(out.status.success(), "plan failed: {}", String::from_utf8_lossy(&out.stderr));
            String::from_utf8(out.stdout).unwrap().replace("\r\n", "\n")
        };
        let plan = plan_of();
        assert_eq!(
            plan,
            "build-plan v1\ntargets 2\ntarget greet.jtr -> built_greet\ntarget count.jtr -> built_count\n"
        );
        for _ in 0..3 {
            assert_eq!(plan_of(), plan, "the same script must always plan the same build");
        }

        // 2. `--build` produces the named executables, and they run.
        let out = Command::new(&jestyrc)
            .args(["plan", "build.jestyr", "--build"])
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(out.status.success(), "build failed: {}", String::from_utf8_lossy(&out.stderr));
        for (name, want) in [("built_greet", "greetings\n"), ("built_count", "7\n")] {
            let exe = dir.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
            assert!(exe.exists(), "{name} was not produced");
            let r = Command::new(&exe).output().unwrap();
            assert_eq!(String::from_utf8_lossy(&r.stdout).replace("\r\n", "\n"), want);
        }

        // 3. A malformed script is refused, and says so in the script's own vocabulary.
        std::fs::write(dir.join("bad.jestyr"), "const targets: str = \"two\"\n").unwrap();
        let out = Command::new(&jestyrc)
            .args(["plan", "bad.jestyr"])
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(!out.status.success(), "a malformed script must fail");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("targets"), "diagnostic should name `targets`: {err}");
    }

    /// **CTFE (workstream G, increment 8) — a build plan as DATA, end-to-end.** Tier 4
    /// described a build by answering questions about an index (`source(i)`,
    /// `output(i)`), because before a comptime `for` a target *list* still had to be
    /// spelled out entry by entry and so bought nothing.
    ///
    /// With tier 7 the list can be **built**, so a description of many targets is not
    /// many branches. Both forms stay supported and `const targets` selects between
    /// them by its type — which this pins by planning the same build both ways and
    /// requiring byte-identical output.
    #[test]
    fn a_build_plan_may_be_a_computed_list() {
        let dir = std::env::temp_dir().join("jestyr_ctfe_planlist_t");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for (f, body) in [
            ("greet.jtr", "fn main() -> i32 { print_str(\"greetings\")\n return 0 }\n"),
            ("count.jtr", "fn main() -> i32 { print_int(7)\n return 0 }\n"),
        ] {
            std::fs::write(dir.join(f), body).unwrap();
        }
        // The plan is a list of `[source, output]` pairs, built by a loop over the one
        // place a target is named.
        std::fs::write(
            dir.join("build.jestyr"),
            "const names: [2]str = [\"greet\", \"count\"]\n\
             const targets = comptime {\n\
             \x20   var t = [[\"\", \"\"]; 2]\n\
             \x20   for i in 0..2 {\n\
             \x20       t[i][0] = names[i] + \".jtr\"\n\
             \x20       t[i][1] = \"built_\" + names[i]\n\
             \x20   }\n\
             \x20   t\n\
             }\n",
        )
        .unwrap();
        // The same build, written the tier-4 way.
        std::fs::write(
            dir.join("indexed.jestyr"),
            "const targets: i64 = 2\n\
             fn stem(i: i64) -> str {\n\
             \x20   if i == 0 { return \"greet\" }\n\
             \x20   return \"count\"\n\
             }\n\
             fn source(i: i64) -> str { return stem(i) + \".jtr\" }\n\
             fn output(i: i64) -> str { return \"built_\" + stem(i) }\n",
        )
        .unwrap();

        let jestyrc = {
            let t = std::env::current_exe().unwrap();
            let profile_dir = t.parent().unwrap().parent().unwrap();
            let p = profile_dir.join(format!("jestyrc{}", std::env::consts::EXE_SUFFIX));
            assert!(p.exists(), "jestyrc binary not found at {}", p.display());
            p
        };
        let plan_of = |script: &str| {
            let out = Command::new(&jestyrc)
                .args(["plan", script])
                .current_dir(&dir)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "plan {script} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8(out.stdout).unwrap().replace("\r\n", "\n")
        };

        let plan = plan_of("build.jestyr");
        assert_eq!(
            plan,
            "build-plan v1\ntargets 2\ntarget greet.jtr -> built_greet\ntarget count.jtr -> built_count\n"
        );
        // The two forms are two ways of writing one build, so they must plan identically.
        assert_eq!(plan, plan_of("indexed.jestyr"), "the two plan forms disagreed");
        for _ in 0..3 {
            assert_eq!(plan_of("build.jestyr"), plan, "a computed plan must be reproducible");
        }

        // And it builds.
        let out = Command::new(&jestyrc)
            .args(["plan", "build.jestyr", "--build"])
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(out.status.success(), "build failed: {}", String::from_utf8_lossy(&out.stderr));
        for (name, want) in [("built_greet", "greetings\n"), ("built_count", "7\n")] {
            let exe = dir.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
            assert!(exe.exists(), "{name} was not produced");
            let r = Command::new(&exe).output().unwrap();
            assert_eq!(String::from_utf8_lossy(&r.stdout).replace("\r\n", "\n"), want);
        }

        // A malformed entry is refused by index, in the script's own vocabulary.
        std::fs::write(dir.join("bad.jestyr"), "const targets = comptime { [[\"a\"]] }\n").unwrap();
        let out = Command::new(&jestyrc)
            .args(["plan", "bad.jestyr"])
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(!out.status.success(), "a malformed list must fail");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("targets[0]"), "diagnostic should name the entry: {err}");
    }

    /// **CTFE (workstream G, increment 5) — bounded artifact generation end-to-end.**
    /// The tier-5 foundation: a build script *computes* the bytes of a generated file,
    /// the plan records the artifact by its **SHA-256** rather than its content, and
    /// `--emit` writes it.
    ///
    /// Note where the boundary sits, because it is the whole design: the evaluator
    /// gained no new power — it computed a string, exactly as it computes any other
    /// comptime value — and the *driver* places the file, only under an explicit
    /// `--emit`. Generation is a pure function whose result the user chooses to write,
    /// never an effect a script can perform. This checks reproducibility (same script →
    /// same digest), that `--emit` is required, and that the generated program is
    /// itself real Jestyr the compiler will accept.
    #[test]
    fn generated_artifacts_are_reproducible_and_written_only_on_demand() {
        let dir = std::env::temp_dir().join("jestyr_ctfe_gen_t");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // The script generates a Jestyr source file — a table of accessors derived at
        // compile time — and then names that generated file as a build target.
        std::fs::write(
            dir.join("build.jestyr"),
            "const targets: i64 = 1\n\
             fn source(i: i64) -> str { return \"gen/table.jtr\" }\n\
             fn output(i: i64) -> str { return \"gen_table\" }\n\
             \n\
             const artifacts: i64 = 1\n\
             fn artifact_path(i: i64) -> str { return \"gen/table.jtr\" }\n\
             fn rows(i: i64) -> str {\n\
             \x20   if i >= 3 { return \"\" }\n\
             \x20   return \"    print_int(\" + \"10\" + \")\\n\" + rows(i + 1)\n\
             }\n\
             fn artifact_text(i: i64) -> str {\n\
             \x20   return \"// generated at compile time -- do not edit\\n\" +\n\
             \x20          \"fn main() -> i32 {\\n\" + rows(0) + \"    return 0\\n}\\n\"\n\
             }\n",
        )
        .unwrap();

        let jestyrc = {
            let t = std::env::current_exe().unwrap();
            t.parent().unwrap().parent().unwrap().join(format!("jestyrc{}", std::env::consts::EXE_SUFFIX))
        };
        let plan_run = |extra: &[&str]| {
            let mut a = vec!["plan", "build.jestyr"];
            a.extend_from_slice(extra);
            Command::new(&jestyrc).args(a).current_dir(&dir).output().unwrap()
        };

        // 1. Planning alone does NOT write the artifact — and says so.
        let out = plan_run(&[]);
        assert!(out.status.success(), "plan failed: {}", String::from_utf8_lossy(&out.stderr));
        let plan = String::from_utf8(out.stdout).unwrap().replace("\r\n", "\n");
        assert!(!dir.join("gen/table.jtr").exists(), "an artifact was written without --emit");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("--emit"),
            "should say how to write it"
        );

        // 2. The plan records the artifact by hash, and the hash is reproducible.
        // The line is `artifact <path> <bytes> sha256 <hex>`.
        let art_line = plan
            .lines()
            .find(|l| l.starts_with("artifact gen/table.jtr "))
            .expect("an artifact line")
            .to_string();
        assert!(art_line.contains(" sha256 "), "{plan}");
        let digest = art_line.rsplit(' ').next().expect("a digest").to_string();
        for _ in 0..3 {
            let again = String::from_utf8(plan_run(&[]).stdout).unwrap().replace("\r\n", "\n");
            assert_eq!(again, plan, "the same script must generate byte-identical artifacts");
        }

        // 3. `--emit --build` writes it, and what was generated is real Jestyr: the
        //    compiler accepts the generated file and the program runs.
        let out = plan_run(&["--emit", "--build"]);
        assert!(out.status.success(), "emit+build failed: {}", String::from_utf8_lossy(&out.stderr));
        let written = std::fs::read_to_string(dir.join("gen/table.jtr")).unwrap();
        assert!(written.starts_with("// generated at compile time"), "{written}");
        assert_eq!(crate::sha256::hex(written.as_bytes()), digest, "the plan's hash must be the bytes on disk");

        let exe = dir.join(format!("gen_table{}", std::env::consts::EXE_SUFFIX));
        assert!(exe.exists(), "the generated program was not built");
        let r = Command::new(&exe).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&r.stdout).replace("\r\n", "\n"), "10\n10\n10\n");
    }

    /// **CTFE (workstream G, increment 6) — comptime tables end-to-end.** Aggregate
    /// comptime values turn CTFE from "compute a number" into "compute a *table*": a
    /// `const` initialised by a comptime block that yields a list becomes an ordinary
    /// static lookup table, indistinguishable in the output from one typed out by hand.
    ///
    /// The emission detail with teeth: a `const` must be a **brace initializer**, since
    /// a C static cannot be initialised by a GNU statement-expression — which is the
    /// shape an expression-position aggregate uses. Both paths are checked here.
    #[test]
    fn comptime_tables_become_real_statics() {
        let dir = std::env::temp_dir().join("jestyr_ctfe_table_t");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let src = "\
fn fib(n: i64) -> i64 {
    if n < 2 { return n }
    return fib(n - 1) + fib(n - 2)
}

const FIB: [8]i64 = comptime { [fib(0), fib(1), fib(2), fib(3), fib(4), fib(5), fib(6), fib(7)] }
const ZEROS: [4]i64 = comptime { [0; 4] }

fn main() -> i32 {
    print_int(FIB[7])
    print_int(FIB.len as i64)
    print_int(ZEROS[3])
    let xs = comptime { [11, 22, 33] }
    print_int(xs[1] as i64)
    print_int(comptime { [1, 2, 3][2] } as i64)
    return 0
}
";
        let f = dir.join("table.jtr");
        std::fs::write(&f, src).unwrap();
        let rel = f.to_str().unwrap();

        let prog = crate::module::load(rel);
        assert!(!prog.diags.iter().any(|d| d.is_error()), "load: {:?}", prog.diags);
        let (info, td) = crate::typeck::check_program(&prog.ast, &prog.modules);
        assert!(!td.iter().any(|d| d.is_error()), "typeck rejected a comptime table: {td:?}");
        let (c_src, _) = crate::cgen::emit(&prog.ast, &info);

        // The table was computed by *this* compiler and is a plain static: the C
        // compiler is handed the answers, not the recursion that produced them.
        assert!(
            c_src.contains("{ { 0, 1, 1, 2, 3, 5, 8, 13 } }"),
            "the fib table is not a brace initializer:\n{c_src}"
        );
        assert!(c_src.contains("{ { 0, 0, 0, 0 } }"), "the repeat table did not fold:\n{c_src}");
        // A static initializer may not be a statement-expression — if the const path
        // fell through to `emit_expr`, this would be `({ … })` and gcc would reject it.
        for line in c_src.lines().filter(|l| l.contains("jestyr_FIB")) {
            assert!(!line.contains("({"), "a static was initialized by a statement-expression: {line}");
        }

        let exe = build_exe(rel);
        let out = Command::new(&exe).output().unwrap();
        assert!(out.status.success(), "run failed: {}", String::from_utf8_lossy(&out.stderr));
        assert_eq!(String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n"), "13\n8\n0\n22\n3\n");
    }

    /// **CTFE (workstream G, increment 7) — a loop-built table end-to-end.** Tier 6 made
    /// a table's *values* computable; tier 7 makes its **shape** computable too. Before
    /// this, a table meant writing `[f(0), f(1), …]` by hand — which does not scale to
    /// the 256-entry lookup tables real code wants.
    ///
    /// What reaches C is still just the numbers: the loop, the mutation and the
    /// recursion all happen in the compiler.
    #[test]
    fn a_comptime_loop_builds_a_real_static_table() {
        let dir = std::env::temp_dir().join("jestyr_ctfe_loop_t");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // A CRC-32 style table: 256 entries, each the result of eight rounds. Nobody
        // would write this out by hand, which is the point.
        let src = "\
fn crc_entry(n: i64) -> i64 {
    var c = n
    for k in 0..8 {
        if c % 2 == 1 {
            c = 3988292384 ^ (c / 2)
        } else {
            c = c / 2
        }
    }
    return c
}

const CRC: [256]i64 = comptime {
    var t = [0; 256]
    for i in 0..256 {
        t[i] = crc_entry(i)
    }
    t
}

const SUM: i64 = comptime {
    var s = 0
    for v in CRC { s += v }
    s
}

fn main() -> i32 {
    print_int(CRC[0])
    print_int(CRC[1])
    print_int(CRC[255])
    print_int(CRC.len as i64)
    print_int(SUM)
    return 0
}
";
        let f = dir.join("crc.jtr");
        std::fs::write(&f, src).unwrap();
        let rel = f.to_str().unwrap();

        let prog = crate::module::load(rel);
        assert!(!prog.diags.iter().any(|d| d.is_error()), "load: {:?}", prog.diags);
        let (info, td) = crate::typeck::check_program(&prog.ast, &prog.modules);
        assert!(!td.iter().any(|d| d.is_error()), "typeck rejected a loop-built table: {td:?}");
        let (c_src, _) = crate::cgen::emit(&prog.ast, &info);

        // The loop ran in the compiler: what reaches C is a plain static of numbers,
        // with no trace of the iteration that produced them. (A `const` takes the
        // value prefix `j_`, not the function prefix `jestyr_`.)
        let crc_line = c_src
            .lines()
            .find(|l| l.contains("j_CRC ="))
            .expect("the table is missing from the C");
        assert!(
            crc_line.contains("{ { 0, 1996959894, 3993919788,"),
            "the CRC table did not fold to its values: {}",
            &crc_line[..crc_line.len().min(160)]
        );
        assert!(!crc_line.contains("for ("), "a comptime loop leaked into the C");
        assert!(c_src.contains("j_SUM = 549755813760"), "the folded sum is missing");

        let exe = build_exe(rel);
        let out = Command::new(&exe).output().unwrap();
        assert!(out.status.success(), "run failed: {}", String::from_utf8_lossy(&out.stderr));
        // These are the standard CRC-32 table's entries, verified against an
        // independent implementation of the same polynomial — so this pins the
        // interpreter's arithmetic, not merely its self-consistency.
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n"),
            "0\n1996959894\n755167117\n256\n549755813760\n"
        );
    }

    /// **Field iteration, end to end — tier 3's blocker, cleared by tier 7.** Reflection
    /// could always answer `@field_name(T, i)` for a constant `i`; the walk was what it
    /// could not express, because the natural way to write one is a helper function
    /// whose parameter is not a constant.
    ///
    /// A comptime `for` binding is not a function parameter, so the loop form folds
    /// where the function form could not — and because typeck never descends into a
    /// comptime body, the reflection call inside it is the interpreter's alone. This is
    /// design §8's "iterate fields, read type info, generate serializers", in ordinary
    /// Jestyr, with no macro language: what reaches C is a string constant.
    #[test]
    fn a_comptime_loop_iterates_struct_fields_end_to_end() {
        let dir = std::env::temp_dir().join("jestyr_ctfe_fields_t");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let src = "\
struct Point { x: i32, y: f64, label: str }

const SHAPE: str = comptime {
    var acc = \"\"
    for i in 0..@field_count(Point) {
        acc += @field_name(Point, i)
        acc += \": \"
        acc += @field_type(Point, i)
        if i + 1 < @field_count(Point) { acc += \", \" }
    }
    acc
}

fn main() -> i32 {
    print_str(SHAPE)
    return 0
}
";
        let f = dir.join("fields.jtr");
        std::fs::write(&f, src).unwrap();
        let rel = f.to_str().unwrap();

        let prog = crate::module::load(rel);
        assert!(!prog.diags.iter().any(|d| d.is_error()), "load: {:?}", prog.diags);
        let (info, td) = crate::typeck::check_program(&prog.ast, &prog.modules);
        assert!(!td.iter().any(|d| d.is_error()), "typeck rejected field iteration: {td:?}");
        let (c_src, _) = crate::cgen::emit(&prog.ast, &info);
        // The walk happened in the compiler: C sees one finished string.
        assert!(
            c_src.contains(r#"JSTR("x: i32, y: f64, label: str")"#),
            "the field walk did not fold to a string constant"
        );

        let exe = build_exe(rel);
        let out = Command::new(&exe).output().unwrap();
        assert!(out.status.success(), "run failed: {}", String::from_utf8_lossy(&out.stderr));
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n"),
            "x: i32, y: f64, label: str\n"
        );
    }

    /// **CTFE port mirror (M2) — the interpreter agrees with the reference.**
    /// `examples/std/ctfe.jtr` is the self-hosted comptime evaluator. This drives both
    /// implementations over the same fixtures and requires that they agree on *what a
    /// program folds to* and on *which programs are refused*.
    ///
    /// What is compared: for every top-level `const`, the folded value byte-for-byte,
    /// and the accept/refuse verdict. What is deliberately **not** compared: the error
    /// message text. The reference interpolates names into its diagnostics
    /// (``constant `A` is defined in terms of itself``) and the `.jtr` subset has no
    /// `format!`; the same concession the P-series made for the parse/typeck refusal
    /// gates, recorded there as "message-TEXT parity is the only follow-up". Both sides
    /// must still *have* a message, so a silent refusal cannot pass.
    /// The reference's half of the one-line value rendering the CTFE dump uses for
    /// aggregates. Mirrored by `render_value` in `examples/std/ctfe.jtr`; the two are a
    /// format, so they are written to be read side by side.
    fn render_ctfe_value(v: &crate::comptime::Value) -> String {
        use crate::comptime::Value;
        match v {
            Value::Int(i) => i.to_string(),
            Value::Bool(b) => b.to_string(),
            Value::Str(s) => format!(
                "\"{}\"",
                s.replace('\\', "\\\\").replace('\n', "\\n").replace('\t', "\\t").replace('\r', "\\r")
            ),
            Value::List(items) => {
                let inner: Vec<String> = items.iter().map(render_ctfe_value).collect();
                format!("[{}]", inner.join(", "))
            }
            Value::Unit => "()".to_string(),
        }
    }

    #[test]
    fn jestyr_ctfe_folding_matches_reference() {
        let exe = build_exe("examples/std/ctfe_cli.jtr");
        let dir = std::env::temp_dir().join("jestyr_ctfe_mirror_t");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let fixtures: &[(&str, &str)] = &[
            ("arith", "const A: i64 = 2 + 3 * 4\nconst B: i64 = (2 + 3) * 4\nconst C: i64 = 17 % 5\nconst D: i64 = 0 - 7 / 2\n"),
            ("bases", "const A: i64 = 0xFF\nconst B: i64 = 0b1010\nconst C: i64 = 1_000_000\n"),
            ("bitwise", "const A: i64 = 1 << 10\nconst B: i64 = 0xF0 | 0x0F\nconst C: i64 = 0xFF ^ 0x0F\nconst D: i64 = ~0\nconst E: i64 = 255 >> 4\n"),
            ("consts", "const A: i64 = 4\nconst B: i64 = A * 2\nconst C: i64 = A + B\n"),
            ("cmp", "const A: bool = 3 < 4\nconst B: bool = 3 >= 4\nconst C: bool = 5 == 5\nconst D: bool = 5 != 5\n"),
            ("shortcircuit", "fn f() -> i64 { return 0 }\nconst A: bool = false and f() == 0\nconst B: bool = true or f() == 1\n"),
            ("ifelse", "const A: i64 = if 2 > 1 { 10 } else { 20 }\nconst B: i64 = if 2 < 1 { 10 } else { 20 }\n"),
            ("calls", "fn double(x: i64) -> i64 { return x * 2 }\nconst A: i64 = double(21)\n"),
            ("recursion", "fn fact(n: i64) -> i64 {\n    if n <= 1 { return 1 }\n    return n * fact(n - 1)\n}\nconst A: i64 = fact(10)\n"),
            ("letbind", "fn area(w: i64, h: i64) -> i64 {\n    let a = w * h\n    return a + 1\n}\nconst A: i64 = area(3, 4)\n"),
            ("strings", "const A: str = \"ab\" + \"cd\"\nconst B: str = \"a\\nb\"\nconst C: bool = \"abc\" < \"abd\"\nconst D: bool = \"x\" == \"x\"\n"),
            ("chars", "const A: i64 = 'A'\nconst B: i64 = '0'\n"),
            ("casts", "const A: i64 = 65 as u8 as i64\n"),
            // comptime blocks — the tier-2 surface the port now parses
            ("blocks", "const A: i64 = comptime { 2 + 2 }\nconst B: i64 = comptime { let x = 4\n x * 2 }\nconst C: i64 = 1 + comptime { 2 * 3 }\nconst D: i64 = comptime { 1 + comptime { 3 } }\n"),
            ("blockcalls", "fn sq(x: i64) -> i64 { return x * x }\nconst N: i64 = 5\nconst A: i64 = comptime { sq(N) + 1 }\n"),
            // tier 3 — reflection over the declared shape
            ("reflect", "struct Point { x: i32, y: f64, label: str }\nconst A: str = @type_name(Point)\nconst B: i64 = @field_count(Point)\nconst C: str = @field_name(Point, 0)\nconst D: str = @field_name(Point, 2)\nconst E: str = @field_type(Point, 1)\nconst F: str = @field_type(Point, 2)\n"),
            // a primitive has a name but no fields; `record` reflects like `struct`
            ("reflect_prim", "const A: str = @type_name(i32)\n"),
            ("reflect_record", "record R { a: u8, b: u64 }\nconst A: i64 = @field_count(R)\nconst B: str = @field_name(R, 1)\n"),
            // methods are not fields, and the index may be any comptime expression
            ("reflect_methods", "struct S { v: i32, fn get(read self) -> i32 { return self.v } }\nconst A: i64 = @field_count(S)\nconst I: i64 = 0\nconst B: str = @field_name(S, I)\n"),
            // composed with the rest of comptime
            ("reflect_compose", "struct P { x: i32, y: i32 }\nconst A: str = comptime { @field_name(P, 0) + \"/\" + @field_name(P, 1) }\nconst B: bool = comptime { @field_count(P) == 2 }\n"),
            // nested/compound field types render through the shared `at_ty`
            ("reflect_tys", "struct Q { a: []i32, b: *mut u8, c: [4]i64 }\nconst A: str = @field_type(Q, 0)\nconst B: str = @field_type(Q, 1)\nconst C: str = @field_type(Q, 2)\n"),
            // refusals: each must be rejected by BOTH, with a reason
            // tier 6 — aggregates. The point of the tier: a comptime TABLE, not just a
            // number. Element order, nesting, `[v; n]`, indexing and `.len` all agree.
            ("agg_lit", "const A = [1, 2, 3]\nconst B = [true, false]\nconst C = [\"a\", \"b\"]\n"),
            ("agg_repeat", "const A = [0; 5]\nconst B = [7; 0]\n"),
            ("agg_nested", "const A = [[1, 2], [3]]\nconst B = [[0; 2]; 3]\n"),
            ("agg_index", "const T = [10, 20, 30]\nconst A: i64 = T[0]\nconst B: i64 = T[2]\nconst C: i64 = T[1 + 1]\n"),
            ("agg_len", "const T = [1, 2, 3, 4]\nconst A: i64 = T.len\nconst B: i64 = \"hello\".len\nconst C: i64 = [0; 9].len\n"),
            ("agg_nested_index", "const T = [[1, 2], [3, 4]]\nconst A: i64 = T[1][0]\nconst B: i64 = T[0].len\n"),
            ("agg_computed", "fn sq(x: i64) -> i64 { return x * x }\nconst T = [sq(1), sq(2), sq(3)]\nconst A: i64 = T[2]\n"),
            ("agg_in_block", "const A = comptime { let t = [1, 2, 3]\n t[1] }\nconst B = comptime { [1, 2] }\n"),
            ("agg_eq", "const A: bool = [1, 2] == [1, 2]\nconst B: bool = [1, 2] == [1, 3]\nconst C: bool = [1, 2] != [1]\nconst D: bool = [[1]] == [[1]]\n"),
            // tier 7 — the comptime `for` + mutation. The point of the tier: a table's
            // SHAPE is computed, not spelled out.
            ("for_table", "fn f(i: i64) -> i64 { return i * i }\nconst T = comptime { var t = [0; 6]\n for i in 0..6 { t[i] = f(i) }\n t }\n"),
            ("for_range_forms", "const A = comptime { var t = [0; 3]\n var k = 0\n for i in 0..=2 { t[k] = i\n k += 1 }\n t }\nconst B = comptime { var s = 0\n for i in 0..10 step 2 { s += i }\n s }\nconst C = comptime { var s = 0\n for i in 5..0 step 0 - 1 { s += i }\n s }\n"),
            ("for_over_list", "const SRC = [3, 5, 7]\nconst A = comptime { var s = 0\n for x in SRC { s += x }\n s }\nconst B = comptime { var t = [0; 3]\n for x, i in SRC { t[i] = x * 2 }\n t }\n"),
            ("for_cond_and_infinite", "const A = comptime { var n = 0\n for n < 5 { n += 1 }\n n }\nconst B = comptime { var n = 0\n for { n += 1\n if n == 4 { break } }\n n }\n"),
            ("for_break_continue", "const A = comptime { var s = 0\n for i in 0..10 { if i == 5 { break }\n s += i }\n s }\nconst B = comptime { var s = 0\n for i in 0..6 { if i % 2 == 0 { continue }\n s += i }\n s }\n"),
            ("for_labelled", "const A = comptime { var s = 0\n for outer: i in 0..4 { for j in 0..4 { if j == 2 { continue outer }\n if i == 3 { break outer }\n s += 1 } }\n s }\n"),
            ("for_else", "const A = comptime { var s = 0\n for i in 0..3 { s += i } else { s += 100 }\n s }\nconst B = comptime { var s = 0\n for i in 0..3 { break } else { s += 100 }\n s }\n"),
            ("for_nested_write", "const A = comptime { var m = [[0; 2]; 3]\n m[0][1] = 5\n m }\n"),
            ("assign_compound", "const A = comptime { var x = 10\n x += 5\n x -= 2\n x *= 3\n x /= 2\n x %= 7\n x }\nconst B = comptime { var b = 12\n b &= 10\n b |= 1\n b ^= 3\n b }\nconst C = comptime { var s = \"a\"\n s += \"b\"\n s }\n"),
            // tier 7 refusals
            ("bad_for_overflow_add", "const A = comptime { var x = 9223372036854775807\n x += 1\n x }\n"),
            ("bad_for_fuel", "const A = comptime { var n = 0\n for i in 0..1000000000 { }\n n }\n"),
            ("bad_for_cond_type", "const A = comptime { for 1 { }\n 0 }\n"),
            ("bad_for_step_zero", "const A = comptime { var s = 0\n for i in 0..4 step 0 { s += i }\n s }\n"),
            ("bad_for_iter_scalar", "const A = comptime { var s = 0\n for x in 7 { s += x }\n s }\n"),
            ("bad_assign_unbound", "const A = comptime { nope = 1\n 0 }\n"),
            ("bad_assign_oob", "const A = comptime { var t = [0; 2]\n t[5] = 1\n t }\n"),
            // aggregate refusals — each rejected by BOTH, with a reason
            ("bad_agg_oob", "const T = [1, 2]\nconst A: i64 = T[5]\n"),
            ("bad_agg_neg", "const T = [1, 2]\nconst A: i64 = T[0 - 1]\n"),
            ("bad_agg_index_scalar", "const A: i64 = 5[0]\n"),
            ("bad_agg_index_type", "const T = [1]\nconst A: i64 = T[\"x\"]\n"),
            ("bad_agg_len_scalar", "const A: i64 = (1).len\n"),
            ("bad_agg_order", "const A: bool = [1] < [2]\n"),
            // the fuel budget is what makes a runaway repeat a diagnostic, not an
            // allocation — and the nested form is the one where the PRODUCT blows up
            ("bad_agg_huge", "const A = [0; 10000000000]\n"),
            ("bad_agg_huge_nested", "const A = [[0; 100000]; 100000]\n"),
            ("bad_agg_repeat_count", "const A = [0; 0 - 1]\n"),
            ("bad_reflect_oob", "struct P { x: i32 }\nconst A: str = @field_name(P, 9)\n"),
            ("bad_reflect_nonstruct", "const A: i64 = @field_count(i32)\n"),
            ("bad_reflect_unknown", "struct P { x: i32 }\nconst A: i64 = @nonsense(P)\n"),
            ("bad_cycle", "const A: i64 = B\nconst B: i64 = A\n"),
            ("bad_recursion", "fn f(n: i64) -> i64 { return f(n + 1) }\nconst A: i64 = f(0)\n"),
            ("bad_divzero", "const A: i64 = 1 / 0\n"),
            ("bad_remzero", "const A: i64 = 1 % 0\n"),
            ("bad_overflow", "const A: i64 = 9223372036854775807 + 1\n"),
            ("bad_missing", "const A: i64 = MISSING\n"),
            ("bad_mixed", "const A: i64 = 1 + true\n"),
            ("bad_float", "const A: i64 = 1.5 as i64\n"),
        ];

        for (name, src) in fixtures {
            let f = dir.join(format!("{name}.jtr"));
            std::fs::write(&f, src).unwrap();
            let out = Command::new(&exe).arg(f.to_str().unwrap()).output().unwrap();
            assert!(out.status.success(), "ctfe_cli failed on {name}");
            let got: Vec<String> =
                String::from_utf8_lossy(&out.stdout).lines().map(|s| s.trim_end().to_string()).collect();

            // The reference's view of the same file, rendered in the port's format.
            let (tokens, _) = crate::lexer::Lexer::new(src).tokenize();
            let (ast, _) = crate::parser::Parser::new(src, tokens).parse();
            let mut want: Vec<String> = Vec::new();
            let mut want_err: Vec<bool> = Vec::new();
            for item in &ast.items {
                let crate::ast::Item::Const(c) = item else { continue };
                want.push(c.name.name.clone());
                match crate::comptime::Interp::new(&ast).eval(c.value) {
                    Ok(crate::comptime::Value::Int(i)) => {
                        want.push("int".into());
                        want.push(i.to_string());
                        want_err.push(false);
                    }
                    Ok(crate::comptime::Value::Bool(b)) => {
                        want.push("bool".into());
                        want.push(b.to_string());
                        want_err.push(false);
                    }
                    Ok(crate::comptime::Value::Str(s)) => {
                        want.push("str".into());
                        // The dump is line-oriented, so the port re-escapes; match it,
                        // or a value containing a newline would split into two records
                        // and read as a divergence where the two actually agree.
                        want.push(
                            s.replace('\\', "\\\\")
                                .replace('\n', "\\n")
                                .replace('\t', "\\t")
                                .replace('\r', "\\r"),
                        );
                        want_err.push(false);
                    }
                    // Aggregates (tier 6): rendered on ONE line so a list never splits
                    // into several records. `render_ctfe_value` is the reference's half
                    // of a format the port's `render_value` must match exactly.
                    Ok(v @ crate::comptime::Value::List(_)) => {
                        want.push("list".into());
                        want.push(render_ctfe_value(&v));
                        want_err.push(false);
                    }
                    Ok(_) => {
                        want.push("unit".into());
                        want_err.push(false);
                    }
                    Err(_) => {
                        want.push("error".into());
                        want_err.push(true);
                    }
                }
            }

            // Walk both records in step. Values must match exactly; an error must be an
            // error on both sides, and the port must say something about it.
            let (mut gi, mut wi, mut ei) = (0usize, 0usize, 0usize);
            while wi < want.len() {
                assert!(gi < got.len(), "{name}: port output ended early\ngot={got:?}\nwant={want:?}");
                assert_eq!(got[gi], want[wi], "{name}: const name diverged");
                let kind = &want[wi + 1];
                assert_eq!(&got[gi + 1], kind, "{name}: outcome kind diverged for `{}`", want[wi]);
                if want_err[ei] {
                    assert!(
                        got.get(gi + 2).map(|m| !m.is_empty()).unwrap_or(false),
                        "{name}: the port refused `{}` without a reason",
                        want[wi]
                    );
                    gi += 3; // name, "error", message
                    wi += 2; // name, "error"
                } else if kind == "unit" {
                    gi += 2;
                    wi += 2;
                } else {
                    assert_eq!(got[gi + 2], want[wi + 2], "{name}: value diverged for `{}`", want[wi]);
                    gi += 3;
                    wi += 3;
                }
                ei += 1;
            }
            assert_eq!(gi, got.len(), "{name}: the port emitted trailing output: {:?}", &got[gi..]);
        }
    }

    /// **Layout (workstream L, increment 1) — the model agrees with the compiler that
    /// decides.** `src/layout.rs` reproduces the C ABI rules for the types Jestyr emits.
    /// A layout model that silently disagreed with reality would be worse than none, so
    /// this does not assert the numbers — it *checks* them: for every corpus type, emit
    /// the program's real C, append a `main` that prints `sizeof`, `_Alignof` and
    /// `offsetof` for each struct and its fields, compile it with the locked `CC_FLAGS`,
    /// and compare what the C compiler reports against what the pass computed.
    ///
    /// This is what makes the later opt-in increments (field reordering, niche packing)
    /// safe to build: they will be reasoning about offsets, and the offsets are now
    /// known to be true rather than believed.
    #[test]
    fn layout_matches_c_sizeof() {
        let dir = std::env::temp_dir().join("jestyr_layout_oracle");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // Files chosen to cover the shapes the model has rules for: plain structs,
        // records, padding-heavy field orders, arrays, `distinct`, and enums.
        for file in [
            "examples/records.jtr",
            "examples/shapes.jtr",
            "examples/layout.jtr",
            "examples/distinct.jtr",
            "examples/arrays.jtr",
            "examples/option.jtr",
            "examples/mmio.jtr",
            "examples/bitfields.jtr",
            // The `@layout(auto)` demo: now that an annotated file is in the corpus, the
            // *reordered* offsets are checked by the same authority as every other one.
            "examples/layout_auto.jtr",
            // Unions. Absent from this list, the model reported `union Bits { i: i32,
            // f: f32 }` as 8 bytes with `f` at offset 4 — members laid out sequentially,
            // because the checked table stores a union as an ordinary aggregate. gcc
            // would have said so on day one.
            "examples/union.jtr",
        ] {
            let src = std::fs::read_to_string(file).unwrap();
            check_layout_against_c(file, &src, &dir);
        }
    }

    /// **`@layout(auto)` (increment L2) — the *reordered* offsets are the C compiler's
    /// too.** Reordering only pays if the model and the backend agree about where the
    /// fields landed; this asks gcc, which is the same authority `layout_matches_c_sizeof`
    /// established, now pointed at the one case where the emitted order is chosen rather
    /// than inherited.
    ///
    /// Deliberately driven by **inline** sources rather than a corpus file: an annotated
    /// `.jtr` in `examples/` is swept by the P2/P3/cgen goldens with no allowlist, so it
    /// would drag the port mirror in with it. Reference-side proof first, corpus file
    /// with the mirror — the Q-S2a ordering.
    #[test]
    fn reordered_layout_matches_c_offsetof() {
        let dir = std::env::temp_dir().join("jestyr_layout_auto_oracle");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let cases: [(&str, &str); 4] = [
            // The classic padding trap, and the shape the report's example uses.
            ("mixed", "@layout(auto) struct T { a: u8, b: u64, c: u8, d: i32 }"),
            // Fat pointers and floats: 16/8 and 4/4 next to single bytes.
            ("fat", "@layout(auto) struct T { flag: bool, name: str, ratio: f32, id: u16 }"),
            // Every field the same alignment — the permutation is the identity, so this
            // checks the tie path emits something gcc still agrees with.
            ("uniform", "@layout(auto) struct T { a: i32, b: i32, c: i32 }"),
            // A reordered struct EMBEDDED in a plain one: the outer offsets depend on
            // the inner size having actually shrunk, which is the propagation path.
            (
                "nested",
                "@layout(auto) struct Inner { a: u8, b: u64, c: u8 }\n\
                 struct Outer { i: Inner, tag: u8 }",
            ),
        ];
        for (name, decls) in cases {
            // A `main` that touches the type, so cgen emits it at all.
            let src = format!("{decls}\nfn main() -> i32 {{ print_int(size_of(T2)) return 0 }}\nstruct T2 {{ x: i32 }}\n");
            check_layout_against_c(name, &src, &dir);
        }
    }

    /// **A folded `@size_of` equals the C compiler's `sizeof`, in the same program.**
    ///
    /// This is the assertion that makes the layout queries safe to use. `@size_of(T)` is
    /// a literal this compiler computed; `size_of(T)` is `sizeof(Jestyr_T)`, which gcc
    /// computes from the struct cgen actually emitted. If they ever disagree, a program
    /// can size a buffer with one and index it with the other — so the test simply runs
    /// both, side by side, and requires each pair to be equal.
    ///
    /// Note what this is *not*: it is not `layout_matches_c_sizeof` again. That one
    /// checks the table-side model through a synthetic probe. This checks the **AST-side**
    /// model, through the value a user's own program prints, on a `@layout(auto)` struct
    /// where the two could most easily part company.
    #[test]
    fn a_folded_layout_query_equals_the_c_compilers_answer() {
        let decls = "struct Header { magic: u8, length: u64, flags: u8 }\n\
                     @layout(auto) struct Tidy { a: u8, b: u64, c: i32, d: u16 }\n\
                     struct Nested { t: Tidy, tag: u8 }\n\
                     union Bits { i: i32, f: f32 }\n\
                     enum Tagged { none, some(v: i64) }\n\
                     enum Maybe { nil, at(p: *mut i32) }\n\
                     distinct Id = u16\n";
        // Each pair is (this compiler's answer, gcc's answer) printed adjacently.
        let body = "fn main() -> i32 { \
            print_int(@size_of(Header)) print_int(size_of(Header)) \
            print_int(@size_of(Tidy)) print_int(size_of(Tidy)) \
            print_int(@size_of(Nested)) print_int(size_of(Nested)) \
            print_int(@size_of(Bits)) print_int(size_of(Bits)) \
            print_int(@size_of(Tagged)) print_int(size_of(Tagged)) \
            print_int(@size_of(Maybe)) print_int(size_of(Maybe)) \
            print_int(@size_of(Id)) print_int(size_of(Id)) \
            print_int(@align_of(Header)) print_int(align_of(Header)) \
            print_int(@align_of(Tidy)) print_int(align_of(Tidy)) \
            print_int(@offset_of(Header, length)) print_int(offset_of(Header, length)) \
            print_int(@offset_of(Tidy, a)) print_int(offset_of(Tidy, a)) \
            print_int(@offset_of(Tidy, b)) print_int(offset_of(Tidy, b)) \
            print_int(@offset_of(Nested, tag)) print_int(offset_of(Nested, tag)) \
            return 0 }";
        let out = run_inline("layout queries", &format!("{decls}{body}"));
        assert_eq!(out.len(), 26, "expected 13 pairs, got {out:?}");
        for pair in out.chunks(2) {
            assert_eq!(
                pair[0], pair[1],
                "a folded layout query disagreed with the C compiler: {out:?}"
            );
        }
        // Agreement alone is not enough: every pair would also match if nothing were
        // reordered and no niche applied, because both sides would then be wrong
        // together. So the values are pinned too, in the order printed above.
        let want = [
            ("size Header", "24"),
            // Declaration order is 32 with `a` first; emission order is
            // b(0), c(8), d(12), a(14) — 16 bytes, `a` last.
            ("size Tidy — reordered", "16"),
            ("size Nested — embeds the reordered Tidy", "24"),
            ("size Bits — a union, members overlap", "4"),
            ("size Tagged — tag + payload", "16"),
            // The one that would otherwise have shipped wrong: a niche enum IS the
            // pointer, not a tag plus one. Every other pair matched while the model
            // still said 16 — it took a value the program prints to catch it.
            ("size Maybe — niche: just the pointer", "8"),
            ("size Id — distinct: its base exactly", "2"),
            ("align Header", "8"),
            ("align Tidy", "8"),
            ("offset Header.length", "8"),
            ("offset Tidy.a — reordered to last", "14"),
            ("offset Tidy.b — reordered to first", "0"),
            ("offset Nested.tag", "16"),
        ];
        assert_eq!(want.len() * 2, out.len(), "the expectation list is out of step");
        for (i, (what, expect)) in want.iter().enumerate() {
            assert_eq!(&out[i * 2], expect, "{what}: got {out:?}");
        }
    }

    /// **`@abi(ref)` changes the calling convention and nothing else.**
    ///
    /// The same source is compiled and run twice — once with the attribute, once
    /// without — and every value must be identical. An ABI change is exactly the kind
    /// that C will compile either way and only misbehave at run time, so the check has
    /// to be a real execution rather than an inspection of the emitted text.
    ///
    /// The cases cover each way an argument can reach a by-reference parameter: a plain
    /// lvalue (`&(j_b)`, the no-copy path the attribute exists for), a **temporary**
    /// (the compound-literal path, where `&` would not compile), a field of another
    /// struct, an array element, a value passed onward to a second `@abi(ref)` callee,
    /// and a mix of by-ref and by-value parameters in one signature — which is where a
    /// mismatch between the signature's parameter order and the call site's would show.
    #[test]
    fn an_abi_ref_function_computes_the_same_answers() {
        let decls = "struct Big { a: i64, b: i64, c: i64, d: i64 }\n\
                     struct Holder { big: Big, tag: i64 }\n\
                     fn make(n: i64) -> Big { return Big { a: n, b: n + 1, c: n + 2, d: n + 3 } }\n";
        // `SUM` and `PASS` are the two functions the annotation is applied to.
        let fns = "fn sum(read v: Big, k: i64, read w: Big) -> i64 \
                   { return v.a + v.b + v.c + v.d + k + w.a }\n\
                   fn pass(read v: Big) -> i64 { return sum(v, 100, v) }\n";
        let body = "fn main() -> i32 {\n\
              let b = make(1)\n\
              let h = Holder { big: make(10), tag: 7 }\n\
              let xs: [2]Big = [make(20), make(30)]\n\
              print_int(sum(b, 5, b))\n\
              print_int(sum(make(2), 5, b))\n\
              print_int(sum(h.big, h.tag, b))\n\
              print_int(sum(xs[1], 0, xs[0]))\n\
              print_int(pass(b))\n\
              return 0\n\
            }\n";
        let plain = format!("{decls}{fns}{body}");
        let annotated = plain.replace("fn sum(", "@abi(ref) fn sum(").replace("fn pass(", "@abi(ref) fn pass(");
        let a = run_inline("abi value", &plain);
        let b = run_inline("abi ref", &annotated);
        assert_eq!(a, b, "`@abi(ref)` changed an observable value");
        // Pin the values too, so a change that broke *both* runs identically would
        // still be caught. `make(n)` sums to `4n + 6`, and `sum(v, k, w)` is
        // `sum(v) + k + w.a`:
        //   sum(b, 5, b)            = 10  + 5   + 1  = 16    (lvalue arg)
        //   sum(make(2), 5, b)      = 14  + 5   + 1  = 20    (temporary)
        //   sum(h.big, h.tag, b)    = 46  + 7   + 1  = 54    (field of a struct)
        //   sum(xs[1], 0, xs[0])    = 126 + 0   + 20 = 146   (array elements)
        //   pass(b) = sum(b, 100, b)= 10  + 100 + 1  = 111   (passed onward)
        assert_eq!(a, ["16", "20", "54", "146", "111"], "unexpected results: {a:?}");
    }

    /// **`@layout(auto)` changes the bytes and nothing else.** The offsets being right
    /// (above) is only half the claim; the other half is that a program cannot *tell*.
    /// So the same source is compiled and run twice — once annotated, once not — and
    /// every value it prints must be identical, while `size_of` must differ, which is
    /// what proves the two runs really were different layouts rather than the attribute
    /// having been quietly dropped.
    ///
    /// The cases are chosen to touch each way a struct's storage is reachable: field
    /// read and write, a nested struct, functional update (`..base`), passing by value
    /// through a function boundary, and an array of the struct — where a wrong size
    /// would corrupt every element after the first.
    #[test]
    fn a_reordered_struct_computes_the_same_answers() {
        let cases: [(&str, &str); 4] = [
            (
                "read/write",
                "fn main() -> i32 { var t = T { a: 1, b: 2, c: 3, d: 4 } t.b = 20 t.a = 10 \
                 print_int(t.a as i64) print_int(t.b as i64) print_int(t.c as i64) print_int(t.d as i64) return 0 }",
            ),
            (
                "spread + by-value call",
                "fn total(read t: T) -> i64 { return t.a as i64 + t.b as i64 + t.c as i64 + t.d as i64 } \
                 fn main() -> i32 { let base = T { a: 1, b: 2, c: 3, d: 4 } let u = T { b: 20, ..base } \
                 print_int(total(base)) print_int(total(u)) return 0 }",
            ),
            // An array of the struct: the element *stride* is its size, so a wrong size
            // corrupts every element after the first. (Read-only: `xs[i].f = v` is a
            // separate, pre-existing cgen gap — the index lowers to a statement
            // expression, which is not an lvalue.)
            (
                "array of the struct",
                "fn main() -> i32 { let xs: [3]T = [T { a: 1, b: 7, c: 3, d: 4 }, T { a: 2, b: 8, c: 3, d: 4 }, T { a: 3, b: 9, c: 3, d: 4 }] \
                 print_int(xs[0].b as i64) print_int(xs[1].b as i64) print_int(xs[2].b as i64) print_int(xs[2].a as i64) return 0 }",
            ),
            (
                "nested",
                "fn main() -> i32 { let n = N { inner: T { a: 1, b: 2, c: 3, d: 4 }, tag: 7 } \
                 print_int(n.inner.b as i64) print_int(n.tag as i64) return 0 }",
            ),
        ];
        let decls = "struct N { inner: T, tag: u8 }\n";
        for (what, body) in cases {
            let plain = format!("struct T {{ a: u8, b: u64, c: u8, d: i32 }}\n{decls}{body}\n");
            let auto = format!("@layout(auto) {plain}");
            let (a, b) = (run_inline(what, &plain), run_inline(what, &auto));
            assert_eq!(a, b, "`@layout(auto)` changed an observable value in the `{what}` case");
        }
        // The layouts really were different — otherwise every comparison above is
        // vacuous. `size_of` is C's own `sizeof`, so this is the C compiler answering.
        let sz = "fn main() -> i32 { print_int(size_of(T)) return 0 }";
        let plain = format!("struct T {{ a: u8, b: u64, c: u8, d: i32 }}\n{sz}\n");
        assert_eq!(run_inline("size", &plain), vec!["24"]);
        assert_eq!(run_inline("size", &format!("@layout(auto) {plain}")), vec!["16"]);
    }

    /// Compile an inline Jestyr source through the real backend, run it, and return its
    /// whitespace-separated output. Single-file only (no module loader), which is what
    /// keeps these cases out of `examples/` and therefore out of the goldens.
    fn run_inline(label: &str, src: &str) -> Vec<String> {
        let (tokens, ld) = crate::lexer::Lexer::new(src).tokenize();
        let (ast, pd) = crate::parser::Parser::new(src, tokens).parse();
        assert!(
            !ld.iter().chain(pd.iter()).any(|d| d.is_error()),
            "{label}: fixture must parse: {:?}",
            pd
        );
        let (info, td) = crate::typeck::check(&ast);
        assert!(!td.iter().any(|d| d.is_error()), "{label}: typeck errors: {td:?}");
        let (c_src, cd) = crate::cgen::emit(&ast, &info);
        assert!(!cd.iter().any(|d| d.is_error()), "{label}: cgen errors: {cd:?}");

        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let uniq = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir();
        let cfile = dir.join(format!("jestyr_inline_{uniq}.c"));
        let exe = dir.join(format!("jestyr_inline_{uniq}{}", std::env::consts::EXE_SUFFIX));
        std::fs::write(&cfile, &c_src).unwrap();
        let cc = crate::find_c_compiler().expect("c-oracle needs a C compiler on PATH");
        let st = Command::new(&cc)
            .args(crate::CC_FLAGS)
            .arg("-o")
            .arg(&exe)
            .arg(&cfile)
            .output()
            .unwrap();
        assert!(st.status.success(), "{label}: gcc failed: {}", String::from_utf8_lossy(&st.stderr));
        let out = Command::new(&exe).output().unwrap();
        assert!(out.status.success(), "{label}: the program did not run");
        String::from_utf8_lossy(&out.stdout).split_whitespace().map(|s| s.to_string()).collect()
    }

    /// **`catch` recovers, and the fallback runs only when it must.**
    ///
    /// The short-circuit is the property worth *running* for: `catch` supplies a
    /// fallback, not a default argument, so evaluating it on the success path would be
    /// wasted work — and, for a fallback with side effects, plainly wrong. No amount of
    /// reading the emitted text proves that; a fallback that prints does.
    ///
    /// The chain is the other case inspection would miss. `a catch b catch c` must be
    /// **right-associative** so it tries each in turn; parsed the other way it would
    /// apply `c` to an already-recovered value and quietly return the wrong one — and
    /// both parses compile.
    #[test]
    fn catch_recovers_and_short_circuits() {
        let src = "\
struct P { x: i32, y: i32 }

fn small(n: i32) -> i32 !{ TooBig } {
    if n > 100 { return err(TooBig) }
    return ok(n * 2)
}
fn noisy() -> i32 { print_int(999) return -1 }
fn make() -> P !{ TooBig } { return err(TooBig) }

// `catch` inside a FALLIBLE function: recovering one call must not change the
// enclosing signature.
fn inner(n: i32) -> i32 !{ TooBig } {
    let v: i32 = small(500) catch n
    return ok(v + 1)
}

fn main() -> i32 {
    print_int((small(5) catch 0) as i64)                      // ok path  -> 10
    print_int((small(500) catch 0) as i64)                    // err path -> 0
    print_int((small(500) catch small(7) catch 99) as i64)    // -> 14, second wins
    print_int((small(500) catch small(900) catch 99) as i64)  // -> 99, both fail
    print_int((small(5) catch noisy()) as i64)                // NO 999 -> 10
    print_int((small(500) catch noisy()) as i64)              // 999, then -1
    let p: P = make() catch P { x: 3, y: 4 }
    print_int(p.x as i64)
    print_int(unwrap(inner(41)) as i64)
    return 0
}
";
        assert_eq!(
            run_inline("catch", src),
            [
                "10", "0", // the ok path, then the err path
                "14", "99", // right-associative chain: second wins; then both fail
                "10", // the success path must NOT print 999 — the short-circuit
                "999", "-1", // …and the error path must
                "3",  // a struct ok-type recovers with a struct fallback
                "42", // `catch` inside a fallible fn leaves it fallible
            ],
            "catch recovered to the wrong value, or evaluated its fallback eagerly"
        );
    }

    /// Emit `src`'s real C, append a `main` printing the C compiler's own
    /// `sizeof`/`_Alignof`/`offsetof` for every emitted struct, and require every number
    /// to match `layout::compute`. Shared by the declaration-order and reordered oracles
    /// so the two cannot drift into checking different things.
    fn check_layout_against_c(label: &str, src: &str, dir: &std::path::Path) {
        {
            let file = label;
            let src = src.to_string();
            let (tokens, _) = crate::lexer::Lexer::new(&src).tokenize();
            let (ast, _) = crate::parser::Parser::new(&src, tokens).parse();
            let (info, _) = crate::typeck::check(&ast);
            let layouts = crate::layout::compute(&ast, &info);
            let (c_src, _) = crate::cgen::emit(&ast, &info);

            // A probe `main` that prints the C compiler's own answers. Structs only:
            // an enum's C shape is a tagged union whose member names are cgen's business,
            // while a struct's field names are the user's and so are stable to name here.
            let mut probe = String::new();
            let mut expect: Vec<(String, u64)> = Vec::new();
            probe.push_str("\nint main(void){\n");
            for t in &layouts {
                if t.incomplete || t.fields.is_empty() {
                    continue;
                }
                let cname = format!("Jestyr_{}", t.name);
                // cgen emits `struct Jestyr_X { … };` (with a forward typedef above it).
                if !c_src.contains(&format!("struct {cname} {{")) {
                    continue; // the type was not emitted (unused / a generic instance)
                }
                probe.push_str(&format!("  printf(\"%zu\\n\", sizeof({cname}));\n"));
                expect.push((format!("{} size", t.name), t.size));
                probe.push_str(&format!("  printf(\"%zu\\n\", _Alignof({cname}));\n"));
                expect.push((format!("{} align", t.name), t.align));
                for f in &t.fields {
                    probe.push_str(&format!(
                        "  printf(\"%zu\\n\", offsetof({cname}, j_{}));\n",
                        f.name
                    ));
                    expect.push((format!("{}.{} offset", t.name, f.name), f.offset));
                }
            }
            probe.push_str("  return 0;\n}\n");
            if expect.is_empty() {
                return;
            }

            // The emitted program already has its own `main`; rename it so the probe's
            // can take over without touching cgen. Matched on the opening `int main(`
            // rather than a full signature, because cgen emits `(void)` or
            // `(int argc, char** argv)` depending on whether the program reads args.
            let body = c_src.replacen("\nint main(", "\nint jestyr_unused_main(", 1);
            let full = format!("#include <stddef.h>\n#include <stdio.h>\n{body}{probe}");
            let stem: String = file.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
            let cfile = dir.join(format!("{stem}.c"));
            let exe = dir.join(format!("{stem}{}", std::env::consts::EXE_SUFFIX));
            std::fs::write(&cfile, &full).unwrap();

            let cc = crate::find_c_compiler().expect("c-oracle needs a C compiler on PATH");
            let out = Command::new(&cc)
                .args(crate::CC_FLAGS)
                .arg(&cfile)
                .arg("-o")
                .arg(&exe)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "layout probe for {file} did not compile: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let run = Command::new(&exe).output().unwrap();
            assert!(run.status.success(), "layout probe for {file} did not run");
            let got: Vec<u64> = String::from_utf8_lossy(&run.stdout)
                .lines()
                .map(|l| l.trim().parse::<u64>().expect("probe prints numbers"))
                .collect();
            assert_eq!(got.len(), expect.len(), "{file}: probe output count");
            for (i, (what, want)) in expect.iter().enumerate() {
                assert_eq!(
                    got[i], *want,
                    "{file}: {what} — the C compiler says {}, the layout pass says {}",
                    got[i], want
                );
            }
            eprintln!("layout: {file} — {} values match the C compiler", expect.len());
        }
    }

    /// **Workstream Q — the SIMD legality pass is *sound*: a certified body computes
    /// the same bits at every lane width.**
    ///
    /// `src/simd.rs` decides which `par for` bodies may be evaluated a lane at a time.
    /// That is a claim about a machine, so — exactly as `layout_matches_c_sizeof` makes
    /// gcc the authority on layout — this makes gcc the authority on vectorization: for
    /// every body the pass **certifies**, the same expression is evaluated scalar-wise
    /// and through GCC vector extensions at widths **2, 4 and 8**, reduced by all four
    /// declared deterministic reductions (sum, xor, min, max), and every answer must be
    /// bit-identical.
    ///
    /// Two design points make it a real test rather than a ritual:
    ///
    /// * **One expression, not two.** Scalar and vector share a single `#define F(x)`,
    ///   so there is no transcription between them; the only variable under test is
    ///   GCC's vector lowering. Only the *select* primitive is written twice (`?:` for
    ///   scalar, a mask blend for vectors) — which is precisely the lowering difference
    ///   the whitelist licenses, so writing it twice is the point, not a workaround.
    /// * **The element count is deliberately not a multiple of any width** (1003), so
    ///   every run exercises an uneven tail — where a lane-width dependence would first
    ///   appear if the reduction were not exactly associative.
    ///
    /// The direction being proved is the one that is a soundness claim. The pass is
    /// conservative: bodies it rejects may well vectorize, and that is not tested,
    /// because nothing depends on it.
    #[test]
    fn simd_lanes_match_scalar_bit_for_bit() {
        // (Jestyr body — must be certified by the pass, C twin — one shared expression)
        let cases: [(&str, &str); 5] = [
            ("x", "(x)"),
            ("x * x", "((x)*(x))"),
            ("(x & 7) | (x << 3)", "(((x) & 7) | ((x) << 3))"),
            ("~x + x * x", "((~(x)) + (x)*(x))"),
            ("if x > 0 { x * x } else { 0 - x }", "SEL((x) > 0, (x)*(x), (0 - (x)))"),
        ];

        // Half of the test: the pass must actually certify each body. A case it
        // rejected would make the C half prove nothing about this pass.
        for (jbody, _) in &cases {
            let src = format!(
                "fn f(read s: []i64) -> i64 {{\n    return par for x in s reduce(core.sum_reduction()) {{ {jbody} }}\n}}\n"
            );
            let (tokens, _) = crate::lexer::Lexer::new(&src).tokenize();
            let (ast, d) = crate::parser::Parser::new(&src, tokens).parse();
            assert!(d.iter().all(|x| !x.is_error()), "fixture must parse: {jbody}");
            let sites = crate::simd::analyze(&ast);
            assert_eq!(sites.len(), 1);
            assert!(
                sites[0].verdict.is_legal(),
                "the pass must certify `{jbody}`, else the lane check proves nothing: {:?}",
                sites[0].verdict
            );
        }

        let mut c = String::new();
        c.push_str(
            "#include <stdint.h>\n#include <stdio.h>\n#include <string.h>\n\
             typedef int64_t v2 __attribute__((vector_size(16)));\n\
             typedef int64_t v4 __attribute__((vector_size(32)));\n\
             typedef int64_t v8 __attribute__((vector_size(64)));\n\
             /* Not a multiple of 2, 4 or 8: every width runs an uneven tail. */\n\
             #define N 1003\n\
             static int64_t a[N];\n\
             /* Deterministic input — no clock, no rand, no host dependence. */\n\
             static void fill(void){ for (int i=0;i<N;i++) a[i] = ((int64_t)i * 7919) % 10007 - 5003; }\n\
             #define SHOW(s,x,mn,mx) printf(\"%lld %lld %lld %lld\\n\", (long long)(s), (long long)(x), (long long)(mn), (long long)(mx))\n\
             /* One scalar reduction, all four declared deterministic operators. */\n\
             #define SCALAR(FN) { int64_t s=0, xr=0, mn=INT64_MAX, mx=INT64_MIN; \\\n\
               for (int i=0;i<N;i++){ int64_t v = FN(a[i]); s+=v; xr^=v; \\\n\
                 if (v<mn) mn=v; if (v>mx) mx=v; } SHOW(s,xr,mn,mx); }\n\
             /* The same four, W lanes at a time, then a lane fold and a scalar tail. */\n\
             #define VEC_HEAD(VT, W, FN) VT va, vv, m; \\\n\
               VT vs = (VT){0}, vx = (VT){0}; \\\n\
               VT vmn = (VT){0} + INT64_MAX, vmx = (VT){0} + INT64_MIN; \\\n\
               int i = 0; \\\n\
               for (; i + (W) <= N; i += (W)) { \\\n\
                 memcpy(&va, &a[i], sizeof(VT)); \\\n\
                 vv = FN(va); \\\n\
                 vs = vs + vv; vx = vx ^ vv; \\\n\
                 m = vv < vmn; vmn = (vv & m) | (vmn & ~m); \\\n\
                 m = vv > vmx; vmx = (vv & m) | (vmx & ~m); } \\\n\
               int64_t s=0, xr=0, mn=INT64_MAX, mx=INT64_MIN; \\\n\
               for (int j=0;j<(W);j++){ s+=vs[j]; xr^=vx[j]; \\\n\
                 if (vmn[j]<mn) mn=vmn[j]; if (vmx[j]>mx) mx=vmx[j]; }\n\
             /* The leftover elements are SCALAR code, so they need the scalar lowering\n\
                of a select — a separate macro precisely so `SEL` can be flipped back\n\
                between the two. Sharing one macro across both silently miscompiled the\n\
                tail on the first run of this test. */\n\
             #define VEC_TAIL(FN) for (; i<N; i++){ int64_t v = FN(a[i]); s+=v; xr^=v; \\\n\
                 if (v<mn) mn=v; if (v>mx) mx=v; } SHOW(s,xr,mn,mx);\n",
        );
        for (i, (_, cexpr)) in cases.iter().enumerate() {
            c.push_str(&format!("#define F{i}(x) {cexpr}\n"));
        }
        // A select lowers differently in the two forms — `?:` for scalars, a mask blend
        // per lane (a GNU vector comparison yields all-ones/all-zeros, and `?:` is not
        // defined on vectors). Flipping this ONE primitive around each section is what
        // lets the body itself stay single-sourced; the tail flips it back because the
        // tail is scalar code.
        let scalar_sel = "#undef SEL\n#define SEL(c,t,f) ((c) ? (t) : (f))\n";
        let vector_sel = "#undef SEL\n#define SEL(c,t,f) (((t) & (c)) | ((f) & ~(c)))\n";
        c.push_str("int main(void){ fill();\n");
        for i in 0..cases.len() {
            c.push_str(scalar_sel);
            c.push_str(&format!("  SCALAR(F{i})\n"));
            for (vt, w) in [("v2", 2), ("v4", 4), ("v8", 8)] {
                c.push_str("  {\n");
                c.push_str(vector_sel);
                c.push_str(&format!("  VEC_HEAD({vt}, {w}, F{i})\n"));
                c.push_str(scalar_sel);
                c.push_str(&format!("  VEC_TAIL(F{i})\n"));
                c.push_str("  }\n");
            }
        }
        c.push_str("  return 0;\n}\n");

        let dir = std::env::temp_dir().join("jestyr_simd_oracle");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let cfile = dir.join("lanes.c");
        let exe = dir.join(format!("lanes{}", std::env::consts::EXE_SUFFIX));
        std::fs::write(&cfile, &c).unwrap();

        // The locked FP/codegen flags — the same ones every `jestyrc` build uses.
        let cc = crate::find_c_compiler().expect("c-oracle needs a C compiler on PATH");
        let out = Command::new(&cc)
            .args(crate::CC_FLAGS)
            .arg(&cfile)
            .arg("-o")
            .arg(&exe)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "the lane probe did not compile: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let run = Command::new(&exe).output().unwrap();
        assert!(run.status.success(), "the lane probe did not run");
        let lines: Vec<String> = String::from_utf8_lossy(&run.stdout)
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect();
        assert_eq!(lines.len(), cases.len() * 4, "four results per body");

        for (i, (jbody, _)) in cases.iter().enumerate() {
            let scalar = &lines[i * 4];
            for (k, w) in [2usize, 4, 8].iter().enumerate() {
                assert_eq!(
                    &lines[i * 4 + 1 + k],
                    scalar,
                    "`{jbody}`: {w}-wide lanes gave `{}` but scalar gave `{scalar}` \
                     (sum xor min max) — the legality pass certified a body whose value \
                     depends on the lane width",
                    lines[i * 4 + 1 + k]
                );
            }
            eprintln!("simd: `{jbody}` — scalar == 2 == 4 == 8 lanes ({scalar})");
        }
    }

    /// **Doc-comment trivia in-language.** `tokens.collect_docs` recovers exactly the
    /// reference lexer's `tokenize_with_docs` third result — kind, block-ness, the whole
    /// comment span, and the text span — over the whole corpus. It finds comments by
    /// scanning the GAPS between tokens (trivia by construction), which is what lets it be
    /// a second, additive pass leaving the golden-pinned token stream untouched.
    #[test]
    fn jestyr_doc_trivia_matches_reference() {
        let exe = build_exe("examples/std/doc_cli.jtr");
        let mut files: Vec<std::path::PathBuf> = Vec::new();
        for dir in ["examples", "examples/std"] {
            if let Ok(rd) = std::fs::read_dir(dir) {
                for e in rd.flatten() {
                    let p = e.path();
                    if p.extension().and_then(|s| s.to_str()) == Some("jtr") {
                        files.push(p);
                    }
                }
            }
        }
        files.sort();

        // The corpus is all `///` line docs, so a fixture carries the edge cases: the block
        // forms, both demotions (`////`, `/***`), the empty `/**/`, a nested plain comment,
        // a trailing doc with no item after it — and, the load-bearing one for a pass that
        // scans between tokens, comment markers INSIDE a string literal, which must not be
        // collected at all.
        let dir = std::env::temp_dir().join("jestyr_doc_trivia_t");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let fixture = dir.join("trivia.jtr");
        std::fs::write(
            &fixture,
            r#"//! module doc one
//! module doc two

/// outer doc for f
//// demoted: not a doc
// plain comment
fn f() -> i32 { return 0 }

/** an outer BLOCK doc
 * with a javadoc margin
 */
fn g() -> i32 { return 1 }

/*! an inner block doc */

/*** demoted block */
/**/
/* plain /* nested */ still plain */
fn h() -> str { return "/// not a doc /* nor this */" }

/// trailing outer doc, attached to nothing
"#,
        )
        .unwrap();
        files.push(fixture);

        let (mut checked, mut docs_seen, mut blocks_seen) = (0usize, 0usize, 0usize);
        for p in &files {
            let f = p.to_str().unwrap();
            let src = std::fs::read_to_string(p).unwrap();
            let (_toks, _diags, want) = crate::lexer::Lexer::new(&src).tokenize_with_docs();
            let out = Command::new(&exe).arg(f).output().unwrap();
            assert!(out.status.success(), "doc_cli failed on {f}");
            let got: Vec<String> = String::from_utf8_lossy(&out.stdout)
                .replace("\r\n", "\n")
                .lines()
                .map(|s| s.to_string())
                .collect();
            assert_eq!(got.len(), want.len(), "{f}: doc-comment COUNT diverged");
            for (i, w) in want.iter().enumerate() {
                let n: Vec<usize> = got[i].split(' ').map(|x| x.parse().unwrap()).collect();
                let kind = if w.kind == crate::doc::DocKind::Inner { 1 } else { 0 };
                assert_eq!(
                    (n[0], n[1], n[2], n[3]),
                    (kind, w.block as usize, w.span.start as usize, w.span.end as usize),
                    "{f}: doc {i} kind/block/span diverged"
                );
                // The reference's `text` IS a slice of the source, so comparing it against
                // the port's text SPAN pins the content and the exact offsets at once.
                assert_eq!(&src[n[4]..n[5]], w.text, "{f}: doc {i} text span diverged");
                docs_seen += 1;
                blocks_seen += w.block as usize;
            }
            // Pin the fixture's shape so it can't silently stop covering the edge cases.
            if f.ends_with("trivia.jtr") {
                let kinds: Vec<(usize, bool)> =
                    want.iter().map(|d| (d.kind == crate::doc::DocKind::Inner) as usize).zip(want.iter().map(|d| d.block)).collect();
                assert_eq!(
                    kinds,
                    vec![(1, false), (1, false), (0, false), (0, true), (1, true), (0, false)],
                    "fixture no longer covers the demotions / block forms / string-literal case"
                );
                assert!(
                    want.iter().all(|d| !d.text.contains("not a doc")),
                    "a comment marker inside a string literal was collected as a doc comment"
                );
            }
            checked += 1;
        }
        assert!(docs_seen > 40, "corpus should exercise real doc comments, saw {docs_seen}");
        eprintln!("doc trivia golden: {checked} file(s), {docs_seen} doc comment(s) ({blocks_seen} block form) identical");
    }

    /// **In-language `doc`.** `jc <file> doc` reproduces `doc::generate(.., html=false)`
    /// byte-for-byte over the whole corpus: the trivia grouping (contiguous `///` runs merge,
    /// a blank line splits them, block comments stand alone), the margin/marker cleaning, the
    /// summary + `#`-section split with fenced code held verbatim, the reconstructed
    /// signatures for all eight target kinds, struct-method grouping, and the Guarantees
    /// block — which comes from the SAME extractor attest uses, so prose and proven facts
    /// cannot drift apart.
    #[test]
    fn jestyr_doc_matches_reference() {
        let jc = build_exe("examples/std/cgen.jtr");
        let mut files: Vec<std::path::PathBuf> = Vec::new();
        for dir in ["examples", "examples/std"] {
            if let Ok(rd) = std::fs::read_dir(dir) {
                for e in rd.flatten() {
                    let p = e.path();
                    if p.extension().and_then(|s| s.to_str()) == Some("jtr") {
                        files.push(p);
                    }
                }
            }
        }
        files.sort();
        let mut diverged: Vec<String> = Vec::new();
        let mut with_docs = 0usize;
        for p in &files {
            let f = p.to_str().unwrap();
            let src = std::fs::read_to_string(p).unwrap();
            let stem = p.file_stem().and_then(|s| s.to_str()).unwrap();
            let (want, _notices) = crate::doc::generate(&src, stem, false);
            let out = Command::new(&jc).args([f, "doc"]).output().unwrap();
            assert!(out.status.success(), "jc doc failed on {f}");
            let got = String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n");
            if got != want {
                let (gl, wl): (Vec<&str>, Vec<&str>) = (got.lines().collect(), want.lines().collect());
                let at = gl.iter().zip(wl.iter()).position(|(a, b)| a != b);
                diverged.push(format!(
                    "{f}: first diff at line {:?}\n  want: {:?}\n  got:  {:?}",
                    at.map(|i| i + 1),
                    at.and_then(|i| wl.get(i)),
                    at.and_then(|i| gl.get(i))
                ));
            }
            if want.contains("### `") {
                with_docs += 1;
            }
        }
        assert!(diverged.is_empty(), "doc output diverged:\n{}", diverged.join("\n"));
        assert!(with_docs > 100, "corpus should render real API pages, saw {with_docs}");

        // The dangling-doc lint: a `///` displaced by a nearer block, and one attached to
        // nothing, are both reported at their own location. (The message text and location are
        // ported; the reference's snippet decoration is not.)
        let dir = std::env::temp_dir().join("jestyr_doc_t");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let dangle = dir.join("dangle.jtr");
        std::fs::write(
            &dangle,
            "/// documents f\nfn f() -> i32 { return 0 }\n\n\
             /// displaced by the nearer block below\n\n\
             /// the nearest block wins\nfn g() -> i32 { return 1 }\n\n\
             /// attached to nothing at all\n",
        )
        .unwrap();
        let src = std::fs::read_to_string(&dangle).unwrap();
        let (want, notices) = crate::doc::generate(&src, "dangle", false);
        assert_eq!(notices.len(), 2, "fixture should produce exactly two dangling docs");
        let out = Command::new(&jc).args([dangle.to_str().unwrap(), "doc"]).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n"), want, "dangle page diverged");
        let stderr = String::from_utf8_lossy(&out.stderr);
        let lints: Vec<&str> = stderr.lines().filter(|l| l.contains("not attached to an item")).collect();
        assert_eq!(lints.len(), 2, "expected two dangling-doc warnings, got: {stderr}");
        // Each warning must sit at exactly the location the reference diagnostic reports.
        for (lint, want_d) in lints.iter().zip(notices.iter()) {
            let upto = &src[..want_d.span.start as usize];
            let (line, col) = (upto.matches('\n').count() + 1, upto.len() - upto.rfind('\n').map_or(0, |i| i + 1) + 1);
            assert!(
                lint.contains(&format!("dangle.jtr:{line}:{col}:")),
                "warning location diverged (want {line}:{col}): {lint}"
            );
        }
        // The nearest block won, so the displaced one's text must not appear on the page.
        assert!(want.contains("the nearest block wins"), "nearest block should be attached");
        assert!(!want.contains("displaced by the nearer block"), "displaced block should not render");
        eprintln!("doc golden: {} file(s) byte-identical; dangling lint located exactly", files.len());
    }

    /// The two sides of the attest-diff fixture: one API surface, evolved so that every
    /// classifier branch fires — removal (pub and internal), addition, a signature change,
    /// `@no_panic` lost and gained, an error added, `requires` dropped, `ensures` dropped, a
    /// refinement removed / newly added / widened / narrowed, and a visibility demotion.
    const ATTEST_API_V1: &str = "\
pub fn stays(a: i32) -> i32 { return a }
pub fn goes_private(a: i32) -> i32 { return a }
pub fn removed_pub(a: i32) -> i32 { return a }
fn removed_priv(a: i32) -> i32 { return a }
pub fn sig_change(a: i32) -> i32 { return a }
@no_panic pub fn loses_np(a: i32) -> i32 { return a }
pub fn gains_np(a: i32) -> i32 { return a }
pub fn errs(a: i32, b: i32) -> i32 !{ Zero } {
    if b == 0 { return err(Zero) }
    return ok(a / b)
}
pub fn reqs(b: i32) -> i32
    requires b != 0
{
    return b
}
pub fn enss(x: i32) -> i32
    ensures result >= 0
{
    if x < 0 { return 0 - x }
    return x
}
pub fn drops_refine(i: usize in 0..8) -> i32 { return i as i32 }
pub fn gains_refine(i: usize) -> i32 { return i as i32 }
pub fn widen(i: usize in 0..10) -> i32 { return i as i32 }
pub fn narrow(i: usize in 0..100) -> i32 { return i as i32 }
pub const LIMIT: i32 = 10
pub struct Pair { pub x: i32, pub y: i32 }
fn main() -> i32 {
    print_int(stays(1) as i64)
    return 0
}
";

    const ATTEST_API_V2: &str = "\
pub fn stays(a: i32) -> i32 { return a }
fn goes_private(a: i32) -> i32 { return a }
pub fn added_new(a: i32) -> i32 { return a }
pub fn sig_change(a: i64) -> i64 { return a }
pub fn loses_np(a: i32) -> i32 { return a }
@no_panic pub fn gains_np(a: i32) -> i32 { return a }
pub fn errs(a: i32, b: i32) -> i32 !{ Zero, Negative } {
    if b == 0 { return err(Zero) }
    if b < 0 { return err(Negative) }
    return ok(a / b)
}
pub fn reqs(b: i32) -> i32 { return b }
pub fn enss(x: i32) -> i32 {
    if x < 0 { return 0 - x }
    return x
}
pub fn drops_refine(i: usize) -> i32 { return i as i32 }
pub fn gains_refine(i: usize in 0..4) -> i32 { return i as i32 }
pub fn widen(i: usize in 0..20) -> i32 { return i as i32 }
pub fn narrow(i: usize in 0..50) -> i32 { return i as i32 }
pub const LIMIT: i32 = 20
pub struct Pair { pub x: i32, pub y: i32 }
fn main() -> i32 {
    print_int(stays(1) as i64)
    return 0
}
";

    /// Render a manifest through the reference, single-file path (no `#line` debug info — the
    /// same shape the port's loader produces for an importless file).
    #[cfg(feature = "c-oracle")]
    fn attest_manifest_of(source_id: &str, src: &str) -> String {
        let (tokens, _) = crate::lexer::Lexer::new(src).tokenize();
        let (ast, _) = crate::parser::Parser::new(src, tokens).parse();
        let (info, _) = crate::typeck::check(&ast);
        crate::attest::manifest(source_id, src, &ast, &info)
    }

    /// **In-language `attest --diff`.** The ported differ — manifest parse-back, `sig_core`
    /// subtraction, the four structured contract sets, integer-range widening, and the total
    /// `(item, verdict, detail)` order — reproduces `attest::diff(…).render()` byte-for-byte,
    /// exit code included (breaking changes fail the gate). Also covers `attest-verify`, which
    /// re-renders the current manifest and diffs it against the recorded one: unchanged source
    /// reports no drift at all, and a changed source reproduces the same report as the
    /// two-manifest diff — proving the freshly-rendered manifest is byte-equal to the recorded
    /// one, C hash included.
    #[test]
    fn jestyr_driver_attest_diff_matches_reference() {
        let jc = build_exe("examples/std/cgen.jtr");
        let dir = std::env::temp_dir().join("jestyr_attest_diff_t");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let v1_src = dir.join("api_v1.jtr");
        let v2_src = dir.join("api_v2.jtr");
        std::fs::write(&v1_src, ATTEST_API_V1).unwrap();
        std::fs::write(&v2_src, ATTEST_API_V2).unwrap();
        let (id1, id2) = (v1_src.to_str().unwrap(), v2_src.to_str().unwrap());
        let (m1, m2) = (attest_manifest_of(id1, ATTEST_API_V1), attest_manifest_of(id2, ATTEST_API_V2));
        let (p1, p2) = (dir.join("v1.manifest"), dir.join("v2.manifest"));
        std::fs::write(&p1, &m1).unwrap();
        std::fs::write(&p2, &m2).unwrap();

        let render = |old: &str, new: &str| -> String {
            let o = crate::attest::parse_manifest(old).unwrap();
            let n = crate::attest::parse_manifest(new).unwrap();
            crate::attest::diff(&o, &n).render()
        };
        let run = |args: [&str; 3]| -> (String, bool) {
            let out = Command::new(&jc).args(args).output().unwrap();
            (String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n"), out.status.success())
        };

        // The evolved surface: every verdict branch, in the reference's total order.
        let want = render(&m1, &m2);
        let (got, ok) = run([p1.to_str().unwrap(), "attest-diff", p2.to_str().unwrap()]);
        assert_eq!(got, want, "attest-diff report diverged from the reference");
        assert!(!ok, "a breaking diff must fail the gate");
        assert!(want.contains("9 breaking, 6 compatible"), "fixture lost coverage: {want}");
        for phrase in [
            "removed (was pub)",
            "removed (internal)",
            "added",
            "no longer `pub`",
            "signature changed:",
            "lost `@no_panic`",
            "gained `@no_panic`",
            "error added `Negative`",
            "`requires b != 0` removed",
            "`ensures result >= 0` removed",
            "constraint removed",
            "newly constrained to `0..4`",
            "widened `0..10` → `0..20`",
            "narrowed `0..100` → `0..50`",
        ] {
            assert!(want.contains(phrase), "fixture no longer covers {phrase:?}");
        }

        // An unchanged surface: no note line, no changes, and the gate passes.
        let want_same = render(&m1, &m1);
        let (got_same, ok_same) = run([p1.to_str().unwrap(), "attest-diff", p1.to_str().unwrap()]);
        assert_eq!(got_same, want_same, "self-diff report diverged from the reference");
        assert!(ok_same, "an empty diff must pass the gate");
        assert!(got_same.contains("no API changes"), "self-diff should be empty: {got_same}");

        // `attest-verify`: re-render the CURRENT manifest and diff the recorded one against it.
        // Against its own source that is a total no-op — including the C hash, so the
        // "emitted C differs" note must NOT appear.
        let (got_v, ok_v) = run([id1, "attest-verify", p1.to_str().unwrap()]);
        assert!(ok_v, "verifying an unchanged source must pass: {got_v}");
        assert!(got_v.contains("no API changes"), "unchanged source reported drift: {got_v}");
        assert!(!got_v.contains("note:"), "freshly-rendered manifest drifted from the recorded one: {got_v}");

        // Against the evolved source it must reproduce the two-manifest report exactly.
        let (got_v2, ok_v2) = run([id2, "attest-verify", p1.to_str().unwrap()]);
        assert_eq!(got_v2, want, "attest-verify report diverged from the two-manifest diff");
        assert!(!ok_v2, "a breaking verify must fail the gate");

        // A non-manifest input fails fast rather than diffing as empty.
        let out = Command::new(&jc)
            .args([v1_src.to_str().unwrap(), "attest-diff", p2.to_str().unwrap()])
            .output()
            .unwrap();
        assert!(!out.status.success(), "a non-manifest input must be refused");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("not a valid attest manifest"),
            "refusal not rendered: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        eprintln!("attest diff/verify: byte-equal to the reference across every verdict branch");
    }

    /// **In-language modules.** `jc <file> build|run` flattens the import closure itself
    /// (the driver's `ml_*` loader: DFS deps-first, imports dropped, `binding.x` -> `x`,
    /// cross-module top-level collisions renamed) — multi-file programs compile through
    /// the ported compiler with no Rust anywhere in the loop. Checked against the Rust
    /// reference's module build, collisions included.
    #[test]
    fn jestyr_driver_builds_multi_module() {
        let jc = build_exe("examples/std/cgen.jtr");
        let dir = std::env::temp_dir().join("jestyr_ml_t");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // Two libs sharing a top-level name (`mag`) — the collision-rename path — plus a
        // diamond (`app` and `vec2` both import `util`).
        std::fs::write(dir.join("util.jtr"), "pub fn mag(x: i32) -> i32 { return x * 10 }\n").unwrap();
        std::fs::write(
            dir.join("vec2.jtr"),
            "import \"util\"\npub struct V2 { pub x: i32, pub y: i32 }\npub fn make(x: i32, y: i32) -> V2 { return V2 { x: x, y: y } }\npub fn mag(v: V2) -> i32 { return util.mag(v.x) + v.y }\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("app.jtr"),
            "import \"util\"\nimport \"vec2\"\nfn main() -> i32 {\n    let v: vec2.V2 = vec2.make(4, 2)\n    print_int(vec2.mag(v) as i64)\n    print_int(util.mag(3) as i64)\n    return 0\n}\n",
        )
        .unwrap();
        let app = dir.join("app.jtr");
        let out = Command::new(&jc).args([app.to_str().unwrap(), "run"]).output().unwrap();
        assert!(out.status.success(), "driver multi-module run failed: {}", String::from_utf8_lossy(&out.stderr));
        let got = String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n");
        // The Rust reference builds the same program through its real module loader.
        let want_exe = build_exe(app.to_str().unwrap());
        let want_out = Command::new(&want_exe).output().unwrap();
        let want = String::from_utf8_lossy(&want_out.stdout).replace("\r\n", "\n");
        assert_eq!(got, want, "multi-module output diverged from the reference module build");
        assert_eq!(want, "42\n30\n", "fixture sanity");

        // Per-FILE diagnostic attribution (the `#line` analogue): an escape error inside an
        // imported module must render against THAT file's original line:col, and an error in
        // the importing file must render at its ORIGINAL line — i.e. unshifted by the removed
        // `import` line above it (the checkpoint mapper's job).
        std::fs::write(
            dir.join("bad_lib.jtr"),
            "// a library whose third line returns a borrow\n// padding line\npub fn leak(read s: String) -> String { return s }\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("app2.jtr"),
            "import \"bad_lib\"\n// padding line\npub fn leak2(read s: String) -> String { return s }\nfn main() -> i32 { return 0 }\n",
        )
        .unwrap();
        let app2 = dir.join("app2.jtr");
        let out = Command::new(&jc).args([app2.to_str().unwrap(), "build"]).output().unwrap();
        assert!(!out.status.success(), "driver must refuse the bad multi-module program");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("bad_lib.jtr:3:48: error: cannot return borrow"),
            "imported module's diagnostic not attributed to its file: {stderr}"
        );
        assert!(
            stderr.contains("app2.jtr:3:"),
            "importer's diagnostic line not corrected for the removed import: {stderr}"
        );
        eprintln!("driver modules: multi-file + collision + diamond + per-file attribution all green");
    }

    /// **The self-build.** The ported compiler compiles ITSELF from its real multi-file
    /// sources (`examples/std/cgen.jtr` + its 9 imports) through its own module loader and
    /// its own gcc driver — and the result is the same compiler: byte-identical C on a
    /// probe file. No Rust and no harness flatten anywhere in the loop.
    #[cfg(feature = "selfhost-fixpoint")]
    #[test]
    fn jestyr_driver_builds_itself() {
        let jc = build_exe("examples/std/cgen.jtr");
        let dir = std::env::temp_dir().join("jestyr_selfbuild_t");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for m in SELFHOST_MODULES {
            std::fs::copy(format!("examples/std/{m}.jtr"), dir.join(format!("{m}.jtr"))).unwrap();
        }
        let root = dir.join("cgen.jtr");
        let out = Command::new(&jc).args([root.to_str().unwrap(), "build"]).output().unwrap();
        assert!(out.status.success(), "self-build failed: {}", String::from_utf8_lossy(&out.stderr));
        let jc2 = dir.join("cgen.exe");
        assert!(jc2.exists(), "self-build produced no exe");
        let probe = "examples/hello.jtr";
        let a = Command::new(&jc).arg(probe).output().unwrap();
        let b = Command::new(&jc2).arg(probe).output().unwrap();
        assert!(b.status.success(), "self-built compiler failed on {probe}");
        assert_eq!(
            String::from_utf8_lossy(&a.stdout),
            String::from_utf8_lossy(&b.stdout),
            "the self-built compiler emits different C"
        );
        eprintln!("SELF-BUILD: jc compiled itself from its multi-file sources via its own loader + driver");
    }

    /// Replace every **offset-derived symbol suffix** with a placeholder, so two builds
    /// can be compared on everything else.
    ///
    /// `concurrent { spawn … }` mints two symbols per task — the thread entry function
    /// `jestyr_task_<N>` and its argument struct `struct _jsp_<N>` — where `N` is the
    /// task body's byte offset **in the merged source buffer**. That is a property of
    /// how the loader concatenated the files, not of the program, so the two loaders
    /// disagree on it by a constant. See the divergence notes on the golden below.
    fn normalize_task_names(line: &str) -> String {
        let mut out = line.to_string();
        for prefix in ["jestyr_task_", "_jsp_"] {
            let mut acc = String::new();
            let mut rest = out.as_str();
            while let Some(at) = rest.find(prefix) {
                acc.push_str(&rest[..at + prefix.len()]);
                rest = &rest[at + prefix.len()..];
                let digits = rest.len() - rest.trim_start_matches(|c: char| c.is_ascii_digit()).len();
                acc.push('N');
                rest = &rest[digits..];
            }
            acc.push_str(rest);
            out = acc;
        }
        out
    }

    /// **The MODULE-path C golden — and the three divergences it turned up.**
    ///
    /// Every existing cgen golden compares the *single-file* path (`typeck::check`, whose
    /// `DebugInfo` is empty), so **nothing has ever checked the C the module loader
    /// produces** — the path `jestyrc build`, `jestyrc emit-c` and `jc <file> build` all
    /// actually use. The handoff (§1) names this golden as the prerequisite for porting
    /// `#line`, on the stated assumption that `#line` is the only difference.
    ///
    /// It is not. Building the golden found three, and all three have the same root
    /// cause — **the two loaders do not produce identical merged buffers**:
    ///
    /// 1. **`#line` directives.** The reference emits them (867 for one corpus file); the
    ///    port emits none. Recorded in §1.
    /// 2. **Per-type artifact order.** The `JestyrSlice_*` typedefs come out permuted,
    ///    because the loaders visit imports in different orders. Harmless to the C
    ///    compiler — they are independent typedefs.
    /// 3. **Spawn-task symbol names.** `jestyr_task_<N>` and its argument struct
    ///    `_jsp_<N>` take `N` from the task body's byte offset in the merged buffer, and
    ///    the two buffers differ by a constant 78 bytes of preamble, so every such name
    ///    differs. Internally consistent in each build, so both programs run correctly.
    ///
    /// None changes behaviour, and each means `jestyrc attest` and `jc attest` hash
    /// different bytes for the same program — the cross-tool reproducibility gap §1
    /// already describes for `#line`, now known to be wider than one item.
    ///
    /// So this test normalizes (2) and (3) and *asserts* they are still only that, while
    /// checking everything else exactly. **When the module path is unified, it tightens
    /// in three steps**: drop the `#line` filter, drop `normalize_task_names`, and set
    /// `strict_order` for every root. Until then it is a real regression gate on all the
    /// module-path C that does agree — which is 122 lines for `io` and the whole of
    /// `par_cost` besides these.
    ///
    /// Files are copied to a temp directory because `jc <file> build` writes its `.c` and
    /// `.exe` beside the source, and a test must not leave artifacts in `examples/`.
    #[test]
    fn jestyr_module_cgen_matches_reference_except_line_directives() {
        let jc = build_exe("examples/std/cgen.jtr");
        let dir = std::env::temp_dir().join("jestyr_modline");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // A root plus its import closure. `io.jtr` is deliberately first: it is the
        // smallest module-path program in the corpus (8 directives), so a divergence
        // shows up in a readable diff rather than a wall of them.
        // `strict_order`: whether the two implementations agree on the *order* of the
        // emitted C, not merely its content. `io` does. `par_cost` does not — see the
        // ordering assertion below — so it is checked as a multiset until the loaders
        // are aligned, at which point this flips to `true`.
        for (root, deps, strict_order) in [
            ("io", &["core"][..], true),
            ("par_cost", &["core", "io", "parallel"][..], false),
        ] {
            for m in deps.iter().chain(std::iter::once(&root)) {
                std::fs::copy(format!("examples/std/{m}.jtr"), dir.join(format!("{m}.jtr"))).unwrap();
            }
            let src_path = dir.join(format!("{root}.jtr"));

            // Reference: the real loader path, which is what populates `DebugInfo`.
            let prog = crate::module::load(src_path.to_str().unwrap());
            assert!(!prog.diags.iter().any(|d| d.is_error()), "{root}: load errors");
            let (info, td) = crate::typeck::check_program(&prog.ast, &prog.modules);
            assert!(!td.iter().any(|d| d.is_error()), "{root}: typeck errors");
            let (want_c, _) = crate::cgen::emit(&prog.ast, &info);

            // Port: `jc <file> build` writes `<stem>.c` beside the source, *then* runs
            // gcc. The exit code is deliberately not checked — a library module such as
            // `io.jtr` has no `main`, so the link step fails while the emitted C, which
            // is the whole subject of this golden, is already correct and on disk. The
            // artifact is the gate instead: emission failing means no file at all.
            let out = Command::new(&jc)
                .args([src_path.to_str().unwrap(), "build"])
                .output()
                .unwrap();
            let got_c = std::fs::read_to_string(dir.join(format!("{root}.c")))
                .unwrap_or_else(|e| {
                    panic!("{root}: the port emitted no C ({e}): {}", String::from_utf8_lossy(&out.stderr))
                });

            let want: Vec<String> = want_c
                .lines()
                .filter(|l| !l.starts_with("#line "))
                .map(normalize_task_names)
                .collect();
            let got: Vec<String> = got_c.lines().map(normalize_task_names).collect();
            let n_directives = want_c.lines().filter(|l| l.starts_with("#line ")).count();

            // Guard both directions, so neither side can make this pass by accident.
            assert!(n_directives > 0, "{root}: the reference emitted no `#line` — is this the module path?");
            assert!(
                !got_c.contains("#line "),
                "{root}: the port now emits `#line` — delete the filter above and compare outright"
            );

            // CONTENT must match exactly, always: same lines, same multiplicities. This
            // is the assertion that catches a real codegen divergence in the module
            // path, which nothing checked before this golden existed.
            let (mut sw, mut sg) = (want.clone(), got.clone());
            sw.sort_unstable();
            sg.sort_unstable();
            if sw != sg {
                let only_want: Vec<&String> = sw.iter().filter(|l| !sg.contains(l)).take(4).collect();
                let only_got: Vec<&String> = sg.iter().filter(|l| !sw.contains(l)).take(4).collect();
                panic!(
                    "{root}: the module-path C differs in CONTENT, not just order\n\
                     only in the reference: {only_want:?}\nonly in the port: {only_got:?}"
                );
            }

            if strict_order {
                if want != got {
                    let at = want.iter().zip(got.iter()).position(|(a, b)| a != b).unwrap_or(0);
                    let lo = at.saturating_sub(2);
                    panic!(
                        "{root}: module-path C diverged at line {at}\nWANT: {:?}\nGOT : {:?}",
                        &want[lo..(lo + 8).min(want.len())],
                        &got[lo..(lo + 8).min(got.len())]
                    );
                }
                eprintln!(
                    "module-path C: {root} — {} lines identical in order; {n_directives} `#line` directives are the only gap",
                    want.len()
                );
            } else {
                // Recorded, not tolerated silently: the two module *loaders* visit
                // imports in different orders, so per-type artifacts (the
                // `JestyrSlice_*` typedefs) come out permuted. Harmless to the C
                // compiler — they are independent typedefs — but it means
                // `jestyrc attest` and `jc attest` hash different bytes for the same
                // program, exactly like the `#line` gap. Asserted to still be *only* an
                // ordering difference, so this cannot quietly widen.
                assert_ne!(
                    want, got,
                    "{root}: the order now agrees — set `strict_order` to true for it"
                );
                eprintln!(
                    "module-path C: {root} — {} lines match as a multiset; order differs (loader visit order) plus {n_directives} `#line` directives",
                    want.len()
                );
            }
        }
    }

    /// The reference's TEST-mode C for `src`: `cgen::emit_tests` — the `jestyrc test` harness
    /// (`time.h` in the prelude, a pass/fail + bench-timing `main` instead of the user wrapper).
    fn rust_cgen_test_dump(src: &str, filter: Option<&str>) -> Vec<String> {
        let (tokens, _) = crate::lexer::Lexer::new(src).tokenize();
        let (ast, _diags) = crate::parser::Parser::new(src, tokens).parse();
        let (info, _d) = crate::typeck::check(&ast);
        let (c, _cd) = crate::cgen::emit_tests_filtered(&ast, &info, filter);
        c.lines().map(|s| s.to_string()).collect()
    }

    /// Run the Jestyr C backend with extra CLI args (`test` / `list` modes) and return stdout lines.
    fn jestyr_cgen_dump_args(exe: &std::path::Path, file: &str, extra: &[&str]) -> Vec<String> {
        let out = Command::new(exe).arg(file).args(extra).output().unwrap();
        assert!(out.status.success(), "jestyr cgen failed on {file} {extra:?}");
        String::from_utf8(out.stdout).unwrap().lines().map(|s| s.to_string()).collect()
    }

    /// Files the Jestyr C backend (`examples/std/cgen.jtr`) already lowers **byte-identically** to
    /// the reference. P5 is grown construct-by-construct, so this starts as a one-file allowlist
    /// and expands; once it covers the corpus it inverts to a (shrinking) denylist, mirroring how
    /// the P2/P3/P4 goldens converged to an empty denylist.
    const CGEN_GOLDEN_ALLOWLIST: &[&str] = &["hello.jtr", "bench_fib.jtr", "eq_fold.jtr", "distinct.jtr", "compute.jtr", "copy_optin.jtr", "io.jtr", "str_ops.jtr", "substr.jtr", "union.jtr", "tests_demo.jtr", "loops.jtr", "slices.jtr", "array_lit.jtr", "errors.jtr", "discriminants.jtr", "shapes.jtr", "recursion.jtr", "rest_pat.jtr", "refine.jtr", "spread.jtr", "layout.jtr", "defaults.jtr", "mmio.jtr", "try_utf8.jtr", "container.jtr", "extern_c.jtr", "bitfields.jtr", "reflect.jtr", "contracts.jtr", "records.jtr", "docs.jtr", "guards.jtr", "builder.jtr", "cow.jtr", "os_str.jtr", "owned_string.jtr", "strings.jtr", "utf8_validate.jtr", "slice_utf8.jtr", "fstring.jtr", "vec.jtr", "orpat.jtr", "ranges.jtr", "drop.jtr", "drop_nested.jtr", "genref.jtr", "loops_else.jtr", "region.jtr", "region_string.jtr", "loops_advanced.jtr", "codepoints.jtr", "bracket_generic.jtr", "generic.jtr", "unsafe_init.jtr", "env.jtr", "bound_method.jtr", "traits_static.jtr", "operators.jtr", "fs.jtr", "str_iter.jtr", "arrays.jtr", "vec_alloc.jtr", "alloc_vtable.jtr", "mem.jtr", "fn_ptr.jtr", "closure_run.jtr", "gen_vtable.jtr", "dynamic_spawn.jtr", "concurrent.jtr", "parallel.jtr", "atomics.jtr", "args.jtr", "await.jtr", "dyn_dispatch.jtr", "attributes.jtr", "niche.jtr", "option.jtr", "nested_match.jtr", "struct_variant.jtr", "vec_generic.jtr", "genlist.jtr", "sync.jtr", "genmethods.jtr", "methods.jtr", "core.jtr", "list.jtr", "mvs.jtr", "collection.jtr", "alloc_demo.jtr", "region_escape.jtr", "typeerr.jtr", "match_check.jtr", "exhaustive_check.jtr", "numbers.jtr", "numerics_canary.jtr", "closures.jtr", "escapes.jtr", "binned.jtr", "cgen.jtr", "channel.jtr", "combinators.jtr", "demo.jtr", "deterministic.jtr", "drop_named_type_param.jtr", "escape.jtr", "files.jtr", "float_bits.jtr", "format_float.jtr", "intern.jtr", "intern_demo.jtr", "lexer.jtr", "mutex.jtr", "par_cost.jtr", "par_for.jtr", "par_reduce.jtr", "par_reduce_int.jtr", "par_soac.jtr", "parse_float.jtr", "parser.jtr", "parser_cli.jtr", "reductions.jtr", "select.jtr", "slice_algos.jtr", "strmap.jtr", "strmap_demo.jtr", "tokens.jtr", "try_read.jtr", "typeck.jtr", "typeck_cli.jtr", "proc_demo.jtr", "escape_cli.jtr", "sha256.jtr", "doc_cli.jtr", "comptime_block.jtr", "comptime_reflect.jtr", "def_order.jtr", "nested_place.jtr", "layout_auto.jtr"];

    /// **P5 cgen golden.** For each allowlisted corpus `.jtr`, the Jestyr C backend must emit C
    /// *byte-identical* to `cgen::emit` (line-for-line; see [`rust_cgen_dump`] for the `#line`-free
    /// target). This is the acceptance bar the R2 fixpoint ultimately rests on. `DUMP_DIVERGE=1`
    /// prints the first differing line for the deep-dive fix loop.
    #[test]
    fn jestyr_cgen_matches_reference() {
        let exe = build_exe("examples/std/cgen.jtr");
        let mut files: Vec<std::path::PathBuf> = Vec::new();
        for dir in ["examples", "examples/std"] {
            if let Ok(rd) = std::fs::read_dir(dir) {
                for e in rd.flatten() {
                    let p = e.path();
                    if p.extension().and_then(|s| s.to_str()) == Some("jtr") {
                        files.push(p);
                    }
                }
            }
        }
        files.sort();
        let mut checked = 0;
        let mut diverged: Vec<String> = Vec::new();
        for p in &files {
            let f = p.to_str().unwrap();
            let base = p.file_name().and_then(|s| s.to_str()).unwrap();
            if !CGEN_GOLDEN_ALLOWLIST.contains(&base) {
                continue;
            }
            let src = std::fs::read_to_string(p).unwrap();
            let got = jestyr_cgen_dump(&exe, f);
            let want = rust_cgen_dump(&src);
            if got != want {
                diverged.push(f.to_string());
                if std::env::var("DUMP_DIVERGE").is_ok() {
                    let first = got.iter().zip(want.iter()).position(|(a, b)| a != b).unwrap_or(0);
                    let lo = first.saturating_sub(2);
                    eprintln!("=== {f} (first diff at line {first}) ===");
                    eprintln!("GOT : {:?}", &got[lo..(lo + 12).min(got.len())]);
                    eprintln!("WANT: {:?}", &want[lo..(lo + 12).min(want.len())]);
                }
            } else {
                checked += 1;
            }
        }
        assert!(diverged.is_empty(), "Jestyr cgen diverged from the reference on: {diverged:?}");
        eprintln!("cgen golden: {checked} file(s)' emitted C byte-identical");
    }

    /// **Test-mode golden (`jestyrc test` parity).** For every allowlisted corpus file, the
    /// Jestyr backend's TEST-mode C (`jc1 <file> test`) must be byte-identical to
    /// `cgen::emit_tests` — a file with no `@test`s still emits a `running 0 test(s)` harness,
    /// so this pins the mode corpus-wide, not just on test-bearing files. On `tests_demo.jtr`
    /// it additionally pins the FILTERED harness (`test add` vs `emit_tests_filtered`), the
    /// `--list` output, and the harness's actual runtime behavior through gcc.
    #[test]
    fn jestyr_cgen_test_mode_matches_reference() {
        let exe = build_exe("examples/std/cgen.jtr");
        let mut files: Vec<std::path::PathBuf> = Vec::new();
        for dir in ["examples", "examples/std"] {
            if let Ok(rd) = std::fs::read_dir(dir) {
                for e in rd.flatten() {
                    let p = e.path();
                    if p.extension().and_then(|s| s.to_str()) == Some("jtr") {
                        files.push(p);
                    }
                }
            }
        }
        files.sort();
        let mut checked = 0;
        let mut diverged: Vec<String> = Vec::new();
        for p in &files {
            let f = p.to_str().unwrap();
            let base = p.file_name().and_then(|s| s.to_str()).unwrap();
            if !CGEN_GOLDEN_ALLOWLIST.contains(&base) {
                continue;
            }
            let src = std::fs::read_to_string(p).unwrap();
            let got = jestyr_cgen_dump_args(&exe, f, &["test"]);
            let want = rust_cgen_test_dump(&src, None);
            if got != want {
                diverged.push(f.to_string());
                if std::env::var("DUMP_DIVERGE").is_ok() {
                    let first = got.iter().zip(want.iter()).position(|(a, b)| a != b).unwrap_or(0);
                    let lo = first.saturating_sub(2);
                    eprintln!("=== {f} [test mode] (first diff at line {first}) ===");
                    eprintln!("GOT : {:?}", &got[lo..(lo + 12).min(got.len())]);
                    eprintln!("WANT: {:?}", &want[lo..(lo + 12).min(want.len())]);
                }
            } else {
                checked += 1;
            }
        }
        assert!(diverged.is_empty(), "Jestyr TEST-mode cgen diverged from the reference on: {diverged:?}");

        // tests_demo.jtr: the filtered harness (codegen-side filtering — the baked
        // `running N test(s)` count equals the runner count), and `--list` parity.
        let demo = "examples/tests_demo.jtr";
        let demo_src = std::fs::read_to_string(demo).unwrap();
        assert_eq!(
            jestyr_cgen_dump_args(&exe, demo, &["test", "add"]),
            rust_cgen_test_dump(&demo_src, Some("add")),
            "filtered test harness diverged"
        );
        {
            let (tokens, _) = crate::lexer::Lexer::new(&demo_src).tokenize();
            let (ast, _) = crate::parser::Parser::new(&demo_src, tokens).parse();
            let want_list: Vec<String> = crate::cgen::list_tests(&ast)
                .into_iter()
                .map(|(name, kind)| {
                    let tag = match kind {
                        crate::cgen::TestKind::Test => "test",
                        crate::cgen::TestKind::Bench => "bench",
                    };
                    format!("{tag} {name}")
                })
                .collect();
            assert_eq!(jestyr_cgen_dump_args(&exe, demo, &["list"]), want_list, "--list diverged");
        }

        // The harness must actually RUN: gcc-build jc1's test-mode C for tests_demo and check
        // the pass/fail protocol end-to-end (2 tests pass, bench line present, exit 0).
        let c_src = jestyr_cgen_dump_args(&exe, demo, &["test"]).join("\n") + "\n";
        let cc = crate::find_c_compiler().expect("c-oracle needs a C compiler on PATH");
        let dir = std::env::temp_dir();
        let cfile = dir.join("jestyr_testmode_demo.c");
        let texe = dir.join(format!("jestyr_testmode_demo{}", std::env::consts::EXE_SUFFIX));
        std::fs::write(&cfile, &c_src).unwrap();
        let mut cmd = Command::new(&cc);
        cmd.args(crate::CC_FLAGS);
        assert!(cmd.arg("-o").arg(&texe).arg(&cfile).status().unwrap().success(), "gcc failed on the test harness");
        let out = Command::new(&texe).output().unwrap();
        assert!(out.status.success(), "test harness exited non-zero");
        let stdout = String::from_utf8(out.stdout).unwrap();
        assert!(stdout.contains("running 2 test(s)"), "harness header wrong: {stdout}");
        assert!(stdout.contains("test add_is_commutative ... ok"), "test line wrong: {stdout}");
        assert!(stdout.contains("bench sum_to_1000 ... "), "bench line missing: {stdout}");
        assert!(stdout.contains("result: 2 passed; 0 failed"), "tally wrong: {stdout}");
        eprintln!("test-mode golden: {checked} file(s)' harness C byte-identical; demo harness ran green");
    }

    /// The self-hosting module closure, in the loader's DFS item order for
    /// `examples/std/cgen.jtr` (each module's imports precede it, diamonds memoized —
    /// exactly the order `module::load` merges their items in).
    const SELFHOST_MODULES: &[&str] =
        &["mem", "intern", "fs", "env", "list", "tokens", "parser", "ctfe", "typeck", "escape", "sha256", "cgen"];

    /// Flatten the multi-module Jestyr compiler into ONE single-file program — the
    /// R2-full "concatenated-source build". The loader already compiles a program as a
    /// single translation unit (one shared arena, one concatenated source buffer;
    /// escape + cgen never learn there was more than one file), so a faithful flatten
    /// is the module semantics minus typeck's visibility checks — vacuous for a
    /// program that already typechecks. The transform, token-level (comments and
    /// strings are untouched because the lexer skips/atomizes them):
    ///  1. drop every `import` declaration;
    ///  2. erase module qualifiers: `binding.x` → `x` (an `Ident` in this module's
    ///     import-binding set, followed by `.`, not itself preceded by `.`);
    ///  3. rename top-level names defined in more than one module to `name__<module>`
    ///     — at the definition, at bare uses in the defining module, and at qualified
    ///     uses everywhere (via step 2's rewrite).
    fn flatten_selfhost_concat() -> String {
        use crate::ast::Item;
        use crate::token::TokenKind;
        use std::collections::HashMap;
        // Pass 1: each module's source + its top-level definition names (real parser —
        // struct methods and impl fns are not top-level and can't collide).
        let mut srcs: Vec<String> = Vec::new();
        let mut defs: Vec<Vec<String>> = Vec::new();
        for m in SELFHOST_MODULES {
            let src = std::fs::read_to_string(format!("examples/std/{m}.jtr")).unwrap();
            let (tokens, ld) = crate::lexer::Lexer::new(&src).tokenize();
            assert!(ld.is_empty(), "lex errors in {m}.jtr");
            let (ast, _pd) = crate::parser::Parser::new(&src, tokens).parse();
            let names: Vec<String> = ast
                .items
                .iter()
                .filter_map(|it| match it {
                    Item::Fn(f) => Some(f.name.name.clone()),
                    Item::Enum(e) => Some(e.name.name.clone()),
                    Item::Const(c) => Some(c.name.name.clone()),
                    Item::Distinct(d) => Some(d.name.name.clone()),
                    Item::Trait(t) => Some(t.name.name.clone()),
                    Item::Extern(e) => Some(e.name.name.clone()),
                    Item::Struct { name, .. } => Some(name.name.clone()),
                    Item::Impl(_) | Item::Import(_) => None,
                })
                .collect();
            srcs.push(src);
            defs.push(names);
        }
        // Cross-module collisions → per-module rename map (module name, item name) → new name.
        let mut seen_in: HashMap<&str, usize> = HashMap::new();
        for names in &defs {
            for n in names {
                *seen_in.entry(n.as_str()).or_insert(0) += 1;
            }
        }
        let mut renames: HashMap<(String, String), String> = HashMap::new();
        for (mi, m) in SELFHOST_MODULES.iter().enumerate() {
            for n in &defs[mi] {
                if seen_in[n.as_str()] > 1 {
                    renames.insert(((*m).to_string(), n.clone()), format!("{n}__{m}"));
                }
            }
        }
        // Pass 2: rewrite each module and concatenate (loader order = merged item order).
        let mut out = String::new();
        for (mi, m) in SELFHOST_MODULES.iter().enumerate() {
            let src = &srcs[mi];
            let (tokens, _) = crate::lexer::Lexer::new(src).tokenize();
            let toks: Vec<_> = tokens.iter().filter(|t| t.kind != TokenKind::Eof).collect();
            // (start, end, replacement) edits, collected in source order.
            let mut edits: Vec<(usize, usize, String)> = Vec::new();
            let mut bindings: HashMap<String, String> = HashMap::new();
            let mut i = 0;
            while i < toks.len() {
                let t = toks[i];
                if t.kind == TokenKind::Import {
                    // `import "path"` [`as` alias] [`= "hash"`] — record the binding, drop the decl.
                    assert_eq!(toks[i + 1].kind, TokenKind::Str, "import path in {m}.jtr");
                    let seg = src[toks[i + 1].span.range()]
                        .trim_matches('"')
                        .rsplit(['/', '\\'])
                        .next()
                        .unwrap()
                        .to_string();
                    let mut j = i + 2;
                    let mut binding = seg.clone();
                    if j < toks.len() && toks[j].kind == TokenKind::As {
                        binding = src[toks[j + 1].span.range()].to_string();
                        j += 2;
                    }
                    if j < toks.len() && toks[j].kind == TokenKind::Eq {
                        j += 2; // pinned hash: `= "<sha256>"`
                    }
                    assert!(
                        SELFHOST_MODULES.contains(&seg.as_str()),
                        "{m}.jtr imports `{seg}` which is outside the self-host closure"
                    );
                    let mut end = toks[j - 1].span.end as usize;
                    if src.as_bytes().get(end) == Some(&b'\n') {
                        end += 1; // take the decl's own line break with it
                    }
                    edits.push((t.span.start as usize, end, String::new()));
                    bindings.insert(binding, seg);
                    i = j;
                    continue;
                }
                if t.kind == TokenKind::Ident {
                    let text = &src[t.span.range()];
                    let prev_dot = i > 0 && toks[i - 1].kind == TokenKind::Dot;
                    let next_dot = i + 1 < toks.len() && toks[i + 1].kind == TokenKind::Dot;
                    if !prev_dot && next_dot && bindings.contains_key(text) {
                        // `binding.x` — erase the qualifier; rename x if its definition collided.
                        // Unambiguous in this closure: no local named like a binding is ever
                        // field-accessed (the only shared name, `fs`, binds scalar ints).
                        let x = toks[i + 2];
                        assert_eq!(x.kind, TokenKind::Ident, "qualified member in {m}.jtr");
                        let target = bindings[text].clone();
                        let xt = src[x.span.range()].to_string();
                        edits.push((t.span.start as usize, x.span.start as usize, String::new()));
                        if let Some(nn) = renames.get(&(target, xt)) {
                            edits.push((x.span.start as usize, x.span.end as usize, nn.clone()));
                        }
                        i += 3;
                        continue;
                    }
                    if !prev_dot {
                        if let Some(nn) = renames.get(&((*m).to_string(), text.to_string())) {
                            edits.push((t.span.start as usize, t.span.end as usize, nn.clone()));
                        }
                    }
                }
                i += 1;
            }
            let mut rewritten = String::with_capacity(src.len());
            let mut cursor = 0usize;
            for (s, e, rep) in edits {
                rewritten.push_str(&src[cursor..s]);
                rewritten.push_str(&rep);
                cursor = e;
            }
            rewritten.push_str(&src[cursor..]);
            out.push_str(&rewritten);
            out.push('\n'); // keep regions disjoint, like the loader
        }
        out
    }

    /// **R2-full golden.** The flattened single-file compiler (see
    /// [`flatten_selfhost_concat`]) must (a) be a diagnostic-free program under the
    /// Rust reference — validating the flatten transform itself — and (b) lower
    /// through the Jestyr-written back end to C **byte-identical** to the
    /// reference's. This is the concat program the jc2≡jc3 fixed point runs on.
    #[test]
    fn jestyr_cgen_concat_matches_reference() {
        let concat = flatten_selfhost_concat();
        let path = std::env::temp_dir().join("jestyr_selfhost_concat.jtr");
        std::fs::write(&path, &concat).unwrap();
        // (a) the concat is a valid program by the reference's own front end.
        let (tokens, ld) = crate::lexer::Lexer::new(&concat).tokenize();
        assert!(ld.is_empty(), "lex errors in the flattened compiler");
        let (ast, pd) = crate::parser::Parser::new(&concat, tokens).parse();
        assert!(
            !pd.iter().any(|d| d.is_error()),
            "parse errors in the flattened compiler: {:?}",
            pd.iter().filter(|d| d.is_error()).take(3).collect::<Vec<_>>()
        );
        let (info, td) = crate::typeck::check(&ast);
        assert!(
            !td.iter().any(|d| d.is_error()),
            "typeck errors in the flattened compiler: {:?}",
            td.iter().filter(|d| d.is_error()).take(3).collect::<Vec<_>>()
        );
        assert!(
            !crate::escape::check(&ast, &info).iter().any(|d| d.is_error()),
            "escape errors in the flattened compiler"
        );
        // (b) byte-identical lowering through the Jestyr back end.
        let exe = build_exe("examples/std/cgen.jtr");
        let got = jestyr_cgen_dump(&exe, path.to_str().unwrap());
        let want = rust_cgen_dump(&concat);
        if got != want && std::env::var("DUMP_DIVERGE").is_ok() {
            let first = got.iter().zip(want.iter()).position(|(a, b)| a != b).unwrap_or(0);
            let lo = first.saturating_sub(2);
            eprintln!("=== concat (first diff at line {first}) ===");
            eprintln!("GOT : {:?}", &got[lo..(lo + 12).min(got.len())]);
            eprintln!("WANT: {:?}", &want[lo..(lo + 12).min(want.len())]);
        }
        assert!(got == want, "Jestyr cgen diverged from the reference on the flattened compiler");
        eprintln!("concat golden: {} lines of C byte-identical", got.len());
    }

    /// **R2 fixpoint — FULL.** The self-hosting proof. `jc1` = the Rust compiler
    /// builds the Jestyr-written compiler (multi-module). `C1` = jc1 lowering its own
    /// flattened source (`flatten_selfhost_concat` — semantically the same program).
    /// `jc2` = gcc builds C1. `C2` = jc2 lowering the same source. **`C1 ≡ C2`
    /// byte-for-byte is the fixed point**: the compiler, compiled by itself,
    /// reproduces its own compilation exactly — so jc3 ≡ jc2 by induction.
    #[cfg(feature = "selfhost-fixpoint")]
    #[test]
    fn selfhost_fixpoint_full() {
        let concat = flatten_selfhost_concat();
        let dir = std::env::temp_dir();
        let path = dir.join("jestyr_selfhost_concat.jtr");
        std::fs::write(&path, &concat).unwrap();
        let jc1 = build_exe("examples/std/cgen.jtr");
        // C1 = jc1(concat).
        let out = Command::new(&jc1).arg(&path).output().unwrap();
        assert!(out.status.success(), "jc1 failed on the flattened compiler");
        let c1 = String::from_utf8(out.stdout).unwrap().replace("\r\n", "\n");
        // jc2 = gcc(C1), with the same recursion headroom jc1 got.
        let cc = crate::find_c_compiler().expect("the fixpoint needs a C compiler on PATH");
        let cfile = dir.join("jestyr_selfhost_jc2.c");
        let jc2 = dir.join(format!("jestyr_selfhost_jc2{}", std::env::consts::EXE_SUFFIX));
        std::fs::write(&cfile, &c1).unwrap();
        let mut cmd = Command::new(&cc);
        cmd.args(crate::CC_FLAGS);
        #[cfg(windows)]
        cmd.arg("-Wl,--stack,67108864");
        if c1.contains("pthread") {
            cmd.arg("-pthread");
        }
        assert!(
            cmd.arg("-o").arg(&jc2).arg(&cfile).status().unwrap().success(),
            "gcc failed on jc1's C for the flattened compiler"
        );
        // C2 = jc2(concat). The fixed point: C2 ≡ C1.
        let out2 = Command::new(&jc2).arg(&path).output().unwrap();
        assert!(out2.status.success(), "jc2 failed on the flattened compiler");
        let c2 = String::from_utf8(out2.stdout).unwrap().replace("\r\n", "\n");
        assert!(c1 == c2, "FIXED POINT BROKEN: jc2's C for the compiler differs from jc1's");
        // jc2 must also BE jc1 on unrelated input (same compiler, not just a quine) —
        // in normal AND test mode.
        let probe = "examples/hello.jtr";
        let a = Command::new(&jc1).arg(probe).output().unwrap();
        let b = Command::new(&jc2).arg(probe).output().unwrap();
        assert_eq!(a.stdout, b.stdout, "jc1 and jc2 disagree on {probe}");
        let tprobe = "examples/tests_demo.jtr";
        let ta = Command::new(&jc1).args([tprobe, "test"]).output().unwrap();
        let tb = Command::new(&jc2).args([tprobe, "test"]).output().unwrap();
        assert_eq!(ta.stdout, tb.stdout, "jc1 and jc2 disagree on {tprobe} in test mode");
        eprintln!(
            "SELF-HOSTING FIXED POINT: jc2 ≡ jc1 on the compiler's own {} lines of C",
            c1.lines().count()
        );
    }

    /// **Bootstrap seed.** `bootstrap/jestyr_seed.c` is the committed self-emitted C of the
    /// flattened compiler (`bootstrap/jestyr_flat.jtr`): building Jestyr from scratch then
    /// needs only a C compiler, never Rust — gcc builds the seed into `jc`, and `jc
    /// bootstrap/jestyr_flat.jtr` must reproduce the seed byte-for-byte (the committed
    /// fixed point; see bootstrap/README.md). This test is the DRIFT GUARD: it regenerates
    /// both artifacts from the live sources and asserts the committed copies are current.
    /// Run with `REFRESH_SEED=1` to rewrite them after a compiler change.
    #[cfg(feature = "selfhost-fixpoint")]
    #[test]
    fn bootstrap_seed_is_current() {
        // LF-normalized up front: the flatten carries the checked-out sources' line endings
        // (CRLF under autocrlf), but the committed pair must be self-consistent — the seed
        // is generated FROM the normalized flat, exactly what a from-scratch bootstrapper
        // feeds back through `jc`.
        let concat = flatten_selfhost_concat().replace("\r\n", "\n");
        let jc1 = build_exe("examples/std/cgen.jtr");
        let path = std::env::temp_dir().join("jestyr_seed_src.jtr");
        std::fs::write(&path, &concat).unwrap();
        let out = Command::new(&jc1).arg(&path).output().unwrap();
        assert!(out.status.success(), "jc1 failed on the flattened compiler");
        let c1 = String::from_utf8(out.stdout).unwrap().replace("\r\n", "\n");
        if std::env::var("REFRESH_SEED").is_ok() {
            std::fs::create_dir_all("bootstrap").unwrap();
            std::fs::write("bootstrap/jestyr_flat.jtr", &concat).unwrap();
            std::fs::write("bootstrap/jestyr_seed.c", &c1).unwrap();
            eprintln!("bootstrap seed REFRESHED ({} lines of C)", c1.lines().count());
        }
        let flat = std::fs::read_to_string("bootstrap/jestyr_flat.jtr")
            .expect("bootstrap/jestyr_flat.jtr missing — run with REFRESH_SEED=1")
            .replace("\r\n", "\n");
        let seed = std::fs::read_to_string("bootstrap/jestyr_seed.c")
            .expect("bootstrap/jestyr_seed.c missing — run with REFRESH_SEED=1")
            .replace("\r\n", "\n");
        assert!(flat == concat, "bootstrap/jestyr_flat.jtr is STALE — rerun with REFRESH_SEED=1");
        assert!(seed == c1, "bootstrap/jestyr_seed.c is STALE — rerun with REFRESH_SEED=1");
        eprintln!("bootstrap seed is current ({} lines of C)", c1.lines().count());
    }

    /// **R2 fixpoint — the subset milestone.** `jc1` = the Rust compiler builds the
    /// Jestyr-written back end (`cgen.jtr`, which imports the Jestyr parser + typeck) into a
    /// native exe. For every allowlisted subset program P, `jc1` compiles P → C; that C must
    /// gcc-build and **run to exactly the stdout/exit of the Rust-compiled P**. The cgen golden
    /// already pins jc1's C byte-identical to the reference's; this closes the loop through gcc
    /// and execution. The full `jc2 ≡ jc3` fixed point (jc1 compiling the compiler *sources*,
    /// then the result recompiling them to identical C) lands when cgen.jtr's construct
    /// coverage reaches the compiler itself; this harness is the scaffold it grows on.
    #[cfg(feature = "selfhost-fixpoint")]
    #[test]
    fn selfhost_fixpoint_subset() {
        let jc1 = build_exe("examples/std/cgen.jtr");
        let cc = crate::find_c_compiler().expect("the fixpoint needs a C compiler on PATH");
        let mut checked = 0;
        for base in CGEN_GOLDEN_ALLOWLIST {
            // A subset file lives in examples/ or examples/std/.
            let path = ["examples", "examples/std"]
                .iter()
                .map(|d| format!("{d}/{base}"))
                .find(|p| std::path::Path::new(p).exists())
                .unwrap_or_else(|| panic!("allowlisted {base} not found"));
            // An import-bearing file golden-compares in its DEGENERATE single-file form
            // (module-qualified calls emit as plain field-calls on both sides), but that C
            // references unresolved names and cannot build — the module path owns its runtime.
            let src = std::fs::read_to_string(&path).unwrap();
            if src.lines().any(|l| l.trim_start().starts_with("import \"")) {
                continue;
            }
            // An ERROR file (parse/type/escape diagnostics) golden-compares in its degenerate
            // form but is not a runnable program — the reference CLI refuses it.
            {
                let (tokens, _) = crate::lexer::Lexer::new(&src).tokenize();
                let (ast, pd) = crate::parser::Parser::new(&src, tokens).parse();
                let (info, td) = crate::typeck::check(&ast);
                if pd.iter().any(|d| d.is_error())
                    || td.iter().any(|d| d.is_error())
                    || crate::escape::check(&ast, &info).iter().any(|d| d.is_error())
                {
                    continue;
                }
            }
            // jc1 (the Jestyr-written compiler) lowers P to C.
            let out = Command::new(&jc1).arg(&path).output().unwrap();
            assert!(out.status.success(), "jc1 failed on {path}");
            let c_src = String::from_utf8(out.stdout).unwrap().replace("\r\n", "\n");
            // A library module (no `main`) golden-compares but can't link into an exe.
            if !c_src.contains("int main(") {
                continue;
            }
            // gcc builds jc1's C.
            let dir = std::env::temp_dir();
            let stem: String = base.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
            let cfile = dir.join(format!("jestyr_fix_{stem}.c"));
            let exe = dir.join(format!("jestyr_fix_{stem}{}", std::env::consts::EXE_SUFFIX));
            std::fs::write(&cfile, &c_src).unwrap();
            let mut cmd = Command::new(&cc);
            cmd.args(crate::CC_FLAGS);
            assert!(
                cmd.arg("-o").arg(&exe).arg(&cfile).status().unwrap().success(),
                "gcc failed on jc1's C for {path}"
            );
            let got = Command::new(&exe).output().unwrap();
            // The Rust reference compiles + runs the same P.
            let want_exe = build_exe(&path);
            let want = Command::new(&want_exe).output().unwrap();
            assert_eq!(got.stdout, want.stdout, "runtime output diverged for {path}");
            assert_eq!(got.status.code(), want.status.code(), "exit code diverged for {path}");
            checked += 1;
        }
        assert!(checked >= 5, "expected several subset programs, ran {checked}");
        eprintln!("selfhost fixpoint subset: jc1's C built + ran identical for {checked} program(s)");
    }

    /// **P2 depth guard.** The Jestyr parser bounds AST *height* at `MAX_EXPR_DEPTH`
    /// (like the reference), so adversarially-deep input terminates with a bounded tree
    /// instead of overflowing. Two shapes stress different stacks: a left-deep fold
    /// (`1+1+…`) parses iteratively but would overflow the *recursive dump* without the
    /// height cap, and deep parens (`(((…)))`) would overflow the *parser's* recursion.
    /// Building/running to completion (bounded output, no crash) is the check.
    #[test]
    fn jestyr_parser_bounds_deep_nesting() {
        let parser = build_exe("examples/std/parser_cli.jtr");
        let deep_fold = "1+".repeat(20_000) + "1";
        let deep_parens = format!("{}x{}", "(".repeat(20_000), ")".repeat(20_000));
        for src in [deep_fold, deep_parens] {
            let probe = std::env::temp_dir().join("jestyr_deep_probe.jtr");
            std::fs::write(&probe, &src).unwrap();
            let out = Command::new(&parser).arg(probe.to_str().unwrap()).output().unwrap();
            assert!(out.status.success(), "parser did not terminate cleanly on {}-byte input", src.len());
            // The tree is capped near MAX_EXPR_DEPTH, so the dump is bounded regardless of
            // input size — an uncapped parser/dump would overflow (crash) on 20k nesting.
            let lines = String::from_utf8(out.stdout).unwrap().lines().count();
            assert!(lines > 0 && lines < 8_000, "dump not height-bounded: {lines} lines");
        }
    }

    /// Build `rel`'s `@test`/`@bench` **harness** (narrowed by `filter`) through the
    /// real gcc pipeline — exactly what `jestyrc test [substr]` does — run it, and
    /// return `(stdout, exit_code)`. The exit code is the runner's pass/fail tally
    /// (`0` iff every baked test passed), so this asserts the end-to-end contract
    /// CI relies on: a filtered run compiles, runs, and reports correctly.
    fn build_tests_and_run(rel: &str, filter: Option<&str>) -> (String, i32) {
        let prog = crate::module::load(rel);
        assert!(!prog.diags.iter().any(|d| d.is_error()), "load errors in {rel}: {:?}", prog.diags);
        let (info, td) = crate::typeck::check_program(&prog.ast, &prog.modules);
        assert!(!td.iter().any(|d| d.is_error()), "typeck errors in {rel}");
        let ed = crate::escape::check(&prog.ast, &info);
        assert!(!ed.iter().any(|d| d.is_error()), "escape errors in {rel}");
        let (c_src, _cd) = crate::cgen::emit_tests_filtered(&prog.ast, &info, filter);

        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let uniq = SEQ.fetch_add(1, Ordering::Relaxed);
        let stem: String = rel.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
        let dir = std::env::temp_dir();
        let cfile = dir.join(format!("jestyr_testrun_{stem}_{uniq}.c"));
        let exe = dir.join(format!("jestyr_testrun_{stem}_{uniq}{}", std::env::consts::EXE_SUFFIX));
        std::fs::write(&cfile, &c_src).unwrap();

        let cc = crate::find_c_compiler().expect("c-oracle needs a C compiler on PATH");
        let mut cmd = Command::new(&cc);
        cmd.args(crate::CC_FLAGS);
        if c_src.contains("pthread") {
            cmd.arg("-pthread");
        }
        assert!(cmd.arg("-o").arg(&exe).arg(&cfile).status().unwrap().success(), "gcc failed for {rel}");
        let out = Command::new(&exe).output().unwrap();
        (String::from_utf8(out.stdout).unwrap(), out.status.code().unwrap_or(-1))
    }

    /// End-to-end runner check on the shipped demo: an unfiltered run runs both
    /// tests and the bench and exits 0; a `doub` filter runs only `doubling_works`
    /// (and drops the bench) yet still exits 0. The exact stdout is pinned so a
    /// harness-shape regression is caught, and the exit code proves the pass/fail
    /// tally reaches the process status.
    #[test]
    fn test_runner_filters_end_to_end() {
        let (all, all_code) = build_tests_and_run("examples/tests_demo.jtr", None);
        assert_eq!(all_code, 0, "all demo tests pass: {all}");
        assert_eq!(
            all.split_whitespace().collect::<Vec<_>>(),
            [
                "running", "2", "test(s)",
                "test", "add_is_commutative", "...", "ok",
                "test", "doubling_works", "...", "ok",
                "bench", "sum_to_1000", "...", "0.000", "ms",
                "result:", "2", "passed;", "0", "failed",
            ]
        );

        let (only, only_code) = build_tests_and_run("examples/tests_demo.jtr", Some("doub"));
        assert_eq!(only_code, 0, "the one selected test passes: {only}");
        assert_eq!(
            only.split_whitespace().collect::<Vec<_>>(),
            [
                "running", "1", "test(s)",
                "test", "doubling_works", "...", "ok",
                "result:", "1", "passed;", "0", "failed",
            ],
            "filter must select only doubling_works and drop the bench"
        );
    }

    #[test]
    fn binned_demo() {
        assert_eq!(toks("examples/std/binned.jtr"), ["4", "4", "1", "1", "1", "1", "1"]);
    }
    #[test]
    fn reductions_demo() {
        assert_eq!(toks("examples/std/reductions.jtr"), ["10", "10", "10", "0", "2", "0"]);
    }
    /// B4 end-to-end: `let y = unsafe { d.* }` reads the pointer and prints 42.
    #[test]
    fn unsafe_init_demo() {
        assert_eq!(toks("examples/std/unsafe_init.jtr"), ["42"]);
    }
    /// B5 end-to-end: inline `from_utf8(slice(u8, buf, 2))` yields "Hi" (len 2).
    #[test]
    fn slice_utf8_demo() {
        assert_eq!(toks("examples/std/slice_utf8.jtr"), ["Hi", "2"]);
    }
    /// B3 end-to-end: `fs.try_read_text` recovers an existing file (ok, len 13)
    /// and takes the err branch on a missing one — no abort either way.
    #[test]
    fn try_read_demo() {
        assert_eq!(toks("examples/std/try_read.jtr"), ["true", "13", "true"]);
    }
    #[test]
    fn numbers_demo() {
        assert_eq!(toks("examples/std/numbers.jtr"), ["12345", "-42", "7", "9", "-4271", "-4271", "3"]);
    }
    #[test]
    fn parse_float_demo() {
        let t = toks("examples/std/parse_float.jtr");
        assert!(t.len() == 16 && t.iter().all(|x| x == "1"), "parse_float: {t:?}");
    }
    #[test]
    fn format_float_demo() {
        let t = toks("examples/std/format_float.jtr");
        assert_eq!(&t[..11], &["1"; 11]);
        assert_eq!(&t[11..], ["1.5e0", "5e-1", "1e0", "3.14159265358979e0"]);
    }
    #[test]
    fn par_reduce_demo() {
        assert_eq!(toks("examples/std/par_reduce.jtr"), ["1", "1", "1"]);
    }
    #[test]
    fn par_reduce_int_demo() {
        // The generalized parallel reduction on real threads: sum/min/max/xor of
        // 1..=17 (153, 1, 17, 1), then four par==serial equality flags. Every
        // reduction is deterministic, so the result is bit-identical to serial.
        assert_eq!(
            toks("examples/std/par_reduce_int.jtr"),
            ["153", "1", "17", "1", "1", "1", "1", "1"]
        );
    }
    /// **Q-S2's headline: the vector lowering computes the same bits as the scalar one.**
    ///
    /// The demo's `par for` loops are compiled twice — once as shipped, once with `@simd`
    /// spliced onto every function — and both binaries must print identical tokens.
    /// Nothing else in the program changes, so any difference is the vector path.
    ///
    /// The element count is **9**, deliberately not a multiple of the 8-lane `i32` width
    /// (nor of 4, nor of 32): every run exercises the scalar remainder, which is where
    /// Q-S1's oracle found the select-lowering bug in its own harness.
    #[test]
    fn simd_lowering_matches_the_scalar_path_bit_for_bit() {
        let plain = std::fs::read_to_string("examples/std/par_for_width.jtr").unwrap();
        // Annotate only `main` — it holds the `par for` loops, and `@simd` on a function
        // with none is (correctly) an error, as Q-S1 made it.
        let annotated = plain.replace("\nfn main()", "\n@simd fn main()");
        let dir = std::env::temp_dir().join("jestyr_simd_lower");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // Beside the real module directory, so `import "core"` still resolves.
        let f = std::path::Path::new("examples/std").join("_simd_annotated_tmp.jtr");
        std::fs::write(&f, &annotated).unwrap();
        let got = std::panic::catch_unwind(|| toks("examples/std/_simd_annotated_tmp.jtr"));
        let _ = std::fs::remove_file(&f);
        let got = got.expect("the annotated program must build and run");
        assert_eq!(
            got,
            ["285", "1", "9", "30"],
            "the vector lowering diverged from the scalar one"
        );
    }

    /// **The shipped `@simd` demo, on real OS threads.** Both loops are vectorized (8
    /// `i32` lanes) over **11** elements, so every run goes through the scalar remainder
    /// too, and each is checked against its serial reference in-program — the two `0`s.
    /// `absum`'s `if`/`else` body is the interesting one: a mask blend in the vector half,
    /// an ordinary conditional in the remainder, one source expression. Repeated to shake
    /// out any thread race.
    #[test]
    fn par_for_simd_demo() {
        for _ in 0..8 {
            assert_eq!(
                toks("examples/std/par_for_simd.jtr"),
                ["30", "0", "110", "0"],
                "a vectorized `par for` diverged from serial"
            );
        }
    }

    /// **The shipped `@layout(auto)` demo, compiled and run.** The two `size_of`s are the
    /// C compiler's own answers, so 32 → 16 is gcc agreeing that the reordering happened
    /// and paid; the four field reads in between are the other half of the claim — the
    /// values are untouched, because construction and access are by name.
    ///
    /// The `24` is the one that would break silently: `Outer` embeds a reordered `Tidy`
    /// by value, so it is only 24 bytes if the *inner* struct really shrank. A model that
    /// failed to propagate reordering into an embedding type would print 40 here and
    /// still pass every other assertion.
    #[test]
    fn layout_auto_demo() {
        assert_eq!(
            toks("examples/layout_auto.jtr"),
            ["32", "16", "1", "2", "3", "4", "24", "16"],
            "the reordered struct changed an observable value"
        );
    }

    /// **`par for` over a narrower element type, on real OS threads.** The reduction
    /// domain is `i64` while the loop iterates `i32` (and `u8`), so this is where the
    /// widening either preserves the guarantee or does not: the parallel sum-of-squares
    /// must equal the serial fold bit-for-bit (the `1`), over 9 elements so the last
    /// worker chunk is uneven. Repeated to shake out any thread race — every token must
    /// be identical each run.
    #[test]
    fn par_for_width_demo() {
        for _ in 0..8 {
            assert_eq!(
                toks("examples/std/par_for_width.jtr"),
                ["285", "1", "9", "30"],
                "a narrow-width `par for` diverged from serial"
            );
        }
    }

    #[test]
    fn par_soac_demo() {
        // Workstream Q tier 1: par_map + par_scan on real OS threads. Two maps and two
        // scans (sum + max, general API) plus the par_scan_sum wrapper all match their
        // serial oracle bit-for-bit over 1003 mixed-sign values with an uneven last
        // chunk (five 1s), then the prefix sum of 1..=1000 ends at 500500. Repeated to
        // shake out any thread race: every token must be identical each run.
        for _ in 0..8 {
            assert_eq!(
                toks("examples/std/par_soac.jtr"),
                ["1", "1", "1", "1", "1", "500500"],
                "a parallel SOAC diverged from serial"
            );
        }
    }
    #[test]
    fn par_cost_demo() {
        // Workstream Q cost model: the `@span(log)` par-for reduction and its
        // `@span(linear)` serial reference both pass the checked cost contract, and
        // agree at runtime — sum of 1..=100 is 5050, bit-identical to serial.
        assert_eq!(toks("examples/std/par_cost.jtr"), ["5050", "1"]);
    }
    #[test]
    fn mutex_demo() {
        // Mutual exclusion on real OS threads: eight tasks each increment one
        // counter through the Mutex protected object. The lock serializes the
        // read-modify-writes, so no update is lost — the total is EXACTLY 8,
        // deterministically (schedule-independent). Run it repeatedly to shake out
        // any race: the answer must be 8 every time.
        for _ in 0..8 {
            assert_eq!(toks("examples/std/mutex.jtr"), ["8"], "mutex lost an update");
        }
    }
    #[test]
    fn select_demo() {
        // `select` over move-only channels on real threads: two spawned producers fill
        // two channels; the main thread drains all four via `select`. Order-independent
        // sum → deterministic: 11+12+21+22 = 66.
        for _ in 0..8 {
            assert_eq!(toks("examples/std/select.jtr"), ["66"], "select result wrong");
        }
    }
    #[test]
    fn deterministic_demo() {
        // A `@deterministic`-certified function (its only parallelism is a checked
        // `par for`) runs and gives the bit-stable result: Σ k² for 1..=10 = 385.
        assert_eq!(toks("examples/std/deterministic.jtr"), ["385"], "@deterministic result wrong");
    }
    #[test]
    fn dynamic_spawn_demo() {
        // Dynamic-N spawn on real threads: a runtime number of tasks (10, then 64),
        // each writing a disjoint slot, joined at the brace. Disjoint writes → the
        // summed result is deterministic. Repeated to shake out races at 64 threads.
        for _ in 0..8 {
            assert_eq!(toks("examples/std/dynamic_spawn.jtr"), ["285", "85344"], "dynamic spawn result wrong");
        }
    }
    #[test]
    fn par_for_demo() {
        // The headline surface on real threads: a parallel sum-of-squares of 1..=13
        // (819), a bit-identical-to-serial flag (1), and a parallel max (13). The
        // reduction is checked deterministic at compile time, so the result is
        // schedule-independent — repeated to confirm.
        for _ in 0..8 {
            assert_eq!(toks("examples/std/par_for.jtr"), ["819", "1", "13"], "par for result wrong");
        }
    }
    #[test]
    fn await_demo() {
        // Task results + await on real OS threads: two tasks compute disjoint partial
        // sums-of-squares in parallel and `await` combines them (385), then a single
        // awaited task (14). Deterministic (the partials are fixed), repeated to shake
        // out join races.
        for _ in 0..8 {
            assert_eq!(toks("examples/std/await.jtr"), ["385", "14"], "await result wrong");
        }
    }
    #[test]
    fn channel_demo() {
        // Move-only channels on real OS threads. Part 1: four producers send 16
        // values, the main thread drains them → 264. Part 2: a cap-2 channel with a
        // concurrent producer + consumer (real backpressure) sums 1..=8 → 36. Both
        // are order-independent sums, so deterministic; repeated to shake out races.
        for _ in 0..8 {
            assert_eq!(toks("examples/std/channel.jtr"), ["264", "36"], "channel transfer wrong");
        }
    }
    #[test]
    fn files_demo() {
        // End-to-end file I/O through the real gcc pipeline: write → exists → read
        // back ("hello, jestyr" = 13 bytes) → remove → gone → missing reads empty.
        assert_eq!(
            toks("examples/std/files.jtr"),
            ["1", "1", "13", "hello,", "jestyr", "1", "0", "0"]
        );
    }
    #[test]
    fn args_demo() {
        // The oracle runs the exe with no extra args (argc = 1, just the program
        // path): count=1, argv[0] non-empty, out-of-range empty, no user args → sum 0.
        assert_eq!(toks("examples/std/args.jtr"), ["1", "1", "0", "0"]);
    }
    #[test]
    fn strmap_demo() {
        // The open-addressing str->i64 symbol table, run through gcc: basic get
        // (1,2,3), a missing key (-1), has (1,0), overwrite (99), count (3), then a
        // 300-key stress run forcing several grow/rehash cycles — count 303, all 300
        // read back correctly (1), and a never-inserted key absent (0).
        assert_eq!(
            toks("examples/std/strmap_demo.jtr"),
            ["1", "2", "3", "-1", "1", "0", "99", "3", "303", "1", "0"]
        );
    }
    #[test]
    fn intern_demo() {
        // The string interner through gcc: first-seen ids (0,1,2), dedup (1,1), count
        // (3), id→string lookup (fn, return), a 200-key round-trip stress (1, 203),
        // and a stable re-intern that doesn't grow the count (1, 203).
        assert_eq!(
            toks("examples/std/intern_demo.jtr"),
            ["0", "1", "2", "1", "1", "3", "fn", "return", "1", "203", "1", "203"]
        );
    }
    #[test]
    fn drop_nested_demo() {
        // Field/payload auto-drop (B1) through the real gcc pipeline. The exact
        // sequence proves: struct fields drop in reverse order (2 before 1), a live
        // enum payload drops (7) while a `leaf` owns nothing (only 150), and a
        // struct-in-a-struct reaches its leaf destructor (9). Run it several times —
        // a stray double-drop or missed drop would shift the token stream.
        for _ in 0..4 {
            assert_eq!(
                toks("examples/drop_nested.jtr"),
                ["100", "2", "1", "200", "7", "150", "300", "9"],
                "nested drop order/count wrong"
            );
        }
    }
    #[test]
    fn nested_place_demo() {
        // Assignment THROUGH a bounds-checked index, end to end through real gcc — which is
        // the authority here, because the defect was not a wrong value but C that did not
        // compile at all ("lvalue required as left operand of assignment").
        //
        // The token stream is chosen so a write that lands in a *copy* of the element still
        // fails: each `2` is element 0 read back after element 1 was written, and `41` is a
        // nested read of a cell written through a different chain. A lowering that silently
        // discarded the store would print `2`→`9` or `41`→`36`.
        //
        // `15`/`115`/`116` are the by-ADDRESS half — an inherent `mut self` receiver, a trait
        // impl's, and a `mut` parameter. Those three take `&<place>`, so before the fix they
        // did not compile; a lowering that handed over a temporary instead would leave the
        // element at `10` and print `10, 10, 10`.
        assert_eq!(
            toks("examples/nested_place.jtr"),
            ["9", "2", "10", "15", "115", "116", "2", "5", "41", "7", "20"],
            "write through a checked index did not land in the aggregate"
        );
    }
    #[test]
    fn drop_glue_for_struct_named_like_generic_param() {
        // A user `struct T` collides in name with the blanket `impl[T] Drop for
        // List(T)`'s generic parameter. Its `List(T)` drop glue must still be emitted —
        // otherwise gcc fails to link `jestyr_impl_Drop__List_T___drop`, so building and
        // running the program at all is the regression check (it prints the pushed count).
        assert_eq!(toks("examples/std/drop_named_type_param.jtr"), ["2"]);
    }
    #[test]
    fn lexer_slice_demo() {
        // The self-hosting lexer slice, no args → lexes its built-in sample
        // `fn add(x: i32, y: i32) -> i32 { return x + y }` (comment + whitespace
        // stripped, `->` one token), then a 6-number summary: 19 tokens, 2 keywords,
        // 8 identifiers, 0 integers, 9 punctuation, 4 distinct user identifiers.
        assert_eq!(
            toks("examples/std/lexer.jtr"),
            [
                "fn", "add", "(", "x", ":", "i32", ",", "y", ":", "i32", ")", "->",
                "i32", "{", "return", "x", "+", "y", "}",
                "19", "2", "8", "0", "9", "4",
            ]
        );
    }

    /// **P1 cross-implementation golden.** The Jestyr-written lexer produces the
    /// *exact same token (lexeme) stream* as the Rust reference lexer, across a
    /// diverse slice of the real corpus — strings, floats, hex/binary ints, char
    /// literals, f-strings, nested block comments, and every multi-char operator.
    /// This is the P1 acceptance test: the front-end port is faithful token-for-token.
    #[test]
    fn jestyr_lexer_matches_reference_on_corpus() {
        let lexer = build_exe("examples/std/lexer.jtr");
        // Walk the *entire* example corpus (examples/ + examples/std/), sorted for
        // determinism — every real Jestyr file, not a hand-picked slice.
        let mut files: Vec<std::path::PathBuf> = Vec::new();
        for dir in ["examples", "examples/std"] {
            if let Ok(rd) = std::fs::read_dir(dir) {
                for e in rd.flatten() {
                    let p = e.path();
                    if p.extension().and_then(|s| s.to_str()) == Some("jtr") {
                        files.push(p);
                    }
                }
            }
        }
        files.sort();
        assert!(files.len() > 20, "expected the whole corpus, found {}", files.len());
        for p in &files {
            let f = p.to_str().unwrap();
            let src = std::fs::read_to_string(p).unwrap_or_else(|e| panic!("read {f}: {e}"));
            let want = rust_lexemes(&src);
            let got = jestyr_lexemes(&lexer, f);
            assert_eq!(got, want, "Jestyr lexer diverged from the reference on {f}");
        }
        eprintln!("cross-checked {} corpus files, all token-for-token identical", files.len());
    }

    /// **P2 token-kind cross-implementation golden.** Beyond lexeme *boundaries* (the
    /// P1 test), the Jestyr lexer must classify every token into the *same kind* as the
    /// reference — keyword vs ident, Int vs Float, each operator — across the whole
    /// corpus. This is the fully-classified token stream the parser (P2) consumes.
    #[test]
    fn jestyr_lexer_kinds_match_reference_on_corpus() {
        let lexer = build_exe("examples/std/lexer.jtr");
        let mut files: Vec<std::path::PathBuf> = Vec::new();
        for dir in ["examples", "examples/std"] {
            if let Ok(rd) = std::fs::read_dir(dir) {
                for e in rd.flatten() {
                    let p = e.path();
                    if p.extension().and_then(|s| s.to_str()) == Some("jtr") {
                        files.push(p);
                    }
                }
            }
        }
        files.sort();
        assert!(files.len() > 20, "expected the whole corpus, found {}", files.len());
        for p in &files {
            let f = p.to_str().unwrap();
            let src = std::fs::read_to_string(p).unwrap_or_else(|e| panic!("read {f}: {e}"));
            let want = rust_kinds(&src);
            let got = jestyr_kinds(&lexer, f);
            assert_eq!(got, want, "Jestyr lexer kinds diverged from the reference on {f}");
        }
        eprintln!("kind-checked {} corpus files, all classified identically", files.len());
    }

    /// **P2a kind-*tag* cross-implementation golden — the strongest form.** The label
    /// golden above prints each token via `describe()`, and for keywords/operators that
    /// label is the *source slice*, so it can't distinguish, say, `+`(Plus) from a
    /// mis-tagged kind that still spans `+`. This golden compares the raw integer
    /// `TokenKind` discriminant of every token to the reference across the whole corpus,
    /// so the `List(Token)` the parser consumes is verified tag-for-tag — the actual
    /// integers the parser will `match` on, not just their rendered labels.
    #[test]
    fn jestyr_lexer_kind_ids_match_reference_on_corpus() {
        let lexer = build_exe("examples/std/lexer.jtr");
        let mut files: Vec<std::path::PathBuf> = Vec::new();
        for dir in ["examples", "examples/std"] {
            if let Ok(rd) = std::fs::read_dir(dir) {
                for e in rd.flatten() {
                    let p = e.path();
                    if p.extension().and_then(|s| s.to_str()) == Some("jtr") {
                        files.push(p);
                    }
                }
            }
        }
        files.sort();
        assert!(files.len() > 20, "expected the whole corpus, found {}", files.len());
        for p in &files {
            let f = p.to_str().unwrap();
            let src = std::fs::read_to_string(p).unwrap_or_else(|e| panic!("read {f}: {e}"));
            let want = rust_kind_ids(&src);
            let got = jestyr_kind_ids(&lexer, f);
            assert_eq!(got, want, "Jestyr lexer kind tags diverged from the reference on {f}");
        }
        eprintln!("tag-checked {} corpus files, all discriminants identical", files.len());
    }

    /// Focused P2a tag probe: pins the exact integer discriminants for one token of
    /// every lexical class, so a regression in the tag numbering names the class. These
    /// are the numbers `examples/std/lexer.jtr` hard-codes and the parser will switch on.
    #[test]
    fn jestyr_lexer_kind_ids_pin_the_numbering() {
        let lexer = build_exe("examples/std/lexer.jtr");
        // ident, int, float, string, char, `_`, a keyword (`fn`=7), and a spread of
        // operators: `+`=79 `->`=77 `..=`=75 `::`=72 `==`=85 `.*`=76 `@`=101.
        let src = "x 1 1.5 \"s\" 'c' _ fn + -> ..= :: == .* @\n";
        let probe = std::env::temp_dir().join("jestyr_tag_probe.jtr");
        std::fs::write(&probe, src).unwrap();
        let got = jestyr_kind_ids(&lexer, probe.to_str().unwrap());
        assert_eq!(got, rust_kind_ids(src), "tag probe diverged from the reference");
        assert_eq!(
            got,
            ["0", "1", "2", "3", "5", "6", "7", "79", "77", "75", "72", "85", "76", "101"],
            "kind-tag numbering must match TokenKind's discriminants: {got:?}"
        );
    }

    /// Focused P1 probe: a crafted file exercising every new token class at once —
    /// strings with escapes, floats with exponent, hex/binary with `_` separators,
    /// `..=`/`.*`, f-strings, char escapes, and a *nested* block comment. Matches the
    /// reference and pins the exact lexemes, so a regression names the exact class.
    #[test]
    fn jestyr_lexer_handles_every_token_class() {
        let lexer = build_exe("examples/std/lexer.jtr");
        let src = "let s = \"a\\n b\"\nlet f = 1.5e-3\nlet h = 0xFF_00\nlet b = 0b1010\n\
                   x == y != z\n0..10 0..=9 p.*\nf\"hi {x}\"\n'a' '\\n'\n\
                   /* nested /* c */ still */ end\n";
        let probe = std::env::temp_dir().join("jestyr_lex_probe.jtr");
        std::fs::write(&probe, src).unwrap();
        let got = jestyr_lexemes(&lexer, probe.to_str().unwrap());
        assert_eq!(got, rust_lexemes(src), "probe diverged from the reference");
        for needle in [
            "\"a\\n b\"", // string literal (with an escape) as one token
            "1.5e-3",     // float with signed exponent
            "0xFF_00",    // hex int with digit separator
            "0b1010",     // binary int
            "==", "!=",   // multi-char comparison operators
            "..", "..=", ".*", // range + deref operators
            "f\"hi {x}\"", // f-string as one token
            "'\\n'",       // char literal with an escape
            "end",         // the token after a *nested* block comment (fully skipped)
        ] {
            assert!(got.iter().any(|t| t == needle), "missing `{needle}` in {got:?}");
        }
    }

    /// Focused P2 kind probe: pins the classifications P1 could not test — keyword vs
    /// ident, and Int vs Float — so a regression names the exact confusion.
    #[test]
    fn jestyr_lexer_kinds_distinguish_int_float_keyword_ident() {
        let lexer = build_exe("examples/std/lexer.jtr");
        let src = "const x = 1 let y = 1.5 fn z\n";
        let probe = std::env::temp_dir().join("jestyr_kind_probe.jtr");
        std::fs::write(&probe, src).unwrap();
        let got = jestyr_kinds(&lexer, probe.to_str().unwrap());
        assert_eq!(got, rust_kinds(src), "kind probe diverged from the reference");
        assert_eq!(
            got,
            ["const", "ident", "=", "int", "let", "ident", "=", "float", "fn", "ident"],
            "keyword/ident and int/float must be distinguished: {got:?}"
        );
    }

    /// The PURE canary demo: exercises the whole numeric stack but prints ONLY
    /// integers and `format_float` strings — never `print_f64`/`printf("%g")`. Its
    /// output is the locked-digest input; pin it token-for-token here too so a change
    /// is caught even without re-deriving the SHA. (See `numerics_canary.jtr`.)
    #[test]
    fn numerics_canary_demo() {
        assert_eq!(
            toks("examples/std/numerics_canary.jtr"),
            [
                // IEEE-754 bit primitives
                "3.14159e0", "1023", "0", "1", "51", "63",
                // serial reductions (10,10,10 | 0,2,0) as deterministic format_float strings
                "1e1", "1e1", "1e1", "0e0", "2e0", "0e0",
                // binned: whole==chunked==4, then the equality flag
                "4e0", "4e0", "1",
                // correct rounding (2^53+4) vs naive (2^53), then per-bin overflow (3000)
                "9.007199254740996e15", "9.007199254740992e15", "3e3",
                // parallel reduction == serial (100), then the equality flag
                "1e2", "1",
                // `par for … reduce(r)`: sum (92), sum-of-squares (1380), max (15),
                // min (-7), xor (-8), then the two `== serial` determinism flags
                "92", "1380", "15", "-7", "-8",
                // the `@simd` section: absum, sumsq, and lanes == the i64 fold (Q-S2)
                "148", "1380", "1",
                "1", "1",
                // parse_float round-trips (parse then format_float)
                "1e-1", "3.0000000000000004e-1", "1.234567890123456e15", "1e10",
                "3.14159265358979e0", "9.007199254740992e15", "5e-324",
                "1.7976931348623157e308",
                // slow path (> 19 digits): identical to short form
                "3.141592653589793e0", "9.007199254740994e15",
                // malformed → rejected
                "1",
                // integer parse/format
                "12345", "-42", "7", "9", "-4271", "-4271",
            ]
        );
    }

    /// The locked canary: SHA-256 over the PURE `numerics_canary.jtr` output. That
    /// demo, by construction, emits only `print_i32` integers and `format_float`
    /// strings — NOTHING routes through C `printf("%g")`, whose float rendering is not
    /// guaranteed identical across libc. So a digest diff can only mean a genuine
    /// numeric-determinism break (an FMA fusion, a reassociation, a rounding change),
    /// never a glibc-vs-msvcrt formatting quirk. Run on a second OS/compiler: an
    /// identical digest *proves* the contract. Re-lock only with a reviewed reason.
    #[test]
    fn numerics_determinism_canary() {
        let mut all = String::new();
        for t in toks("examples/std/numerics_canary.jtr") {
            all.push_str(&t);
            all.push('\n');
        }
        let digest = sha256::hex(all.as_bytes());
        assert_eq!(
            digest, "4389bf8328ae7e018ebf0fb6ca4f94dd95f65eb7fb24568e55b1170b809868bc",
            "numerics output changed — if intentional, re-lock; output was:\n{all}"
        );
    }
}
