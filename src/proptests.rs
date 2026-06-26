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

/// The token-kind sequence (Eof dropped) — the lexer's structural output, used by
/// the whitespace-insensitivity metamorphic property.
fn token_kinds(src: &str) -> Vec<TokenKind> {
    let (tokens, _) = Lexer::new(src).tokenize();
    tokens.iter().map(|t| t.kind).filter(|k| *k != TokenKind::Eof).collect()
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
        fn valid_programs_compile_deterministically(p in arb_program()) {
            prop_assert_eq!(compile(&p), compile(&p));
        }

        /// A generated valid program lowers to *some* C built on the runtime prelude
        /// (cgen never silently produces nothing for a well-formed program).
        #[test]
        fn valid_program_emits_prelude(p in arb_program()) {
            let (c, _) = compile(&p);
            prop_assert!(c.contains("#include"), "no prelude in: {}", c);
        }

        /// **Metamorphic — whitespace insensitivity.** The lexer discards layout, so
        /// doubling every space leaves the token-kind sequence unchanged.
        #[test]
        fn whitespace_does_not_change_tokens(p in arb_program()) {
            prop_assert_eq!(token_kinds(&p), token_kinds(&p.replace(' ', "   ")));
        }

        /// **Metamorphic — comment insensitivity.** Comments are trivia (no tokens,
        /// no AST nodes), so prepending one cannot change the emitted C.
        #[test]
        fn comments_do_not_change_codegen(p in arb_program()) {
            let plain = compile(&p).0;
            let commented = compile(&format!("// a comment\n{p}")).0;
            prop_assert_eq!(plain, commented);
        }

        /// The AST printer is total and **idempotent** (printing twice is stable) on
        /// any generated program.
        #[test]
        fn printer_is_total_and_stable(p in arb_program()) {
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
    fn arb_program() -> impl Strategy<Value = String> {
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
    fn arb_expr() -> impl Strategy<Value = String> {
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
