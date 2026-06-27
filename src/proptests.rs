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
