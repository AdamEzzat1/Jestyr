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
            digest, "886d1b6aa0d4e57af37763903f34bcaff000fcc06929f07d3a4d031cc92af7e3",
            "numerics output changed — if intentional, re-lock; output was:\n{all}"
        );
    }
}
