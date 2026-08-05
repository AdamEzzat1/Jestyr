//! Stage ⑥: the C backend.
//!
//! Lowers the **non-generic subset** of a (already type- and ownership-checked)
//! Jestyr program to portable C99, which a system C compiler turns into a native
//! binary. Because the earlier passes guarantee well-formedness, this is a mostly
//! mechanical syntax-directed walk — it never has to reason about safety.
//!
//! ## Key technique: context-directed lowering
//! Jestyr is expression-oriented (an `if` yields a value; a body's tail
//! expression is its result); C is statement-oriented. Rather than build an IR,
//! emission threads a `ret: bool` flag: in *return position* the tail of each
//! branch becomes a `return`, so `if c { a } else { b }` lowers to
//! `if (c) { return a; } else { return b; }`.
//!
//! ## Naming (collision-free by construction)
//!  * user types     → `Jestyr_<Name>`
//!  * user functions → `jestyr_<name>`
//!  * values/fields  → `j_<name>`   (so a variable named `int` can't clash)
//!  * print intrinsics → `jestyr_rt_*`
//!
//! ## Out of scope (reported as diagnostics, not emitted wrong)
//! generics, enums/`match`, methods/`self`, `?`/error sets, ranges, attributes.
//! `print_int` / `print_str` / `print_bool` are temporary prelude intrinsics that
//! stand in for a standard library.

use std::collections::{HashMap, HashSet};
use std::fmt::Write;

use crate::ast::*;
use crate::comptime;
use crate::diag::Diagnostic;
use crate::module::ModId;
use crate::span::Span;
use crate::typeck::unify_tp;
use crate::types::{prim_ty, ImplCall, MethodRes, Ty, TypeInfo, TypeKindG};

/// Lower a program to C, ending with the ordinary entry-point wrapper around
/// the user's `main`.
pub fn emit(ast: &Ast, info: &TypeInfo) -> (String, Vec<Diagnostic>) {
    emit_program(ast, info, false, false, None, false)
}

/// Like [`emit`], but annotates every inserted scope-exit drop call with a
/// `/* drop … */` comment so the implicit-but-inspectable drop glue is visible in
/// the emitted C (drives `jestyrc emit-c --show-drops`). Implicit ≠ hidden: the
/// "transparent cost" thesis says auto-inserted control flow must be inspectable.
pub fn emit_show_drops(ast: &Ast, info: &TypeInfo) -> (String, Vec<Diagnostic>) {
    emit_program(ast, info, false, true, None, false)
}

/// Like [`emit`], but instruments the error paths with a **debug error trace**
/// (`jestyrc build/run <file> --error-traces` — roadmap Error-handling tier 4,
/// Zig-style). Three instrumentation points:
///
/// * `err(E)` — the **origin**: resets the trace and records where the error was born.
/// * `e?` — each **propagation** hop records itself before the early return, so the
///   trace reads as the error's path up the stack.
/// * `unwrap(e)` on an error — the **surfacing** point: prints the trace to stderr.
///   (stderr, so the program's *stdout* — the thing the determinism canaries hash —
///   is untouched even when a trace fires.)
///
/// Strictly opt-in and per-invocation: without the flag this function is never
/// called and the emitted C is byte-identical, which is what keeps all corpus
/// goldens, the attest hashes, the fixpoint and the seed out of scope. The trace
/// buffer is a fixed-size ring in the emitted C — no allocation, no dependence on
/// program state, so instrumentation cannot change program behaviour.
pub fn emit_error_traces(ast: &Ast, info: &TypeInfo) -> (String, Vec<Diagnostic>) {
    emit_program(ast, info, false, false, None, true)
}

/// Lower a program to C in *test* mode: instead of the `main` wrapper, emit a
/// harness `main` that runs every `@test` (reporting pass/fail) and times every
/// `@bench`. Drives `jestyrc test` (roadmap workstream O).
pub fn emit_tests(ast: &Ast, info: &TypeInfo) -> (String, Vec<Diagnostic>) {
    emit_program(ast, info, true, false, None, false)
}

/// Like [`emit_tests`], but bakes only the `@test`/`@bench` items whose name
/// *contains* `filter` into the harness — the codegen half of `jestyrc test
/// <substr>` name filtering. With `None`, identical to [`emit_tests`]. Filtering
/// at codegen (not at runtime via `argv`) keeps the harness's `running N test(s)`
/// line equal to the *baked* count, so an empty filter is byte-for-byte the
/// unfiltered harness. (Workstream O.)
pub fn emit_tests_filtered(ast: &Ast, info: &TypeInfo, filter: Option<&str>) -> (String, Vec<Diagnostic>) {
    emit_program(ast, info, true, false, filter, false)
}

/// A runnable harness entry: a `@test` (a no-arg `-> bool`, `true` = pass) or a
/// `@bench` (a no-arg body timed with `clock()`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TestKind {
    Test,
    Bench,
}

/// The runnable `@test`/`@bench` items of a program, in source order — the data
/// behind `jestyrc test --list` (and the oracle for the filtered harness). Mirrors
/// `test_main`'s `runnable` predicate exactly (non-generic, backend-supported), so
/// the list never names a test the harness would silently skip. Pure: needs no
/// `TypeInfo` and never compiles, so `--list` is toolchain-free.
pub fn list_tests(ast: &Ast) -> Vec<(String, TestKind)> {
    ast.items
        .iter()
        .filter_map(|it| match it {
            Item::Fn(f) if is_generic_ast(ast, f) || !fn_supported_ast(ast, f) => None,
            Item::Fn(f) if f.has_attr("test") => Some((f.name.name.clone(), TestKind::Test)),
            Item::Fn(f) if f.has_attr("bench") => Some((f.name.name.clone(), TestKind::Bench)),
            _ => None,
        })
        .collect()
}

/// A `comptime <name>: type` parameter — an erased monomorphization knob, not a
/// runtime argument. Free-function form of `Cgen::is_type_param`, so `list_tests`
/// can share the predicate without a fully-built `Cgen`.
fn is_type_param_ast(ast: &Ast, p: &Param) -> bool {
    p.comptime && p.ty.is_some_and(|t| matches!(ast.type_at(t).kind, TypeKind::TypeKw))
}

/// A monomorphization template — has a `comptime T: type` or a bracket-form
/// `[T: Bound]` generic. Such a function is never emitted directly, so it can't be
/// a runnable test. Free-function form of `Cgen::is_generic`.
fn is_generic_ast(ast: &Ast, f: &FnDecl) -> bool {
    !f.generics.is_empty() || f.params.iter().any(|p| is_type_param_ast(ast, p))
}

/// Backend-emittable: no `self` (methods) and no `comptime` *value* parameters
/// (only `comptime` *type* parameters are ok). Free-function form of
/// `Cgen::fn_supported`.
fn fn_supported_ast(ast: &Ast, f: &FnDecl) -> bool {
    f.params.iter().all(|p| !p.is_self && (!p.comptime || is_type_param_ast(ast, p)))
}

fn emit_program(
    ast: &Ast,
    info: &TypeInfo,
    test_mode: bool,
    show_drops: bool,
    test_filter: Option<&str>,
    error_traces: bool,
) -> (String, Vec<Diagnostic>) {
    // Index every enum variant by name, so the backend can construct and match
    // on them by finding the owning enum and the variant's payload fields.
    let mut variants = HashMap::new();
    for (i, item) in ast.items.iter().enumerate() {
        if let Item::Enum(e) = item {
            // Key by the *canonical* variant name and store the *canonical* enum
            // name, so two modules' same-named variants/enums don't alias (bare
            // for any non-colliding name, so output is unchanged there).
            let m = *info.item_mod.get(i).unwrap_or(&0);
            for v in &e.variants {
                variants.insert(
                    crate::types::canon(m, &v.name.name, &info.dup_variants),
                    VariantInfo {
                        enum_name: crate::types::canon(m, &e.name.name, &info.dup_types),
                        fields: v.fields.iter().map(|(id, t)| (id.name.clone(), *t)).collect(),
                    },
                );
            }
        }
    }

    // Names of generic functions (those with `comptime <name>: type` parameters).
    // They are templates: never emitted directly, only as monomorphized instances.
    let mut generics = HashSet::new();
    // Every error name across all error sets, mapped to a distinct integer tag.
    let mut error_tags: HashMap<String, i64> = HashMap::new();
    for (i, item) in ast.items.iter().enumerate() {
        if let Item::Fn(f) = item {
            // A function is monomorphized if it has a `comptime T: type` parameter
            // *or* a bracket-form `[T: Bound]` generic — both are templates emitted
            // per concrete instantiation, never directly.
            let is_gen = !f.generics.is_empty()
                || f.params.iter().any(|p| {
                    p.comptime && p.ty.is_some_and(|t| matches!(ast.type_at(t).kind, TypeKind::TypeKw))
                });
            if is_gen {
                // Keyed on the canonical name so two modules' generic templates of
                // the same name stay distinct (bare name when not duplicated).
                let m = *info.item_mod.get(i).unwrap_or(&0);
                generics.insert(crate::types::canon(m, &f.name.name, &info.dup_fns));
            }
            if let Some(es) = &f.errors {
                for name in &es.names {
                    let next = error_tags.len() as i64 + 1;
                    error_tags.entry(name.name.clone()).or_insert(next);
                }
            }
        }
        // Fallible STRUCT METHODS declare error sets too, and their tags live in the
        // same whole-program map — an error name means one integer everywhere,
        // whoever declares it. Scanned in declaration order after the item's own set,
        // so adding a method never renumbers a free function's tags. (Trait-impl
        // methods are deliberately absent: a fallible impl is refused — calls are
        // typed by the trait's signature, which has no error-set syntax.)
        if let Item::Struct { body, .. } = item {
            for m in &body.members {
                if let StructMember::Method(f) = m {
                    if let Some(es) = &f.errors {
                        for name in &es.names {
                            let next = error_tags.len() as i64 + 1;
                            error_tags.entry(name.name.clone()).or_insert(next);
                        }
                    }
                }
            }
        }
    }

    let mut g = Cgen {
        ast,
        info,
        out: String::new(),
        diags: Vec::new(),
        depth: 0,
        dbg_last: None,
        cur_mod: 0,
        ptr_params: HashSet::new(),
        variants,
        tmp: 0,
        generics,
        subst: HashMap::new(),
        instances: Vec::new(),
        struct_instances: Vec::new(),
        enum_instances: Vec::new(),
        error_tags,
        cur_result: String::new(),
        cur_ensures: Vec::new(),
        cur_ret_cty: String::new(),
        closures: Vec::new(),
        closure_index: HashMap::new(),
        capture_set: HashSet::new(),
        method_instances: Vec::new(),
        self_cty: String::new(),
        self_is_ptr: false,
        extern_fns: ast
            .items
            .iter()
            .filter_map(|it| match it {
                Item::Extern(e) => Some(e.name.name.clone()),
                _ => None,
            })
            .collect(),
        dyn_traits: ast
            .types
            .iter()
            .filter_map(|t| match &t.kind {
                TypeKind::Dyn(n) => Some(n.name.clone()),
                _ => None,
            })
            .collect(),
        dyn_guard: HashSet::new(),
        spawn_sites: Vec::new(),
        // Gate the `try_read_file` runtime + result typedef on actual use, so an
        // unrelated program's C is byte-identical (the additive invariant).
        uses_try_read: ast.exprs.iter().any(|e| {
            if let ExprKind::Call { callee, .. } = &e.kind {
                matches!(&ast.expr_at(*callee).kind, ExprKind::Name(n) if n.name == "try_read_file")
            } else {
                false
            }
        }),
        uses_run_command: ast.exprs.iter().any(|e| {
            if let ExprKind::Call { callee, .. } = &e.kind {
                matches!(&ast.expr_at(*callee).kind, ExprKind::Name(n) if n.name == "run_command")
            } else {
                false
            }
        }),
        uses_eprint: ast.exprs.iter().any(|e| {
            if let ExprKind::Call { callee, .. } = &e.kind {
                matches!(&ast.expr_at(*callee).kind, ExprKind::Name(n) if n.name == "eprint_str")
            } else {
                false
            }
        }),
        task_handles: HashMap::new(),
        dyn_spawn_active: false,
        slice_instances: Vec::new(),
        simd_sites: std::collections::HashMap::new(),
        array_instances: Vec::new(),
        genref_instances: Vec::new(),
        fn_type_instances: Vec::new(),
        cur_refines: HashMap::new(),
        scratch_reset: None,
        cont_label: None,
        break_label: None,
        variant_trackers: HashMap::new(),
        cur_no_panic: false,
        no_mangle: ast
            .items
            .iter()
            .filter_map(|it| match it {
                // `main` is already exported as the C `main` by the entry wrapper,
                // so `@no_mangle` on it is a redundant no-op rather than a rename.
                Item::Fn(f) if f.has_attr("no_mangle") && f.name.name != "main" => {
                    Some(f.name.name.clone())
                }
                _ => None,
            })
            .collect(),
        no_mangle_consts: ast
            .items
            .iter()
            .filter_map(|it| match it {
                Item::Const(c) if c.has_attr("no_mangle") => Some(c.name.name.clone()),
                _ => None,
            })
            .collect(),
        test_mode,
        test_filter: test_filter.map(str::to_string),
        show_drops,
        error_traces,
        drop_stack: Vec::new(),
        cur_moved: HashSet::new(),
        def_cap: None,
    };
    g.spawn_sites = g.collect_spawns();
    g.genref_instances = g.collect_genrefs();
    let (instances, method_instances) = g.collect_all_instances();
    g.instances = instances;
    g.method_instances = method_instances;
    // After `instances`: `collect_slices` also walks monomorphized generic function
    // signatures, so a `[]T` parameter of a generic combinator contributes its
    // concrete `JestyrSlice_<T>` typedef even when the caller never writes a
    // `slice(T, …)` literal locally.
    g.simd_sites = g.collect_simd_sites();
    g.slice_instances = g.collect_slices();
    g.array_instances = g.collect_arrays();
    g.struct_instances = g.collect_struct_instances();
    g.enum_instances = g.collect_enum_instances();
    // After struct instances: a monomorphized generic-struct's fn-pointer fields
    // contribute concrete `JestyrFn_…` typedefs, so this must run last.
    g.fn_type_instances = g.collect_fn_types();
    let (closures, closure_index) = g.collect_closures();
    g.closures = closures;
    g.closure_index = closure_index;
    g.prelude();
    g.forward_types();
    // Forward-declare every monomorphized generic struct/enum instance too, before
    // the fn-pointer typedefs — so a `JestyrFn_…` may *return* a generic enum by
    // value (`fn(T) -> Option(U)`, the monadic-combinator shape). C accepts a
    // forward-declared aggregate as a fn-pointer return/param type; the bodies
    // follow in `gen_struct_defs`/`gen_enum_defs`.
    g.gen_forward_types();
    // Function-pointer typedefs come right after the forward struct/enum
    // typedefs (so a `JestyrFn_…` over a struct/enum sees its forward name) and
    // *before* `struct_defs`, so a struct may hold a fn-pointer field — the
    // hand-written-vtable shape (e.g. an allocator interface).
    g.fn_type_typedefs();
    // Aggregate definitions are captured per-unit and flushed in a topological order
    // so a struct embedding another aggregate *by value* (e.g. a `List(E)` field) is
    // emitted after that aggregate's definition. A no-op reorder for programs with no
    // forward by-value dependency (byte-identical output).
    g.begin_def_capture();
    g.struct_defs();
    g.enum_defs();
    g.gen_struct_defs();
    g.gen_enum_defs();
    g.slice_struct_defs();
    g.simd_vector_defs();
    g.array_struct_defs();
    g.genref_struct_defs();
    g.result_defs();
    g.flush_def_capture();
    // `dyn Trait` vtable structs + fat-pointer typedefs — after the value typedefs
    // (a method's arg/return types are named) and before any function uses them.
    g.dyn_typedefs();
    g.extern_protos();
    g.closure_types();
    g.fn_protos();
    g.method_protos();
    g.impl_protos();
    // Vtable shims + static instances — after `impl_protos` so the shims can call
    // the (forward-declared) impl methods.
    g.dyn_vtables();
    g.spawn_runtime();
    g.consts();
    g.closure_fns();
    g.fn_defs();
    g.method_defs();
    g.impl_defs();
    if g.test_mode {
        g.test_main();
    } else {
        g.main_wrapper();
    }
    (g.out, g.diags)
}

#[derive(Clone)]
struct VariantInfo {
    enum_name: String,
    fields: Vec<(String, TypeId)>,
}

/// A *niche-optimized* enum: exactly two variants — one nullary and one carrying
/// a single thin-pointer payload — so the whole enum is represented as just that
/// pointer (the nullary variant is encoded as `NULL`). Zero tag, zero padding:
/// `size_of(Option(*T)) == size_of(*T)`. (CJC-inspired §1.3; Rust niche opt.)
#[derive(Clone)]
struct NicheInfo {
    /// The nullary variant — encoded as the null pointer.
    none_variant: String,
    /// The single-field variant — encoded as the payload pointer itself.
    some_variant: String,
    /// The pointer payload type (the enum's whole representation).
    payload: Ty,
}

/// Does this type lower to a single pointer with a usable `NULL` niche? Raw
/// pointers and zero-cost region references do; a generational `&T` (fat:
/// `{ptr, gen}`) and a slice `[]T` (fat: `{ptr, len}`) do not.
fn is_niche_pointer(t: &Ty) -> bool {
    matches!(t, Ty::Ptr { .. } | Ty::RegionRef(_))
}

/// The pointee of a *plain* pointer (`*T` / `&[r]T`), for looking through an
/// `indirect`/raw field when matching a constructor against it. A fat generational
/// `&T` is excluded — its deref is checked, not a structural `(*p)`.
fn pointer_pointee(t: &Ty) -> Option<Ty> {
    match t {
        Ty::Ptr { inner, .. } => Some((**inner).clone()),
        Ty::RegionRef(inner) => Some((**inner).clone()),
        _ => None,
    }
}

/// A `spawn <fn>(args)` site inside a `concurrent` block. Each becomes an
/// argument struct + a `void*` thread trampoline, keyed by the call's expr id.
#[derive(Clone)]
struct SpawnSite {
    /// The inner *call* expression's id — the suffix of `_jsp_<id>` / `jestyr_task_<id>`.
    call_id: ExprId,
    fn_name: String,
    args: Vec<ExprId>,
}

/// A `let h = spawn f(…)` task handle live inside a `concurrent` scope: maps the
/// binding name to the C thread/arg-struct/joined-flag suffix (`_jt<idx>` etc.) and
/// the task's result C type (`None` for a `void` target). `await h` joins-if-needed
/// (the `_jd<idx>` flag guards against a double-join at the brace) and reads `.ret`.
#[derive(Clone)]
struct TaskHandle {
    idx: usize,
    ret_cty: Option<String>,
}

/// A unit of monomorphization work. Generic functions and generic-struct methods
/// can each pull in the other, so a single worklist threads them together.
enum Work {
    /// A generic free function instantiated at `(name, type args)`.
    Fn(String, Vec<Ty>),
    /// A method `(ctor, type args, method name)` of a (generic) struct.
    Method(String, Vec<Ty>, String),
}

/// A lambda-lifted closure: an environment of captured values, a set of
/// parameters, a return type, and the body to emit as a top-level function.
#[derive(Clone)]
struct ClosureInfo {
    /// The closure expression's id — also the suffix of every emitted C name
    /// (`JestyrEnv_<id>`, `JestyrClosure_<id>`, `jestyr_lam_<id>`).
    id: ExprId,
    /// Parameter (name, optional annotated type).
    params: Vec<(String, Option<TypeId>)>,
    /// The closure's return type (the inferred type of its body).
    ret: Ty,
    /// Captured free variables (name + type), copied into the environment.
    captures: Vec<(String, Ty)>,
    /// The body expression.
    body: ExprId,
}

struct Cgen<'a> {
    ast: &'a Ast,
    info: &'a TypeInfo,
    out: String,
    diags: Vec<Diagnostic>,
    depth: usize,
    /// The `(path, line)` of the last `#line` directive emitted, so per-statement
    /// debug info only emits a directive when the source line actually changes
    /// (increment b). Reset to `None` at each function entry. Always `None`'s
    /// effect on the empty-debug path: `mark_line` is a no-op there.
    dbg_last: Option<(String, u32)>,
    /// Names of the current function's by-pointer (`mut`/`out`) parameters, which
    /// must be dereferenced on use.
    ptr_params: HashSet<String>,
    /// *canonical* variant name → its enum and payload field list.
    variants: HashMap<String, VariantInfo>,
    /// The module whose item is currently being emitted — so a bare type/variant
    /// name resolves to the right `Jestyr_<type>` C symbol when the name collides
    /// across modules (collidable types). 0 (the root) for synthesized contexts;
    /// harmless there because `canon` is the identity for any non-colliding name.
    cur_mod: ModId,
    /// counter for unique `match` scrutinee temporaries.
    tmp: usize,
    /// names of generic function templates.
    generics: HashSet<String>,
    /// active type-parameter substitution while emitting a monomorphized instance.
    subst: HashMap<String, Ty>,
    /// every monomorphized instance to emit: (generic fn name, concrete type args).
    instances: Vec<(String, Vec<Ty>)>,
    /// every monomorphized generic-struct instance: (ctor name, concrete type args).
    struct_instances: Vec<(String, Vec<Ty>)>,
    /// every monomorphized generic-enum instance: (ctor name, concrete type args).
    enum_instances: Vec<(String, Vec<Ty>)>,
    /// error name → its integer tag.
    error_tags: HashMap<String, i64>,
    /// the C result-struct type of the function currently being emitted (empty if
    /// the function is not fallible). Used by `ok`/`err`/`?`.
    cur_result: String,
    /// `ensures` postconditions of the function being emitted (checked before
    /// every value return, with `result` bound to the returned value).
    cur_ensures: Vec<ExprId>,
    /// the C return type of the function being emitted (for the `result` spill).
    cur_ret_cty: String,
    /// every lambda-lifted closure to emit.
    closures: Vec<ClosureInfo>,
    /// closure-expr id → index into `closures`.
    closure_index: HashMap<ExprId, usize>,
    /// while emitting a lifted closure body, the names captured from the
    /// environment (rendered as `j__env->j_<name>` rather than `j_<name>`).
    capture_set: HashSet<String>,
    /// every monomorphized generic-struct method to emit: (ctor, type args, method).
    method_instances: Vec<(String, Vec<Ty>, String)>,
    /// while emitting a struct method, the C type of its `self` (empty otherwise).
    self_cty: String,
    /// is the current method's `self` passed by pointer (`mut`/`out self`)?
    self_is_ptr: bool,
    /// names declared via `extern "c"` — called by their bare C name, not mangled.
    extern_fns: HashSet<String>,
    /// trait names used as `dyn Trait` anywhere — each gets a synthesized vtable
    /// struct + fat-pointer typedef, and a static vtable per `impl` (Stage F).
    dyn_traits: HashSet<String>,
    /// exprs currently being emitted *as* a `dyn` coercion — a recursion guard so
    /// `emit_dyn_coercion` can re-emit the underlying concrete value (Stage F).
    dyn_guard: HashSet<ExprId>,
    /// every `spawn` site, for emitting per-site arg structs + trampolines.
    spawn_sites: Vec<SpawnSite>,
    /// Whether the program calls `try_read_file` (B3), so its runtime helper and
    /// the `JestyrResult_String` typedef are emitted *only when used* — keeping the
    /// C for every program that doesn't use it byte-identical.
    uses_try_read: bool,
    /// `run_command(cmd) -> i32` (the self-hosted driver's gcc step) is used —
    /// gate its runtime helper so unrelated programs stay byte-identical.
    uses_run_command: bool,
    /// `eprint_str(s)` (stderr diagnostics for the self-hosted driver) is used.
    uses_eprint: bool,
    /// task handles (`let h = spawn …`) live in the current `concurrent` scope,
    /// keyed by binding name — consumed by `await`. Saved/restored across nesting.
    task_handles: HashMap<String, TaskHandle>,
    /// true while emitting the body of a `concurrent` block that has dynamic-N spawns
    /// (a `spawn` inside a loop): a bare `spawn` then pushes onto the growable handle
    /// array `_dt`/`_da` rather than getting a fixed numbered handle.
    dyn_spawn_active: bool,
    /// distinct slice element types, for emitting one `JestyrSlice_<T>` per type.
    slice_instances: Vec<Ty>,
    /// The `par for` sites an `@simd` function declares AND `simd::classify` certifies —
    /// the loops this run lowers to vectors. Keyed by the `ParFor` node so `emit_par_for`
    /// needs no enclosing-function context.
    simd_sites: std::collections::HashMap<ExprId, Ty>,
    /// Every distinct fixed-size array type `[N]T` the program uses (one
    /// `JestyrArr_<T>_<N>` typedef each). Each is a `Ty::Array`.
    array_instances: Vec<Ty>,
    /// distinct generational-reference element types (one `JestyrRef_<T>` each).
    genref_instances: Vec<Ty>,
    /// distinct function-pointer signatures, for emitting one `JestyrFn_<sig>`
    /// typedef each (so a fn-pointer is a plain named type everywhere it appears).
    fn_type_instances: Vec<Ty>,
    /// the current function's refined parameters: name → its `in <range>` expr.
    /// Used to *elide* a slice bounds check when the index is provably in range.
    cur_refines: HashMap<String, ExprId>,
    /// for a region-scoped loop, the scratch arena name whose per-iteration reset
    /// is still pending (armed by `emit_for`, consumed at the top of the body).
    scratch_reset: Option<String>,
    /// for a labeled loop, the label whose `<label>__continue:` target must be
    /// emitted at the bottom of the body (armed by `emit_for`, consumed by the
    /// body emitter).
    cont_label: Option<String>,
    /// for the *innermost* loop that has an `else`, the label a plain (unlabeled)
    /// `break` must `goto` so it skips the `else` block (whose `<label>__break:`
    /// target sits *after* the `else`). `None` when the innermost loop has no
    /// `else`, so a plain `break` lowers to C `break`. Saved/restored per loop so
    /// it always names the nearest loop.
    break_label: Option<String>,
    /// `variant <expr>` node id → its hoisted tracker index (pre-scanned per loop).
    variant_trackers: HashMap<ExprId, usize>,
    /// is the function being emitted `@no_panic`? If so, a non-elided (faulting)
    /// slice index is a compile error rather than a runtime bounds check.
    cur_no_panic: bool,
    /// functions marked `@no_mangle` — emitted under their bare Jestyr name (no
    /// `jestyr_` prefix) so they expose a stable C symbol, the export counterpart
    /// to `extern "c"` import. Validation forbids this on generic functions.
    no_mangle: HashSet<String>,
    /// `const`s marked `@no_mangle` — emitted as an external `<name>` global (not
    /// `static const j_<name>`); references render bare. (A local shadowing such a
    /// name would mis-resolve — top-level names are globally unique by design.)
    no_mangle_consts: HashSet<String>,
    /// emitting a `jestyrc test` harness (`@test`/`@bench` runner) rather than the
    /// ordinary `main` wrapper.
    test_mode: bool,
    /// when set, the test harness bakes only the `@test`/`@bench` items whose name
    /// *contains* this substring — `jestyrc test <substr>` name filtering, applied
    /// at codegen so `running N test(s)` equals the baked count. `None` = run all.
    test_filter: Option<String>,
    /// annotate each inserted drop call with a `/* drop … */` comment so the
    /// implicit drop glue is inspectable (`--show-drops`). Implicit ≠ hidden.
    show_drops: bool,
    /// instrument the error paths with a debug trace (`--error-traces`): `err` is the
    /// origin, each `?` a hop, `unwrap`-on-error the surfacing print. Off for every
    /// golden/corpus emission, so non-users are byte-identical.
    error_traces: bool,
    /// the live-droppable stack: one entry per open `{ }` block, holding the owned
    /// `Drop`-implementing locals declared in it (in declaration order). A normal
    /// fall-through drops the top entry in reverse; a `return` drops every live
    /// entry, innermost first. Because a local is only registered *as its `let` is
    /// emitted*, an early `return` never drops a not-yet-declared local — this is
    /// the static, drop-flag-free liveness the ownership model buys us.
    drop_stack: Vec<Vec<DropLocal>>,
    /// names of the current function's locals whose value *escapes* (is returned,
    /// passed by value to a call, captured into a struct, or rebound) and so must
    /// **not** get scope-exit drop glue — the consumer owns it. Over-approximated
    /// (any by-value escape suppresses the drop), so the result is leak-safe: a
    /// value is dropped at most once, never twice.
    cur_moved: HashSet<String>,
    /// While emitting aggregate *definitions* (structs/enums/generic instances/
    /// slices/arrays/genrefs/results), captures each definition as a segment of the
    /// output buffer plus its by-value type dependencies, so `flush_def_capture` can
    /// re-emit them in a topological order — a struct that embeds another aggregate
    /// *by value* (e.g. a field of type `List(E)`) needs that aggregate's full C
    /// definition to precede it. `None` outside the capture window. The sort is
    /// stable (input order preserved except where a dependency forces a move), so a
    /// program with no forward by-value dependency is emitted byte-identically.
    def_cap: Option<DefCapture>,
}

/// Accumulates aggregate-definition output for topological reordering
/// (see [`Cgen::def_cap`]). Segments cover the capture buffer in order; a segment
/// with `cname: Some(_)` is one aggregate definition (dependency-orderable), and a
/// `cname: None` segment is interstitial glue (blank lines, intrinsic typedefs) held
/// in place.
#[derive(Default)]
struct DefCapture {
    segs: Vec<DefSeg>,
    /// End offset (into the capture buffer) of the last finalized segment.
    last: usize,
    /// The open unit's (cname, deps, start offset), between `def_begin`/`def_end`.
    pending: Option<(String, Vec<String>, usize)>,
    /// The real output buffer, set aside while definitions accumulate in `self.out`.
    real: String,
}

/// One segment of captured aggregate-definition output.
struct DefSeg {
    /// The C type name this segment *defines* (for dependency matching), or `None`
    /// for interstitial glue.
    cname: Option<String>,
    /// The C type names this definition embeds *by value* (must precede it).
    deps: Vec<String>,
    /// Half-open byte range into the capture buffer.
    start: usize,
    end: usize,
}

/// One owned local that needs scope-exit drop glue — either it has its own `Drop`
/// impl, or it transitively owns a field/payload that does (design Phase 3, §2.8).
#[derive(Clone)]
struct DropLocal {
    /// the Jestyr local name (emitted as `j_<name>`).
    name: String,
    /// the local's type — walked at emission to drop its own value (if `Drop`) and
    /// then recurse into owned struct fields / live enum payloads.
    ty: Ty,
}

impl<'a> Cgen<'a> {
    fn diag(&mut self, span: Span, msg: impl Into<String>) {
        self.diags.push(Diagnostic::new(msg, span));
    }

    /// The canonical *type* name owned by module `m` — the `Jestyr_<type>` C symbol
    /// (bare unless the type name collides across modules, so output is unchanged
    /// for every collision-free program).
    fn canon_type_in(&self, m: ModId, name: &str) -> String {
        crate::types::canon(m, name, &self.info.dup_types)
    }

    /// The canonical type name resolved from the module currently being emitted.
    fn canon_type(&self, name: &str) -> String {
        self.canon_type_in(self.cur_mod, name)
    }

    /// The canonical *variant* name resolved from the module currently being
    /// emitted (the key of the `variants` table).
    fn canon_variant(&self, name: &str) -> String {
        crate::types::canon(self.cur_mod, name, &self.info.dup_variants)
    }

    /// The owning module of the item at `ast.items[i]`.
    fn item_module(&self, i: usize) -> ModId {
        *self.info.item_mod.get(i).unwrap_or(&0)
    }

    /// The module an import `binding` refers to, from the module being emitted —
    /// so a `mod.Type` path resolves to that module's (possibly colliding) type.
    fn path_target(&self, binding: &str) -> Option<ModId> {
        self.info.imports.get(self.cur_mod).and_then(|m| m.get(binding)).copied()
    }

    fn raw(&mut self, s: impl AsRef<str>) {
        self.out.push_str(s.as_ref());
    }

    fn line(&mut self, s: impl AsRef<str>) {
        let pad = "    ".repeat(self.depth);
        let _ = writeln!(self.out, "{pad}{}", s.as_ref());
    }

    /// Emit a C `#line <line> "<file>"` preprocessor directive for `span` — but
    /// only when the resolved `(path, line)` differs from the last one emitted, so
    /// a run of statements on the same source line costs one directive, not many
    /// (increment b). A debugger/profiler then maps the generated C that follows
    /// back to its `.jtr` source (gcc carries `#line` into DWARF).
    ///
    /// A **no-op** when there is no source-region info — the single-file unit-test
    /// path leaves `TypeInfo::debug` empty, keeping its emitted C byte-identical.
    /// The directive is written at column 0 (preprocessor directives are not
    /// indented) and the path is normalized to forward slashes so a Windows path
    /// like `C:\a\b.jtr` does not turn `\a`/`\b` into C string escapes. `#line` is
    /// purely additive: it never changes behavior or the locked FP determinism flags.
    /// The `jestyr_et_push("<file>", <line>)` call for an error-trace hop at `span` —
    /// or an empty string when tracing is off, so every instrumentation site can be
    /// written unconditionally and non-users stay byte-identical by construction.
    ///
    /// Location resolution is `mark_line`'s (the module loader's `DebugInfo`, paths
    /// normalized to forward slashes so the trace is host-independent). On the
    /// single-file path `DebugInfo` is empty and there is no file to name; `<input>:0`
    /// is emitted rather than nothing, so a trace still shows its *shape* (origin plus
    /// hop count) even where the mapping has no names to offer.
    fn et_push(&mut self, span: Span) -> String {
        if !self.error_traces {
            return String::new();
        }
        match self.info.debug.span_to_file_line(span) {
            Some((p, l)) => {
                let norm = p.replace('\\', "/");
                format!("jestyr_et_push(\"{norm}\", {l}); ")
            }
            None => "jestyr_et_push(\"<input>\", 0); ".to_string(),
        }
    }

    fn mark_line(&mut self, span: Span) {
        // Resolve to an owned `(path, line)` first, ending the borrow of `self.info`
        // before mutating `self.out`/`self.dbg_last`.
        let resolved = self.info.debug.span_to_file_line(span).map(|(p, l)| (p.replace('\\', "/"), l));
        if let Some((norm, line)) = resolved {
            if self.dbg_last.as_ref().map(|(p, l)| (p.as_str(), *l)) != Some((norm.as_str(), line)) {
                let _ = writeln!(self.out, "#line {line} \"{norm}\"");
                self.dbg_last = Some((norm, line));
            }
        }
    }

    /// The source span of a statement, for `#line` mapping: `Let`/`Return` carry
    /// their own span; an expression-statement uses its expression's span.
    fn stmt_span(&self, stmt: &Stmt) -> Span {
        match stmt {
            Stmt::Let { span, .. } => *span,
            Stmt::Return { span, .. } => *span,
            Stmt::Expr(e) => self.ast.expr_at(*e).span,
        }
    }

    // --- top-level sections ---

    fn prelude(&mut self) {
        self.raw("#include <stdint.h>\n#include <stdbool.h>\n#include <stddef.h>\n#include <stdio.h>\n#include <stdlib.h>\n#include <string.h>\n#include <assert.h>\n");
        if !self.spawn_sites.is_empty() {
            self.raw("#include <pthread.h>\n");
        }
        if self.test_mode {
            self.raw("#include <time.h>\n"); // `@bench` timing via clock()
        }
        self.raw("\n");
        if self.error_traces {
            // The debug error-trace runtime (`--error-traces`). A fixed-size buffer of
            // (file, line) hops — no allocation, so instrumentation cannot fail or
            // change program behaviour, and stderr-only, so the program's stdout (the
            // thing determinism canaries hash) is untouched even when a trace fires.
            // Overflow keeps the OLDEST hops: the origin is the entry a reader needs
            // most, and a trace that silently rotated it away would point mid-path.
            self.raw(
                "/* --error-traces runtime: origin + propagation hops, printed at unwrap-on-error */\n\
                 static const char* jestyr_et_file[64];\n\
                 static int jestyr_et_line[64];\n\
                 static int jestyr_et_n = 0;\n\
                 static inline void jestyr_et_reset(void) { jestyr_et_n = 0; }\n\
                 static inline void jestyr_et_push(const char* f, int l) {\n\
                 \x20   if (jestyr_et_n < 64) { jestyr_et_file[jestyr_et_n] = f; jestyr_et_line[jestyr_et_n] = l; jestyr_et_n++; }\n\
                 }\n\
                 static void jestyr_et_dump(void) {\n\
                 \x20   fprintf(stderr, \"error trace (origin first):\\n\");\n\
                 \x20   for (int _i = 0; _i < jestyr_et_n; _i++)\n\
                 \x20       fprintf(stderr, \"  %s:%d%s\\n\", jestyr_et_file[_i], jestyr_et_line[_i], _i == 0 ? \" (error created here)\" : \"\");\n\
                 }\n\n",
            );
        }
        self.raw("/* Jestyr string view — a length-carrying `{ptr, len}` (a borrowed UTF-8 view,\n");
        self.raw("   like Zig `[]const u8` / Rust `&str`). `.len` is O(1); no `strlen`. A bare\n");
        self.raw("   `cstr` (null-terminated `const char*`) is the distinct C-interop type. */\n");
        self.raw("typedef struct { const char* ptr; size_t len; } JestyrStr;\n");
        self.raw("#define JSTR(s) ((JestyrStr){ (s), sizeof(s) - 1 })\n");
        self.raw("/* O(n) codepoint count (vs O(1) `.len`): a UTF-8 leading byte is one whose top two bits aren't 10. */\n");
        self.raw("static size_t jestyr_rt_count_cp(JestyrStr s) { size_t n = 0; for (size_t k = 0; k < s.len; k++) if (((uint8_t)s.ptr[k] & 0xC0u) != 0x80u) n++; return n; }\n");
        self.raw("/* Simplified grapheme segmentation: a cluster is a base codepoint plus following combining marks (é = e + U+0301). ZWJ emoji sequences are not merged — full UAX#29 needs Unicode tables. */\n");
        self.raw("static bool jestyr_rt_is_combining(uint32_t cp) { return (cp >= 0x0300u && cp <= 0x036Fu) || (cp >= 0x1AB0u && cp <= 0x1AFFu) || (cp >= 0x1DC0u && cp <= 0x1DFFu) || (cp >= 0x20D0u && cp <= 0x20FFu) || (cp >= 0xFE20u && cp <= 0xFE2Fu); }\n");
        self.raw("/* Decode the UTF-8 codepoint at *k, advancing *k past it (replacement char on a bad lead byte). */\n");
        self.raw("static uint32_t jestyr_rt_decode_cp(const char* p, size_t len, size_t* k) { size_t i = *k; uint8_t b = (uint8_t)p[i]; uint32_t cp; size_t n; if (b < 0x80u) { cp = b; n = 1; } else if ((b & 0xE0u) == 0xC0u) { cp = b & 0x1Fu; n = 2; } else if ((b & 0xF0u) == 0xE0u) { cp = b & 0x0Fu; n = 3; } else if ((b & 0xF8u) == 0xF0u) { cp = b & 0x07u; n = 4; } else { *k = i + 1; return 0xFFFDu; } for (size_t j = 1; j < n && i + j < len; j++) cp = (cp << 6) | ((uint8_t)p[i + j] & 0x3Fu); *k = i + n; return cp; }\n");
        self.raw("/* O(n) grapheme-cluster count: base codepoints (combining marks attach to the preceding base). */\n");
        self.raw("static size_t jestyr_rt_count_graphemes(JestyrStr s) { size_t k = 0, n = 0; while (k < s.len) { uint32_t cp = jestyr_rt_decode_cp(s.ptr, s.len, &k); if (!jestyr_rt_is_combining(cp)) n++; } return n; }\n");
        self.raw("/* Validate a byte range as UTF-8 (used at the bytes->str boundary). */\n");
        self.raw("static bool jestyr_rt_valid_utf8(const char* p, size_t len) { size_t k = 0; while (k < len) { uint8_t b = (uint8_t)p[k]; size_t n; if (b < 0x80u) n = 1; else if ((b & 0xE0u) == 0xC0u) n = 2; else if ((b & 0xF0u) == 0xE0u) n = 3; else if ((b & 0xF8u) == 0xF0u) n = 4; else return false; if (k + n > len) return false; for (size_t j = 1; j < n; j++) if (((uint8_t)p[k + j] & 0xC0u) != 0x80u) return false; k += n; } return true; }\n");
        self.raw("/* A boundary-checked sub-view (Rust discipline): start<=end<=len, and both on UTF-8 char boundaries. Zero-copy. */\n");
        self.raw("static bool jestyr_rt_is_boundary(JestyrStr s, size_t i) { return i == s.len || ((uint8_t)s.ptr[i] & 0xC0u) != 0x80u; }\n");
        self.raw("static JestyrStr jestyr_rt_substr(JestyrStr s, size_t start, size_t end) { assert(start <= end && end <= s.len); assert(jestyr_rt_is_boundary(s, start) && jestyr_rt_is_boundary(s, end)); return (JestyrStr){ s.ptr + start, end - start }; }\n");
        self.raw("/* Byte-level string operations: equality, prefix/suffix, search, trim. All view-based (find/trim are zero-copy). */\n");
        self.raw("static bool jestyr_rt_str_eq(JestyrStr a, JestyrStr b) { return a.len == b.len && memcmp(a.ptr, b.ptr, a.len) == 0; }\n");
        self.raw("static bool jestyr_rt_starts_with(JestyrStr s, JestyrStr p) { return s.len >= p.len && memcmp(s.ptr, p.ptr, p.len) == 0; }\n");
        self.raw("static bool jestyr_rt_ends_with(JestyrStr s, JestyrStr p) { return s.len >= p.len && memcmp(s.ptr + (s.len - p.len), p.ptr, p.len) == 0; }\n");
        self.raw("static int64_t jestyr_rt_find(JestyrStr s, JestyrStr n) { if (n.len == 0) return 0; if (n.len > s.len) return -1; for (size_t i = 0; i + n.len <= s.len; i++) if (memcmp(s.ptr + i, n.ptr, n.len) == 0) return (int64_t)i; return -1; }\n");
        self.raw("static bool jestyr_rt_contains(JestyrStr s, JestyrStr n) { return jestyr_rt_find(s, n) >= 0; }\n");
        self.raw("static JestyrStr jestyr_rt_trim(JestyrStr s) { size_t a = 0, b = s.len; while (a < b) { char c = s.ptr[a]; if (c==' '||c=='\\t'||c=='\\n'||c=='\\r') a++; else break; } while (b > a) { char c = s.ptr[b-1]; if (c==' '||c=='\\t'||c=='\\n'||c=='\\r') b--; else break; } return (JestyrStr){ s.ptr + a, b - a }; }\n");
        self.raw("/* ASCII case-insensitive equality — the opt-in normalization-aware compare. Full Unicode case-folding / NFC normalization needs the Unicode tables (deferred). */\n");
        self.raw("static char jestyr_rt_ascii_lower(char c) { return (c >= 'A' && c <= 'Z') ? (char)(c + 32) : c; }\n");
        self.raw("static bool jestyr_rt_eq_fold(JestyrStr a, JestyrStr b) { if (a.len != b.len) return false; for (size_t i = 0; i < a.len; i++) if (jestyr_rt_ascii_lower(a.ptr[i]) != jestyr_rt_ascii_lower(b.ptr[i])) return false; return true; }\n\n");
        self.raw("/* Jestyr owned String — a heap-owned, growable buffer (the owned half of the\n");
        self.raw("   owned/view split). `string_view` borrows it as a `str` view; no copy. */\n");
        self.raw("typedef struct { char* ptr; size_t len; size_t cap; } JestyrString;\n");
        self.raw("static JestyrString jestyr_rt_str_new(void) { JestyrString s; s.ptr = NULL; s.len = 0; s.cap = 0; return s; }\n");
        self.raw("static JestyrString jestyr_rt_str_from(JestyrStr v) { JestyrString s; s.cap = v.len ? v.len : 1; s.ptr = (char*)malloc(s.cap); memcpy(s.ptr, v.ptr, v.len); s.len = v.len; return s; }\n");
        self.raw("static void jestyr_rt_str_push(JestyrString* s, JestyrStr v) { if (s->len + v.len > s->cap) { size_t nc = s->cap ? s->cap * 2 : 8; while (nc < s->len + v.len) nc *= 2; s->ptr = (char*)realloc(s->ptr, nc); s->cap = nc; } memcpy(s->ptr + s->len, v.ptr, v.len); s->len += v.len; }\n");
        self.raw("static JestyrStr jestyr_rt_str_view(JestyrString* s) { return (JestyrStr){ s->ptr, s->len }; }\n");
        self.raw("static void jestyr_rt_str_free(JestyrString* s) { free(s->ptr); s->ptr = NULL; s->len = 0; s->cap = 0; }\n");
        self.raw("/* Append an integer's decimal digits (for f-string interpolation; copies). */\n");
        self.raw("static void jestyr_rt_str_push_i64(JestyrString* s, int64_t v) { char b[24]; int n = snprintf(b, sizeof(b), \"%lld\", (long long)v); if (n < 0) n = 0; jestyr_rt_str_push(s, (JestyrStr){ b, (size_t)n }); }\n");
        self.raw("/* Lossily decode unvalidated platform text (os_str) into a proven UTF-8 String, replacing each ill-formed byte with U+FFFD. */\n");
        self.raw("/* Cow<str>: borrowed-or-owned, visible. cap==0 means borrowed (no allocation, free is a no-op); cap>0 means owned. cow_to_mut is the copy-on-write point. */\n");
        self.raw("typedef struct { char* ptr; size_t len; size_t cap; } JestyrCow;\n");
        self.raw("static JestyrCow jestyr_rt_cow_borrow(JestyrStr v) { JestyrCow c; c.ptr = (char*)v.ptr; c.len = v.len; c.cap = 0; return c; }\n");
        self.raw("static bool jestyr_rt_cow_is_owned(JestyrCow c) { return c.cap > 0; }\n");
        self.raw("static JestyrStr jestyr_rt_cow_view(JestyrCow c) { return (JestyrStr){ c.ptr, c.len }; }\n");
        self.raw("static JestyrCow jestyr_rt_cow_to_mut(JestyrCow c) { if (c.cap > 0) return c; JestyrCow o; o.cap = c.len ? c.len : 1; o.ptr = (char*)malloc(o.cap); memcpy(o.ptr, c.ptr, c.len); o.len = c.len; return o; }\n");
        self.raw("static void jestyr_rt_cow_free(JestyrCow* c) { if (c->cap > 0) free(c->ptr); c->ptr = NULL; c->len = 0; c->cap = 0; }\n");
        self.raw("static JestyrString jestyr_rt_to_str_lossy(JestyrStr os) { JestyrString out = jestyr_rt_str_new(); size_t k = 0; while (k < os.len) { uint8_t b = (uint8_t)os.ptr[k]; size_t n; bool ok = true; if (b < 0x80u) n = 1; else if ((b & 0xE0u) == 0xC0u) n = 2; else if ((b & 0xF0u) == 0xE0u) n = 3; else if ((b & 0xF8u) == 0xF0u) n = 4; else { ok = false; n = 1; } if (ok && k + n <= os.len) { for (size_t j = 1; j < n; j++) if (((uint8_t)os.ptr[k + j] & 0xC0u) != 0x80u) { ok = false; break; } } else if (n > 1) ok = false; if (ok) { jestyr_rt_str_push(&out, (JestyrStr){ os.ptr + k, n }); k += n; } else { jestyr_rt_str_push(&out, (JestyrStr){ \"\\xEF\\xBF\\xBD\", 3 }); k += 1; } } return out; }\n\n");
        self.raw("/* Jestyr Builder — an iolist: a list of `str` *fragments* collected with no\n");
        self.raw("   copying; `builder_build` sums the lengths, allocates once, and flattens in a\n");
        self.raw("   single pass (Erlang iodata). Fragments must outlive the build. */\n");
        self.raw("typedef struct { JestyrStr* frags; size_t n; size_t cap; } JestyrBuilder;\n");
        self.raw("static JestyrBuilder jestyr_rt_b_new(void) { JestyrBuilder b; b.frags = NULL; b.n = 0; b.cap = 0; return b; }\n");
        self.raw("static void jestyr_rt_b_push(JestyrBuilder* b, JestyrStr v) { if (b->n == b->cap) { size_t nc = b->cap ? b->cap * 2 : 8; b->frags = (JestyrStr*)realloc(b->frags, nc * sizeof(JestyrStr)); b->cap = nc; } b->frags[b->n++] = v; }\n");
        self.raw("static JestyrString jestyr_rt_b_build(JestyrBuilder* b) { size_t total = 0; for (size_t i = 0; i < b->n; i++) total += b->frags[i].len; JestyrString s; s.cap = total ? total : 1; s.ptr = (char*)malloc(s.cap); s.len = total; size_t off = 0; for (size_t i = 0; i < b->n; i++) { memcpy(s.ptr + off, b->frags[i].ptr, b->frags[i].len); off += b->frags[i].len; } return s; }\n");
        self.raw("static void jestyr_rt_b_free(JestyrBuilder* b) { free(b->frags); b->frags = NULL; b->n = 0; b->cap = 0; }\n\n");
        self.raw("/* Jestyr runtime prelude — temporary print intrinsics (stand-in for a stdlib). */\n");
        self.raw("static void jestyr_rt_print_int(int64_t x) { printf(\"%lld\\n\", (long long) x); }\n");
        self.raw("static void jestyr_rt_print_float(double x) { printf(\"%g\\n\", x); }\n");
        self.raw("static void jestyr_rt_print_str(JestyrStr s) { printf(\"%.*s\\n\", (int) s.len, s.ptr); }\n");
        self.raw("static void jestyr_rt_print_bool(bool b) { printf(\"%s\\n\", b ? \"true\" : \"false\"); }\n\n");
        if self.uses_eprint {
            self.raw("/* Stderr line print (driver diagnostics) — same shape as print_str, other stream. */\n");
            self.raw("static void jestyr_rt_eprint_str(JestyrStr s) { fprintf(stderr, \"%.*s\\n\", (int) s.len, s.ptr); }\n\n");
        }
        self.raw("/* Jestyr file I/O (self-hosting plumbing): whole-file read/write. A `str` path\n");
        self.raw("   is a {ptr,len} view (not NUL-terminated), so each call copies it into a\n");
        self.raw("   NUL-terminated temporary for libc. Binary mode + whole-file-at-once = no\n");
        self.raw("   buffering or newline-translation surprises (deterministic across platforms). */\n");
        self.raw("static char* jestyr_rt_cpath(JestyrStr p) { char* c = (char*)malloc(p.len + 1); memcpy(c, p.ptr, p.len); c[p.len] = '\\0'; return c; }\n");
        self.raw("static JestyrString jestyr_rt_read_file(JestyrStr path) { char* cp = jestyr_rt_cpath(path); FILE* f = fopen(cp, \"rb\"); free(cp); JestyrString s = jestyr_rt_str_new(); if (!f) return s; if (fseek(f, 0, SEEK_END) != 0) { fclose(f); return s; } long sz = ftell(f); if (sz < 0) { fclose(f); return s; } rewind(f); size_t cap = (size_t)sz ? (size_t)sz : 1; s.ptr = (char*)malloc(cap); s.cap = cap; s.len = fread(s.ptr, 1, (size_t)sz, f); fclose(f); return s; }\n");
        self.raw("static bool jestyr_rt_write_file(JestyrStr path, JestyrStr data) { char* cp = jestyr_rt_cpath(path); FILE* f = fopen(cp, \"wb\"); free(cp); if (!f) return false; size_t put = data.len ? fwrite(data.ptr, 1, data.len, f) : 0; int rc = fclose(f); return rc == 0 && put == data.len; }\n");
        self.raw("static bool jestyr_rt_file_exists(JestyrStr path) { char* cp = jestyr_rt_cpath(path); FILE* f = fopen(cp, \"rb\"); free(cp); if (f) { fclose(f); return true; } return false; }\n");
        self.raw("static bool jestyr_rt_remove_file(JestyrStr path) { char* cp = jestyr_rt_cpath(path); int rc = remove(cp); free(cp); return rc == 0; }\n");
        // Recoverable read (B3): reports open/read failure via its `bool` return and
        // writes the whole file into `*out`. Emitted only when `try_read_file` is
        // used, so a program that doesn't use it is byte-identical. Uses only the
        // always-present `jestyr_rt_cpath`/`jestyr_rt_str_new`, so it needs no
        // forward reference to `JestyrResult_String` (that lives in `result_defs`).
        if self.uses_try_read {
            self.raw("/* Recoverable whole-file read: false on open/seek failure (the `String !IoError` err branch). */\n");
            self.raw("static bool jestyr_rt_try_read_file(JestyrStr path, JestyrString* out) { *out = jestyr_rt_str_new(); char* cp = jestyr_rt_cpath(path); FILE* f = fopen(cp, \"rb\"); free(cp); if (!f) return false; if (fseek(f, 0, SEEK_END) != 0) { fclose(f); return false; } long sz = ftell(f); if (sz < 0) { fclose(f); return false; } rewind(f); size_t cap = (size_t)sz ? (size_t)sz : 1; out->ptr = (char*)malloc(cap); out->cap = cap; out->len = fread(out->ptr, 1, (size_t)sz, f); fclose(f); return true; }\n");
        }
        // Driving an external command (the self-hosted driver's gcc invocation).
        // Emitted only on use; reuses the NUL-terminating `jestyr_rt_cpath`.
        if self.uses_run_command {
            self.raw("/* Run an external command via system(): the self-hosted driver's compile step. */\n");
            self.raw("static int32_t jestyr_rt_run_command(JestyrStr cmd) { char* cp = jestyr_rt_cpath(cmd); int rc = system(cp); free(cp); return (int32_t)rc; }\n");
        }
        self.raw("\n");
        self.raw("/* Command-line arguments (self-hosting plumbing): argv is captured in main()\n");
        self.raw("   into file-scope globals, exposed to Jestyr as arg_count() -> i32 and\n");
        self.raw("   arg(i) -> str (a zero-copy view into argv[i]; out-of-range yields empty).\n");
        self.raw("   arg(0) is the program path. argv strings are NUL-terminated, OS-owned. */\n");
        self.raw("static int jestyr_rt_argc = 0;\n");
        self.raw("static char** jestyr_rt_argv = NULL;\n");
        self.raw("static int64_t jestyr_rt_arg_count(void) { return (int64_t)jestyr_rt_argc; }\n");
        // Length by manual scan, not strlen — the generated C keeps its zero-strlen
        // invariant (str is length-carrying; strlen was deliberately retired).
        self.raw("static JestyrStr jestyr_rt_arg(int64_t i) { if (i < 0 || i >= (int64_t)jestyr_rt_argc) return (JestyrStr){ \"\", 0 }; const char* a = jestyr_rt_argv[i]; size_t n = 0; while (a[n]) n++; return (JestyrStr){ a, n }; }\n\n");
        if self.uses_arena() {
            self.raw("/* Jestyr bump arena — backs region refs (`&[r]T`) and the std arena allocator. */\n");
            self.raw("typedef struct { char* buf; size_t off; size_t cap; } JestyrArena;\n");
            self.raw("static JestyrArena jestyr_arena_new(size_t cap) { JestyrArena a; a.buf = (char*)malloc(cap); a.off = 0; a.cap = cap; return a; }\n");
            self.raw("static void* jestyr_arena_alloc(JestyrArena* a, size_t n) { size_t al = (n + 7u) & ~(size_t)7u; void* p = a->buf + a->off; a->off += al; return p; }\n");
            self.raw("static void jestyr_arena_free(JestyrArena* a) { free(a->buf); }\n\n");
        }
    }

    /// Does the program use the bump-arena runtime — either a `region` block or
    /// one of the value-level arena intrinsics (`arena_open`/`_alloc`/`_close`)
    /// that back the std arena allocator?
    fn uses_arena(&self) -> bool {
        self.ast.exprs.iter().any(|e| match &e.kind {
            ExprKind::Region { .. } => true,
            ExprKind::For { region: Some(_), .. } => true, // region-scoped scratch loop
            ExprKind::Call { callee, .. } => matches!(
                &self.ast.expr_at(*callee).kind,
                ExprKind::Name(n) if matches!(n.name.as_str(),
                    "arena_open" | "arena_alloc" | "arena_close")
            ),
            _ => false,
        })
    }

    // --- aggregate-definition capture + topological flush (see `def_cap`) ---

    /// Redirect output into a capture buffer so the aggregate-definition phases
    /// register each definition as a segment; [`Self::flush_def_capture`] then emits
    /// them in dependency order.
    fn begin_def_capture(&mut self) {
        let real = std::mem::take(&mut self.out);
        self.def_cap = Some(DefCapture { real, ..Default::default() });
    }

    /// Open a definition segment named `cname` that embeds `deps` by value. Text
    /// written until [`Self::def_end`] is that definition's body; any text written
    /// since the previous segment becomes an anonymous glue segment held in place.
    fn def_begin(&mut self, cname: String, deps: Vec<String>) {
        let start = self.out.len();
        if let Some(cap) = &mut self.def_cap {
            if start > cap.last {
                cap.segs.push(DefSeg { cname: None, deps: Vec::new(), start: cap.last, end: start });
                cap.last = start;
            }
            cap.pending = Some((cname, deps, start));
        }
    }

    /// Close the current definition segment.
    fn def_end(&mut self) {
        let end = self.out.len();
        if let Some(cap) = &mut self.def_cap {
            if let Some((cname, deps, start)) = cap.pending.take() {
                cap.segs.push(DefSeg { cname: Some(cname), deps, start, end });
                cap.last = end;
            }
        }
    }

    /// The C type name a *by-value* field of C type `c` depends on being complete —
    /// `None` for a pointer (a forward declaration suffices) or a type that names no
    /// aggregate unit (a primitive never matches a unit, so it is a harmless no-op).
    fn dep_of_cty(c: String) -> Option<String> {
        if c.contains('*') {
            None
        } else {
            Some(c)
        }
    }

    /// Push `dep` (if any) onto `deps`, de-duplicated.
    fn add_dep(deps: &mut Vec<String>, dep: Option<String>) {
        if let Some(d) = dep {
            if !deps.contains(&d) {
                deps.push(d);
            }
        }
    }

    /// The by-value aggregate dependencies of a struct body's fields, spelled with the
    /// same `c_ty_ast` the definition uses (so a `List(E)` field yields exactly the
    /// `Jestyr_List__E` unit name). `cur_mod` must already be set for the owning item.
    fn aggregate_field_deps_ast(&mut self, body: &StructBody) -> Vec<String> {
        let mut deps = Vec::new();
        for m in &body.members {
            if let StructMember::Field { ty, .. } = m {
                let c = self.c_ty_ast(*ty);
                Self::add_dep(&mut deps, Self::dep_of_cty(c));
            }
        }
        deps
    }

    /// End the capture window and emit the collected definitions in a topological
    /// order (each definition after the aggregates it embeds by value). A stable
    /// post-order DFS in segment order: a program with no forward by-value
    /// dependency emits in the original order (byte-identical).
    fn flush_def_capture(&mut self) {
        let Some(cap) = self.def_cap.take() else { return };
        let mut segs = cap.segs;
        // Any trailing text after the last segment is glue.
        let buflen = self.out.len();
        if buflen > cap.last {
            segs.push(DefSeg { cname: None, deps: Vec::new(), start: cap.last, end: buflen });
        }
        // Swap the captured definitions out and restore the real output buffer.
        let buf = std::mem::replace(&mut self.out, cap.real);

        // Map each named definition to its segment index for dependency resolution.
        let mut by_name: HashMap<&str, usize> = HashMap::new();
        for (i, s) in segs.iter().enumerate() {
            if let Some(n) = &s.cname {
                by_name.insert(n.as_str(), i);
            }
        }
        // Iterative post-order DFS (deps before dependents), stable in segment order.
        let n = segs.len();
        let mut state = vec![0u8; n]; // 0 = unvisited, 1 = on stack, 2 = done
        let mut order: Vec<usize> = Vec::with_capacity(n);
        for root in 0..n {
            if state[root] != 0 {
                continue;
            }
            state[root] = 1;
            let mut stack: Vec<(usize, usize)> = vec![(root, 0)];
            while let Some(&(node, di)) = stack.last() {
                let deps = &segs[node].deps;
                if di < deps.len() {
                    stack.last_mut().unwrap().1 += 1; // advance to the next dep
                    if let Some(&j) = by_name.get(deps[di].as_str()) {
                        if state[j] == 0 {
                            state[j] = 1;
                            stack.push((j, 0));
                        }
                    }
                } else {
                    order.push(node);
                    state[node] = 2;
                    stack.pop();
                }
            }
        }
        for i in order {
            let s = &segs[i];
            self.out.push_str(&buf[s.start..s.end]);
        }
    }

    fn forward_types(&mut self) {
        let ast = self.ast;
        for (i, item) in ast.items.iter().enumerate() {
            self.cur_mod = self.item_module(i);
            match item {
                Item::Struct { name, is_union, .. } => {
                    let kw = if *is_union { "union" } else { "struct" };
                    let c = self.canon_type(&name.name);
                    self.raw(format!("typedef {kw} Jestyr_{c} Jestyr_{c};\n"));
                }
                // `distinct UserId = u64` → a zero-cost C typedef of the base.
                Item::Distinct(dd) => {
                    let base = self.c_ty_ast(dd.base);
                    let c = self.canon_type(&dd.name.name);
                    self.raw(format!("typedef {base} Jestyr_{c};\n"));
                }
                Item::Enum(e) => {
                    // Generic-enum templates and niche-optimized enums have no
                    // `Jestyr_<E>` struct, so they need no forward typedef.
                    if e.is_generic() {
                        continue;
                    }
                    if self
                        .info
                        .table
                        .type_index
                        .get(&self.canon_type(&e.name.name))
                        .is_some_and(|&i| self.niche_enum_at(i).is_some())
                    {
                        continue;
                    }
                    let c = self.canon_type(&e.name.name);
                    self.raw(format!("typedef struct Jestyr_{c} Jestyr_{c};\n"));
                }
                _ => {}
            }
        }
        self.raw("\n");
    }

    /// If the type at table index `i` is a niche-optimizable enum, describe it.
    /// Qualifies when it has exactly two variants — one nullary, one with a single
    /// *thin-pointer* field (`*T` or `&[r]T`; a fat `&T`/`[]T` has no null niche).
    fn niche_enum_at(&self, i: usize) -> Option<NicheInfo> {
        let decl = self.info.table.types.get(i)?;
        let TypeKindG::Enum { variants } = &decl.kind else { return None };
        if variants.len() != 2 {
            return None;
        }
        let mut none_variant = None;
        let mut some = None;
        for (vname, fields) in variants {
            match fields.as_slice() {
                [] => none_variant = Some(vname.clone()),
                [t] if is_niche_pointer(t) => some = Some((vname.clone(), t.clone())),
                _ => return None, // a non-niche-able variant disqualifies the enum
            }
        }
        let none_variant = none_variant?;
        let (some_variant, payload) = some?;
        Some(NicheInfo { none_variant, some_variant, payload })
    }

    /// Niche info for an enum by name (used at construction sites).
    fn niche_enum_named(&self, name: &str) -> Option<NicheInfo> {
        let i = *self.info.table.type_index.get(name)?;
        self.niche_enum_at(i)
    }

    /// Niche info for a *generic enum instance* `ctor(args)`: substitute the type
    /// arguments into the variant templates, then apply the same niche rule. So
    /// The generic-enum declaration whose *canonical* name is `ctor` (so a
    /// collided generic enum is found by its disambiguated key — bare otherwise).
    fn find_generic_enum(&self, ctor: &str) -> Option<&'a EnumDecl> {
        self.ast.items.iter().enumerate().find_map(|(i, it)| match it {
            Item::Enum(e)
                if e.is_generic() && self.canon_type_in(self.item_module(i), &e.name.name) == ctor =>
            {
                Some(e)
            }
            _ => None,
        })
    }

    /// `Option(*T)`/`Option(&[r]T)` inherit the niche optimization automatically.
    fn niche_enum_instance(&self, ctor: &str, args: &[Ty]) -> Option<NicheInfo> {
        let e = self.find_generic_enum(ctor)?;
        if e.variants.len() != 2 {
            return None;
        }
        let subst: HashMap<String, Ty> = e
            .type_params
            .iter()
            .map(|p| p.name.clone())
            .zip(args.iter().cloned())
            .collect();
        let mut none_variant = None;
        let mut some = None;
        for v in &e.variants {
            match v.fields.as_slice() {
                [] => none_variant = Some(v.name.name.clone()),
                [(_, tid)] => {
                    let ty = self.ast_type_to_ty(*tid, &subst);
                    if is_niche_pointer(&ty) {
                        some = Some((v.name.name.clone(), ty));
                    } else {
                        return None;
                    }
                }
                _ => return None,
            }
        }
        let (some_variant, payload) = some?;
        Some(NicheInfo { none_variant: none_variant?, some_variant, payload })
    }

    /// Does this type lower to a tagged-union enum (has a `.tag` field)? True for a
    /// plain or generic enum that is *not* niche-optimized (a niche enum is a bare
    /// pointer with no tag). Used to read an enum's discriminant via `e as int`.
    fn is_tagged_enum(&self, t: &Ty) -> bool {
        match t {
            Ty::Named(i) => {
                matches!(self.info.table.types[*i].kind, TypeKindG::Enum { .. })
                    && self.niche_enum_at(*i).is_none()
            }
            Ty::GenEnum { ctor, args } => self.niche_enum_instance(ctor, args).is_none(),
            _ => false,
        }
    }

    /// The type-param → arg substitution for a generic enum instance `ctor(args)`.
    fn gen_enum_subst(&self, ctor: &str, args: &[Ty]) -> HashMap<String, Ty> {
        self.find_generic_enum(ctor)
            .map(|e| {
                e.type_params
                    .iter()
                    .map(|p| p.name.clone())
                    .zip(args.iter().cloned())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Lower each enum to a tagged union: a `tag` enum plus a `union` of the
    /// payload-carrying variants. Nullary variants contribute a tag constant but
    /// no union member. A niche-optimized enum is skipped (it has no struct).
    fn enum_defs(&mut self) {
        let ast = self.ast;
        for (i, item) in ast.items.iter().enumerate() {
            self.cur_mod = self.item_module(i);
            if let Item::Enum(e) = item {
                // A generic enum is a *template* (monomorphized per instantiation,
                // like a generic struct/fn) — never emitted directly. (Codegen of
                // the instances is the next sub-step; see design §2.2b.)
                if e.is_generic() {
                    continue;
                }
                // A niche-optimized enum has no tag/union struct — it *is* its
                // pointer payload, so emit nothing here (see `c_type`/`c_ty_ast`).
                if self
                    .info
                    .table
                    .type_index
                    .get(&self.canon_type(&e.name.name))
                    .is_some_and(|&i| self.niche_enum_at(i).is_some())
                {
                    continue;
                }
                let en = self.canon_type(&e.name.name);
                let mut deps = Vec::new();
                for v in &e.variants {
                    for (_, fty) in &v.fields {
                        let c = self.c_ty_ast(*fty);
                        Self::add_dep(&mut deps, Self::dep_of_cty(c));
                    }
                }
                self.def_begin(format!("Jestyr_{en}"), deps);
                self.raw(format!("enum Jestyr_{en}_tag {{\n"));
                for v in &e.variants {
                    // An explicit discriminant sets the tag's integer value.
                    match v.discriminant {
                        Some(d) => {
                            let val = self.emit_expr(d);
                            self.raw(format!("    Jestyr_{en}_{} = {val},\n", v.name.name));
                        }
                        None => self.raw(format!("    Jestyr_{en}_{},\n", v.name.name)),
                    }
                }
                self.raw("};\n");

                self.raw(format!("struct Jestyr_{en} {{\n"));
                self.raw(format!("    enum Jestyr_{en}_tag tag;\n"));
                if e.variants.iter().any(|v| !v.fields.is_empty()) {
                    self.raw("    union {\n");
                    for v in &e.variants {
                        if v.fields.is_empty() {
                            continue;
                        }
                        self.raw("        struct { ");
                        for (fname, fty) in &v.fields {
                            let fcty = self.c_ty_ast(*fty);
                            self.raw(format!("{fcty} j_{}; ", fname.name));
                        }
                        self.raw(format!("}} {};\n", v.name.name));
                    }
                    self.raw("    } u;\n");
                }
                self.raw("};\n\n");
                self.def_end();
            }
        }
    }

    /// The C name of the result struct carrying ok-type `ok`.
    fn result_c_name(&self, ok: &Ty) -> String {
        format!("JestyrResult_{}", self.ty_mangle(ok))
    }

    // --- generic structs ---

    fn gen_struct_c_name(&self, ctor: &str, args: &[Ty]) -> String {
        let parts: Vec<String> = args.iter().map(|t| self.ty_mangle(t)).collect();
        format!("Jestyr_{ctor}__{}", parts.join("_"))
    }

    /// The C struct name for a fixed-size array `[N]T` — one `typedef` per distinct
    /// (element, length), holding a C array field so it copies/returns by value.
    fn array_c_name(&self, elem: &Ty, len: usize) -> String {
        format!("JestyrArr_{}_{len}", self.ty_mangle(elem))
    }

    /// Evaluate a `[N]T` length expression to a `usize`. An integer literal is the
    /// common case and is parsed directly (identical result, no interpreter setup);
    /// anything else is folded by the comptime interpreter, which is the same
    /// evaluation typeck performed — so the C type name and Jestyr's own
    /// `Ty::Array { len }` can never disagree. Typeck has already rejected a length
    /// it could not evaluate, so the 0 fallback here is unreachable for a checked
    /// program (it keeps codegen total for the error path).
    fn array_len(&self, id: ExprId) -> usize {
        match &self.ast.expr_at(id).kind {
            ExprKind::Int(text) => {
                let t: String = text.chars().filter(|c| *c != '_').collect();
                if let Some(h) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
                    usize::from_str_radix(h, 16).unwrap_or(0)
                } else if let Some(b) = t.strip_prefix("0b").or_else(|| t.strip_prefix("0B")) {
                    usize::from_str_radix(b, 2).unwrap_or(0)
                } else {
                    t.parse::<usize>().unwrap_or(0)
                }
            }
            _ => crate::comptime::Interp::new(self.ast).eval_usize(id).unwrap_or(0),
        }
    }

    /// Lower an AST type to a `Ty`, applying the given type-parameter substitution.
    fn ast_type_to_ty(&self, id: TypeId, subst: &HashMap<String, Ty>) -> Ty {
        match &self.ast.type_at(id).kind {
            TypeKind::Name(n) => {
                if let Some(t) = subst.get(&n.name) {
                    t.clone()
                } else if let Some(p) = prim_ty(&n.name) {
                    Ty::Prim(p)
                } else if let Some(&i) = self.info.table.type_index.get(&self.canon_type(&n.name)) {
                    Ty::Named(i)
                } else {
                    Ty::Opaque(n.name.clone())
                }
            }
            TypeKind::Ptr { mutbl, inner } => {
                Ty::Ptr { mutbl: *mutbl, inner: Box::new(self.ast_type_to_ty(*inner, subst)) }
            }
            TypeKind::App { ctor, args } => {
                let aty: Vec<Ty> = args.iter().map(|a| self.ast_type_to_ty(*a, subst)).collect();
                let key = self.canon_type(&ctor.name);
                if self.enum_is_generic(&key) {
                    Ty::GenEnum { ctor: key, args: aty }
                } else {
                    Ty::GenStruct { ctor: ctor.name.clone(), args: aty }
                }
            }
            TypeKind::Slice(inner) => Ty::Slice(Box::new(self.ast_type_to_ty(*inner, subst))),
            TypeKind::Array { len, elem } => Ty::Array {
                elem: Box::new(self.ast_type_to_ty(*elem, subst)),
                len: self.array_len(*len),
            },
            TypeKind::GenRef(inner) => Ty::GenRef(Box::new(self.ast_type_to_ty(*inner, subst))),
            TypeKind::RegionRef { inner, .. } => {
                Ty::RegionRef(Box::new(self.ast_type_to_ty(*inner, subst)))
            }
            TypeKind::Fn { params, ret_conv, ret } => {
                let ps: Vec<(Conv, Box<Ty>)> = params
                    .iter()
                    .map(|p| (p.conv, Box::new(self.ast_type_to_ty(p.ty, subst))))
                    .collect();
                let r = match ret {
                    Some(t) => self.ast_type_to_ty(*t, subst),
                    None => Ty::Unit,
                };
                Ty::Fn { params: ps, ret: Box::new(r), ret_conv: *ret_conv }
            }
            TypeKind::Dyn(n) => Ty::Opaque(format!("dyn {}", n.name)),
            // A module-qualified type `mod.Type`, resolved in the *target* module
            // (via the import map) so it picks that module's type even when the name
            // collides across modules.
            TypeKind::Path { module, name, args } => {
                if args.is_empty() {
                    if let Some(t) = subst.get(&name.name) {
                        t.clone()
                    } else if let Some(p) = prim_ty(&name.name) {
                        Ty::Prim(p)
                    } else {
                        let key = match self.path_target(&module.name) {
                            Some(t) => self.canon_type_in(t, &name.name),
                            None => self.canon_type(&name.name),
                        };
                        match self.info.table.type_index.get(&key) {
                            Some(&i) => Ty::Named(i),
                            None => Ty::Opaque(name.name.clone()),
                        }
                    }
                } else {
                    let aty: Vec<Ty> = args.iter().map(|a| self.ast_type_to_ty(*a, subst)).collect();
                    let key = match self.path_target(&module.name) {
                        Some(t) => self.canon_type_in(t, &name.name),
                        None => self.canon_type(&name.name),
                    };
                    if self.enum_is_generic(&key) {
                        Ty::GenEnum { ctor: key, args: aty }
                    } else {
                        Ty::GenStruct { ctor: name.name.clone(), args: aty }
                    }
                }
            }
            TypeKind::TypeKw => Ty::TypeKw,
            TypeKind::Error => Ty::Error,
        }
    }

    /// The `struct { … }` body a generic-struct constructor returns.
    fn ctor_struct_body(&self, f: &FnDecl) -> Option<&'a StructBody> {
        let ast = self.ast;
        for stmt in &f.body.stmts {
            let e = match stmt {
                Stmt::Return { value: Some(e), .. } => *e,
                Stmt::Expr(e) => *e,
                _ => continue,
            };
            if let ExprKind::StructType(b) = &ast.expr_at(e).kind {
                return Some(b);
            }
        }
        None
    }

    /// Walk a `Ty`, recording every applied generic struct it mentions.
    fn collect_gen_struct(&self, t: &Ty, seen: &mut HashSet<String>, order: &mut Vec<(String, Vec<Ty>)>) {
        match t {
            Ty::GenStruct { ctor, args } => {
                for a in args {
                    self.collect_gen_struct(a, seen, order);
                }
                let cname = self.gen_struct_c_name(ctor, args);
                if seen.insert(cname) {
                    order.push((ctor.clone(), args.clone()));
                }
            }
            Ty::Ptr { inner, .. } => self.collect_gen_struct(inner, seen, order),
            Ty::Result(ok) => self.collect_gen_struct(ok, seen, order),
            _ => {}
        }
    }

    fn collect_struct_instances(&self) -> Vec<(String, Vec<Ty>)> {
        let ast = self.ast;
        let mut seen = HashSet::new();
        let mut order = Vec::new();
        let empty = HashMap::new();
        for item in &ast.items {
            if let Item::Fn(f) = item {
                if !self.is_generic(f) {
                    self.collect_structs_in_fn(f, &empty, &mut seen, &mut order);
                }
            }
            // A generic struct mentioned inside a trait-`impl` method body needs
            // its concrete instance emitted, just as a free function's would.
            if let Item::Impl(im) = item {
                for f in &im.methods {
                    self.collect_structs_in_fn(f, &empty, &mut seen, &mut order);
                }
            }
        }
        for (name, args) in self.instances.clone() {
            if let Some(f) = self.find_fn(&name) {
                let subst = self.make_subst(f, &args);
                self.collect_structs_in_fn(f, &subst, &mut seen, &mut order);
            }
        }
        // Each method instance needs its receiver struct emitted, plus whatever
        // generic structs its body mentions under the struct's substitution.
        for (ctor, args, method) in self.method_instances.clone() {
            if !args.is_empty() {
                let recv = Ty::GenStruct { ctor: ctor.clone(), args: args.clone() };
                self.collect_gen_struct(&recv, &mut seen, &mut order);
            }
            if let Some(mf) = self.find_struct_method_cg(&ctor, &method) {
                let subst = self.method_subst(&ctor, &args);
                self.collect_structs_in_fn(mf, &subst, &mut seen, &mut order);
            }
        }
        // A *collided* generic enum referenced here is lowered without per-item
        // module context (this collection runs before `cur_mod` is set), so it can be
        // misclassified as a generic struct under its bare name. Its real,
        // module-canon instances are gathered by `collect_enum_instances`; drop any
        // "struct" instance whose name is in fact a generic enum.
        order.retain(|(ctor, _)| !self.is_generic_enum_anywhere(ctor));
        order
    }

    /// Does any module define a generic enum named `bare` (by its source name)?
    /// Used to filter a collided generic enum mis-collected as a generic struct.
    fn is_generic_enum_anywhere(&self, bare: &str) -> bool {
        self.ast
            .items
            .iter()
            .any(|it| matches!(it, Item::Enum(e) if e.is_generic() && e.name.name == bare))
    }

    fn collect_structs_in_fn(
        &self,
        f: &FnDecl,
        subst: &HashMap<String, Ty>,
        seen: &mut HashSet<String>,
        order: &mut Vec<(String, Vec<Ty>)>,
    ) {
        for p in &f.params {
            if let Some(t) = p.ty {
                let ty = self.ast_type_to_ty(t, subst);
                self.collect_gen_struct(&ty, seen, order);
            }
        }
        if let Some(t) = f.ret_ty {
            let ty = self.ast_type_to_ty(t, subst);
            self.collect_gen_struct(&ty, seen, order);
        }
        self.collect_structs_in_block(&f.body, subst, seen, order);
    }

    fn collect_structs_in_block(
        &self,
        b: &Block,
        subst: &HashMap<String, Ty>,
        seen: &mut HashSet<String>,
        order: &mut Vec<(String, Vec<Ty>)>,
    ) {
        for s in &b.stmts {
            match s {
                Stmt::Let { ty: Some(t), init, .. } => {
                    let ty = self.ast_type_to_ty(*t, subst);
                    self.collect_gen_struct(&ty, seen, order);
                    if let Some(e) = init {
                        self.collect_structs_in_expr(*e, subst, seen, order);
                    }
                }
                Stmt::Let { init: Some(e), .. } => self.collect_structs_in_expr(*e, subst, seen, order),
                Stmt::Return { value: Some(v), .. } => self.collect_structs_in_expr(*v, subst, seen, order),
                Stmt::Expr(e) => self.collect_structs_in_expr(*e, subst, seen, order),
                _ => {}
            }
        }
    }

    fn collect_structs_in_expr(
        &self,
        id: ExprId,
        subst: &HashMap<String, Ty>,
        seen: &mut HashSet<String>,
        order: &mut Vec<(String, Vec<Ty>)>,
    ) {
        let ast = self.ast;
        match &ast.expr_at(id).kind {
            ExprKind::GenStructLit { ctor, type_args, fields } => {
                let args: Vec<Ty> = type_args.iter().map(|a| self.eval_type_arg(*a, subst)).collect();
                let ty = Ty::GenStruct { ctor: ctor.name.clone(), args };
                self.collect_gen_struct(&ty, seen, order);
                for fi in fields {
                    self.collect_structs_in_expr(fi.value, subst, seen, order);
                }
            }
            ExprKind::Call { callee, args } => {
                self.collect_structs_in_expr(*callee, subst, seen, order);
                for a in args {
                    self.collect_structs_in_expr(*a, subst, seen, order);
                }
            }
            ExprKind::Binary { lhs, rhs, .. } => {
                self.collect_structs_in_expr(*lhs, subst, seen, order);
                self.collect_structs_in_expr(*rhs, subst, seen, order);
            }
            ExprKind::Unary { rhs, .. } => self.collect_structs_in_expr(*rhs, subst, seen, order),
            ExprKind::Assign { target, value, .. } => {
                self.collect_structs_in_expr(*target, subst, seen, order);
                self.collect_structs_in_expr(*value, subst, seen, order);
            }
            ExprKind::Field { base, .. } => self.collect_structs_in_expr(*base, subst, seen, order),
            ExprKind::Index { base, index } => {
                self.collect_structs_in_expr(*base, subst, seen, order);
                self.collect_structs_in_expr(*index, subst, seen, order);
            }
            ExprKind::Deref { base } => self.collect_structs_in_expr(*base, subst, seen, order),
            ExprKind::Cast { expr, ty } => {
                let t = self.ast_type_to_ty(*ty, subst);
                self.collect_gen_struct(&t, seen, order);
                self.collect_structs_in_expr(*expr, subst, seen, order);
            }
            ExprKind::Try { base } => self.collect_structs_in_expr(*base, subst, seen, order),
            // Both children, or a struct used *only* as a fallback gets no typedef.
            ExprKind::Catch { base, fallback, .. } => {
                self.collect_structs_in_expr(*base, subst, seen, order);
                self.collect_structs_in_expr(*fallback, subst, seen, order);
            }
            ExprKind::StructLit { fields, spread, .. } => {
                for fi in fields {
                    self.collect_structs_in_expr(fi.value, subst, seen, order);
                }
                if let Some(s) = spread {
                    self.collect_structs_in_expr(*s, subst, seen, order);
                }
            }
            ExprKind::If { cond, then, els } => {
                self.collect_structs_in_expr(*cond, subst, seen, order);
                self.collect_structs_in_block(then, subst, seen, order);
                if let Some(e) = els {
                    self.collect_structs_in_expr(*e, subst, seen, order);
                }
            }
            ExprKind::Match { scrut, arms } => {
                self.collect_structs_in_expr(*scrut, subst, seen, order);
                for a in arms {
                    if let Some(g) = a.guard {
                        self.collect_structs_in_expr(g, subst, seen, order);
                    }
                    self.collect_structs_in_expr(a.body, subst, seen, order);
                }
            }
            ExprKind::Block(b) | ExprKind::Unsafe(b) => self.collect_structs_in_block(b, subst, seen, order),
            ExprKind::Closure { body, .. } => self.collect_structs_in_expr(*body, subst, seen, order),
            ExprKind::For { head, body, els, .. } => {
                match head {
                    ForHead::While(c) => self.collect_structs_in_expr(*c, subst, seen, order),
                    ForHead::Iter { sources, .. } => {
                        for s in sources {
                            self.collect_structs_in_expr(*s, subst, seen, order);
                        }
                    }
                    ForHead::Infinite => {}
                }
                self.collect_structs_in_block(body, subst, seen, order);
                if let Some(els) = els {
                    self.collect_structs_in_block(els, subst, seen, order);
                }
            }
            ExprKind::Invariant(e) | ExprKind::Variant(e) => self.collect_structs_in_expr(*e, subst, seen, order),
            ExprKind::ParFor { iter, reduction, body, .. } => {
                self.collect_structs_in_expr(*iter, subst, seen, order);
                self.collect_structs_in_expr(*reduction, subst, seen, order);
                self.collect_structs_in_expr(*body, subst, seen, order);
            }
            ExprKind::Select(arms) => {
                for arm in arms {
                    self.collect_structs_in_expr(arm.chan, subst, seen, order);
                    self.collect_structs_in_block(&arm.body, subst, seen, order);
                }
            }
            _ => {}
        }
    }

    /// Emit a forward typedef and a definition for each monomorphized struct.
    /// Forward-declare every monomorphized generic struct/enum instance (a niche
    /// enum instance is a bare pointer, so it has no struct). Emitted before the
    /// fn-pointer typedefs so a `fn(T) -> Option(U)` return resolves; the bodies
    /// follow in `gen_struct_defs` / `gen_enum_defs`.
    fn gen_forward_types(&mut self) {
        let mut any = false;
        for (ctor, args) in self.struct_instances.clone() {
            let cname = self.gen_struct_c_name(&ctor, &args);
            self.raw(format!("typedef struct {cname} {cname};\n"));
            any = true;
        }
        for (ctor, args) in self.enum_instances.clone() {
            if self.niche_enum_instance(&ctor, &args).is_some() {
                continue;
            }
            let cname = self.gen_struct_c_name(&ctor, &args);
            self.raw(format!("typedef struct {cname} {cname};\n"));
            any = true;
        }
        if any {
            self.raw("\n");
        }
    }

    fn gen_struct_defs(&mut self) {
        for (ctor, args) in self.struct_instances.clone() {
            self.emit_struct_instance(&ctor, &args);
        }
    }

    fn emit_struct_instance(&mut self, ctor: &str, args: &[Ty]) {
        let Some(f) = self.find_fn(ctor) else { return };
        let Some(body) = self.ctor_struct_body(f) else {
            self.diag(f.name.span, format!("`{ctor}`: generic-struct constructor must `return struct {{ … }}`"));
            return;
        };
        let names = self.type_param_names(f);
        let subst: HashMap<String, Ty> = names.into_iter().zip(args.iter().cloned()).collect();
        let cname = self.gen_struct_c_name(ctor, args);
        let mut deps = Vec::new();
        for m in &body.members {
            if let StructMember::Field { ty, .. } = m {
                let fty = self.ast_type_to_ty(*ty, &subst);
                let c = self.c_type(&fty);
                Self::add_dep(&mut deps, Self::dep_of_cty(c));
            }
        }
        self.def_begin(cname.clone(), deps);
        self.raw(format!("struct {cname} {{\n"));
        for m in &body.members {
            if let StructMember::Field { name, ty, .. } = m {
                let fty = self.ast_type_to_ty(*ty, &subst);
                let fc = self.c_type(&fty);
                self.raw(format!("    {fc} j_{};\n", name.name));
            }
        }
        self.raw("};\n\n");
        self.def_end();
    }

    // --- generic enums (monomorphization) ---

    /// Is this type fully concrete (no unresolved type parameter / inference gap)?
    /// Only concrete instances can be monomorphized into C.
    fn is_concrete(t: &Ty) -> bool {
        match t {
            Ty::Opaque(_) | Ty::Unknown | Ty::Error => false,
            Ty::Ptr { inner, .. }
            | Ty::Slice(inner)
            | Ty::GenRef(inner)
            | Ty::RegionRef(inner)
            | Ty::Result(inner) => Self::is_concrete(inner),
            Ty::GenStruct { args, .. } | Ty::GenEnum { args, .. } => {
                args.iter().all(Self::is_concrete)
            }
            Ty::Array { elem, .. } => Self::is_concrete(elem),
            Ty::Fn { params, ret, .. } => {
                params.iter().all(|(_, t)| Self::is_concrete(t)) && Self::is_concrete(ret)
            }
            _ => true,
        }
    }

    /// Every generic-enum instance the program uses — found by scanning every
    /// expression's inferred type plus all function signatures for `GenEnum`.
    /// (Instances arising only *inside* a generic function body, under a yet-
    /// unapplied substitution, aren't collected here — a documented limitation.)
    fn collect_enum_instances(&self) -> Vec<(String, Vec<Ty>)> {
        let mut seen = HashSet::new();
        let mut order = Vec::new();
        for t in &self.info.expr_types {
            self.collect_gen_enum(t, &mut seen, &mut order);
        }
        for sig in self.info.table.fns.values() {
            for p in &sig.params {
                self.collect_gen_enum(&p.ty, &mut seen, &mut order);
            }
            self.collect_gen_enum(&sig.ret, &mut seen, &mut order);
        }
        order
    }

    fn collect_gen_enum(
        &self,
        t: &Ty,
        seen: &mut HashSet<String>,
        order: &mut Vec<(String, Vec<Ty>)>,
    ) {
        match t {
            Ty::GenEnum { ctor, args } => {
                for a in args {
                    self.collect_gen_enum(a, seen, order);
                }
                if args.iter().all(Self::is_concrete) {
                    let cname = self.gen_struct_c_name(ctor, args);
                    if seen.insert(cname) {
                        order.push((ctor.clone(), args.clone()));
                    }
                }
            }
            Ty::GenStruct { args, .. } => {
                for a in args {
                    self.collect_gen_enum(a, seen, order);
                }
            }
            Ty::Ptr { inner, .. }
            | Ty::Slice(inner)
            | Ty::GenRef(inner)
            | Ty::RegionRef(inner)
            | Ty::Result(inner) => self.collect_gen_enum(inner, seen, order),
            Ty::Fn { params, ret, .. } => {
                for (_, t) in params {
                    self.collect_gen_enum(t, seen, order);
                }
                self.collect_gen_enum(ret, seen, order);
            }
            _ => {}
        }
    }

    fn gen_enum_defs(&mut self) {
        // Forward typedefs are emitted earlier by `gen_forward_types` (before the
        // fn-pointer typedefs); here we emit only the bodies.
        for (ctor, args) in self.enum_instances.clone() {
            self.emit_enum_instance(&ctor, &args);
        }
    }

    /// Emit one monomorphized generic-enum instance as a tagged union (a niche
    /// instance is skipped — it lowers to its bare pointer payload).
    fn emit_enum_instance(&mut self, ctor: &str, args: &[Ty]) {
        if self.niche_enum_instance(ctor, args).is_some() {
            return;
        }
        let Some(e) = self.find_generic_enum(ctor).cloned() else {
            return;
        };
        let subst: HashMap<String, Ty> = e
            .type_params
            .iter()
            .map(|p| p.name.clone())
            .zip(args.iter().cloned())
            .collect();
        let cname = self.gen_struct_c_name(ctor, args);
        let mut deps = Vec::new();
        for v in &e.variants {
            for (_, tid) in &v.fields {
                let fty = self.ast_type_to_ty(*tid, &subst);
                let c = self.c_type(&fty);
                Self::add_dep(&mut deps, Self::dep_of_cty(c));
            }
        }
        self.def_begin(cname.clone(), deps);
        self.raw(format!("enum {cname}_tag {{\n"));
        for v in &e.variants {
            match v.discriminant {
                Some(d) => {
                    let val = self.emit_expr(d);
                    self.raw(format!("    {cname}_{} = {val},\n", v.name.name));
                }
                None => self.raw(format!("    {cname}_{},\n", v.name.name)),
            }
        }
        self.raw("};\n");
        self.raw(format!("struct {cname} {{\n"));
        self.raw(format!("    enum {cname}_tag tag;\n"));
        if e.variants.iter().any(|v| !v.fields.is_empty()) {
            self.raw("    union {\n");
            for v in &e.variants {
                if v.fields.is_empty() {
                    continue;
                }
                self.raw("        struct { ");
                for (fname, tid) in &v.fields {
                    let fty = self.ast_type_to_ty(*tid, &subst);
                    let fcty = self.c_type(&fty);
                    self.raw(format!("{fcty} j_{}; ", fname.name));
                }
                self.raw(format!("}} {};\n", v.name.name));
            }
            self.raw("    } u;\n");
        }
        self.raw("};\n\n");
        self.def_end();
    }

    /// Emit one tagged result struct per distinct ok-type used by a fallible
    /// function: `{ bool is_err; <T> ok; int err; }`.
    fn result_defs(&mut self) {
        let ast = self.ast;
        let mut seen: HashSet<String> = HashSet::new();
        // `try_from_utf8(...) -> str !Utf8Error` is an *intrinsic*, so its result
        // type isn't discovered from a fn signature — emit it up front (and seed
        // `seen` so a user `str !E` function doesn't duplicate the typedef).
        self.def_begin("JestyrResult_str".to_string(), Vec::new());
        self.raw("typedef struct { bool is_err; JestyrStr ok; int err; } JestyrResult_str;\n");
        self.def_end();
        seen.insert("JestyrResult_str".to_string());
        // `try_read_file(...) -> String !IoError` is an intrinsic, so its result type
        // isn't discovered from a fn signature — emit it (only when used, to keep
        // unrelated programs byte-identical) and seed `seen` so a user `String !E`
        // function doesn't duplicate the typedef.
        if self.uses_try_read {
            let rname = self.result_c_name(&Ty::Prim("String"));
            self.def_begin(rname.clone(), Vec::new());
            self.raw(format!("typedef struct {{ bool is_err; JestyrString ok; int err; }} {rname};\n"));
            self.def_end();
            seen.insert(rname);
        }
        for item in &ast.items {
            if let Item::Fn(f) = item {
                if f.errors.is_none() {
                    continue;
                }
                let ok = self.info.table.fns.get(&self.fn_canon(f)).map(|s| s.ret.clone()).unwrap_or(Ty::Unit);
                let cname = self.result_c_name(&ok);
                if !seen.insert(cname.clone()) {
                    continue;
                }
                let deps = if ok != Ty::Unit {
                    Self::dep_of_cty(self.c_type(&ok)).into_iter().collect()
                } else {
                    Vec::new()
                };
                self.def_begin(cname.clone(), deps);
                self.raw(format!("typedef struct {{ bool is_err; "));
                if ok != Ty::Unit {
                    let okc = self.c_type(&ok);
                    self.raw(format!("{okc} ok; "));
                }
                self.raw(format!("int err; }} {cname};\n"));
                self.def_end();
            }
        }
        // Fallible METHODS need their result typedefs too. Walked per *instance*
        // (`method_instances` is collected before this runs), because a generic
        // struct's method has one ok type per instantiation — `Box(i32).get` and
        // `Box(str).get` are two typedefs, exactly as two monomorphized functions
        // would be. The ok type is the declared return lowered through the
        // instance's substitution.
        for (ctor, args, method) in self.method_instances.clone() {
            let Some(f) = self.find_struct_method_cg(&ctor, &method) else { continue };
            if f.errors.is_none() {
                continue;
            }
            let (ret_ty, span_subst) = (f.ret_ty, self.method_subst(&ctor, &args));
            let ok = ret_ty.map(|t| self.ast_type_to_ty(t, &span_subst)).unwrap_or(Ty::Unit);
            self.emit_result_def(&ok, &mut seen);
        }
        self.raw("\n");
    }

    /// Emit one tagged-result typedef for ok type `ok`, deduped through `seen` —
    /// shared by the free-function scan and the method scans above so the three
    /// cannot drift on the struct's shape.
    fn emit_result_def(&mut self, ok: &Ty, seen: &mut HashSet<String>) {
        let cname = self.result_c_name(ok);
        if !seen.insert(cname.clone()) {
            return;
        }
        let deps = if *ok != Ty::Unit {
            Self::dep_of_cty(self.c_type(ok)).into_iter().collect()
        } else {
            Vec::new()
        };
        self.def_begin(cname.clone(), deps);
        self.raw("typedef struct { bool is_err; ".to_string());
        if *ok != Ty::Unit {
            let okc = self.c_type(ok);
            self.raw(format!("{okc} ok; "));
        }
        self.raw(format!("int err; }} {cname};\n"));
        self.def_end();
    }

    fn struct_defs(&mut self) {
        let ast = self.ast;
        for (i, item) in ast.items.iter().enumerate() {
            self.cur_mod = self.item_module(i);
            if let Item::Struct { name, body, attrs, is_union, .. } = item {
                let attr = self.struct_attr(attrs);
                let kw = if *is_union { "union" } else { "struct" };
                let c = self.canon_type(&name.name);
                let deps = self.aggregate_field_deps_ast(body);
                self.def_begin(format!("Jestyr_{c}"), deps);
                self.raw(format!("{kw}{attr} Jestyr_{c} {{\n"));
                // `@layout(auto)` reorders the *declaration*; everything else — every
                // read, write and construction — is by name, so this is the only place
                // in the backend the choice is visible. See `struct_field_order`.
                for m in self.struct_field_order(&name.name, body, attrs) {
                    if let StructMember::Field { name: fname, ty, volatile, bits, .. } = m {
                        let cty = self.c_ty_ast(*ty);
                        let vol = if *volatile { "volatile " } else { "" };
                        // A bit-field width lowers to C `: N` (e.g. `uint8_t j_flags : 3`).
                        let bf = match bits {
                            Some(n) => format!(" : {n}"),
                            None => String::new(),
                        };
                        self.raw(format!("    {vol}{cty} j_{}{bf};\n", fname.name));
                    }
                }
                self.raw("};\n\n");
                self.def_end();
            }
        }
    }

    /// The members of a struct in **emission order**.
    ///
    /// Declaration order for everything, except a struct that asked for
    /// `@layout(auto)`, whose *fields* are permuted by `layout::field_order` — one
    /// function, shared with the `jestyrc layout` report, so the report can never
    /// describe an order the backend does not emit (the `at_ty` / `simd::classify`
    /// rule).
    ///
    /// ## Why reordering the declaration is safe here and nowhere else
    /// cgen constructs a struct with **designated initializers** (`(Jestyr_P){ .j_x =
    /// 1, .j_y = 2 }`) and reads it by name (`p.j_x`). Both are order-independent in C,
    /// so permuting the declaration changes the *storage* and nothing else — no
    /// initializer needs rewriting, no access site needs to know. `@offset_of` and
    /// `size_of` lower to C's own `offsetof`/`sizeof`, so they follow the new order for
    /// free rather than needing to be taught about it.
    ///
    /// Non-field members (methods) are returned untouched and in place: they are not
    /// emitted here at all, and filtering them would be a second thing to keep in sync.
    fn struct_field_order<'b>(
        &self,
        name: &str,
        body: &'b StructBody,
        attrs: &[Attribute],
    ) -> Vec<&'b StructMember> {
        let members: Vec<&StructMember> = body.members.iter().collect();
        let Some(perm) = crate::layout::field_order(self.ast, self.info, name, attrs) else {
            return members;
        };
        // `perm` indexes the *fields* in declaration order, while `members` also holds
        // methods. Map through the field positions so the two agree.
        let fields: Vec<usize> = members
            .iter()
            .enumerate()
            .filter(|(_, m)| matches!(m, StructMember::Field { .. }))
            .map(|(i, _)| i)
            .collect();
        // A permutation that does not cover the declared fields would silently drop or
        // duplicate one; fall back to declaration order rather than emit a wrong struct.
        if perm.len() != fields.len() {
            return members;
        }
        let mut out = Vec::with_capacity(members.len());
        let mut slot = 0usize;
        for (i, m) in members.iter().enumerate() {
            if fields.contains(&i) {
                out.push(members[fields[perm[slot]]]);
                slot += 1;
            } else {
                out.push(m);
            }
        }
        out
    }

    /// Translate item attributes that affect struct layout into a GNU
    /// `__attribute__((…))` clause: `@packed` → `packed`, `@align(n)` →
    /// `aligned(n)`. `@layout(c)` is the default C layout, and `@layout(auto)` is
    /// handled by [`Self::struct_field_order`] rather than by an attribute clause —
    /// the order is *chosen*, not delegated to the C compiler, so the emitted struct
    /// is the same bytes under any conforming compiler. Unknown attributes are ignored.
    fn struct_attr(&self, attrs: &[Attribute]) -> String {
        let mut parts = Vec::new();
        for a in attrs {
            match a.name.as_str() {
                "packed" => parts.push("packed".to_string()),
                "align" => {
                    if let Some(&arg) = a.args.first() {
                        if let ExprKind::Int(n) = &self.ast.expr_at(arg).kind {
                            parts.push(format!("aligned({})", c_int_literal(n)));
                        }
                    }
                }
                _ => {}
            }
        }
        if parts.is_empty() {
            String::new()
        } else {
            format!(" __attribute__(({}))", parts.join(", "))
        }
    }

    fn fn_protos(&mut self) {
        let ast = self.ast;
        // non-generic functions
        for (i, item) in ast.items.iter().enumerate() {
            self.cur_mod = self.item_module(i);
            if let Item::Fn(f) = item {
                if self.is_generic(f) || !self.fn_supported(f) {
                    continue;
                }
                self.subst.clear();
                let cname = self.c_fn_name(&self.fn_canon(f));
                let sig = self.fn_signature(f, &cname);
                self.raw(format!("{sig};\n"));
            }
        }
        // monomorphized instances
        for (name, args) in self.instances.clone() {
            if let Some(f) = self.find_fn(&name) {
                self.subst = self.make_subst(f, &args);
                let sig = self.fn_signature(f, &format!("jestyr_{}", self.mangle(&name, &args)));
                self.raw(format!("{sig};\n"));
                self.subst.clear();
            }
        }
        self.raw("\n");
    }

    /// Emit a C prototype for each `extern "c"` declaration. The function is
    /// called by its bare name and resolved by the linker (design §12).
    fn extern_protos(&mut self) {
        let ast = self.ast;
        let mut any = false;
        for item in &ast.items {
            if let Item::Extern(e) = item {
                any = true;
                let ret = match e.ret_ty {
                    Some(t) => self.c_ty_ast(t),
                    None => "void".to_string(),
                };
                let params = self.extern_params_str(e);
                self.raw(format!("{ret} {}({});\n", e.name.name, params));
            }
        }
        if any {
            self.raw("\n");
        }
    }

    fn extern_params_str(&mut self, e: &ExternFn) -> String {
        let mut parts = Vec::new();
        for p in &e.params {
            if p.comptime || p.is_self {
                continue;
            }
            let base = match p.ty {
                Some(t) => self.c_ty_ast(t),
                None => "int".to_string(),
            };
            // No `restrict`: a foreign C function makes no aliasing promise.
            let cty = if matches!(p.conv, Conv::Mut | Conv::Out) { format!("{base}*") } else { base };
            parts.push(format!("{cty} j_{}", p.name.name));
        }
        if parts.is_empty() {
            "void".to_string()
        } else {
            parts.join(", ")
        }
    }

    fn consts(&mut self) {
        let ast = self.ast;
        for (i, item) in ast.items.iter().enumerate() {
            self.cur_mod = self.item_module(i);
            if let Item::Const(c) = item {
                let cty = if let Some(t) = c.ty {
                    self.c_ty_ast(t)
                } else {
                    let t = self.info.type_of(c.value).clone();
                    self.c_type(&t)
                };
                // An array-literal const must be a brace initializer, not the
                // statement-expression `emit_expr` would give (a `static const`
                // cannot be initialized by a GNU statement-expression). The C array
                // type is `struct { T a[N]; }`, so the initializer is `{ { … } }`.
                let arr_init: Option<(Vec<ExprId>, Option<ExprId>)> =
                    match &ast.expr_at(c.value).kind {
                        ExprKind::ArrayLit { elems } => Some((elems.clone(), None)),
                        ExprKind::ArrayRepeat { value, count } => {
                            Some((vec![*value], Some(*count)))
                        }
                        _ => None,
                    };
                // A `comptime` block that produced an AGGREGATE needs the same brace
                // form, for the same reason — and this is the payoff of tier 6: a
                // lookup table *computed* by the compiler becomes an ordinary static,
                // indistinguishable from one typed out by hand.
                let comptime_table = match &ast.expr_at(c.value).kind {
                    ExprKind::Comptime(_) => match comptime::Interp::new(ast).eval(c.value) {
                        Ok(comptime::Value::List(items)) => Some(c_comptime_brace(&items)),
                        _ => None,
                    },
                    _ => None,
                };
                let v = if let Some(table) = comptime_table {
                    table
                } else if let Some((elems, repeat)) = arr_init {
                    let parts: Vec<String> = if let Some(count) = repeat {
                        let one = self.emit_expr(elems[0]);
                        vec![one; self.array_len(count)]
                    } else {
                        elems.iter().map(|e| self.emit_expr(*e)).collect()
                    };
                    format!("{{ {{ {} }} }}", parts.join(", "))
                } else {
                    self.emit_expr(c.value)
                };
                // `@section(".name")` places the global in a named linker section.
                let section = c
                    .attr("section")
                    .and_then(|a| a.args.first())
                    .and_then(|arg| match &self.ast.expr_at(*arg).kind {
                        ExprKind::Str(s) => Some(format!(" __attribute__((section({s})))")),
                        _ => None,
                    })
                    .unwrap_or_default();
                if self.no_mangle_consts.contains(&c.name.name) {
                    // Exported as a bare external symbol (no `static`, no `j_` prefix).
                    self.raw(format!("const {cty} {}{section} = {v};\n", c.name.name));
                } else {
                    self.raw(format!("static const {cty} j_{}{section} = {v};\n", c.name.name));
                }
            }
        }
        self.raw("\n");
    }

    fn fn_defs(&mut self) {
        let ast = self.ast;
        // non-generic functions
        for (i, item) in ast.items.iter().enumerate() {
            self.cur_mod = self.item_module(i);
            if let Item::Fn(f) = item {
                if self.is_generic(f) {
                    continue; // emitted as monomorphized instances below
                }
                if !self.fn_supported(f) {
                    self.diag(
                        f.name.span,
                        format!("`{}`: the C backend does not support methods (`self`) yet", f.name.name),
                    );
                    continue;
                }
                self.subst.clear();
                let cname = self.c_fn_name(&self.fn_canon(f));
                self.emit_fn(f, &cname);
            }
        }
        // monomorphized instances
        for (name, args) in self.instances.clone() {
            if let Some(f) = self.find_fn(&name) {
                self.subst = self.make_subst(f, &args);
                self.emit_fn(f, &format!("jestyr_{}", self.mangle(&name, &args)));
                self.subst.clear();
            }
        }
    }

    /// Emit one function body. The C name is supplied by the caller (mangled for
    /// instances), and `self.subst` (set by the caller) substitutes type
    /// parameters. `comptime` type parameters are erased from the signature.
    fn emit_fn(&mut self, f: &FnDecl, c_name: &str) {
        // `mut`/`out` have always been pointers; `@abi(ref)` adds the large read-only
        // aggregates to the same set. That set is the *only* thing the body needs to
        // know — a name in it renders as `(*j_x)`, so every field read, every pass-on
        // and every capture follows without another line of backend change. Reusing the
        // existing indirection is what makes an ABI change a small increment.
        self.ptr_params = f
            .params
            .iter()
            .filter(|p| !p.comptime && matches!(p.conv, Conv::Mut | Conv::Out))
            .map(|p| p.name.name.clone())
            .collect();
        self.ptr_params.extend(self.abi_ref_params(f));
        self.cur_result = self.fn_result_type(f);
        self.cur_ensures = f.ensures.clone();
        self.cur_ret_cty = self.ret_type(f);
        self.cur_no_panic = f.no_panic;
        self.cur_refines =
            f.params.iter().filter_map(|p| p.refine.map(|r| (p.name.name.clone(), r))).collect();

        let mut moved = HashSet::new();
        self.collect_moved(&f.body, &mut moved);
        self.cur_moved = moved;

        let sig = self.fn_signature(f, c_name);
        let returns_value = self.ret_type(f) != "void";
        self.raw(format!("{sig}\n"));
        // Map the function's emitted body back to its `.jtr` declaration line. The
        // per-statement directives (increment b) then refine the mapping; reset the
        // dedup state so this entry directive always fires for a new function.
        self.dbg_last = None;
        self.mark_line(f.name.span);
        self.emit_fn_body(&f.body, returns_value, &f.requires);
        self.raw("\n");
        self.ptr_params.clear();
        self.cur_result.clear();
        self.cur_ensures.clear();
        self.cur_ret_cty.clear();
        self.cur_refines.clear();
        self.cur_no_panic = false;
        self.cur_moved.clear();
    }

    /// Like `emit_body`, but prefixed with the function's `requires`
    /// preconditions as `assert`s (active in debug, elided under `-DNDEBUG`).
    fn emit_fn_body(&mut self, block: &Block, ret: bool, requires: &[ExprId]) {
        self.line("{");
        self.depth += 1;
        self.drop_scope_enter();
        for r in requires {
            // Point a precondition's `assert` at the `requires` clause itself, so a
            // contract failure blames the `.jtr` contract, not generated C (incr. c).
            let sp = self.ast.expr_at(*r).span;
            self.mark_line(sp);
            let c = self.emit_expr(*r);
            self.line(format!("assert({c});"));
        }
        let n = block.stmts.len();
        for (i, stmt) in block.stmts.iter().enumerate() {
            let last = i + 1 == n;
            if last && ret {
                // The tail is emitted as a `return` directly (bypassing
                // `emit_stmt`), so map its line here for per-statement debug info.
                let sp = self.stmt_span(stmt);
                self.mark_line(sp);
                match stmt {
                    Stmt::Expr(e) => self.emit_return(Some(*e)),
                    Stmt::Return { value, .. } => self.emit_return(*value),
                    _ => self.emit_stmt(stmt),
                }
            } else {
                self.emit_stmt(stmt);
            }
        }
        // A function body that returns by value has already dropped (the tail was a
        // `return`); a fall-through (void) body drops its locals here.
        if block_diverges(block, ret) {
            self.drop_scope_exit_discard();
        } else {
            self.drop_scope_exit_emit();
        }
        self.depth -= 1;
        self.line("}");
    }

    /// Emit a value-returning `return`, checking any `ensures` postconditions
    /// first (with `result` — emitted as `j_result` — bound to the value).
    fn emit_value_return(&mut self, value: String) {
        if self.cur_ensures.is_empty() && !self.has_live_drops() {
            self.line(format!("return {value};"));
            return;
        }
        // Spill to a temp *before* running drops, so the returned value can't read
        // a local we're about to drop (use-after-drop). The returned value itself,
        // if a moved-out local, is never in a drop scope (`cur_moved`), so it is
        // not double-freed. `__auto_type` covers methods, where `cur_ret_cty` is
        // unset.
        let decl =
            if self.cur_ret_cty.is_empty() { "__auto_type".to_string() } else { self.cur_ret_cty.clone() };
        self.line(format!("{decl} j_result = {value};"));
        for post in self.cur_ensures.clone() {
            // Point a postcondition's `assert` at the `ensures` clause (increment c).
            let sp = self.ast.expr_at(post).span;
            self.mark_line(sp);
            let c = self.emit_expr(post);
            self.line(format!("assert({c});"));
        }
        self.emit_all_drops();
        self.line("return j_result;");
    }

    // --- Drop / RAII: static, drop-flag-free scope-exit glue (design Phase 3) ---

    /// The `impl Drop for <T>` type key if `ty` has a `Drop` impl, else `None`.
    /// Only concrete named/primitive receivers are recognised (a generic `Drop`
    /// impl is future work) — that keeps the glue sound: an unrecognised type
    /// simply gets no auto-drop.
    fn drop_key_of(&self, ty: &Ty) -> Option<String> {
        let key = self.info.table.ty_key(ty);
        if key.is_empty() {
            return None;
        }
        // A concrete `impl Drop for <ty>`.
        if self.info.table.impl_index.contains_key(&("Drop".to_string(), key.clone())) {
            return Some(key);
        }
        // A blanket `impl[T] Drop for Ctor(T)` covering this instantiation.
        if let Ty::GenStruct { ctor, .. } = ty {
            if self.generic_drop_impl(ctor).is_some() {
                return Some(key);
            }
        }
        None
    }

    /// Register a freshly-declared local for scope-exit drop glue, if its type
    /// *needs drop* (has a `Drop` impl, or transitively owns a field/payload that
    /// does) and its value does not escape (`cur_moved`). Appends to the innermost
    /// open drop scope; a no-op when there is none.
    fn register_drop_local(&mut self, name: &str, ty: &Ty) {
        if self.cur_moved.contains(name) {
            return;
        }
        if !self.needs_drop(ty) {
            return;
        }
        if let Some(scope) = self.drop_stack.last_mut() {
            scope.push(DropLocal { name: name.to_string(), ty: ty.clone() });
        }
    }

    /// True if a value of `ty` requires any drop glue: it has its own `Drop` impl,
    /// or it transitively owns a by-value struct field / enum payload that needs
    /// drop. Pointers, references, and niche payloads are **not** followed —
    /// dropping through the heap is a `Drop` impl's own job, and stopping at
    /// indirection also guarantees termination (a by-value aggregate can't contain
    /// itself). `@copy` aggregates never drop.
    fn needs_drop(&self, ty: &Ty) -> bool {
        if ty.is_copy(&self.info.table) {
            return false;
        }
        if self.drop_key_of(ty).is_some() {
            return true;
        }
        // A concrete struct: any owned field needs drop?
        if let Some(fields) = self.aggregate_drop_fields(ty) {
            return fields.iter().any(|(_, fty)| self.needs_drop(fty));
        }
        // A concrete (non-niche) enum: any live-variant payload needs drop?
        if let Some(variants) = self.enum_drop_variants(ty) {
            return variants.iter().any(|(_, payload)| payload.iter().any(|(_, fty)| self.needs_drop(fty)));
        }
        false
    }

    /// The owned (drop-recursable) fields of a concrete `struct`/`record`:
    /// `(field-name, field-type)` in declaration order, or `None` if `ty` is not a
    /// named non-generic struct. Pointer/reference fields are dropped from the list
    /// (we don't follow indirection); their own `Drop` impl, if any, is reached only
    /// when the field is itself a by-value aggregate.
    fn aggregate_drop_fields(&self, ty: &Ty) -> Option<Vec<(String, Ty)>> {
        let Ty::Named(i) = ty else { return None };
        let decl = self.info.table.types.get(*i)?;
        if !matches!(decl.kind, TypeKindG::Struct { .. }) {
            return None;
        }
        // Read field names + types from the AST decl (the type table's struct
        // fields carry the same data, but the AST is the source of truth for the
        // `j_<name>` C accessor and is what the enum path must use anyway).
        let name = &decl.name;
        for item in &self.ast.items {
            if let Item::Struct { name: sname, body, is_union, .. } = item {
                if &sname.name != name {
                    continue;
                }
                // An untagged `union` has no single live field — never auto-drop it.
                if *is_union {
                    return Some(Vec::new());
                }
                let empty = HashMap::new();
                let fields = body
                    .members
                    .iter()
                    .filter_map(|m| match m {
                        StructMember::Field { name: fname, ty: fty, .. } => {
                            Some((fname.name.clone(), self.ast_type_to_ty(*fty, &empty)))
                        }
                        _ => None,
                    })
                    .filter(|(_, t)| !Self::is_indirect_ty(t))
                    .collect();
                return Some(fields);
            }
        }
        Some(Vec::new())
    }

    /// The variants of a concrete (non-generic, non-niche) `enum` that the drop
    /// walker must consider: `(variant-name, [(payload-field-name, type)])` in
    /// declaration order. `None` if `ty` is not such an enum. Pointer payloads are
    /// dropped from each variant's list (indirection is not followed).
    fn enum_drop_variants(&self, ty: &Ty) -> Option<Vec<(String, Vec<(String, Ty)>)>> {
        let Ty::Named(i) = ty else { return None };
        let decl = self.info.table.types.get(*i)?;
        if !matches!(decl.kind, TypeKindG::Enum { .. }) {
            return None;
        }
        // A niche-optimized enum has no tag/union; its payload is a bare pointer we
        // don't follow. Only its own `Drop` impl (if any) runs — never a payload walk.
        if self.niche_enum_at(*i).is_some() {
            return Some(Vec::new());
        }
        let name = decl.name.clone();
        for item in &self.ast.items {
            if let Item::Enum(e) = item {
                if e.name.name != name || e.is_generic() {
                    continue;
                }
                let empty = HashMap::new();
                let variants = e
                    .variants
                    .iter()
                    .map(|v| {
                        let payload = v
                            .fields
                            .iter()
                            .map(|(fname, fty)| (fname.name.clone(), self.ast_type_to_ty(*fty, &empty)))
                            .filter(|(_, t)| !Self::is_indirect_ty(t))
                            .collect();
                        (v.name.name.clone(), payload)
                    })
                    .collect();
                return Some(variants);
            }
        }
        Some(Vec::new())
    }

    /// Indirection we do not drop-recurse through (a raw pointer or a reference):
    /// the heap behind it is owned by a `Drop` impl, not by structural recursion.
    fn is_indirect_ty(ty: &Ty) -> bool {
        matches!(ty, Ty::Ptr { .. } | Ty::GenRef(_) | Ty::RegionRef(_))
    }

    /// Emit the drop glue for one local: its own `Drop::drop` (if any), then a
    /// recursive walk of its owned fields/payloads. The C local is `j_<name>`.
    fn emit_one_drop(&mut self, d: &DropLocal) {
        let ty = d.ty.clone();
        let place = format!("j_{}", d.name);
        self.emit_drop_place(&place, &ty);
    }

    /// Emit drop glue for the value at C lvalue `place` of type `ty`: first its own
    /// `Drop::drop` (the receiver passed by `&place`, since `drop` takes `mut self`),
    /// then — additively — a recursion into each owned struct field / live enum
    /// payload in **reverse declaration order**. A value with no droppable fields
    /// emits exactly its own drop (or nothing), so output is byte-identical for any
    /// program that doesn't use field/payload drop. Pointers/references are not
    /// followed (the heap behind them is a `Drop` impl's responsibility), which also
    /// bounds the recursion.
    fn emit_drop_place(&mut self, place: &str, ty: &Ty) {
        // 1. The value's own destructor, if it has one.
        if let Some(key) = self.drop_key_of(ty) {
            if self.show_drops {
                self.line(format!("/* drop {place} : {key} */"));
            }
            let f = impl_method_c_name("Drop", &key, "drop");
            self.line(format!("{f}(&{place});"));
        }
        // 2. Owned struct fields, reverse declaration order.
        if let Some(fields) = self.aggregate_drop_fields(ty) {
            for (fname, fty) in fields.iter().rev() {
                if self.needs_drop(fty) {
                    let sub = format!("{place}.j_{fname}");
                    self.emit_drop_place(&sub, fty);
                }
            }
            return;
        }
        // 3. Live enum payload — switch on the tag, drop only the active variant's
        //    owned payload fields (reverse declaration order). Variants with no
        //    droppable payload contribute no `case`.
        if let Some(variants) = self.enum_drop_variants(ty) {
            let droppable: Vec<(String, Vec<(String, Ty)>)> = variants
                .into_iter()
                .filter(|(_, payload)| payload.iter().any(|(_, fty)| self.needs_drop(fty)))
                .collect();
            if droppable.is_empty() {
                return;
            }
            let prefix = self.enum_tag_prefix(ty);
            self.line(format!("switch ({place}.tag) {{"));
            for (vname, payload) in &droppable {
                self.line(format!("case {prefix}_{vname}: {{"));
                for (fname, fty) in payload.iter().rev() {
                    if self.needs_drop(fty) {
                        let sub = format!("{place}.u.{vname}.j_{fname}");
                        self.emit_drop_place(&sub, fty);
                    }
                }
                self.line("break;".to_string());
                self.line("}".to_string());
            }
            self.line("default: break;".to_string());
            self.line("}".to_string());
        }
    }

    /// Open a drop scope for a `{ }` block.
    fn drop_scope_enter(&mut self) {
        self.drop_stack.push(Vec::new());
    }

    /// Close the innermost drop scope, emitting its locals' drops in reverse
    /// declaration order (the normal fall-through path).
    fn drop_scope_exit_emit(&mut self) {
        if let Some(scope) = self.drop_stack.pop() {
            for d in scope.iter().rev() {
                let d = d.clone();
                self.emit_one_drop(&d);
            }
        }
    }

    /// Close the innermost drop scope without emitting (the block already diverged
    /// via a `return`, which dropped everything).
    fn drop_scope_exit_discard(&mut self) {
        self.drop_stack.pop();
    }

    /// Drop every live local across all open scopes (innermost first) — the
    /// cleanup run *before* a `return`. Does not pop: the C locals stay in scope
    /// until the actual `return` statement that follows.
    fn emit_all_drops(&mut self) {
        let scopes: Vec<Vec<DropLocal>> = self.drop_stack.iter().rev().cloned().collect();
        for scope in &scopes {
            for d in scope.iter().rev() {
                let d = d.clone();
                self.emit_one_drop(&d);
            }
        }
    }

    /// True if any open scope holds a live droppable — i.e. a `return` here needs
    /// to spill its value to a temp, run drops, then return.
    fn has_live_drops(&self) -> bool {
        self.drop_stack.iter().any(|s| !s.is_empty())
    }

    /// Compute the set of a function's locals whose value *escapes* and so must
    /// not be auto-dropped. Over-approximated (leak-safe): a name is "moved" if it
    /// is returned, passed by value to any call, captured into a struct literal,
    /// rebound, or used as the receiver of a `take self` method. Borrows (a field
    /// read, an index, a `read`/`mut self` method call) do **not** move it.
    fn collect_moved(&self, block: &Block, out: &mut HashSet<String>) {
        let n = block.stmts.len();
        for (i, stmt) in block.stmts.iter().enumerate() {
            match stmt {
                Stmt::Let { init: Some(e), .. } => {
                    if let Some(name) = self.as_name(*e) {
                        out.insert(name);
                    }
                    self.collect_moved_expr(*e, out);
                }
                Stmt::Let { init: None, .. } => {}
                Stmt::Return { value: Some(e), .. } => {
                    if let Some(name) = self.as_name(*e) {
                        out.insert(name);
                    }
                    self.collect_moved_expr(*e, out);
                }
                Stmt::Return { value: None, .. } => {}
                Stmt::Expr(e) => {
                    // The block's tail expression may be an implicit return value.
                    if i + 1 == n {
                        if let Some(name) = self.as_name(*e) {
                            out.insert(name);
                        }
                    }
                    self.collect_moved_expr(*e, out);
                }
            }
        }
    }

    /// The bare local name an expression refers to, if it is exactly `Name(n)`.
    fn as_name(&self, id: ExprId) -> Option<String> {
        match &self.ast.expr_at(id).kind {
            ExprKind::Name(n) => Some(n.name.clone()),
            _ => None,
        }
    }

    /// The receiver-parameter convention of a resolved method/impl/operator call,
    /// if the callee is a `recv.m(args)` form. Used to spot a `take self` consume.
    fn recv_conv_of(&self, call_id: ExprId) -> Option<Conv> {
        if let Some(mr) = self.info.method_calls.get(&call_id) {
            return Some(mr.recv_conv);
        }
        if let Some(ic) = self.info.impl_calls.get(&call_id) {
            return self
                .find_impl_method(&ic.trait_name, &ic.type_key, &ic.method)
                .and_then(|f| f.params.iter().find(|p| p.is_self).map(|p| p.conv));
        }
        None
    }

    /// Mark each bare-name argument that lands at a `take` (consuming) parameter of
    /// `call_id` as moved. Borrow conventions (`read`/`mut`/`out`) do not move, so a
    /// droppable handed to a borrowing call still drops at scope exit. A callee we
    /// can't resolve to a user `FnDecl` (an intrinsic, closure, or `dyn` call) never
    /// takes ownership of a droppable in the bootstrap, so it moves nothing.
    ///
    /// Alignment differs by call shape. A **free/qualified** call passes its
    /// `comptime` type arguments as leading args (`vec_push(i32, v, x)`), so args
    /// align 1:1 with the callee's non-`self` params and a type-param position is
    /// skipped. A **method-sugar / impl** call's receiver consumes the first
    /// runtime param, so the explicit args align after it.
    fn mark_take_args(
        &self,
        call_id: ExprId,
        callee: ExprId,
        args: &[ExprId],
        out: &mut HashSet<String>,
    ) {
        // A trait-impl method call: the receiver is `self`, explicit args follow it.
        if let Some(ic) = self.info.impl_calls.get(&call_id) {
            if let Some(f) = self.find_impl_method(&ic.trait_name, &ic.type_key, &ic.method) {
                self.mark_method_arg_takes(f, args, out);
            }
            return;
        }
        // Method sugar `base.m(args)` — a struct method (self) or a free-fn-method
        // (the receiver is the first regular param), resolved via `method_calls`.
        if let Some(mr) = self.info.method_calls.get(&call_id) {
            if let Some(f) = self.find_fn(&mr.fn_name) {
                self.mark_method_arg_takes(f, args, out);
            }
            return;
        }
        // A free or module-qualified call: args align 1:1 with the params.
        let fname = if let Some(q) = self.info.qualified.get(&call_id) {
            Some(q.clone())
        } else if let ExprKind::Name(n) = &self.ast.expr_at(callee).kind {
            Some(n.name.clone())
        } else {
            None
        };
        if let Some(f) = fname.and_then(|n| self.find_fn(&n)) {
            self.mark_free_arg_takes(f, args, out);
        }
    }

    /// Free/qualified call: each non-`self` param takes the arg at the same index;
    /// a `comptime` type-param position carries a *type* argument, so it is skipped.
    fn mark_free_arg_takes(&self, f: &FnDecl, args: &[ExprId], out: &mut HashSet<String>) {
        let params: Vec<&Param> = f.params.iter().filter(|p| !p.is_self).collect();
        for (p, a) in params.iter().zip(args.iter()) {
            if self.is_type_param(p) {
                continue;
            }
            if p.conv == Conv::Take {
                if let Some(name) = self.as_name(*a) {
                    out.insert(name);
                }
            }
        }
    }

    /// Method-sugar / impl call: the receiver consumed the first *runtime* param
    /// (non-`self`, non-`comptime`) when the method is a free-fn-method, or `self`
    /// when it is a struct/impl method; the explicit args align to the rest.
    fn mark_method_arg_takes(&self, f: &FnDecl, args: &[ExprId], out: &mut HashSet<String>) {
        let runtime: Vec<Conv> =
            f.params.iter().filter(|p| !p.is_self && !p.comptime).map(|p| p.conv).collect();
        // A `self`-method's receiver is the (filtered-out) `self` param, so args
        // start at runtime[0]; a free-fn-method's receiver is runtime[0], so args
        // start at runtime[1].
        let offset = if f.params.iter().any(|p| p.is_self) { 0 } else { 1 };
        for (i, a) in args.iter().enumerate() {
            if matches!(runtime.get(i + offset), Some(Conv::Take)) {
                if let Some(name) = self.as_name(*a) {
                    out.insert(name);
                }
            }
        }
    }

    fn collect_moved_expr(&self, id: ExprId, out: &mut HashSet<String>) {
        let ast = self.ast;
        match &ast.expr_at(id).kind {
            ExprKind::Call { callee, args } => {
                // Only a `take` (consuming) argument moves the value; a `read`/`mut`/
                // `out` borrow does not — so a droppable passed to a borrowing method
                // (`vec.push(x)`, `free(vec)` taking `mut`) still drops at scope exit.
                self.mark_take_args(id, *callee, args, out);
                // A `take self` receiver consumes the base.
                if matches!(self.recv_conv_of(id), Some(Conv::Take)) {
                    if let ExprKind::Field { base, .. } = &ast.expr_at(*callee).kind {
                        if let Some(name) = self.as_name(*base) {
                            out.insert(name);
                        }
                    }
                }
                self.collect_moved_expr(*callee, out);
                for a in args {
                    self.collect_moved_expr(*a, out);
                }
            }
            ExprKind::Assign { target, value, .. } => {
                if let Some(name) = self.as_name(*value) {
                    out.insert(name);
                }
                self.collect_moved_expr(*target, out);
                self.collect_moved_expr(*value, out);
            }
            ExprKind::StructLit { fields, spread, .. } => {
                for f in fields {
                    if let Some(name) = self.as_name(f.value) {
                        out.insert(name);
                    }
                    self.collect_moved_expr(f.value, out);
                }
                if let Some(s) = spread {
                    self.collect_moved_expr(*s, out);
                }
            }
            ExprKind::GenStructLit { fields, .. } => {
                for f in fields {
                    if let Some(name) = self.as_name(f.value) {
                        out.insert(name);
                    }
                    self.collect_moved_expr(f.value, out);
                }
            }
            ExprKind::Binary { lhs, rhs, .. } => {
                self.collect_moved_expr(*lhs, out);
                self.collect_moved_expr(*rhs, out);
            }
            ExprKind::Unary { rhs, .. } => self.collect_moved_expr(*rhs, out),
            ExprKind::Range { lo, hi, .. } => {
                if let Some(l) = lo {
                    self.collect_moved_expr(*l, out);
                }
                if let Some(h) = hi {
                    self.collect_moved_expr(*h, out);
                }
            }
            ExprKind::Field { base, .. } => self.collect_moved_expr(*base, out),
            ExprKind::Index { base, index } => {
                self.collect_moved_expr(*base, out);
                self.collect_moved_expr(*index, out);
            }
            ExprKind::Deref { base } => self.collect_moved_expr(*base, out),
            ExprKind::Cast { expr, .. } => self.collect_moved_expr(*expr, out),
            ExprKind::Try { base } => self.collect_moved_expr(*base, out),
            ExprKind::Catch { base, fallback, .. } => {
                self.collect_moved_expr(*base, out);
                self.collect_moved_expr(*fallback, out);
            }
            ExprKind::If { cond, then, els } => {
                self.collect_moved_expr(*cond, out);
                self.collect_moved(then, out);
                if let Some(e) = els {
                    self.collect_moved_expr(*e, out);
                }
            }
            ExprKind::Match { scrut, arms } => {
                self.collect_moved_expr(*scrut, out);
                for a in arms {
                    if let Some(g) = a.guard {
                        self.collect_moved_expr(g, out);
                    }
                    self.collect_moved_expr(a.body, out);
                }
            }
            ExprKind::Block(b) | ExprKind::Unsafe(b) | ExprKind::Concurrent(b) => {
                self.collect_moved(b, out)
            }
            ExprKind::Region { body, .. } => self.collect_moved(body, out),
            ExprKind::Closure { body, .. } => self.collect_moved_expr(*body, out),
            ExprKind::Spawn(inner) => self.collect_moved_expr(*inner, out),
            ExprKind::ParFor { iter, reduction, body, .. } => {
                self.collect_moved_expr(*iter, out);
                self.collect_moved_expr(*reduction, out);
                self.collect_moved_expr(*body, out);
            }
            ExprKind::Select(arms) => {
                for arm in arms {
                    self.collect_moved_expr(arm.chan, out);
                    self.collect_moved(&arm.body, out);
                }
            }
            ExprKind::For { head, body, els, .. } => {
                match head {
                    ForHead::While(c) => self.collect_moved_expr(*c, out),
                    ForHead::Iter { sources, .. } => {
                        for s in sources {
                            self.collect_moved_expr(*s, out);
                        }
                    }
                    ForHead::Infinite => {}
                }
                self.collect_moved(body, out);
                if let Some(els) = els {
                    self.collect_moved(els, out);
                }
            }
            ExprKind::Invariant(e) | ExprKind::Variant(e) => self.collect_moved_expr(*e, out),
            ExprKind::FString { exprs, .. } => {
                for e in exprs {
                    self.collect_moved_expr(*e, out);
                }
            }
            _ => {}
        }
    }

    /// The canonical name of the item at `ast.items[i]` (bare unless its name
    /// collides across modules; see [`crate::types::canon`]).
    fn canon_item(&self, i: usize, name: &str) -> String {
        let m = *self.info.item_mod.get(i).unwrap_or(&0);
        crate::types::canon(m, name, &self.info.dup_fns)
    }

    /// The canonical name of a top-level function declaration, found by identity
    /// among the program's items. A nested method (not a top-level item) is never
    /// a cross-module collision, so it falls back to its bare name.
    fn fn_canon(&self, f: &FnDecl) -> String {
        for (i, it) in self.ast.items.iter().enumerate() {
            if let Item::Fn(g) = it {
                if std::ptr::eq(g, f) {
                    return self.canon_item(i, &f.name.name);
                }
            }
        }
        f.name.name.clone()
    }

    /// The C result-struct name if `f` is fallible, otherwise empty.
    fn fn_result_type(&self, f: &FnDecl) -> String {
        if f.errors.is_none() {
            return String::new();
        }
        let ok = self.info.table.fns.get(&self.fn_canon(f)).map(|s| s.ret.clone()).unwrap_or(Ty::Unit);
        self.result_c_name(&ok)
    }

    /// The C symbol for a *non-generic* user function: its bare name when
    /// `@no_mangle`, otherwise the collision-free `jestyr_<name>`. (Generic
    /// instances are always mangled with their type arguments and never reach
    /// here — `@no_mangle` on a generic is rejected during validation.)
    fn c_fn_name(&self, name: &str) -> String {
        if self.no_mangle.contains(name) {
            name.to_string()
        } else {
            format!("jestyr_{name}")
        }
    }

    /// Translate a function's optimization/ABI/tooling attributes into a leading
    /// declaration clause: a storage prefix (`@inline` → `static inline`, so the
    /// always-inline definition has internal linkage and dodges C's inline-
    /// linkage pitfall) plus a GNU `__attribute__((…))` group. These are pure
    /// hints — they never change *what* the function computes (the design rule).
    fn fn_attr_prefix(&self, f: &FnDecl) -> String {
        let mut storage = String::new();
        let mut gnu: Vec<String> = Vec::new();
        for a in &f.attrs {
            match a.name.as_str() {
                "inline" => {
                    storage = "static inline ".to_string();
                    gnu.push("always_inline".to_string());
                }
                "no_inline" => gnu.push("noinline".to_string()),
                "hot" => gnu.push("hot".to_string()),
                "cold" => gnu.push("cold".to_string()),
                "section" => {
                    // The section name literal already carries its quotes, so
                    // `section(<str>)` is a ready-made C clause.
                    if let Some(arg) = a.args.first() {
                        if let ExprKind::Str(s) = &self.ast.expr_at(*arg).kind {
                            gnu.push(format!("section({s})"));
                        }
                    }
                }
                "must_use" => gnu.push("warn_unused_result".to_string()),
                "deprecated" => {
                    // The message literal already carries its quotes verbatim, so
                    // `deprecated(<str>)` is a ready-made C string literal.
                    let clause = match a.args.first() {
                        Some(arg) => match &self.ast.expr_at(*arg).kind {
                            ExprKind::Str(s) => format!("deprecated({s})"),
                            _ => "deprecated".to_string(),
                        },
                        None => "deprecated".to_string(),
                    };
                    gnu.push(clause);
                }
                _ => {} // `no_panic` is handled in the body; layout attrs aren't on fns
            }
        }
        let attr = if gnu.is_empty() {
            String::new()
        } else {
            format!("__attribute__(({})) ", gnu.join(", "))
        };
        format!("{storage}{attr}")
    }

    fn fn_signature(&mut self, f: &FnDecl, c_name: &str) -> String {
        let prefix = self.fn_attr_prefix(f);
        let ret = self.ret_type(f);
        let params = self.params_str(f);
        format!("{prefix}{ret} {c_name}({params})")
    }

    fn main_wrapper(&mut self) {
        let ast = self.ast;
        let mut main_has_ret = None;
        for item in &ast.items {
            if let Item::Fn(f) = item {
                if f.name.name == "main" && self.fn_supported(f) {
                    main_has_ret = Some(f.ret_ty.is_some());
                    break;
                }
            }
        }
        match main_has_ret {
            // main() captures argc/argv into the globals that back arg()/arg_count(),
            // so a Jestyr program can read its command line (the assignment also means
            // the params are "used" — no unused-parameter noise).
            Some(true) => self.raw("int main(int argc, char** argv) { jestyr_rt_argc = argc; jestyr_rt_argv = argv; return (int) jestyr_main(); }\n"),
            Some(false) => self.raw("int main(int argc, char** argv) { jestyr_rt_argc = argc; jestyr_rt_argv = argv; jestyr_main(); return 0; }\n"),
            None => {}
        }
    }

    /// Emit the `jestyrc test` harness `main`: run each `@test` (a no-arg fn
    /// returning `bool`), tallying pass/fail; then time each `@bench`. Exits
    /// non-zero if any test fails. (User `main` is ignored in test mode.)
    fn test_main(&mut self) {
        let ast = self.ast;
        let runnable = |f: &FnDecl| !self.is_generic(f) && self.fn_supported(f);
        // A name passes the optional `jestyrc test <substr>` filter iff it contains
        // the substring (`None` = no filter = everything). Cloned out of `self`
        // first so it doesn't alias the `runnable` closure's borrow.
        let filter = self.test_filter.clone();
        let passes = |name: &str| filter.as_deref().is_none_or(|f| name.contains(f));
        let tests: Vec<String> = ast
            .items
            .iter()
            .filter_map(|it| match it {
                Item::Fn(f) if f.has_attr("test") && runnable(f) && passes(&f.name.name) => {
                    Some(f.name.name.clone())
                }
                _ => None,
            })
            .collect();
        let benches: Vec<String> = ast
            .items
            .iter()
            .filter_map(|it| match it {
                Item::Fn(f) if f.has_attr("bench") && runnable(f) && passes(&f.name.name) => {
                    Some(f.name.name.clone())
                }
                _ => None,
            })
            .collect();

        self.raw("int main(void) {\n");
        self.raw(format!(
            "    int _passed = 0, _failed = 0;\n    printf(\"running {} test(s)\\n\");\n",
            tests.len()
        ));
        for t in &tests {
            let call = self.c_fn_name(t);
            self.raw(format!("    printf(\"test {t} ... \"); fflush(stdout);\n"));
            self.raw(format!(
                "    if ({call}()) {{ printf(\"ok\\n\"); _passed++; }} else {{ printf(\"FAILED\\n\"); _failed++; }}\n"
            ));
        }
        for b in &benches {
            let call = self.c_fn_name(b);
            self.raw(format!(
                "    {{ clock_t _s = clock(); {call}(); clock_t _e = clock(); \
                 printf(\"bench {b} ... %.3f ms\\n\", (double)(_e - _s) * 1000.0 / CLOCKS_PER_SEC); }}\n"
            ));
        }
        self.raw(
            "    printf(\"\\nresult: %d passed; %d failed\\n\", _passed, _failed);\n    return _failed == 0 ? 0 : 1;\n}\n",
        );
    }

    // --- signatures ---

    fn ret_type(&mut self, f: &FnDecl) -> String {
        // A fallible function returns its tagged result struct, not the bare type.
        if f.errors.is_some() {
            return self.fn_result_type(f);
        }
        match f.ret_ty {
            Some(t) => self.c_ty_ast(t),
            None => "void".to_string(),
        }
    }

    fn params_str(&mut self, f: &FnDecl) -> String {
        let byref = self.abi_ref_params(f);
        let mut parts = Vec::new();
        for p in &f.params {
            // `self` (methods) and `comptime` type parameters are not runtime
            // parameters — the latter are erased by monomorphization.
            if p.is_self || p.comptime {
                continue;
            }
            let base = match p.ty {
                Some(t) => self.c_ty_ast(t),
                None => "int".to_string(),
            };
            // `@abi(ref)`: a large read-only aggregate crosses as `const T*` instead of
            // being copied. `const` is not decoration — it is the C-level statement of
            // what `read` already promises, so the compiler enforces the read-only half
            // of the convention rather than trusting it.
            let cty = if byref.contains(&p.name.name) {
                format!("const {base}*")
            } else {
                borrow_ptr_cty(&base, p.conv)
            };
            parts.push(format!("{cty} j_{}", p.name.name));
        }
        if parts.is_empty() {
            "void".to_string()
        } else {
            parts.join(", ")
        }
    }

    /// The parameters of `f` that `@abi(ref)` passes by `const T*`.
    ///
    /// Empty unless the function carries `@abi(ref)`, which is what keeps every program
    /// that does not opt in byte-identical.
    ///
    /// ## Which parameters qualify, and why not all of them
    /// A parameter qualifies when it is **read-only** (`read`, or the default borrow —
    /// `mut`/`out` already pass a pointer, and a `take` parameter is an ownership
    /// transfer whose copy is the point) and its type is an aggregate **larger than two
    /// machine words**.
    ///
    /// The size threshold matters. Below it, a by-value pass is already one or two
    /// registers and a pointer would be *slower* plus an indirection at every use — an
    /// ABI attribute that pessimized the small cases would be a bad attribute. Sixteen
    /// bytes is where the common C ABIs stop passing aggregates in registers.
    ///
    /// The size comes from `layout.rs`, which is the payoff L1 was built for: a
    /// parameter whose layout is **not knowable** (a generic instance, an opaque type)
    /// is left by value rather than guessed at, so the convention is never chosen from a
    /// number the compiler had to invent.
    fn abi_ref_params(&self, f: &FnDecl) -> HashSet<String> {
        let mut out = HashSet::new();
        if !self.wants_abi_ref(f) {
            return out;
        }
        let model = crate::layout::Model::default();
        for p in &f.params {
            if p.is_self || p.comptime || matches!(p.conv, Conv::Mut | Conv::Out | Conv::Take) {
                continue;
            }
            let Some(tid) = p.ty else { continue };
            let ty = self.ast_type_to_ty(tid, &self.subst);
            // Only an aggregate — a scalar or a pointer is already one word, and
            // wrapping it in another pointer is pure loss.
            if !matches!(ty, Ty::Named(_) | Ty::Array { .. }) {
                continue;
            }
            if let Some(l) = crate::layout::layout_of(self.info, &model, &ty) {
                if l.size > 2 * 8 {
                    out.insert(p.name.name.clone());
                }
            }
        }
        out
    }

    /// The **runtime parameter positions** a call to `name` must pass by address
    /// because the callee carries `@abi(ref)`.
    ///
    /// Positions rather than names, because a call site has arguments, not parameters.
    /// Empty for every function that did not opt in, which is what keeps existing call
    /// sites byte-identical.
    fn abi_ref_positions(&self, name: &str) -> HashSet<usize> {
        let mut out = HashSet::new();
        let Some(f) = self.find_fn(name) else { return out };
        if !self.wants_abi_ref(f) {
            return out;
        }
        let byref = self.abi_ref_params(f);
        for (i, p) in f.params.iter().filter(|p| !p.is_self && !p.comptime).enumerate() {
            if byref.contains(&p.name.name) {
                out.insert(i);
            }
        }
        out
    }

    /// Render `arg` as a `const T*` for an `@abi(ref)` parameter.
    ///
    /// ## Why this is two cases and not one
    /// `&(e)` is only legal when `e` is an lvalue, and a `read` argument may be any
    /// expression — `f(make_point())` is a call whose result has no address. Taking
    /// `&` of it does not compile; spilling it into a GNU statement expression and
    /// returning that address is *worse*, because the temporary dies at the closing
    /// brace and the callee would read freed stack.
    ///
    /// The rvalue case therefore uses a **compound literal of array type**:
    /// `(const T[1]){ e }` initializes a one-element array from the value and decays to
    /// `const T*`. Its lifetime is the enclosing block, so it comfortably outlives the
    /// call, and it is plain C99 rather than a GNU extension.
    ///
    /// The lvalue case still takes the address directly, and that matters: it is the
    /// only path that avoids a copy, which is the entire point of the attribute. An
    /// implementation that always used the compound literal would be correct and
    /// completely pointless.
    fn abi_ref_arg(&mut self, arg: ExprId, rendered: &str) -> String {
        if is_c_lvalue(self.ast, arg) {
            return format!("&({rendered})");
        }
        let cty = self.c_type(&self.info.type_of(arg).clone());
        format!("(const {cty}[1]){{ {rendered} }}")
    }

    /// Does `f` carry `@abi(ref)`? (The vocabulary is validated in `attrs.rs`; this
    /// only reads the spelling, so the two cannot disagree about what `ref` means.)
    fn wants_abi_ref(&self, f: &FnDecl) -> bool {
        let Some(a) = f.attr("abi") else { return false };
        matches!(
            a.args.first().map(|id| &self.ast.expr_at(*id).kind),
            Some(ExprKind::Name(n)) if n.name == "ref"
        )
    }

    // --- statements ---

    fn emit_body(&mut self, block: &Block, ret: bool) {
        self.line("{");
        self.depth += 1;
        self.drop_scope_enter();
        let n = block.stmts.len();
        for (i, stmt) in block.stmts.iter().enumerate() {
            let last = i + 1 == n;
            if last && ret {
                // The tail is emitted as a `return` directly (bypassing
                // `emit_stmt`), so map its line here for per-statement debug info.
                let sp = self.stmt_span(stmt);
                self.mark_line(sp);
                match stmt {
                    Stmt::Expr(e) => self.emit_return(Some(*e)),
                    Stmt::Return { value, .. } => self.emit_return(*value),
                    _ => self.emit_stmt(stmt),
                }
            } else {
                self.emit_stmt(stmt);
            }
        }
        if block_diverges(block, ret) {
            self.drop_scope_exit_discard();
        } else {
            self.drop_scope_exit_emit();
        }
        self.depth -= 1;
        self.line("}");
    }

    fn emit_stmt(&mut self, stmt: &Stmt) {
        // Per-statement `#line`: map the C that follows back to this statement's
        // source line (deduped — only emits when the line changes).
        let sp = self.stmt_span(stmt);
        self.mark_line(sp);
        match stmt {
            Stmt::Let { name, ty, init, .. } => {
                let cty = if let Some(t) = ty {
                    self.c_ty_ast(*t)
                } else if let Some(e) = init {
                    // a closure-bound local takes the closure's concrete struct type
                    if let Some(&_idx) = self.closure_index.get(e) {
                        format!("JestyrClosure_{}", e.0)
                    } else {
                        let t = self.info.type_of(*e).clone();
                        self.c_type(&t)
                    }
                } else {
                    "int".to_string()
                };
                let text = if let Some(e) = init {
                    let v = self.emit_expr(*e);
                    format!("{cty} j_{} = {v};", name.name)
                } else {
                    format!("{cty} j_{};", name.name)
                };
                self.line(text);
                // Register the local for scope-exit drop glue *after* its
                // declaration, so an earlier `return` never references it.
                let lty = if let Some(t) = ty {
                    self.ast_type_to_ty(*t, &self.subst.clone())
                } else if let Some(e) = init {
                    self.info.type_of(*e).clone()
                } else {
                    Ty::Unknown
                };
                self.register_drop_local(&name.name, &lty);
            }
            Stmt::Return { value, .. } => self.emit_return(*value),
            Stmt::Expr(e) => {
                let ast = self.ast;
                match &ast.expr_at(*e).kind {
                    ExprKind::If { .. } => self.emit_if(*e, false),
                    ExprKind::Match { .. } => self.emit_match(*e, false),
                    ExprKind::Block(b) => self.emit_body(b, false),
                    ExprKind::Unsafe(b) => self.emit_body(b, false),
                    ExprKind::Concurrent(b) => self.emit_concurrent(b),
                    ExprKind::Select(arms) => self.emit_select(arms),
                    ExprKind::Region { name, body } => self.emit_region(&name.name, body),
                    ExprKind::For { label, head, region, body, els } => {
                        self.emit_for(label.as_ref(), head, region.as_ref(), body, els.as_ref())
                    }
                    // A bare `spawn f(args)` reached *inside* a `concurrent` block's
                    // dynamic region (e.g. a spawn-in-a-loop): push it onto the growable
                    // handle array rather than erroring.
                    ExprKind::Spawn(inner) if self.dyn_spawn_active => self.emit_dyn_spawn(*inner),
                    _ => {
                        let v = self.emit_expr(*e);
                        self.line(format!("{v};"));
                    }
                }
            }
        }
    }

    fn emit_return(&mut self, value: Option<ExprId>) {
        let Some(e) = value else {
            self.emit_all_drops();
            self.line("return;");
            return;
        };
        let ast = self.ast;
        match &ast.expr_at(e).kind {
            ExprKind::If { .. } => self.emit_if(e, true),
            ExprKind::Match { .. } => self.emit_match(e, true),
            ExprKind::Block(b) => self.emit_body(b, true),
            ExprKind::Unsafe(b) => self.emit_body(b, true),
            // A loop has no value; emit it as a statement (a non-void function
            // still needs an explicit `return` after it).
            ExprKind::For { label, head, region, body, els } => {
                self.emit_for(label.as_ref(), head, region.as_ref(), body, els.as_ref())
            }
            _ => {
                let v = self.emit_expr(e);
                self.emit_value_return(v);
            }
        }
    }

    fn emit_if(&mut self, e: ExprId, ret: bool) {
        let ast = self.ast;
        let (cond, then_blk, els) = match &ast.expr_at(e).kind {
            ExprKind::If { cond, then, els } => (*cond, then, *els),
            _ => return,
        };
        let c = self.emit_expr(cond);
        self.line(format!("if ({c})"));
        self.emit_body(then_blk, ret);
        if let Some(els_id) = els {
            self.line("else");
            match &ast.expr_at(els_id).kind {
                ExprKind::If { .. } => self.emit_if(els_id, ret),
                ExprKind::Block(b) => self.emit_body(b, ret),
                _ => {
                    // parse always wraps a non-if else in a block, so this is rare.
                    let v = self.emit_expr(els_id);
                    self.line("{");
                    self.depth += 1;
                    if ret {
                        self.emit_value_return(v);
                    } else {
                        self.line(format!("{v};"));
                    }
                    self.depth -= 1;
                    self.line("}");
                }
            }
        }
    }

    /// Lower a `match` on a niche-optimized enum: the scrutinee is a pointer, so
    /// dispatch on `!= NULL` (the `some` payload) vs `== NULL` (`none`) instead of
    /// a tag `switch`. The `some` payload binding *is* the scrutinee pointer.
    fn emit_niche_match(&mut self, e: ExprId, ret: bool, n: &NicheInfo) {
        let ast = self.ast;
        let (scrut, arms) = match &ast.expr_at(e).kind {
            ExprKind::Match { scrut, arms } => (*scrut, arms),
            _ => return,
        };
        if arms.iter().any(|a| a.guard.is_some()) {
            return self.emit_guarded_niche_match(e, ret, n);
        }
        let cty = self.c_type(&n.payload);
        let scrut_c = self.emit_expr(scrut);
        let tmp = format!("jm_{}", self.tmp);
        self.tmp += 1;
        self.line(format!("{cty} {tmp} = {scrut_c};"));

        // Classify the arms (bodies + any binding) into some / none / catch-all.
        let mut some_arm: Option<(ExprId, Option<String>)> = None;
        let mut none_arm: Option<ExprId> = None;
        let mut default_arm: Option<(ExprId, Option<String>)> = None;
        for arm in arms {
            match &ast.pat_at(arm.pat).kind {
                PatKind::Variant { name, subpats } if name.name == n.some_variant => {
                    let bind = subpats.first().and_then(|sp| match &ast.pat_at(*sp).kind {
                        PatKind::Ident(b) => Some(b.name.clone()),
                        _ => None,
                    });
                    some_arm = Some((arm.body, bind));
                }
                PatKind::Ident(name) if name.name == n.none_variant => none_arm = Some(arm.body),
                PatKind::Wildcard => default_arm = Some((arm.body, None)),
                PatKind::Ident(b) => default_arm = Some((arm.body, Some(b.name.clone()))),
                PatKind::Or(_) => self.diag(
                    ast.pat_at(arm.pat).span,
                    "or-patterns on a niche-optimized enum aren't supported yet",
                ),
                _ => {}
            }
        }

        // `some` branch: tmp != NULL, with the payload bound to the pointer.
        self.line(format!("if ({tmp} != (({cty})0))"));
        self.line("{");
        self.depth += 1;
        let some = some_arm.or_else(|| default_arm.clone());
        if let Some((body, bind)) = some {
            if let Some(b) = bind {
                self.line(format!("{cty} j_{b} = {tmp};"));
            }
            self.emit_arm_body(body, ret);
        }
        self.depth -= 1;
        self.line("}");
        // `none` branch: tmp == NULL.
        self.line("else");
        self.line("{");
        self.depth += 1;
        if let Some(body) = none_arm {
            self.emit_arm_body(body, ret);
        } else if let Some((body, bind)) = default_arm {
            if let Some(b) = bind {
                self.line(format!("{cty} j_{b} = {tmp};"));
            }
            self.emit_arm_body(body, ret);
        }
        self.depth -= 1;
        self.line("}");
    }

    /// The guarded counterpart of [`emit_niche_match`]: a niche enum whose `match`
    /// has a guard can't use the simple two-way null branch (a later arm may handle
    /// a value a failed guard skipped), so it becomes an ordered if-chain on the
    /// null test — `some` is `ptr != NULL`, `none` is `ptr == NULL`, a catch-all is
    /// unconditional — each AND-ed with its guard.
    fn emit_guarded_niche_match(&mut self, e: ExprId, ret: bool, n: &NicheInfo) {
        let ast = self.ast;
        let (scrut, arms) = match &ast.expr_at(e).kind {
            ExprKind::Match { scrut, arms } => (*scrut, arms),
            _ => return,
        };
        let cty = self.c_type(&n.payload);
        let scrut_c = self.emit_expr(scrut);
        let tmp = format!("jm_{}", self.tmp);
        self.tmp += 1;
        let end = format!("jm_end_{}", self.tmp);
        self.tmp += 1;
        self.line(format!("{cty} {tmp} = {scrut_c};"));
        let null = format!("(({cty})0)");

        let mut has_uncond_default = false;
        for arm in arms {
            let guard = arm.guard;
            // The C pattern test (None = unconditional catch-all) and an optional
            // binding name (the payload pointer for `some`, the whole value for a
            // binding catch-all).
            let (cond, bind): (Option<String>, Option<String>) = match &ast.pat_at(arm.pat).kind {
                PatKind::Variant { name, subpats } if name.name == n.some_variant => {
                    let b = subpats.first().and_then(|sp| match &ast.pat_at(*sp).kind {
                        PatKind::Ident(b) => Some(b.name.clone()),
                        _ => None,
                    });
                    (Some(format!("{tmp} != {null}")), b)
                }
                PatKind::Ident(name) if name.name == n.none_variant => {
                    (Some(format!("{tmp} == {null}")), None)
                }
                PatKind::Wildcard => (None, None),
                PatKind::Ident(b) => (None, Some(b.name.clone())),
                PatKind::Or(_) => {
                    self.diag(
                        ast.pat_at(arm.pat).span,
                        "or-patterns on a niche-optimized enum aren't supported yet",
                    );
                    continue;
                }
                _ => continue,
            };
            if cond.is_none() && guard.is_none() {
                has_uncond_default = true;
            }
            match &cond {
                Some(c) => {
                    self.line(format!("if ({c})"));
                    self.line("{");
                }
                None => self.line("{"),
            }
            self.depth += 1;
            if let Some(b) = bind {
                self.line(format!("{cty} j_{b} = {tmp};"));
            }
            self.emit_guarded_arm(arm.body, guard, ret, &end);
            self.depth -= 1;
            self.line("}");
        }
        if !ret {
            self.line(format!("{end}: ;"));
        } else if !has_uncond_default {
            self.line("__builtin_unreachable();");
        }
    }

    /// Lower a `match` on an enum to a `switch` on the tag. The scrutinee is
    /// spilled to a temporary so it is evaluated exactly once.
    fn emit_match(&mut self, e: ExprId, ret: bool) {
        let ast = self.ast;
        let (scrut, arms) = match &ast.expr_at(e).kind {
            ExprKind::Match { scrut, arms } => (*scrut, arms),
            _ => return,
        };

        // Resolve the scrutinee type through the active monomorphization
        // substitution: inside a generic function `o: Option(T)` is inferred as
        // `Option(T)` with `T` opaque, but the instance being emitted binds `T` to a
        // concrete type, so the tag prefix, C type, and payload bindings must use it.
        let scrut_ty = apply_subst(&self.info.type_of(scrut).clone(), &self.subst);
        // A nested sub-pattern (a constructor inside a variant's fields) needs the
        // recursive decision-tree lowering — the flat switch/if-chain can't dispatch
        // it. Flat matches (bindings/`_`/`..` fields) keep their optimized paths.
        if arms.iter().any(|a| self.pat_needs_nesting(a.pat)) {
            return self.emit_nested_match(e, ret, &scrut_ty);
        }
        // A niche-optimized enum (plain or a generic instance) matches on a null
        // test, not a tag `switch`.
        match &scrut_ty {
            Ty::Named(i) => {
                if let Some(n) = self.niche_enum_at(*i) {
                    return self.emit_niche_match(e, ret, &n);
                }
            }
            Ty::GenEnum { ctor, args } => {
                if let Some(n) = self.niche_enum_instance(ctor, args) {
                    return self.emit_niche_match(e, ret, &n);
                }
            }
            _ => {}
        }
        // A scalar scrutinee (integer/char/bool) dispatches on the value itself via
        // an ordered if-chain — there's no tag to `switch` on.
        if let Ty::Prim(p) = &scrut_ty {
            if crate::typeck::is_scalar_match_ty(p) {
                return self.emit_scalar_match(e, ret, &scrut_ty);
            }
        }
        // The C tag-enum prefix and the type-arg substitution (empty for a plain
        // enum; type-params → args for a generic instance), so payload bindings
        // get their concrete C type.
        let (tag_prefix, subst): (String, HashMap<String, Ty>) = match &scrut_ty {
            Ty::Named(i)
                if matches!(self.info.table.types[*i].kind, TypeKindG::Enum { .. }) =>
            {
                (format!("Jestyr_{}", self.info.table.types[*i].name), HashMap::new())
            }
            Ty::GenEnum { ctor, args } => {
                (self.gen_struct_c_name(ctor, args), self.gen_enum_subst(ctor, args))
            }
            _ => {
                self.diag(ast.expr_at(e).span, "the C backend only supports `match` on enum values");
                return;
            }
        };

        // Any guarded arm forces the ordered-if-chain lowering: a C `switch` can't
        // place two `case`s on the same tag (arms differing only by guard) nor fall
        // through to a later arm when a guard fails.
        if arms.iter().any(|a| a.guard.is_some()) {
            return self.emit_guarded_match(e, ret, &tag_prefix, &subst, &scrut_ty);
        }

        let scrut_c = self.emit_expr(scrut);
        let cty = self.c_type(&scrut_ty);
        let tmp = format!("jm_{}", self.tmp);
        self.tmp += 1;
        self.line(format!("{cty} {tmp} = {scrut_c};"));
        self.line(format!("switch ({tmp}.tag)"));
        self.line("{");
        self.depth += 1;

        let mut has_default = false;
        for arm in arms {
            match &ast.pat_at(arm.pat).kind {
                PatKind::Variant { name: vname, subpats } => {
                    self.line(format!("case {tag_prefix}_{}:", vname.name));
                    self.line("{");
                    self.depth += 1;
                    if let Some(vi) = self.variants.get(&self.canon_variant(&vname.name)).cloned() {
                        for (i, sp) in subpats.iter().enumerate() {
                            match &ast.pat_at(*sp).kind {
                                // a plain binding → project the field
                                PatKind::Ident(bind)
                                    if !self.variants.contains_key(&self.canon_variant(&bind.name)) =>
                                {
                                    if let Some((fname, fty)) = vi.fields.get(i) {
                                        // Substitute the instance's type args (no-op for
                                        // a plain enum) so the binding's C type is concrete.
                                        let ft = self.ast_type_to_ty(*fty, &subst);
                                        let fcty = self.c_type(&ft);
                                        self.line(format!(
                                            "{fcty} j_{} = {tmp}.u.{}.j_{fname};",
                                            bind.name, vname.name
                                        ));
                                    }
                                }
                                // a wildcard or `..` rest ignores the field
                                PatKind::Wildcard | PatKind::Rest => {}
                                // The frontend (Maranget) understands nested patterns,
                                // but the flat switch/if-chain backend can't dispatch on
                                // them yet — a clear diagnostic beats a silent miscompile.
                                _ => self.diag(
                                    ast.pat_at(*sp).span,
                                    "nested patterns aren't supported by the backend yet — bind the field and `match` it separately",
                                ),
                            }
                        }
                    }
                    self.emit_arm_body(arm.body, ret);
                    if !ret {
                        self.line("break;");
                    }
                    self.depth -= 1;
                    self.line("}");
                }
                PatKind::Ident(vname) if self.variants.contains_key(&self.canon_variant(&vname.name)) => {
                    // a nullary variant pattern, e.g. `none`
                    self.line(format!("case {tag_prefix}_{}:", vname.name));
                    self.line("{");
                    self.depth += 1;
                    self.emit_arm_body(arm.body, ret);
                    if !ret {
                        self.line("break;");
                    }
                    self.depth -= 1;
                    self.line("}");
                }
                PatKind::Ident(bind) => {
                    // a binding catch-all (binds the whole scrutinee)
                    has_default = true;
                    self.line("default:");
                    self.line("{");
                    self.depth += 1;
                    self.line(format!("{cty} j_{} = {tmp};", bind.name));
                    self.emit_arm_body(arm.body, ret);
                    if !ret {
                        self.line("break;");
                    }
                    self.depth -= 1;
                    self.line("}");
                }
                PatKind::Wildcard => {
                    has_default = true;
                    self.line("default:");
                    self.line("{");
                    self.depth += 1;
                    self.emit_arm_body(arm.body, ret);
                    if !ret {
                        self.line("break;");
                    }
                    self.depth -= 1;
                    self.line("}");
                }
                PatKind::Or(_) => {
                    // An or-pattern of nullary variants → stacked `case` labels
                    // sharing one body (`red | green => …`).
                    match self.or_variant_names(arm.pat) {
                        Some(names) => {
                            for nm in &names {
                                self.line(format!("case {tag_prefix}_{nm}:"));
                            }
                            self.line("{");
                            self.depth += 1;
                            self.emit_arm_body(arm.body, ret);
                            if !ret {
                                self.line("break;");
                            }
                            self.depth -= 1;
                            self.line("}");
                        }
                        None => self.diag(
                            ast.pat_at(arm.pat).span,
                            "an or-pattern here must combine nullary variants (payload bindings can't be shared)",
                        ),
                    }
                }
                PatKind::Lit(_) | PatKind::Range { .. } => {
                    // Scalar patterns are invalid on an enum scrutinee (a type error).
                    self.diag(
                        ast.pat_at(arm.pat).span,
                        "literal/range patterns only apply to a scalar `match`",
                    );
                }
                // A bare `..` is only meaningful as a variant's last field, handled
                // by the per-variant binding loop above — not as a whole arm.
                // A struct-variant pattern is intercepted by `emit_nested_match`
                // upstream — unreachable on the flat paths.
                PatKind::Rest | PatKind::StructVariant { .. } => {}
                PatKind::Error => {}
            }
        }

        self.depth -= 1;
        self.line("}");
        // The frontend proved the match exhaustive; tell C so it doesn't warn
        // about falling off the end of a non-void function.
        if ret && !has_default {
            self.line("__builtin_unreachable();");
        }
    }

    /// Lower a tagged-enum `match` that has at least one guarded arm to an ordered
    /// if-else-if chain. Each arm is `if (tag matches) { bind payload; if (guard) {
    /// body } }`; when a guard is false control simply falls through to the next
    /// arm. In statement position a fired arm `goto`s the shared end label; in
    /// return position the body's `return` ends the function (no label needed).
    fn emit_guarded_match(
        &mut self,
        e: ExprId,
        ret: bool,
        tag_prefix: &str,
        subst: &HashMap<String, Ty>,
        scrut_ty: &Ty,
    ) {
        let ast = self.ast;
        let (scrut, arms) = match &ast.expr_at(e).kind {
            ExprKind::Match { scrut, arms } => (*scrut, arms),
            _ => return,
        };
        let scrut_c = self.emit_expr(scrut);
        let cty = self.c_type(scrut_ty);
        let tmp = format!("jm_{}", self.tmp);
        self.tmp += 1;
        let end = format!("jm_end_{}", self.tmp);
        self.tmp += 1;
        self.line(format!("{cty} {tmp} = {scrut_c};"));

        // Does some *unguarded* arm handle every remaining value (an `_` or a
        // whole-value binding)? If so, control can't fall off the chain, so no
        // trailing `__builtin_unreachable` is needed in return position.
        let mut has_uncond_default = false;
        for arm in arms {
            let guard = arm.guard;
            match &ast.pat_at(arm.pat).kind {
                PatKind::Variant { name: vname, subpats } => {
                    self.line(format!("if ({tmp}.tag == {tag_prefix}_{})", vname.name));
                    self.line("{");
                    self.depth += 1;
                    if let Some(vi) = self.variants.get(&self.canon_variant(&vname.name)).cloned() {
                        for (i, sp) in subpats.iter().enumerate() {
                            match &ast.pat_at(*sp).kind {
                                PatKind::Ident(bind)
                                    if !self.variants.contains_key(&self.canon_variant(&bind.name)) =>
                                {
                                    if let Some((fname, fty)) = vi.fields.get(i) {
                                        let ft = self.ast_type_to_ty(*fty, subst);
                                        let fcty = self.c_type(&ft);
                                        self.line(format!(
                                            "{fcty} j_{} = {tmp}.u.{}.j_{fname};",
                                            bind.name, vname.name
                                        ));
                                    }
                                }
                                PatKind::Wildcard | PatKind::Rest => {}
                                _ => self.diag(
                                    ast.pat_at(*sp).span,
                                    "nested patterns aren't supported by the backend yet — bind the field and `match` it separately",
                                ),
                            }
                        }
                    }
                    self.emit_guarded_arm(arm.body, guard, ret, &end);
                    self.depth -= 1;
                    self.line("}");
                }
                PatKind::Ident(vname) if self.variants.contains_key(&self.canon_variant(&vname.name)) => {
                    // a nullary variant pattern, e.g. `none`
                    self.line(format!("if ({tmp}.tag == {tag_prefix}_{})", vname.name));
                    self.line("{");
                    self.depth += 1;
                    self.emit_guarded_arm(arm.body, guard, ret, &end);
                    self.depth -= 1;
                    self.line("}");
                }
                PatKind::Ident(bind) => {
                    // a binding catch-all (binds the whole scrutinee)
                    if guard.is_none() {
                        has_uncond_default = true;
                    }
                    self.line("{");
                    self.depth += 1;
                    self.line(format!("{cty} j_{} = {tmp};", bind.name));
                    self.emit_guarded_arm(arm.body, guard, ret, &end);
                    self.depth -= 1;
                    self.line("}");
                }
                PatKind::Wildcard => {
                    if guard.is_none() {
                        has_uncond_default = true;
                    }
                    self.line("{");
                    self.depth += 1;
                    self.emit_guarded_arm(arm.body, guard, ret, &end);
                    self.depth -= 1;
                    self.line("}");
                }
                PatKind::Or(_) => {
                    // An or-pattern of nullary variants → an OR-ed tag test.
                    match self.or_variant_names(arm.pat) {
                        Some(names) => {
                            let test = names
                                .iter()
                                .map(|nm| format!("{tmp}.tag == {tag_prefix}_{nm}"))
                                .collect::<Vec<_>>()
                                .join(" || ");
                            self.line(format!("if ({test})"));
                            self.line("{");
                            self.depth += 1;
                            self.emit_guarded_arm(arm.body, guard, ret, &end);
                            self.depth -= 1;
                            self.line("}");
                        }
                        None => self.diag(
                            ast.pat_at(arm.pat).span,
                            "an or-pattern here must combine nullary variants (payload bindings can't be shared)",
                        ),
                    }
                }
                PatKind::Lit(_) | PatKind::Range { .. } => {
                    self.diag(
                        ast.pat_at(arm.pat).span,
                        "literal/range patterns only apply to a scalar `match`",
                    );
                }
                // A struct-variant pattern is intercepted by `emit_nested_match`
                // upstream — unreachable on the flat paths.
                PatKind::Rest | PatKind::StructVariant { .. } => {}
                PatKind::Error => {}
            }
        }
        if !ret {
            self.line(format!("{end}: ;"));
        } else if !has_uncond_default {
            // The frontend proved exhaustiveness via unguarded arms covering every
            // tag, so every value returns before reaching here.
            self.line("__builtin_unreachable();");
        }
    }

    /// Emit one arm of a guarded if-chain: gate the body on the guard if present,
    /// and once the body runs, leave the chain (a `goto` to `end` in statement
    /// position; the body's own `return` in return position).
    fn emit_guarded_arm(&mut self, body: ExprId, guard: Option<ExprId>, ret: bool, end: &str) {
        match guard {
            Some(g) => {
                let gc = self.emit_expr(g);
                self.line(format!("if ({gc})"));
                self.line("{");
                self.depth += 1;
                self.emit_arm_body(body, ret);
                if !ret {
                    self.line(format!("goto {end};"));
                }
                self.depth -= 1;
                self.line("}");
            }
            None => {
                self.emit_arm_body(body, ret);
                if !ret {
                    self.line(format!("goto {end};"));
                }
            }
        }
    }

    /// The C boolean test for a scalar pattern against `tmp` — `None` means the
    /// pattern always matches (a wildcard or binding). Or-patterns OR their
    /// alternatives; an invalid pattern diagnoses and yields a never-true test.
    fn scalar_pat_cond(&mut self, pat: PatId, tmp: &str) -> Option<String> {
        let ast = self.ast;
        match &ast.pat_at(pat).kind {
            PatKind::Lit(le) => {
                let lc = self.emit_expr(*le);
                Some(format!("{tmp} == ({lc})"))
            }
            PatKind::Range { lo, hi, inclusive } => {
                let loc = self.emit_expr(*lo);
                let hic = self.emit_expr(*hi);
                let op = if *inclusive { "<=" } else { "<" };
                Some(format!("{tmp} >= ({loc}) && {tmp} {op} ({hic})"))
            }
            PatKind::Wildcard | PatKind::Ident(_) => None,
            PatKind::Or(alts) => {
                let mut parts = Vec::new();
                for p in alts {
                    match self.scalar_pat_cond(*p, tmp) {
                        None => return None, // an alternative matches everything
                        Some(c) => parts.push(format!("({c})")),
                    }
                }
                Some(parts.join(" || "))
            }
            _ => {
                self.diag(ast.pat_at(pat).span, "this pattern isn't valid on a scalar `match`");
                Some("0".to_string())
            }
        }
    }

    /// The enum-variant tag names a (nullary) pattern covers — for stacking `case`
    /// labels and OR-ing tag tests. `None` if the pattern isn't a nullary variant
    /// or an or-pattern of them (payload bindings can't be shared across
    /// alternatives in the bootstrap).
    fn or_variant_names(&self, pat: PatId) -> Option<Vec<String>> {
        match &self.ast.pat_at(pat).kind {
            PatKind::Ident(n) if self.variants.contains_key(&self.canon_variant(&n.name)) => Some(vec![n.name.clone()]),
            PatKind::Variant { name, subpats } if subpats.is_empty() => Some(vec![name.name.clone()]),
            PatKind::Or(alts) => {
                let mut out = Vec::new();
                for p in alts {
                    out.extend(self.or_variant_names(*p)?);
                }
                Some(out)
            }
            _ => None,
        }
    }

    /// Lower a `match` on a scalar (integer/char/bool) scrutinee to an ordered
    /// if-else-if chain on the *value*: a literal arm tests `==`, a range arm tests
    /// `>=` / `<`(`=`), and a wildcard or binding is the catch-all. Guards compose
    /// (the same `emit_guarded_arm`). The frontend requires a catch-all, so control
    /// never falls off the chain.
    fn emit_scalar_match(&mut self, e: ExprId, ret: bool, scrut_ty: &Ty) {
        let ast = self.ast;
        let (scrut, arms) = match &ast.expr_at(e).kind {
            ExprKind::Match { scrut, arms } => (*scrut, arms),
            _ => return,
        };
        let scrut_c = self.emit_expr(scrut);
        let cty = self.c_type(scrut_ty);
        let tmp = format!("jm_{}", self.tmp);
        self.tmp += 1;
        let end = format!("jm_end_{}", self.tmp);
        self.tmp += 1;
        self.line(format!("{cty} {tmp} = {scrut_c};"));

        let mut has_uncond_default = false;
        for arm in arms {
            let guard = arm.guard;
            // On a scalar scrutinee a bare identifier is a binding catch-all (no
            // enum variant can match an integer).
            let bind = match &ast.pat_at(arm.pat).kind {
                PatKind::Ident(b) => Some(b.name.clone()),
                _ => None,
            };
            // The C value test (None = unconditional catch-all). Or-patterns OR the
            // alternatives' tests together.
            let cond = self.scalar_pat_cond(arm.pat, &tmp);
            if cond.is_none() && guard.is_none() {
                has_uncond_default = true;
            }
            match &cond {
                Some(c) => {
                    self.line(format!("if ({c})"));
                    self.line("{");
                }
                None => self.line("{"),
            }
            self.depth += 1;
            if let Some(b) = bind {
                self.line(format!("{cty} j_{b} = {tmp};"));
            }
            self.emit_guarded_arm(arm.body, guard, ret, &end);
            self.depth -= 1;
            self.line("}");
        }
        if !ret {
            self.line(format!("{end}: ;"));
        } else if !has_uncond_default {
            self.line("__builtin_unreachable();");
        }
    }

    // --- nested-pattern dispatch (the decision-tree backend) ---

    /// Does this pattern contain a *nested* sub-pattern the flat switch/if-chain
    /// paths can't dispatch — a variant/literal/range/nullary-variant inside a
    /// variant's fields? (A plain binding, `_`, or `..` field is still flat.)
    fn pat_needs_nesting(&self, pat: PatId) -> bool {
        match &self.ast.pat_at(pat).kind {
            PatKind::Variant { subpats, .. } => subpats.iter().any(|sp| !self.is_flat_subpat(*sp)),
            // A struct-variant pattern is always matched by name → the recursive path.
            PatKind::StructVariant { .. } => true,
            PatKind::Or(alts) => alts.iter().any(|a| self.pat_needs_nesting(*a)),
            _ => false,
        }
    }

    fn is_flat_subpat(&self, sp: PatId) -> bool {
        match &self.ast.pat_at(sp).kind {
            PatKind::Wildcard | PatKind::Rest => true,
            PatKind::Ident(n) => !self.variants.contains_key(&self.canon_variant(&n.name)), // a binding, not a variant
            _ => false,
        }
    }

    fn pat_is_constructor(&self, pat: PatId) -> bool {
        match &self.ast.pat_at(pat).kind {
            PatKind::Variant { .. }
            | PatKind::StructVariant { .. }
            | PatKind::Lit(_)
            | PatKind::Range { .. } => true,
            PatKind::Ident(n) => self.variants.contains_key(&self.canon_variant(&n.name)), // a nullary variant
            PatKind::Or(alts) => alts.iter().any(|a| self.pat_is_constructor(*a)),
            _ => false,
        }
    }

    fn niche_of_ty(&self, t: &Ty) -> Option<NicheInfo> {
        match t {
            Ty::Named(i) => self.niche_enum_at(*i),
            Ty::GenEnum { ctor, args } => self.niche_enum_instance(ctor, args),
            _ => None,
        }
    }

    /// The C tag-enum prefix for a (non-niche) enum type.
    fn enum_tag_prefix(&self, subject_ty: &Ty) -> String {
        match subject_ty {
            Ty::Named(i) => format!("Jestyr_{}", self.info.table.types[*i].name),
            Ty::GenEnum { ctor, args } => self.gen_struct_c_name(ctor, args),
            _ => String::new(),
        }
    }

    /// The C boolean test that `subject` (of enum type `subject_ty`) is variant
    /// `vname` — a tag comparison, or a null test for a niche enum.
    fn variant_tag_test(&mut self, subject: &str, subject_ty: &Ty, vname: &str) -> String {
        if let Some(n) = self.niche_of_ty(subject_ty) {
            let cty = self.c_type(&n.payload);
            return if vname == n.some_variant {
                format!("{subject} != (({cty})0)")
            } else {
                format!("{subject} == (({cty})0)")
            };
        }
        let prefix = self.enum_tag_prefix(subject_ty);
        format!("{subject}.tag == {prefix}_{vname}")
    }

    /// The C l-value path and Jestyr type of field `i` of variant `vname` reached
    /// through `subject`. For a niche enum the lone payload *is* the subject.
    fn variant_field(
        &mut self,
        subject: &str,
        subject_ty: &Ty,
        vname: &str,
        i: usize,
    ) -> Option<(String, Ty)> {
        if let Some(n) = self.niche_of_ty(subject_ty) {
            if vname == n.some_variant && i == 0 {
                return Some((subject.to_string(), n.payload.clone()));
            }
            return None;
        }
        let vi = self.variants.get(&self.canon_variant(vname))?.clone();
        let (fname, fty_id) = vi.fields.get(i)?;
        let subst = match subject_ty {
            Ty::GenEnum { ctor, args } => self.gen_enum_subst(ctor, args),
            _ => HashMap::new(),
        };
        let fty = self.ast_type_to_ty(*fty_id, &subst);
        Some((format!("{subject}.u.{vname}.j_{fname}"), fty))
    }

    /// [`variant_field`] but selecting the field by *name* (for struct-variant
    /// patterns `circle { r }`).
    fn variant_field_by_name(
        &mut self,
        subject: &str,
        subject_ty: &Ty,
        vname: &str,
        fieldname: &str,
    ) -> Option<(String, Ty)> {
        if let Some(n) = self.niche_of_ty(subject_ty) {
            if vname == n.some_variant {
                return Some((subject.to_string(), n.payload.clone()));
            }
            return None;
        }
        let vi = self.variants.get(&self.canon_variant(vname))?.clone();
        let (fname, fty_id) = vi.fields.iter().find(|(f, _)| f == fieldname)?;
        let subst = match subject_ty {
            Ty::GenEnum { ctor, args } => self.gen_enum_subst(ctor, args),
            _ => HashMap::new(),
        };
        let fty = self.ast_type_to_ty(*fty_id, &subst);
        Some((format!("{subject}.u.{vname}.j_{fname}"), fty))
    }

    /// Compile a pattern to `(C boolean test, binding statements)` against the
    /// value at C-expression `subject` of type `subject_ty`. `"1"` means the
    /// pattern is irrefutable (a wildcard/binding). Recurses into sub-patterns,
    /// auto-dereferencing pointer fields (so `node(leaf(_), ..)` looks *through*
    /// an `indirect Tree`).
    fn pat_test(&mut self, subject: &str, subject_ty: &Ty, pat: PatId) -> (String, Vec<String>) {
        let ast = self.ast;
        match &ast.pat_at(pat).kind {
            PatKind::Wildcard | PatKind::Rest | PatKind::Error => ("1".to_string(), vec![]),
            PatKind::Ident(n) if !self.variants.contains_key(&self.canon_variant(&n.name)) => {
                let cty = self.c_type(subject_ty);
                ("1".to_string(), vec![format!("{cty} j_{} = {subject};", n.name)])
            }
            PatKind::Ident(n) => (self.variant_tag_test(subject, subject_ty, &n.name), vec![]),
            PatKind::Variant { name, subpats } => {
                let mut tests = vec![self.variant_tag_test(subject, subject_ty, &name.name)];
                let mut binds = vec![];
                for (i, sp) in subpats.iter().enumerate() {
                    if matches!(ast.pat_at(*sp).kind, PatKind::Rest) {
                        break; // trailing `..` ignores the remaining fields
                    }
                    if let Some((fpath, fty)) =
                        self.variant_field(subject, subject_ty, &name.name, i)
                    {
                        let (t, b) = self.pat_test_auto(&fpath, &fty, *sp);
                        if t != "1" {
                            tests.push(t);
                        }
                        binds.extend(b);
                    }
                }
                (tests.join(" && "), binds)
            }
            PatKind::StructVariant { name, fields, .. } => {
                // Like `Variant`, but fields are matched by name (omitted fields and
                // `..` are simply not tested/bound).
                let mut tests = vec![self.variant_tag_test(subject, subject_ty, &name.name)];
                let mut binds = vec![];
                for (fname, subpat) in fields {
                    if let Some((fpath, fty)) =
                        self.variant_field_by_name(subject, subject_ty, &name.name, &fname.name)
                    {
                        let (t, b) = self.pat_test_auto(&fpath, &fty, *subpat);
                        if t != "1" {
                            tests.push(t);
                        }
                        binds.extend(b);
                    }
                }
                (tests.join(" && "), binds)
            }
            PatKind::Lit(e) => {
                let lc = self.emit_expr(*e);
                (format!("{subject} == ({lc})"), vec![])
            }
            PatKind::Range { lo, hi, inclusive } => {
                let loc = self.emit_expr(*lo);
                let hic = self.emit_expr(*hi);
                let op = if *inclusive { "<=" } else { "<" };
                (format!("{subject} >= ({loc}) && {subject} {op} ({hic})"), vec![])
            }
            PatKind::Or(alts) => {
                let mut tests = vec![];
                for a in alts {
                    let (t, b) = self.pat_test(subject, subject_ty, *a);
                    if !b.is_empty() {
                        self.diag(
                            ast.pat_at(*a).span,
                            "or-pattern alternatives can't bind values in a nested `match` yet",
                        );
                    }
                    tests.push(format!("({t})"));
                }
                (tests.join(" || "), vec![])
            }
        }
    }

    /// [`pat_test`], auto-dereferencing a pointer field when matching a constructor
    /// pattern against it (so a recursive `indirect`/`*T` field is looked through).
    fn pat_test_auto(&mut self, subject: &str, ty: &Ty, pat: PatId) -> (String, Vec<String>) {
        if self.pat_is_constructor(pat) {
            if let Some(inner) = pointer_pointee(ty) {
                let deref = format!("(*{subject})");
                return self.pat_test(&deref, &inner, pat);
            }
        }
        self.pat_test(subject, ty, pat)
    }

    /// Lower a `match` containing nested patterns to an ordered if-chain whose arm
    /// conditions are recursive pattern tests. Guards compose (the same
    /// `emit_guarded_arm`); a fired arm `goto`s the end label or `return`s.
    fn emit_nested_match(&mut self, e: ExprId, ret: bool, scrut_ty: &Ty) {
        let ast = self.ast;
        let (scrut, arms) = match &ast.expr_at(e).kind {
            ExprKind::Match { scrut, arms } => (*scrut, arms),
            _ => return,
        };
        let scrut_c = self.emit_expr(scrut);
        let cty = self.c_type(scrut_ty);
        let tmp = format!("jm_{}", self.tmp);
        self.tmp += 1;
        let end = format!("jm_end_{}", self.tmp);
        self.tmp += 1;
        self.line(format!("{cty} {tmp} = {scrut_c};"));

        let mut has_uncond_default = false;
        for arm in arms {
            let (test, binds) = self.pat_test(&tmp, scrut_ty, arm.pat);
            let uncond = test == "1";
            if uncond && arm.guard.is_none() {
                has_uncond_default = true;
            }
            if uncond {
                self.line("{");
            } else {
                self.line(format!("if ({test})"));
                self.line("{");
            }
            self.depth += 1;
            for b in binds {
                self.line(b);
            }
            self.emit_guarded_arm(arm.body, arm.guard, ret, &end);
            self.depth -= 1;
            self.line("}");
        }
        if !ret {
            self.line(format!("{end}: ;"));
        } else if !has_uncond_default {
            self.line("__builtin_unreachable();");
        }
    }

    fn emit_arm_body(&mut self, body: ExprId, ret: bool) {
        let ast = self.ast;
        match &ast.expr_at(body).kind {
            ExprKind::If { .. } => self.emit_if(body, ret),
            ExprKind::Match { .. } => self.emit_match(body, ret),
            ExprKind::Block(b) => self.emit_body(b, ret),
            ExprKind::Unsafe(b) => self.emit_body(b, ret),
            _ => {
                let v = self.emit_expr(body);
                if ret {
                    self.emit_value_return(v);
                } else {
                    self.line(format!("{v};"));
                }
            }
        }
    }

    /// Is `name` a generic enum (a monomorphizable template)?
    fn enum_is_generic(&self, name: &str) -> bool {
        self.find_generic_enum(name).is_some()
    }

    /// Emit a two-`str`-argument string operation `helper(a, b)` (equality,
    /// prefix/suffix, search).
    fn emit_str_binop(&mut self, helper: &str, args: &[ExprId]) -> String {
        let a = args.first().map(|x| self.emit_expr(*x)).unwrap_or_else(|| "(JestyrStr){0,0}".to_string());
        let b = args.get(1).map(|x| self.emit_expr(*x)).unwrap_or_else(|| "(JestyrStr){0,0}".to_string());
        format!("{helper}({a}, {b})")
    }

    /// The fields of a (non-generic) struct that declare a `= <expr>` default,
    /// by name — used to fill omitted fields in a struct literal. Defaults should
    /// be constant expressions (they're emitted at each construction site).
    fn struct_field_defaults(&self, name: &str) -> Vec<(String, ExprId)> {
        for item in &self.ast.items {
            if let Item::Struct { name: sname, body, .. } = item {
                if sname.name == name {
                    return body
                        .members
                        .iter()
                        .filter_map(|m| match m {
                            StructMember::Field { name: fname, default: Some(d), .. } => {
                                Some((fname.name.clone(), *d))
                            }
                            _ => None,
                        })
                        .collect();
                }
            }
        }
        Vec::new()
    }

    /// Construct an enum variant from a *named*-field literal, `circle { r: 2.0 }`.
    /// Like [`emit_variant_construct`] but the fields are designated by name (so
    /// order doesn't matter and the niche/generic cases are handled the same way).
    fn emit_struct_variant_construct(
        &mut self,
        id: ExprId,
        vname: &str,
        fields: &[FieldInit],
    ) -> String {
        let vi = match self.variants.get(&self.canon_variant(vname)).cloned() {
            Some(v) => v,
            None => return "0".to_string(),
        };
        // A generic-enum instance: the instantiation comes from the inferred type,
        // resolved through the active monomorphization substitution (so an `Option(U)`
        // built inside a generic function names the concrete `Option(i32)` instance).
        let inferred = self.info.type_of(id).clone();
        if let Ty::GenEnum { ctor, args } = apply_subst(&inferred, &self.subst) {
            if !args.iter().all(Self::is_concrete) {
                let sp = self.ast.expr_at(id).span;
                self.diag(sp, format!("cannot infer the type arguments of generic enum `{ctor}` here"));
                return "0".to_string();
            }
            if let Some(n) = self.niche_enum_instance(&ctor, &args) {
                if vname == n.some_variant {
                    return fields.first().map(|fi| self.emit_expr(fi.value)).unwrap_or_else(|| "0".to_string());
                }
                let pcty = self.c_type(&n.payload);
                return format!("(({pcty})0)");
            }
            let cname = self.gen_struct_c_name(&ctor, &args);
            return self.struct_variant_literal(&cname, &cname, vname, fields);
        }
        // A niche-optimized (non-generic) enum: the value *is* the payload pointer.
        if let Some(n) = self.niche_enum_named(&vi.enum_name) {
            if vname == n.some_variant {
                return fields.first().map(|fi| self.emit_expr(fi.value)).unwrap_or_else(|| "0".to_string());
            }
            let cty = self.c_type(&n.payload);
            return format!("(({cty})0)");
        }
        let prefix = format!("Jestyr_{}", vi.enum_name);
        self.struct_variant_literal(&prefix, &prefix, vname, fields)
    }

    /// Emit a tagged-union literal with designated field initializers.
    fn struct_variant_literal(
        &mut self,
        cname: &str,
        prefix: &str,
        vname: &str,
        fields: &[FieldInit],
    ) -> String {
        let mut s = format!("({cname}){{ .tag = {prefix}_{vname}");
        if !fields.is_empty() {
            let _ = write!(s, ", .u.{vname} = {{ ");
            for (i, fi) in fields.iter().enumerate() {
                if i > 0 {
                    s.push_str(", ");
                }
                let v = self.emit_expr(fi.value);
                let _ = write!(s, ".j_{} = {v}", fi.name.name);
            }
            s.push_str(" }");
        }
        s.push_str(" }");
        s
    }

    fn emit_variant_construct(
        &mut self,
        construct_id: ExprId,
        vi: &VariantInfo,
        vname: &str,
        args: &[ExprId],
    ) -> String {
        // A generic-enum instance: the instantiation comes from this expression's
        // inferred type (`Option(i32)`), so the right monomorphized struct is used.
        // Inside a generic function the inferred type is `Option(U)` with `U` still
        // opaque; the active monomorphization substitution resolves it to a concrete
        // instance (`{U -> i32}` => `Option(i32)`) before we check or name it.
        let inferred = self.info.type_of(construct_id).clone();
        if let Ty::GenEnum { ctor, args: targs } = apply_subst(&inferred, &self.subst) {
            if !targs.iter().all(Self::is_concrete) {
                self.diag(
                    self.ast.expr_at(construct_id).span,
                    format!("cannot infer the type arguments of generic enum `{ctor}` here"),
                );
                return "0".to_string();
            }
            // Niche instance → bare pointer (`some` is the value, `none` is NULL).
            if let Some(n) = self.niche_enum_instance(&ctor, &targs) {
                if vname == n.some_variant {
                    return args.first().map(|a| self.emit_expr(*a)).unwrap_or_else(|| "0".into());
                }
                let pcty = self.c_type(&n.payload);
                return format!("(({pcty})0)");
            }
            let cname = self.gen_struct_c_name(&ctor, &targs);
            let mut s = format!("({cname}){{ .tag = {cname}_{vname}");
            if !args.is_empty() {
                let _ = write!(s, ", .u.{vname} = {{ ");
                for (i, a) in args.iter().enumerate() {
                    if i > 0 {
                        s.push_str(", ");
                    }
                    let e = self.emit_expr(*a);
                    s.push_str(&e);
                }
                s.push_str(" }");
            }
            s.push_str(" }");
            return s;
        }
        // Niche-optimized (non-generic) enum: the value *is* the pointer payload.
        if let Some(n) = self.niche_enum_named(&vi.enum_name) {
            if vname == n.some_variant {
                return args.first().map(|a| self.emit_expr(*a)).unwrap_or_else(|| "0".to_string());
            }
            let cty = self.c_type(&n.payload);
            return format!("(({cty})0)");
        }
        let mut s = format!("(Jestyr_{}){{ .tag = Jestyr_{}_{vname}", vi.enum_name, vi.enum_name);
        if !args.is_empty() {
            let _ = write!(s, ", .u.{vname} = {{ ");
            for (i, a) in args.iter().enumerate() {
                if i > 0 {
                    s.push_str(", ");
                }
                let e = self.emit_expr(*a);
                s.push_str(&e);
            }
            s.push_str(" }");
        }
        s.push_str(" }");
        s
    }

    // --- expressions ---

    fn emit_expr(&mut self, id: ExprId) -> String {
        // A value coerced to `dyn Trait` is wrapped into a `{ data, vtable }` fat
        // pointer (Stage F). The guard lets `emit_dyn_coercion` re-emit the
        // underlying concrete value through this same path without re-wrapping.
        if !self.dyn_guard.contains(&id) {
            if let Some(tr) = self.info.dyn_coercions.get(&id).cloned() {
                return self.emit_dyn_coercion(id, &tr);
            }
        }
        let ast = self.ast;
        let data = ast.expr_at(id);
        let span = data.span;
        match &data.kind {
            ExprKind::Int(l) => c_int_literal(l),
            // A `comptime` block reaches C as its VALUE. Re-evaluating here rather
            // than reading a side table set by typeck is the same choice `array_len`
            // already makes: the interpreter is pure and total, so a second run is
            // guaranteed to agree with the first, and no phase has to carry state.
            //
            // Total, like every cgen path: typeck has already reported an unevaluable
            // block, so this cannot be the first place a user hears about it.
            ExprKind::Comptime(_) => match crate::comptime::Interp::new(ast).eval(id) {
                // An aggregate fills a fresh array value, exactly as a written
                // `[a, b, …]` does — the two are indistinguishable in the output.
                // (At a `const` initializer this path is bypassed for the brace form;
                // see `consts`, which is what a large lookup table needs.)
                Ok(comptime::Value::List(items)) => {
                    let ty = apply_subst(&self.info.type_of(id).clone(), &self.subst);
                    let aty = match &ty {
                        Ty::Array { .. } => self.c_type(&ty),
                        _ => "int".to_string(),
                    };
                    let n = self.tmp;
                    self.tmp += 1;
                    let mut s = format!("({{ {aty} _cl{n};");
                    for (i, it) in items.iter().enumerate() {
                        let _ = write!(s, " _cl{n}.a[{i}] = ({});", c_comptime_scalar(it));
                    }
                    let _ = write!(s, " _cl{n}; }})");
                    s
                }
                Ok(v) => c_comptime_scalar(&v),
                Err(_) => "0".to_string(),
            },
            ExprKind::Float(l) => l.chars().filter(|c| *c != '_').collect(),
            // A string literal is a length-carrying view; `JSTR` snapshots its
            // compile-time byte length via `sizeof(lit) - 1`.
            ExprKind::Str(l) => format!("JSTR({l})"),
            ExprKind::FString { parts, exprs } => self.emit_fstring(parts, exprs),
            ExprKind::Char(l) => l.clone(),
            ExprKind::Bool(b) => if *b { "true" } else { "false" }.to_string(),
            ExprKind::Null => "NULL".to_string(),
            ExprKind::Name(n) => {
                // inside a lifted closure body, a captured name lives in the env
                if self.capture_set.contains(&n.name) {
                    return format!("(j__env->j_{})", n.name);
                }
                // a bare name that is a *nullary* enum variant, e.g. `none`. A
                // payload-bearing variant referenced bare is not a construction
                // (it would be called), so it must not shadow a same-named local.
                if let Some(vi) = self.variants.get(&self.canon_variant(&n.name)).cloned() {
                    if vi.fields.is_empty() {
                        let vname = n.name.clone();
                        return self.emit_variant_construct(id, &vi, &vname, &[]);
                    }
                }
                if self.ptr_params.contains(&n.name) {
                    format!("(*j_{})", n.name)
                } else if self.no_mangle_consts.contains(&n.name) {
                    // A `@no_mangle` const is referenced by its bare exported name.
                    n.name.clone()
                } else {
                    format!("j_{}", n.name)
                }
            }
            ExprKind::SelfValue => {
                if self.self_cty.is_empty() {
                    self.diag(span, "the C backend does not support `self` outside a method yet");
                    "0".to_string()
                } else if self.self_is_ptr {
                    "(*j_self)".to_string()
                } else {
                    "j_self".to_string()
                }
            }
            ExprKind::SelfType => {
                self.diag(span, "the C backend does not support `Self` as a value yet");
                "0".to_string()
            }
            ExprKind::Attr(_) => {
                self.diag(span, "the C backend does not support attributes yet");
                "0".to_string()
            }
            ExprKind::Unary { op, rhs } => {
                // `&<fn>` — taking the address of a *top-level function* yields a
                // thin function-pointer value (the function's mangled C symbol,
                // or the bare name for an `extern "c"` callee). This is the
                // explicit way to obtain a fn-pointer; a function name is not a
                // `j_`-prefixed local, so the generic `&j_name` path is wrong here.
                if matches!(op, UnOp::Ref) {
                    if let ExprKind::Name(n) = &self.ast.expr_at(*rhs).kind {
                        if self.extern_fns.contains(&n.name) {
                            return format!("(&{})", n.name);
                        }
                        // Canonical name for a colliding function referenced by
                        // address (the checker recorded it on the name expr);
                        // bare otherwise, so non-colliding `&fn` is unchanged.
                        let cname =
                            self.info.call_sym.get(rhs).cloned().unwrap_or_else(|| n.name.clone());
                        if self.info.table.fns.contains_key(&cname) {
                            return format!("(&{})", self.c_fn_name(&cname));
                        }
                    }
                }
                let r = self.emit_expr(*rhs);
                format!("({}{r})", unop_c(*op))
            }
            ExprKind::Binary { op, lhs, rhs } => {
                // Operator traits (Stage E): a binary op the type checker resolved
                // through `impl <OpTrait> for <lhs>` lowers to a direct call of the
                // impl method (`lhs` is the receiver, `rhs` the argument).
                if let Some(ic) = self.info.impl_calls.get(&id).cloned() {
                    return self.emit_operator_call(&ic, *op, *lhs, *rhs);
                }
                let l = self.emit_expr(*lhs);
                let r = self.emit_expr(*rhs);
                format!("({l} {} {r})", binop_c(*op))
            }
            ExprKind::Assign { op, target, value } => {
                // A target that reaches *through* a checked index (`xs[i].f = v`,
                // `m[i][j] = v`) is a place `emit_expr` cannot produce at all — it
                // would yield the statement-expression *value* and gcc would report
                // "lvalue required as left operand of assignment". `emit_place`
                // lowers the whole projection chain through element addresses.
                if self.place_through_checked_index(*target) {
                    let t = self.emit_place(*target, true);
                    let v = self.emit_expr(*value);
                    return format!("{t} {} {v}", assign_c(*op));
                }
                // A slice-index target (`s[i] = v`) needs an *lvalue* — but `emit_expr`
                // on an `Index` yields the bounds-checked *statement-expression*
                // (an rvalue). Emit the bounds check then assign through the element
                // pointer. (The slice is spilled to a temp so a side-effecting base is
                // evaluated once; copying the `{ptr,len}` view still writes the buffer.)
                if let ExprKind::Index { base, index } = &self.ast.expr_at(*target).kind {
                    let bt = apply_subst(&self.info.type_of(*base).clone(), &self.subst);
                    if matches!(bt, Ty::Slice(_)) {
                        let aop = assign_c(*op);
                        let proven = self.index_in_range(*base, *index);
                        let b = self.emit_expr(*base);
                        let i = self.emit_expr(*index);
                        let v = self.emit_expr(*value);
                        if proven {
                            return format!("({b}).ptr[({i})] {aop} {v}");
                        }
                        let sty = self.c_type(&bt);
                        let n = self.tmp;
                        self.tmp += 1;
                        return format!(
                            "({{ {sty} _s{n} = ({b}); size_t _ix{n} = (size_t)({i}); assert(_ix{n} < _s{n}.len); _s{n}.ptr[_ix{n}] {aop} {v}; }})"
                        );
                    }
                    // `arr[i] = v` — assign into the array's inline field, bounds-checked
                    // against the constant length, through the array's address (no copy).
                    if let Ty::Array { len, .. } = &bt {
                        let nlen = *len;
                        let aop = assign_c(*op);
                        let aty = self.c_type(&bt);
                        let b = self.emit_expr(*base);
                        let i = self.emit_expr(*index);
                        let v = self.emit_expr(*value);
                        let n = self.tmp;
                        self.tmp += 1;
                        return format!(
                            "({{ {aty}* _a{n} = &({b}); size_t _ix{n} = (size_t)({i}); assert(_ix{n} < {nlen}); _a{n}->a[_ix{n}] {aop} {v}; }})"
                        );
                    }
                }
                let t = self.emit_expr(*target);
                let v = self.emit_expr(*value);
                format!("{t} {} {v}", assign_c(*op))
            }
            ExprKind::Call { callee, args } => self.emit_call(id, *callee, args),
            ExprKind::Field { base, name } => {
                // Module-qualified const (`mem.PAGE`): emit the const directly.
                if let Some(qname) = self.info.qualified.get(&id).cloned() {
                    return format!("j_{qname}");
                }
                let bt = apply_subst(&self.info.type_of(*base).clone(), &self.subst);
                // A fixed-size array's `.len` is its constant length (not a struct
                // field). (`base` is a place expression in practice, so not emitting it
                // loses no side effect.)
                if let Ty::Array { len, .. } = &bt {
                    if name.name == "len" {
                        return format!("((size_t){len})");
                    }
                }
                let b = self.emit_expr(*base);
                // A slice's `ptr`/`len` are real C fields (not `j_`-prefixed).
                if matches!(bt, Ty::Slice(_)) && (name.name == "len" || name.name == "ptr") {
                    format!("{b}.{}", name.name)
                } else if matches!(bt, Ty::Prim("str")) {
                    // A string view carries its length (O(1)); `.ptr`/`.cstr` expose
                    // the underlying bytes (`.cstr` is null-terminated for a literal).
                    match name.name.as_str() {
                        "len" => format!("{b}.len"),
                        "ptr" | "cstr" => format!("{b}.ptr"),
                        _ => format!("{b}.j_{}", name.name),
                    }
                } else if matches!(bt, Ty::Prim("String")) && name.name == "len" {
                    format!("{b}.len") // an owned String's byte length, O(1)
                } else {
                    format!("{b}.j_{}", name.name)
                }
            }
            ExprKind::Index { base, index } => {
                // Resolve through the active monomorphization subst so a generic
                // `[]T` indexed inside a generic function names `JestyrSlice_i32`.
                let bt = apply_subst(&self.info.type_of(*base).clone(), &self.subst);
                // `s[i..j]` on a string → a boundary-checked, zero-copy sub-view.
                if matches!(bt, Ty::Prim("str")) {
                    let range = match &self.ast.expr_at(*index).kind {
                        ExprKind::Range { lo, hi, inclusive } => Some((*lo, *hi, *inclusive)),
                        _ => None,
                    };
                    if let Some((lo, hi, inclusive)) = range {
                        let b = self.emit_expr(*base);
                        let lo_c = lo.map(|e| self.emit_expr(e)).unwrap_or_else(|| "0".to_string());
                        let hi_c = match hi {
                            Some(e) => {
                                let h = self.emit_expr(e);
                                if inclusive { format!("(({h}) + 1)") } else { h }
                            }
                            None => format!("({b}).len"),
                        };
                        return format!("jestyr_rt_substr({b}, {lo_c}, {hi_c})");
                    }
                }
                let proven = matches!(bt, Ty::Slice(_)) && self.index_in_range(*base, *index);
                // An array index takes `&base`, so the base has to be a *place*: for
                // `m[i][j]` the inner `m[i]` is itself a checked index and `&({ … })`
                // is "lvalue required as unary '&' operand". Every other base emits
                // exactly as before (`emit_place` falls through to `emit_expr`).
                let b = if matches!(bt, Ty::Array { .. }) {
                    self.emit_place(*base, false)
                } else {
                    self.emit_expr(*base)
                };
                let i = self.emit_expr(*index);
                if matches!(bt, Ty::Prim("str")) {
                    // A string view indexes into its byte buffer.
                    format!("((uint8_t)({b}).ptr[({i})])")
                } else if let Ty::Array { len, .. } = &bt {
                    // A fixed-size array indexes its inline `a[N]` field, bounds-checked
                    // against the constant length. We take the array's *address* (not a
                    // copy) so reading one element never copies the whole array. The
                    // pointer is `const` (this is a read) so indexing a `const` table
                    // does not discard the qualifier.
                    let nlen = *len;
                    let aty = self.c_type(&bt);
                    let n = self.tmp;
                    self.tmp += 1;
                    format!(
                        "({{ const {aty}* _a{n} = &({b}); size_t _ix{n} = (size_t)({i}); assert(_ix{n} < {nlen}); _a{n}->a[_ix{n}]; }})"
                    )
                } else if !matches!(bt, Ty::Slice(_)) {
                    format!("({b})[{i}]")
                } else if proven {
                    // Refinement proved `i < s.len` → elide the bounds check.
                    format!("(({b}).ptr[({i})])")
                } else {
                    // A faulting (bounds-checked) index is forbidden in @no_panic.
                    if self.cur_no_panic {
                        self.diag(
                            span,
                            "indexing may fault in a `@no_panic` function — iterate with `for i in 0..s.len { … }` so the index is provably in range",
                        );
                    }
                    // Bounds-checked: spill the slice once, assert, then index.
                    let sty = self.c_type(&bt);
                    let n = self.tmp;
                    self.tmp += 1;
                    format!(
                        "({{ {sty} _s{n} = ({b}); size_t _ix{n} = (size_t)({i}); assert(_ix{n} < _s{n}.len); _s{n}.ptr[_ix{n}]; }})"
                    )
                }
            }
            ExprKind::ArrayRepeat { value, count } => {
                // `[v; N]` — a fixed-size array value. Evaluate `v` once into a temp,
                // then fill all N elements (a statement-expression yielding the array).
                let ty = apply_subst(&self.info.type_of(id).clone(), &self.subst);
                let (aty, ecty, nlen) = match &ty {
                    Ty::Array { elem, len } => (self.c_type(&ty), self.c_type(elem), *len),
                    _ => ("int".to_string(), "int".to_string(), self.array_len(*count)),
                };
                let v = self.emit_expr(*value);
                let n = self.tmp;
                self.tmp += 1;
                format!(
                    "({{ {aty} _ar{n}; {ecty} _v{n} = ({v}); for (size_t _k{n} = 0; _k{n} < {nlen}; _k{n}++) _ar{n}.a[_k{n}] = _v{n}; _ar{n}; }})"
                )
            }
            ExprKind::ArrayLit { elems } => {
                // `[e0, e1, …]` — fill each element of a fresh array value (a
                // statement-expression yielding the array). At a `const`/static
                // initializer this path is bypassed for a brace initializer (see
                // `consts`), which is the form a large lookup table needs.
                let ty = apply_subst(&self.info.type_of(id).clone(), &self.subst);
                let aty = match &ty {
                    Ty::Array { .. } => self.c_type(&ty),
                    _ => "int".to_string(),
                };
                let n = self.tmp;
                self.tmp += 1;
                let mut s = format!("({{ {aty} _al{n};");
                for (i, e) in elems.iter().enumerate() {
                    let v = self.emit_expr(*e);
                    let _ = write!(s, " _al{n}.a[{i}] = ({v});");
                }
                let _ = write!(s, " _al{n}; }})");
                s
            }
            ExprKind::Cast { expr, ty } => {
                let cty = self.c_ty_ast(*ty);
                let src_ty = self.info.type_of(*expr).clone();
                let e = self.emit_expr(*expr);
                // Casting a tagged enum to an integer reads its discriminant
                // (the tag), which carries any explicit `= value`.
                if self.is_tagged_enum(&src_ty) {
                    format!("({cty})(({e}).tag)")
                } else {
                    format!("({cty})({e})")
                }
            }
            ExprKind::Deref { base } => {
                let bt = self.info.type_of(*base).clone();
                let b = self.emit_expr(*base);
                if let Ty::GenRef(_) = bt {
                    // Generational deref: fault if the allocation's generation no
                    // longer matches the reference's snapshot (use-after-free).
                    let cty = self.c_type(&bt);
                    let n = self.tmp;
                    self.tmp += 1;
                    format!(
                        "({{ {cty} _r{n} = ({b}); assert(((uint64_t*)_r{n}.ptr)[-1] == _r{n}.gen); *_r{n}.ptr; }})"
                    )
                } else {
                    format!("(*{b})")
                }
            }
            ExprKind::StructLit { path, fields, spread } => {
                if path.name == "Self" {
                    self.diag(span, "the C backend does not support `Self { .. }` (methods) yet");
                    return "0".to_string();
                }
                // `circle { r: 2.0 }` — a *struct-variant construction*: the path is
                // an enum variant, not a struct type.
                if self.variants.contains_key(&self.canon_variant(&path.name)) {
                    return self.emit_struct_variant_construct(id, &path.name, fields);
                }
                // `Point { x: 9, ..p }` — functional update: copy `p`, then assign the
                // listed fields. A GNU statement-expression keeps it an expression.
                if let Some(sp) = spread {
                    let base = self.emit_expr(*sp);
                    let tmp = format!("jss_{}", self.tmp);
                    self.tmp += 1;
                    let mut s = format!("({{ Jestyr_{} {tmp} = {base}; ", self.canon_type(&path.name));
                    for fi in fields {
                        let v = self.emit_expr(fi.value);
                        let _ = write!(s, "{tmp}.j_{} = {v}; ", fi.name.name);
                    }
                    let _ = write!(s, "{tmp}; }})");
                    return s;
                }
                let mut s = format!("(Jestyr_{}){{ ", self.canon_type(&path.name));
                let mut first = true;
                for fi in fields {
                    if !first {
                        s.push_str(", ");
                    }
                    first = false;
                    let v = self.emit_expr(fi.value);
                    let _ = write!(s, ".j_{} = {v}", fi.name.name);
                }
                // Fill any omitted field that declares a default `= <expr>`.
                for (fname, dexpr) in self.struct_field_defaults(&path.name) {
                    if fields.iter().any(|fi| fi.name.name == fname) {
                        continue;
                    }
                    if !first {
                        s.push_str(", ");
                    }
                    first = false;
                    let v = self.emit_expr(dexpr);
                    let _ = write!(s, ".j_{fname} = {v}");
                }
                s.push_str(" }");
                s
            }
            ExprKind::GenStructLit { ctor, type_args, fields } => {
                let subst = self.subst.clone();
                let args: Vec<Ty> = type_args.iter().map(|a| self.eval_type_arg(*a, &subst)).collect();
                let cname = self.gen_struct_c_name(&ctor.name, &args);
                let mut s = format!("({cname}){{ ");
                for (i, fi) in fields.iter().enumerate() {
                    if i > 0 {
                        s.push_str(", ");
                    }
                    let v = self.emit_expr(fi.value);
                    let _ = write!(s, ".j_{} = {v}", fi.name.name);
                }
                s.push_str(" }");
                s
            }
            ExprKind::Try { base } => {
                // Lower `e?` to a statement-expression that early-returns on error
                // and yields the ok value otherwise. (GCC/Clang extension.)
                if self.cur_result.is_empty() {
                    self.diag(span, "`?` used outside a fallible function");
                    return self.emit_expr(*base);
                }
                let base_c = self.emit_expr(*base);
                let res_ty = self.c_type(&self.info.type_of(*base).clone());
                let cur = self.cur_result.clone();
                let tmp = format!("_q{}", self.tmp);
                self.tmp += 1;
                // `--error-traces`: each `?` records itself as a propagation HOP
                // before the early return, so the surfaced trace reads as the
                // error's path up the stack. The flag-off arm is the ORIGINAL string,
                // character for character — reusing the braced form for both ("just
                // one redundant brace") would change every fallible program's C and
                // invalidate the corpus goldens, the port mirror and the seed at once.
                if self.error_traces {
                    let hop = self.et_push(span);
                    return format!(
                        "({{ {res_ty} {tmp} = {base_c}; if ({tmp}.is_err) {{ {hop}return ({cur}){{ .is_err = true, .err = {tmp}.err }}; }} {tmp}.ok; }})"
                    );
                }
                format!(
                    "({{ {res_ty} {tmp} = {base_c}; if ({tmp}.is_err) return ({cur}){{ .is_err = true, .err = {tmp}.err }}; {tmp}.ok; }})"
                )
            }
            ExprKind::Catch { base, binder, fallback, rethrow } => {
                // `e catch v` — recover. Where `?` early-returns the error, this
                // substitutes a value and carries on, so it needs no enclosing
                // fallible function and emits no `return`.
                //
                // The fallback must be evaluated **only** on the error path — it is a
                // fallback, not a default argument, and computing it eagerly would
                // both cost work and run its side effects on the success path. C's
                // conditional operator gives exactly that short-circuit, so this is a
                // `?:` and not two statements.
                let base_c = self.emit_expr(*base);
                let bt = apply_subst(&self.info.type_of(*base).clone(), &self.subst);
                let res_ty = self.c_type(&bt);
                let tmp = format!("_ct{}", self.tmp);
                self.tmp += 1;

                // `catch |e| return e` — the explicit-propagate form: exactly `?`'s
                // lowering (early return, error tag preserved), with the same
                // requirement, because it returns an error to the caller.
                if *rethrow {
                    if self.cur_result.is_empty() {
                        self.diag(span, "`catch |e| return e` used outside a fallible function");
                        return format!("(({base_c}).ok)");
                    }
                    let cur = self.cur_result.clone();
                    return format!(
                        "({{ {res_ty} {tmp} = {base_c}; if ({tmp}.is_err) return ({cur}){{ .is_err = true, .err = {tmp}.err }}; {tmp}.ok; }})"
                    );
                }

                // `catch |e| fallback` — the binder is a `const int` scoped to the
                // error branch, so the fallback can read the tag. A `?:` cannot carry
                // a declaration, so this form lowers as an if/else over a result
                // variable instead; the binder-less form KEEPS its original `?:`
                // string, character for character — it predates the binder, and
                // rewriting it through this shape would diff `error_catch.jtr`
                // against the port mirror and the seed.
                if let Some(b) = binder {
                    let bname = b.name.clone();
                    let okty = match &bt {
                        Ty::Result(ok) => self.c_type(ok),
                        _ => "int".to_string(),
                    };
                    let fb = self.emit_expr(*fallback);
                    let val = format!("_cv{}", self.tmp);
                    self.tmp += 1;
                    if matches!(bt, Ty::Result(ref ok) if **ok == Ty::Unit) {
                        return format!(
                            "({{ {res_ty} {tmp} = {base_c}; if ({tmp}.is_err) {{ const int j_{bname} = {tmp}.err; (void)j_{bname}; {fb}; }} }})"
                        );
                    }
                    return format!(
                        "({{ {res_ty} {tmp} = {base_c}; {okty} {val}; if ({tmp}.is_err) {{ const int j_{bname} = {tmp}.err; (void)j_{bname}; {val} = ({fb}); }} else {{ {val} = {tmp}.ok; }} {val}; }})"
                    );
                }

                // The result is spilled to a temp so `base` is evaluated once: it is
                // read twice below (`.is_err` and `.ok`), and a call in base position
                // would otherwise run twice.
                let fb = self.emit_expr(*fallback);
                if matches!(bt, Ty::Result(ref ok) if **ok == Ty::Unit) {
                    // A `!E`-only result carries no `ok` member to read, so the value
                    // is the fallback or nothing at all.
                    return format!("({{ {res_ty} {tmp} = {base_c}; if ({tmp}.is_err) {{ {fb}; }} }})");
                }
                format!("({{ {res_ty} {tmp} = {base_c}; {tmp}.is_err ? ({fb}) : {tmp}.ok; }})")
            }
            ExprKind::Range { .. } => {
                self.diag(span, "the C backend does not support ranges yet");
                "0".to_string()
            }
            ExprKind::Match { .. } => {
                self.diag(span, "the C backend does not support `match` yet");
                "0".to_string()
            }
            ExprKind::StructType(_) => {
                self.diag(span, "the C backend does not support type-expressions yet");
                "0".to_string()
            }
            ExprKind::Unsafe(b) | ExprKind::Block(b) => {
                // In *value* position (e.g. `let v = unsafe { (p + i).* }`), a block
                // yields its tail expression's value. `unsafe` is a compile-time
                // permission marker with no runtime effect, so it lowers to exactly
                // its inner value — that is the B4 self-host unblock (an `unsafe`
                // block is now a valid `let`/`var` initializer). We support the
                // single-tail-expression form (by far the common case, and all the
                // self-host readers need); a value block with leading statements
                // would need a GNU statement-expression with drop-safe spilling and
                // stays a clear error for now.
                if let [Stmt::Expr(e)] = b.stmts.as_slice() {
                    self.emit_expr(*e)
                } else {
                    self.diag(
                        span,
                        "a block used as a value must be a single tail expression here \
                         (only statement/return position supports multi-statement blocks)",
                    );
                    "0".to_string()
                }
            }
            // An `if`/`else` used as a VALUE. When both arms reduce to a single tail
            // expression it lowers to C's conditional operator — which evaluates exactly
            // one side, so it matches `if` semantics precisely, needs no temporary, and
            // raises no drop question at all.
            //
            // That last part is why this is not the "statement-expression with drop-safe
            // spilling" the multi-statement case still waits for: there is nothing to
            // spill. An arm carrying statements keeps the old diagnostic.
            //
            // It cannot disturb byte-identity either: every program this newly accepts
            // used to be a compile error, so no corpus file can contain one.
            ExprKind::If { cond, then, els } => match (
                Self::value_tail_of_block(then),
                els.and_then(|e| self.value_tail_of_expr(e)),
            ) {
                (Some(t), Some(f)) => {
                    let c = self.emit_expr(*cond);
                    let a = self.emit_expr(t);
                    let b = self.emit_expr(f);
                    format!("(({c}) ? ({a}) : ({b}))")
                }
                _ => {
                    self.diag(span, "this control-flow expression is only supported in statement or return position");
                    "0".to_string()
                }
            },
            ExprKind::Closure { .. } => self.emit_closure_literal(id),
            ExprKind::Concurrent(_) => {
                self.diag(span, "`concurrent` is only supported in statement position");
                "0".to_string()
            }
            ExprKind::Spawn(_) => {
                self.diag(span, "`spawn` may only appear inside a `concurrent` block");
                "0".to_string()
            }
            ExprKind::Await(task) => {
                // `await h`: resolve the handle bound by `let h = spawn …` in the
                // enclosing `concurrent` scope, join it if not already joined (the
                // `_jd` flag guards the brace's safety-net join), and yield `.ret`.
                let name = match &ast.expr_at(*task).kind {
                    ExprKind::Name(n) => Some(n.name.clone()),
                    _ => None,
                };
                match name.and_then(|n| self.task_handles.get(&n).cloned()) {
                    Some(h) => {
                        let i = h.idx;
                        let join = format!("if (!_jd{i}) {{ pthread_join(_jt{i}, NULL); _jd{i} = 1; }}");
                        match h.ret_cty {
                            Some(_) => format!("({{ {join} _ja{i}.ret; }})"),
                            None => format!("({{ {join} }})"),
                        }
                    }
                    None => {
                        self.diag(
                            span,
                            "`await` expects a task handle bound by `let h = spawn …` in the enclosing `concurrent` block",
                        );
                        "0".to_string()
                    }
                }
            }
            ExprKind::ParFor { var, iter, reduction, body } => {
                self.emit_par_for(id, var, *iter, *reduction, *body)
            }
            ExprKind::Select(_) => {
                self.diag(span, "`select` is only supported in statement position");
                "0".to_string()
            }
            ExprKind::Region { .. } => {
                self.diag(span, "`region` is only supported in statement position");
                "0".to_string()
            }
            ExprKind::Break(l) => match l {
                Some(lbl) => format!("goto {}__break", lbl.name),
                // A plain `break` in a loop that has an `else` must `goto` past the
                // `else` block (its target sits after the `else`); otherwise a plain
                // C `break` is exactly right.
                None => match &self.break_label {
                    Some(name) => format!("goto {name}__break"),
                    None => "break".to_string(),
                },
            },
            ExprKind::Continue(l) => match l {
                Some(lbl) => format!("goto {}__continue", lbl.name),
                None => "continue".to_string(),
            },
            ExprKind::Invariant(e) => {
                let c = self.emit_expr(*e);
                format!("assert({c})")
            }
            ExprKind::Variant(e) => {
                // Termination measure: assert it's `>= 0` and strictly less than
                // last iteration's value (tracked in a hoisted `_vt<t>`).
                let inner = self.emit_expr(*e);
                match self.variant_trackers.get(&id).copied() {
                    Some(t) => format!(
                        "({{ int64_t _vv{t} = (int64_t)({inner}); assert(_vv{t} >= 0); assert(_vv{t} < _vt{t}); _vt{t} = _vv{t}; }})"
                    ),
                    None => format!("assert((int64_t)({inner}) >= 0)"),
                }
            }
            ExprKind::For { .. } => {
                self.diag(span, "a loop is only supported in statement or return position");
                "0".to_string()
            }
            ExprKind::Error => "0".to_string(),
        }
    }

    fn emit_call(&mut self, call_id: ExprId, callee: ExprId, args: &[ExprId]) -> String {
        let ast = self.ast;
        // Module-qualified call (`mem.allocate(x)`): the type checker resolved the
        // callee to a plain function; emit a direct call (design §9).
        if let Some(qname) = self.info.qualified.get(&call_id).cloned() {
            return self.emit_named_call(&qname, args);
        }
        // Method-call sugar: the type checker resolved `base.name(args)` to a
        // concrete (possibly monomorphized) function; emit a free call with the
        // receiver threaded in as the first argument.
        if let Some(mr) = self.info.method_calls.get(&call_id).cloned() {
            return self.emit_method_call(callee, &mr, args);
        }
        // `recv.m(args)` resolved through an `impl Trait for <recv>` (traits,
        // Stage C): a direct, statically-dispatched call to the mangled impl
        // method, the receiver threaded in as the first argument.
        if let Some(ic) = self.info.impl_calls.get(&call_id).cloned() {
            return self.emit_impl_call(callee, &ic, args);
        }
        // `x.m(args)` on a *bracket type parameter* `T`, resolved through its bound
        // (the "Zig fix"). The concrete receiver type is `T`'s binding in the
        // *current monomorphization* — so we recover it from `self.subst` and
        // dispatch to `impl <bound> for <that type>`, reusing the Stage C path.
        if let Some(bmc) = self.info.bound_method_calls.get(&call_id).cloned() {
            let concrete = self.subst.get(&bmc.type_param).cloned().unwrap_or(Ty::Unknown);
            let ic = ImplCall {
                trait_name: bmc.trait_name,
                type_key: self.info.table.ty_key(&concrete),
                method: bmc.method,
            };
            return self.emit_impl_call(callee, &ic, args);
        }
        // `d.m(args)` on a `dyn Trait` receiver — a *dynamic* call through the
        // vtable slot: `d.vtable->m(d.data, args)` (traits, Stage F).
        if let Some(method) = self.info.dyn_calls.get(&call_id).cloned() {
            if let ExprKind::Field { base, .. } = &self.ast.expr_at(callee).kind {
                let recv = self.emit_expr(*base);
                let mut parts = vec![format!("{recv}.data")];
                for a in args {
                    parts.push(self.emit_expr(*a));
                }
                return format!("{recv}.vtable->{method}({})", parts.join(", "));
            }
        }
        // Invoking a closure value (a local bound to one, or an inline closure).
        if self.is_closure_typed(callee) {
            return self.emit_closure_invoke(callee, args);
        }
        // Invoking a *thin function-pointer* value — a local/parameter/field whose
        // type is `fn(...) -> R`. We tell this from a direct call to a named
        // function purely by the callee's **type** (the "disambiguate by field
        // type" rule: `a.alloc_fn(x)` is a pointer call because the field's type
        // says so — no `(a.f)()` ceremony). An indirect call needs no `jestyr_`
        // name mangling: the value already *is* the address to jump to.
        if matches!(self.info.type_of(callee), Ty::Fn { .. }) {
            return self.emit_fn_ptr_invoke(callee, args);
        }
        // `@address(0x…)` — a pointer at a fixed address (MMIO; design §16).
        if let ExprKind::Attr(n) = &ast.expr_at(callee).kind {
            if n.name == "address" {
                let addr = args.first().map(|a| self.emit_expr(*a)).unwrap_or_else(|| "0".to_string());
                return format!("((void*)({addr}))");
            }
            // Compile-time reflection (roadmap G tier 3) — `@type_name(T)`,
            // `@field_count(T)`, `@field_name(T, i)`, `@field_type(T, i)`. Unlike
            // `size_of`/`align_of`/`offset_of`, these are answered by *this* compiler
            // rather than deferred to C, so they reach the output as a literal.
            //
            // Total, and never the first place a user hears about a bad query:
            // `check_reflect_call` has already diagnosed it during checking.
            // …and the three layout queries (workstream L), which are the same idea
            // applied to sizes rather than shapes: `@size_of(T)` is answered *here*
            // from `layout.rs`, while the bare `size_of(T)` below still lowers to C's
            // `sizeof`. That split is what lets this land byte-identically — every
            // existing program uses the bare name and emits exactly the C it did before.
            if comptime::is_reflect_intrinsic(&n.name) || comptime::is_layout_intrinsic(&n.name) {
                return match comptime::Interp::new(ast).eval(call_id) {
                    Ok(comptime::Value::Int(i)) => i.to_string(),
                    Ok(comptime::Value::Str(s)) => format!("JSTR({})", c_string_literal(&s)),
                    _ => "0".to_string(),
                };
            }
        }
        if let ExprKind::Name(n) = &ast.expr_at(callee).kind {
            // enum-variant constructor with a payload, e.g. `circle(2.0)`
            if let Some(vi) = self.variants.get(&self.canon_variant(&n.name)).cloned() {
                let vname = n.name.clone();
                return self.emit_variant_construct(call_id, &vi, &vname, args);
            }

            // print intrinsics
            let intrinsic = match n.name.as_str() {
                "print_int" => Some("jestyr_rt_print_int"),
                "print_float" => Some("jestyr_rt_print_float"),
                "print_str" => Some("jestyr_rt_print_str"),
                "print_bool" => Some("jestyr_rt_print_bool"),
                _ => None,
            };
            if let Some(rt) = intrinsic {
                let a = args.first().map(|a| self.emit_expr(*a)).unwrap_or_else(|| "0".to_string());
                return format!("{rt}({a})");
            }

            // Result construction (`ok`/`err`) and inspection (`is_err`/`unwrap`).
            match n.name.as_str() {
                "ok" => {
                    let v = args.first().map(|a| self.emit_expr(*a)).unwrap_or_else(|| "0".to_string());
                    if self.cur_result.is_empty() {
                        self.diag(self.ast.expr_at(callee).span, "`ok` used outside a fallible function");
                        return v;
                    }
                    return format!("({}){{ .is_err = false, .ok = ({v}) }}", self.cur_result);
                }
                "err" => {
                    let tag = args.first().and_then(|a| self.error_tag_of(*a)).unwrap_or(0);
                    if self.cur_result.is_empty() {
                        self.diag(self.ast.expr_at(callee).span, "`err` used outside a fallible function");
                        return "0".to_string();
                    }
                    // `--error-traces`: `err` is the trace's ORIGIN — reset, then
                    // record where the error was born. Reset here rather than at the
                    // surfacing print, so a recovered-then-recreated error never shows
                    // a stale path from a previous failure.
                    if self.error_traces {
                        let push = self.et_push(self.ast.expr_at(callee).span);
                        return format!(
                            "({{ jestyr_et_reset(); {push}({}){{ .is_err = true, .err = {tag} }}; }})",
                            self.cur_result
                        );
                    }
                    return format!("({}){{ .is_err = true, .err = {tag} }}", self.cur_result);
                }
                "is_err" => {
                    let v = args.first().map(|a| self.emit_expr(*a)).unwrap_or_else(|| "0".to_string());
                    return format!("(({v}).is_err)");
                }
                "unwrap" => {
                    // `--error-traces`: unwrap-on-error is the SURFACING point — the
                    // one place the recorded path is printed (to stderr, so hashed
                    // stdout is untouched). The value is spilled so the operand is
                    // evaluated once (`.is_err` then `.ok`), and behaviour otherwise
                    // matches untraced unwrap exactly — same `.ok` read, no abort —
                    // so the flag can never change what a program computes.
                    if self.error_traces {
                        if let Some(a) = args.first() {
                            let rt = self.c_type(&apply_subst(&self.info.type_of(*a).clone(), &self.subst));
                            let v = self.emit_expr(*a);
                            let tmp = format!("_uw{}", self.tmp);
                            self.tmp += 1;
                            return format!(
                                "({{ {rt} {tmp} = {v}; if ({tmp}.is_err) jestyr_et_dump(); {tmp}.ok; }})"
                            );
                        }
                    }
                    let v = args.first().map(|a| self.emit_expr(*a)).unwrap_or_else(|| "0".to_string());
                    return format!("(({v}).ok)");
                }
                // Allocation intrinsics (a stand-in for the allocator/C-interop story).
                "alloc_i32" => {
                    let n = args.first().map(|a| self.emit_expr(*a)).unwrap_or_else(|| "0".to_string());
                    return format!("((int32_t*) malloc((size_t)({n}) * sizeof(int32_t)))");
                }
                "realloc_i32" => {
                    let p = args.first().map(|a| self.emit_expr(*a)).unwrap_or_else(|| "NULL".to_string());
                    let n = args.get(1).map(|a| self.emit_expr(*a)).unwrap_or_else(|| "0".to_string());
                    return format!("((int32_t*) realloc({p}, (size_t)({n}) * sizeof(int32_t)))");
                }
                "free_ptr" => {
                    let p = args.first().map(|a| self.emit_expr(*a)).unwrap_or_else(|| "NULL".to_string());
                    return format!("free({p})");
                }
                // File I/O (self-hosting plumbing). `read_file` yields an owned `String`
                // of the whole file (empty if it can't be opened); `try_read_file`
                // is the *recoverable* form — `String !IoError` — so a compiler can
                // report a missing/unreadable file instead of silently getting "".
                // `write_file`/`file_exists` yield `bool`.
                "read_file" => {
                    let p = args.first().map(|a| self.emit_expr(*a)).unwrap_or_else(|| "(JestyrStr){0,0}".to_string());
                    return format!("jestyr_rt_read_file({p})");
                }
                // `try_read_file(path) -> String !IoError` — the recoverable read.
                // Lowered inline (like `try_from_utf8`) to a statement-expression: the
                // runtime helper reports open/read failure via its bool return and
                // writes the file into an out-param, which we wrap into the tagged
                // result. `.err = 1` is the single `IoError` tag.
                "try_read_file" => {
                    let p = args.first().map(|a| self.emit_expr(*a)).unwrap_or_else(|| "(JestyrStr){0,0}".to_string());
                    let rname = self.result_c_name(&Ty::Prim("String"));
                    return format!(
                        "({{ JestyrString _s; bool _ok = jestyr_rt_try_read_file({p}, &_s); \
                         _ok ? ({rname}){{ .is_err = false, .ok = _s }} \
                             : ({rname}){{ .is_err = true, .err = 1 }}; }})"
                    );
                }
                "run_command" => {
                    let c = args.first().map(|a| self.emit_expr(*a)).unwrap_or_else(|| "(JestyrStr){0,0}".to_string());
                    return format!("jestyr_rt_run_command({c})");
                }
                "eprint_str" => {
                    let s = args.first().map(|a| self.emit_expr(*a)).unwrap_or_else(|| "(JestyrStr){0,0}".to_string());
                    return format!("jestyr_rt_eprint_str({s})");
                }
                "write_file" => {
                    let p = args.first().map(|a| self.emit_expr(*a)).unwrap_or_else(|| "(JestyrStr){0,0}".to_string());
                    let d = args.get(1).map(|a| self.emit_expr(*a)).unwrap_or_else(|| "(JestyrStr){0,0}".to_string());
                    return format!("jestyr_rt_write_file({p}, {d})");
                }
                "file_exists" => {
                    let p = args.first().map(|a| self.emit_expr(*a)).unwrap_or_else(|| "(JestyrStr){0,0}".to_string());
                    return format!("jestyr_rt_file_exists({p})");
                }
                "remove_file" => {
                    let p = args.first().map(|a| self.emit_expr(*a)).unwrap_or_else(|| "(JestyrStr){0,0}".to_string());
                    return format!("jestyr_rt_remove_file({p})");
                }
                // Command-line args. `arg_count()` is argc; `arg(i)` is a `str` view of
                // argv[i] (arg(0) = program path, out-of-range = empty).
                "arg_count" => return "jestyr_rt_arg_count()".to_string(),
                "arg" => {
                    let i = args.first().map(|a| self.emit_expr(*a)).unwrap_or_else(|| "0".to_string());
                    return format!("jestyr_rt_arg((int64_t)({i}))");
                }
                // --- Concurrency (workstream N) ---
                // Atomics: sequentially-consistent ops on an `int64_t` cell, via GCC
                // `__atomic_*` builtins (no `<stdatomic.h>`, no special type). A
                // shared counter incremented from many threads is data-race-free and
                // its final value is deterministic regardless of interleaving — the
                // foundation of the numerics-scaling story.
                "atomic_store" => {
                    let p = args.first().map(|a| self.emit_expr(*a)).unwrap_or_else(|| "NULL".to_string());
                    let v = args.get(1).map(|a| self.emit_expr(*a)).unwrap_or_else(|| "0".to_string());
                    return format!("__atomic_store_n((int64_t*)({p}), (int64_t)({v}), __ATOMIC_SEQ_CST)");
                }
                "atomic_load" => {
                    let p = args.first().map(|a| self.emit_expr(*a)).unwrap_or_else(|| "NULL".to_string());
                    return format!("__atomic_load_n((int64_t*)({p}), __ATOMIC_SEQ_CST)");
                }
                "atomic_add" => {
                    let p = args.first().map(|a| self.emit_expr(*a)).unwrap_or_else(|| "NULL".to_string());
                    let v = args.get(1).map(|a| self.emit_expr(*a)).unwrap_or_else(|| "0".to_string());
                    return format!("__atomic_fetch_add((int64_t*)({p}), (int64_t)({v}), __ATOMIC_SEQ_CST)");
                }
                "atomic_sub" => {
                    let p = args.first().map(|a| self.emit_expr(*a)).unwrap_or_else(|| "NULL".to_string());
                    let v = args.get(1).map(|a| self.emit_expr(*a)).unwrap_or_else(|| "0".to_string());
                    return format!("__atomic_fetch_sub((int64_t*)({p}), (int64_t)({v}), __ATOMIC_SEQ_CST)");
                }
                // Atomic exchange (test-and-set primitive): store `v` and return the
                // PREVIOUS value as one indivisible step. This is the single extra atom
                // a spinlock needs — `lock_acquire` spins on `atomic_xchg(lock, 1)`
                // until it observes the previous value `0` (the lock was free, and is
                // now ours). The Mutex protected object (`std/sync.jtr`) is built
                // entirely on this plus `atomic_store` for release.
                "atomic_xchg" => {
                    let p = args.first().map(|a| self.emit_expr(*a)).unwrap_or_else(|| "NULL".to_string());
                    let v = args.get(1).map(|a| self.emit_expr(*a)).unwrap_or_else(|| "0".to_string());
                    return format!("__atomic_exchange_n((int64_t*)({p}), (int64_t)({v}), __ATOMIC_SEQ_CST)");
                }
                // Region allocation: `region_alloc(r, T, value)` — bump-allocate
                // into region `r`'s arena and return a zero-cost `&[r]T` (plain ptr).
                "region_alloc" => {
                    let arena = args
                        .first()
                        .and_then(|a| match &ast.expr_at(*a).kind {
                            ExprKind::Name(n) => Some(format!("j_{}", n.name)),
                            _ => None,
                        })
                        .unwrap_or_else(|| "0".to_string());
                    let subst = self.subst.clone();
                    let elem = args.get(1).map(|a| self.eval_type_arg(*a, &subst)).unwrap_or(Ty::Unknown);
                    let ecty = self.c_type(&elem);
                    let v = args.get(2).map(|a| self.emit_expr(*a)).unwrap_or_else(|| "0".to_string());
                    let n = self.tmp;
                    self.tmp += 1;
                    return format!(
                        "({{ {ecty}* _p{n} = ({ecty}*)jestyr_arena_alloc(&{arena}, sizeof({ecty})); *_p{n} = ({v}); _p{n}; }})"
                    );
                }
                // Region-allocated strings (the differentiator): copy a `str` into a
                // region arena, returning a view into it. The whole arena is freed at
                // the region's end — zero individual frees, lexically scoped.
                "region_str" => {
                    let arena = args
                        .first()
                        .and_then(|a| match &ast.expr_at(*a).kind {
                            ExprKind::Name(rn) => Some(format!("j_{}", rn.name)),
                            _ => None,
                        })
                        .unwrap_or_else(|| "0".to_string());
                    let v = args.get(1).map(|a| self.emit_expr(*a)).unwrap_or_else(|| "(JestyrStr){0,0}".to_string());
                    let n = self.tmp;
                    self.tmp += 1;
                    return format!(
                        "({{ JestyrStr _sv{n} = {v}; char* _p{n} = (char*)jestyr_arena_alloc(&{arena}, _sv{n}.len); memcpy(_p{n}, _sv{n}.ptr, _sv{n}.len); (JestyrStr){{ _p{n}, _sv{n}.len }}; }})"
                    );
                }
                "region_concat" => {
                    let arena = args
                        .first()
                        .and_then(|a| match &ast.expr_at(*a).kind {
                            ExprKind::Name(rn) => Some(format!("j_{}", rn.name)),
                            _ => None,
                        })
                        .unwrap_or_else(|| "0".to_string());
                    let a = args.get(1).map(|x| self.emit_expr(*x)).unwrap_or_else(|| "(JestyrStr){0,0}".to_string());
                    let b = args.get(2).map(|x| self.emit_expr(*x)).unwrap_or_else(|| "(JestyrStr){0,0}".to_string());
                    let n = self.tmp;
                    self.tmp += 1;
                    return format!(
                        "({{ JestyrStr _a{n} = {a}; JestyrStr _b{n} = {b}; size_t _t{n} = _a{n}.len + _b{n}.len; char* _p{n} = (char*)jestyr_arena_alloc(&{arena}, _t{n}); memcpy(_p{n}, _a{n}.ptr, _a{n}.len); memcpy(_p{n} + _a{n}.len, _b{n}.ptr, _b{n}.len); (JestyrStr){{ _p{n}, _t{n} }}; }})"
                    );
                }
                // Expose a `str`'s bytes as an unvalidated `[]u8` (the platform-bytes
                // view; re-validate with `from_utf8`). The reverse of the boundary.
                "bytes" => {
                    let s = args.first().map(|a| self.emit_expr(*a)).unwrap_or_else(|| "(JestyrStr){0,0}".to_string());
                    let sn = self.slice_c_name(&Ty::Prim("u8"));
                    let n = self.tmp;
                    self.tmp += 1;
                    return format!(
                        "({{ JestyrStr _bv{n} = {s}; ({sn}){{ (uint8_t*)_bv{n}.ptr, _bv{n}.len }}; }})"
                    );
                }
                // Value-level bump arena (std arena allocator). `arena_open(cap)`
                // heap-allocates an arena and returns an opaque `*mut u8` handle;
                // `arena_alloc(h, T, n)` bump-allocates `n` T's; `arena_close(h)`
                // frees the whole arena in O(1).
                "arena_open" => {
                    let cap = args.first().map(|a| self.emit_expr(*a)).unwrap_or_else(|| "0".to_string());
                    let n = self.tmp;
                    self.tmp += 1;
                    return format!(
                        "({{ JestyrArena* _a{n} = (JestyrArena*)malloc(sizeof(JestyrArena)); *_a{n} = jestyr_arena_new((size_t)({cap})); (uint8_t*)_a{n}; }})"
                    );
                }
                "arena_alloc" => {
                    let h = args.first().map(|a| self.emit_expr(*a)).unwrap_or_else(|| "NULL".to_string());
                    let subst = self.subst.clone();
                    let elem = args.get(1).map(|a| self.eval_type_arg(*a, &subst)).unwrap_or(Ty::Unknown);
                    let ecty = self.c_type(&elem);
                    let n = args.get(2).map(|a| self.emit_expr(*a)).unwrap_or_else(|| "0".to_string());
                    return format!(
                        "(({ecty}*) jestyr_arena_alloc((JestyrArena*)({h}), (size_t)({n}) * sizeof({ecty})))"
                    );
                }
                "arena_close" => {
                    let h = args.first().map(|a| self.emit_expr(*a)).unwrap_or_else(|| "NULL".to_string());
                    let n = self.tmp;
                    self.tmp += 1;
                    return format!(
                        "({{ JestyrArena* _a{n} = (JestyrArena*)({h}); jestyr_arena_free(_a{n}); free(_a{n}); }})"
                    );
                }
                // Generational reference: allocate `[gen | T]`, init, return {ptr, gen}.
                "gen_new" => {
                    let subst = self.subst.clone();
                    let elem = args.first().map(|a| self.eval_type_arg(*a, &subst)).unwrap_or(Ty::Unknown);
                    let ecty = self.c_type(&elem);
                    let name = self.genref_c_name(&elem);
                    let v = args.get(1).map(|a| self.emit_expr(*a)).unwrap_or_else(|| "0".to_string());
                    let n = self.tmp;
                    self.tmp += 1;
                    return format!(
                        "({{ void* _b{n} = malloc(8 + sizeof({ecty})); *(uint64_t*)_b{n} = 1; {ecty}* _p{n} = ({ecty}*)((char*)_b{n} + 8); *_p{n} = ({v}); ({name}){{ _p{n}, 1 }}; }})"
                    );
                }
                // Invalidate a generational reference: bump the generation so every
                // outstanding reference goes stale. (Bootstrap: leaks the block; a
                // real impl keeps generations in a reuse-safe pool.)
                "gen_free" => {
                    let rty = args.first().map(|a| self.info.type_of(*a).clone()).unwrap_or(Ty::Unknown);
                    let r = args.first().map(|a| self.emit_expr(*a)).unwrap_or_else(|| "0".to_string());
                    let cty = self.c_type(&rty);
                    let n = self.tmp;
                    self.tmp += 1;
                    return format!("({{ {cty} _r{n} = ({r}); ((uint64_t*)_r{n}.ptr)[-1]++; }})");
                }
                // Slice construction: `slice(T, ptr, len)` → `{ptr, len}`.
                "slice" => {
                    let subst = self.subst.clone();
                    let elem = args.first().map(|a| self.eval_type_arg(*a, &subst)).unwrap_or(Ty::Unknown);
                    let name = self.slice_c_name(&elem);
                    let p = args.get(1).map(|a| self.emit_expr(*a)).unwrap_or_else(|| "NULL".to_string());
                    let n = args.get(2).map(|a| self.emit_expr(*a)).unwrap_or_else(|| "0".to_string());
                    return format!("({name}){{ {p}, (size_t)({n}) }}");
                }
                // Compile-time size of a type, e.g. `size_of(Packed)`.
                "size_of" => {
                    let subst = self.subst.clone();
                    let ty = args.first().map(|a| self.eval_type_arg(*a, &subst)).unwrap_or(Ty::Unknown);
                    let cty = self.c_type(&ty);
                    return format!("sizeof({cty})");
                }
                // Validate-at-boundary: `from_utf8([]u8) -> str` is the *only* way to
                // turn raw bytes into a `str`, so every `str` is proven valid UTF-8.
                // It checks once (asserts), then the validity is a trusted invariant.
                "from_utf8" => {
                    if let Some(a) = args.first().copied() {
                        let bt = self.info.type_of(a).clone();
                        let bcty = self.c_type(&bt);
                        let b = self.emit_expr(a);
                        return format!(
                            "({{ {bcty} _u = {b}; assert(jestyr_rt_valid_utf8((const char*)_u.ptr, _u.len)); (JestyrStr){{ (const char*)_u.ptr, _u.len }}; }})"
                        );
                    }
                    return "(JestyrStr){0,0}".to_string();
                }
                // Recoverable validate-at-boundary: `try_from_utf8([]u8) -> str !Utf8Error`
                // returns a Result (`is_err`/`unwrap`/`?` compose) instead of trapping.
                "try_from_utf8" => {
                    if let Some(a) = args.first().copied() {
                        let bt = self.info.type_of(a).clone();
                        let bcty = self.c_type(&bt);
                        let b = self.emit_expr(a);
                        return format!(
                            "({{ {bcty} _u = {b}; jestyr_rt_valid_utf8((const char*)_u.ptr, _u.len) \
                             ? (JestyrResult_str){{ .is_err = false, .ok = (JestyrStr){{ (const char*)_u.ptr, _u.len }} }} \
                             : (JestyrResult_str){{ .is_err = true, .err = 1 }}; }})"
                        );
                    }
                    return "(JestyrResult_str){ .is_err = true, .err = 1 }".to_string();
                }
                // An explicit, recoverable UTF-8 check (when you want to branch
                // rather than trap).
                "is_utf8" => {
                    if let Some(a) = args.first().copied() {
                        let bt = self.info.type_of(a).clone();
                        let bcty = self.c_type(&bt);
                        let b = self.emit_expr(a);
                        return format!(
                            "({{ {bcty} _u = {b}; jestyr_rt_valid_utf8((const char*)_u.ptr, _u.len); }})"
                        );
                    }
                    return "false".to_string();
                }
                // O(n) codepoint count — the cost-visible companion to O(1) `.len`.
                "count_codepoints" => {
                    let s = args.first().map(|a| self.emit_expr(*a)).unwrap_or_else(|| "(JestyrStr){0,0}".to_string());
                    return format!("jestyr_rt_count_cp({s})");
                }
                // O(n) grapheme-cluster count (the correctness ceiling, opt-in).
                "count_graphemes" => {
                    let s = args.first().map(|a| self.emit_expr(*a)).unwrap_or_else(|| "(JestyrStr){0,0}".to_string());
                    return format!("jestyr_rt_count_graphemes({s})");
                }
                // `substr(s, start, end)` — a boundary-checked zero-copy sub-view
                // (the named form of `s[start..end]`).
                "substr" => {
                    let s = args.first().map(|a| self.emit_expr(*a)).unwrap_or_else(|| "(JestyrStr){0,0}".to_string());
                    let start = args.get(1).map(|a| self.emit_expr(*a)).unwrap_or_else(|| "0".to_string());
                    let end = args.get(2).map(|a| self.emit_expr(*a)).unwrap_or_else(|| format!("({s}).len"));
                    return format!("jestyr_rt_substr({s}, {start}, {end})");
                }
                // Byte-level string operations (all view-based; `find`/`trim` zero-copy).
                "str_eq" => return self.emit_str_binop("jestyr_rt_str_eq", args),
                "eq_fold" => return self.emit_str_binop("jestyr_rt_eq_fold", args),
                "starts_with" => return self.emit_str_binop("jestyr_rt_starts_with", args),
                "ends_with" => return self.emit_str_binop("jestyr_rt_ends_with", args),
                "contains" => return self.emit_str_binop("jestyr_rt_contains", args),
                "find" => return self.emit_str_binop("jestyr_rt_find", args),
                "trim" => {
                    let s = args.first().map(|a| self.emit_expr(*a)).unwrap_or_else(|| "(JestyrStr){0,0}".to_string());
                    return format!("jestyr_rt_trim({s})");
                }
                // `os_str` — unvalidated platform text (the WTF-8 / OsStr role).
                // `os_from_bytes` reinterprets raw bytes as an os_str view (no check);
                // `to_str_lossy` decodes it into a proven `String` (U+FFFD for bad bytes).
                "os_from_bytes" => {
                    let b = args.first().map(|a| self.emit_expr(*a)).unwrap_or_else(|| "(JestyrStr){0,0}".to_string());
                    return format!("({{ __typeof__({b}) _ob = {b}; (JestyrStr){{ (const char*)_ob.ptr, _ob.len }}; }})");
                }
                "to_str_lossy" => {
                    let os = args.first().map(|a| self.emit_expr(*a)).unwrap_or_else(|| "(JestyrStr){0,0}".to_string());
                    return format!("jestyr_rt_to_str_lossy({os})");
                }
                // Cow<str> — borrowed-or-owned, with the allocation visible.
                "cow_borrow" => {
                    let s = args.first().map(|a| self.emit_expr(*a)).unwrap_or_else(|| "(JestyrStr){0,0}".to_string());
                    return format!("jestyr_rt_cow_borrow({s})");
                }
                "cow_to_mut" => {
                    let c = args.first().map(|a| self.emit_expr(*a)).unwrap_or_else(|| "(JestyrCow){0}".to_string());
                    return format!("jestyr_rt_cow_to_mut({c})");
                }
                "cow_view" => {
                    let c = args.first().map(|a| self.emit_expr(*a)).unwrap_or_else(|| "(JestyrCow){0}".to_string());
                    return format!("jestyr_rt_cow_view({c})");
                }
                "cow_is_owned" => {
                    let c = args.first().map(|a| self.emit_expr(*a)).unwrap_or_else(|| "(JestyrCow){0}".to_string());
                    return format!("jestyr_rt_cow_is_owned({c})");
                }
                "cow_free" => {
                    let c = args.first().map(|a| self.emit_expr(*a)).unwrap_or_else(|| "(JestyrCow){0}".to_string());
                    return format!("jestyr_rt_cow_free(&{c})");
                }
                // Owned, growable `String` (the owned half of the owned/view split).
                "string_new" => return "jestyr_rt_str_new()".to_string(),
                "string_from" => {
                    let v = args.first().map(|a| self.emit_expr(*a)).unwrap_or_else(|| "(JestyrStr){0,0}".to_string());
                    return format!("jestyr_rt_str_from({v})");
                }
                "string_push" => {
                    let s = args.first().map(|a| self.emit_expr(*a)).unwrap_or_default();
                    let v = args.get(1).map(|a| self.emit_expr(*a)).unwrap_or_else(|| "(JestyrStr){0,0}".to_string());
                    return format!("jestyr_rt_str_push(&{s}, {v})");
                }
                // Borrow the owned buffer as a `str` view — no copy (owned → view).
                "string_view" => {
                    let s = args.first().map(|a| self.emit_expr(*a)).unwrap_or_default();
                    return format!("jestyr_rt_str_view(&{s})");
                }
                "string_free" => {
                    let s = args.first().map(|a| self.emit_expr(*a)).unwrap_or_default();
                    return format!("jestyr_rt_str_free(&{s})");
                }
                // Builder / iolist — collect `str` fragments with no copy, flatten once.
                "builder_new" => return "jestyr_rt_b_new()".to_string(),
                "builder_push" => {
                    let b = args.first().map(|a| self.emit_expr(*a)).unwrap_or_default();
                    let v = args.get(1).map(|a| self.emit_expr(*a)).unwrap_or_else(|| "(JestyrStr){0,0}".to_string());
                    return format!("jestyr_rt_b_push(&{b}, {v})");
                }
                // Flatten the fragment list into one owned `String` in a single pass.
                "builder_build" => {
                    let b = args.first().map(|a| self.emit_expr(*a)).unwrap_or_default();
                    return format!("jestyr_rt_b_build(&{b})");
                }
                "builder_free" => {
                    let b = args.first().map(|a| self.emit_expr(*a)).unwrap_or_default();
                    return format!("jestyr_rt_b_free(&{b})");
                }
                // Compile-time alignment of a type, e.g. `align_of(Packed)` → `_Alignof`.
                "align_of" => {
                    let subst = self.subst.clone();
                    let ty = args.first().map(|a| self.eval_type_arg(*a, &subst)).unwrap_or(Ty::Unknown);
                    let cty = self.c_type(&ty);
                    return format!("_Alignof({cty})");
                }
                // Byte offset of a field within a struct: `offset_of(Point, y)` →
                // `offsetof(Jestyr_Point, j_y)`. The second argument is a bare field
                // name (an identifier expression), not a value.
                "offset_of" => {
                    let subst = self.subst.clone();
                    let ty = args.first().map(|a| self.eval_type_arg(*a, &subst)).unwrap_or(Ty::Unknown);
                    let cty = self.c_type(&ty);
                    let field = args.get(1).and_then(|a| match &self.ast.expr_at(*a).kind {
                        ExprKind::Name(fname) => Some(fname.name.clone()),
                        _ => None,
                    });
                    match field {
                        Some(f) => return format!("offsetof({cty}, j_{f})"),
                        None => {
                            self.diag(
                                ast.expr_at(call_id).span,
                                "`offset_of(T, field)` needs a bare field name as its second argument",
                            );
                            return "0".to_string();
                        }
                    }
                }
                // Generic allocation: first argument is the element *type*.
                "alloc" => {
                    let subst = self.subst.clone();
                    let ty = args.first().map(|a| self.eval_type_arg(*a, &subst)).unwrap_or(Ty::Unknown);
                    let cty = self.c_type(&ty);
                    let n = args.get(1).map(|a| self.emit_expr(*a)).unwrap_or_else(|| "0".to_string());
                    return format!("(({cty}*) malloc((size_t)({n}) * sizeof({cty})))");
                }
                "realloc" => {
                    let subst = self.subst.clone();
                    let ty = args.first().map(|a| self.eval_type_arg(*a, &subst)).unwrap_or(Ty::Unknown);
                    let cty = self.c_type(&ty);
                    let p = args.get(1).map(|a| self.emit_expr(*a)).unwrap_or_else(|| "NULL".to_string());
                    let n = args.get(2).map(|a| self.emit_expr(*a)).unwrap_or_else(|| "0".to_string());
                    return format!("(({cty}*) realloc({p}, (size_t)({n}) * sizeof({cty})))");
                }
                _ => {}
            }

            // An `extern "c"` function: call it by its bare C name (the linker
            // resolves it), passing `mut`/`out` arguments by address.
            if self.extern_fns.contains(&n.name) {
                let convs: Vec<Conv> = self
                    .info
                    .table
                    .fns
                    .get(&n.name)
                    .map(|s| s.params.iter().map(|p| p.conv).collect())
                    .unwrap_or_default();
                let mut parts = Vec::new();
                for (i, a) in args.iter().enumerate() {
                    let e = if matches!(convs.get(i), Some(Conv::Mut) | Some(Conv::Out)) {
                        self.emit_addr_arg(*a)
                    } else {
                        self.emit_expr(*a)
                    };
                    parts.push(e);
                }
                return format!("{}({})", n.name, parts.join(", "));
            }

            // The callee's canonical name: the type checker recorded it when an
            // unqualified call targets a name that collides across modules;
            // otherwise it is the bare name (so non-colliding calls are unchanged).
            let cname = self.info.call_sym.get(&call_id).cloned().unwrap_or_else(|| n.name.clone());

            // A generic function: pick (or already-collected) the monomorphized
            // instance for these type arguments and call it.
            if self.generics.contains(&cname) {
                return self.emit_generic_call(&cname, args);
            }

            // A known function: take `&arg` for `mut`/`out` parameters.
            let convs: Vec<Conv> = self
                .info
                .table
                .fns
                .get(&cname)
                .map(|sig| sig.params.iter().map(|p| p.conv).collect())
                .unwrap_or_default();
            let byref = self.abi_ref_positions(&cname);
            let mut parts = Vec::new();
            for (i, a) in args.iter().enumerate() {
                let e = if matches!(convs.get(i), Some(Conv::Mut) | Some(Conv::Out)) {
                    self.emit_addr_arg(*a)
                } else if byref.contains(&i) {
                    // `@abi(ref)`: this parameter is `const T*` in the callee.
                    let v = self.emit_expr(*a);
                    self.abi_ref_arg(*a, &v)
                } else {
                    self.emit_expr(*a)
                };
                parts.push(e);
            }
            return format!("{}({})", self.c_fn_name(&cname), parts.join(", "));
        }

        let c = self.emit_expr(callee);
        let parts: Vec<String> = args.iter().map(|a| self.emit_expr(*a)).collect();
        format!("{c}({})", parts.join(", "))
    }

    /// Emit a direct call to a user function by *bare* name — the lowering for a
    /// module-qualified call once the type checker has resolved it. Handles the
    /// generic, `extern "c"`, and ordinary cases (a qualified target is always a
    /// user item, never an intrinsic / variant constructor).
    fn emit_named_call(&mut self, name: &str, args: &[ExprId]) -> String {
        if self.generics.contains(name) {
            return self.emit_generic_call(name, args);
        }
        let convs: Vec<Conv> = self
            .info
            .table
            .fns
            .get(name)
            .map(|sig| sig.params.iter().map(|p| p.conv).collect())
            .unwrap_or_default();
        let byref = self.abi_ref_positions(name);
        let mut parts = Vec::new();
        for (i, a) in args.iter().enumerate() {
            let e = if matches!(convs.get(i), Some(Conv::Mut) | Some(Conv::Out)) {
                self.emit_addr_arg(*a)
            } else if byref.contains(&i) {
                // `@abi(ref)`: this parameter is `const T*` in the callee.
                let v = self.emit_expr(*a);
                self.abi_ref_arg(*a, &v)
            } else {
                self.emit_expr(*a)
            };
            parts.push(e);
        }
        if self.extern_fns.contains(name) {
            format!("{}({})", name, parts.join(", "))
        } else {
            // `@no_mangle` callees are reached by their bare C name too.
            format!("{}({})", self.c_fn_name(name), parts.join(", "))
        }
    }

    /// Emit a method call `base.name(args)` the type checker resolved to a free
    /// (possibly generic) function. The receiver becomes the first argument,
    /// taken by `&` for a `mut`/`out` receiver parameter.
    fn emit_method_call(&mut self, callee: ExprId, mr: &MethodRes, args: &[ExprId]) -> String {
        let base = match &self.ast.expr_at(callee).kind {
            ExprKind::Field { base, .. } => *base,
            _ => return "0".to_string(),
        };
        // Resolve type arguments through any active monomorphization substitution
        // (so a method call inside a generic body picks the right instance).
        let subst = self.subst.clone();
        let targs: Vec<Ty> = mr.type_args.iter().map(|t| apply_subst(t, &subst)).collect();

        let recv = if matches!(mr.recv_conv, Conv::Mut | Conv::Out) {
            self.emit_addr_arg(base)
        } else {
            self.emit_expr(base)
        };
        let mut parts = vec![recv];

        // The callee name and explicit-argument conventions depend on whether
        // this resolved to a struct method (item C) or a free function (item A).
        let (name, arg_convs): (String, Vec<Conv>) = if let Some(ctor) = &mr.recv_ctor {
            let convs = self
                .find_struct_method_cg(ctor, &mr.fn_name)
                .map(|f| {
                    f.params.iter().filter(|p| !p.comptime && !p.is_self).map(|p| p.conv).collect()
                })
                .unwrap_or_default();
            (self.method_c_name(ctor, &targs, &mr.fn_name), convs)
        } else {
            let convs = self
                .find_fn(&mr.fn_name)
                .map(|f| f.params.iter().filter(|p| !p.comptime).skip(1).map(|p| p.conv).collect())
                .unwrap_or_default();
            // A free-function method (item A) may itself be `@no_mangle`, so route
            // the non-generic name through `c_fn_name`. (A generic free method is
            // never `@no_mangle` — validation forbids it — so the mangled path stays.)
            let nm = if targs.is_empty() {
                self.c_fn_name(&mr.fn_name)
            } else {
                format!("jestyr_{}", self.mangle(&mr.fn_name, &targs))
            };
            (nm, convs)
        };

        for (i, a) in args.iter().enumerate() {
            let e = if matches!(arg_convs.get(i), Some(Conv::Mut) | Some(Conv::Out)) {
                self.emit_addr_arg(*a)
            } else {
                self.emit_expr(*a)
            };
            parts.push(e);
        }
        format!("{name}({})", parts.join(", "))
    }

    // --- closures (lambda lifting) ---

    /// Find every closure in the non-generic parts of the program and lift it to
    /// an environment + a top-level function. (Closures inside *generic* bodies
    /// are deferred — their capture types depend on the monomorphization.)
    fn collect_closures(&self) -> (Vec<ClosureInfo>, HashMap<ExprId, usize>) {
        let mut found: Vec<ExprId> = Vec::new();
        let mut seen: HashSet<u32> = HashSet::new();
        for item in &self.ast.items {
            match item {
                Item::Fn(f) if !self.is_generic(f) => {
                    self.find_closures_block(&f.body, &mut found, &mut seen)
                }
                Item::Const(c) => self.find_closures_expr(c.value, &mut found, &mut seen),
                Item::Struct { body, .. } => {
                    for m in &body.members {
                        if let StructMember::Method(f) = m {
                            if !self.is_generic(f) {
                                self.find_closures_block(&f.body, &mut found, &mut seen);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        let mut closures = Vec::new();
        let mut index = HashMap::new();
        for (i, cid) in found.iter().enumerate() {
            index.insert(*cid, i);
            closures.push(self.make_closure_info(*cid));
        }
        (closures, index)
    }

    fn find_closures_block(&self, b: &Block, found: &mut Vec<ExprId>, seen: &mut HashSet<u32>) {
        for s in &b.stmts {
            match s {
                Stmt::Let { init: Some(e), .. } => self.find_closures_expr(*e, found, seen),
                Stmt::Return { value: Some(v), .. } => self.find_closures_expr(*v, found, seen),
                Stmt::Expr(e) => self.find_closures_expr(*e, found, seen),
                _ => {}
            }
        }
    }

    fn find_closures_expr(&self, id: ExprId, found: &mut Vec<ExprId>, seen: &mut HashSet<u32>) {
        let ast = self.ast;
        match &ast.expr_at(id).kind {
            ExprKind::Closure { body, .. } => {
                if seen.insert(id.0) {
                    found.push(id);
                }
                self.find_closures_expr(*body, found, seen);
            }
            ExprKind::Call { callee, args } => {
                self.find_closures_expr(*callee, found, seen);
                for a in args {
                    self.find_closures_expr(*a, found, seen);
                }
            }
            ExprKind::Binary { lhs, rhs, .. } => {
                self.find_closures_expr(*lhs, found, seen);
                self.find_closures_expr(*rhs, found, seen);
            }
            ExprKind::Unary { rhs, .. } => self.find_closures_expr(*rhs, found, seen),
            ExprKind::Assign { target, value, .. } => {
                self.find_closures_expr(*target, found, seen);
                self.find_closures_expr(*value, found, seen);
            }
            ExprKind::Field { base, .. } => self.find_closures_expr(*base, found, seen),
            ExprKind::Index { base, index } => {
                self.find_closures_expr(*base, found, seen);
                self.find_closures_expr(*index, found, seen);
            }
            ExprKind::Deref { base } | ExprKind::Try { base } => {
                self.find_closures_expr(*base, found, seen)
            }
            // A closure written as a fallback still has to be lifted.
            ExprKind::Catch { base, fallback, .. } => {
                self.find_closures_expr(*base, found, seen);
                self.find_closures_expr(*fallback, found, seen);
            }
            ExprKind::StructLit { fields, spread, .. } => {
                for fi in fields {
                    self.find_closures_expr(fi.value, found, seen);
                }
                if let Some(s) = spread {
                    self.find_closures_expr(*s, found, seen);
                }
            }
            ExprKind::GenStructLit { fields, .. } => {
                for fi in fields {
                    self.find_closures_expr(fi.value, found, seen);
                }
            }
            ExprKind::If { cond, then, els } => {
                self.find_closures_expr(*cond, found, seen);
                self.find_closures_block(then, found, seen);
                if let Some(e) = els {
                    self.find_closures_expr(*e, found, seen);
                }
            }
            ExprKind::Match { scrut, arms } => {
                self.find_closures_expr(*scrut, found, seen);
                for a in arms {
                    if let Some(g) = a.guard {
                        self.find_closures_expr(g, found, seen);
                    }
                    self.find_closures_expr(a.body, found, seen);
                }
            }
            ExprKind::Block(b) | ExprKind::Unsafe(b) => self.find_closures_block(b, found, seen),
            ExprKind::For { head, body, els, .. } => {
                match head {
                    ForHead::While(c) => self.find_closures_expr(*c, found, seen),
                    ForHead::Iter { sources, .. } => {
                        for s in sources {
                            self.find_closures_expr(*s, found, seen);
                        }
                    }
                    ForHead::Infinite => {}
                }
                self.find_closures_block(body, found, seen);
                if let Some(els) = els {
                    self.find_closures_block(els, found, seen);
                }
            }
            ExprKind::Invariant(e) | ExprKind::Variant(e) => self.find_closures_expr(*e, found, seen),
            ExprKind::ParFor { iter, reduction, body, .. } => {
                self.find_closures_expr(*iter, found, seen);
                self.find_closures_expr(*reduction, found, seen);
                self.find_closures_expr(*body, found, seen);
            }
            ExprKind::Select(arms) => {
                for arm in arms {
                    self.find_closures_expr(arm.chan, found, seen);
                    self.find_closures_block(&arm.body, found, seen);
                }
            }
            _ => {}
        }
    }

    fn make_closure_info(&self, cid: ExprId) -> ClosureInfo {
        let (params, body) = match &self.ast.expr_at(cid).kind {
            ExprKind::Closure { params, body } => (params, *body),
            _ => unreachable!("make_closure_info on a non-closure"),
        };
        let pnames: HashSet<String> = params.iter().map(|p| p.name.name.clone()).collect();

        // Captures = free variables of the body that are not parameters and not
        // global names (functions, consts, variants, types, intrinsics).
        let mut refs: Vec<(String, ExprId)> = Vec::new();
        self.collect_refs(body, &mut refs);
        let mut captures: Vec<(String, Ty)> = Vec::new();
        let mut capset: HashSet<String> = HashSet::new();
        for (name, rid) in refs {
            if pnames.contains(&name) || self.is_global_name(&name) || capset.contains(&name) {
                continue;
            }
            capset.insert(name.clone());
            captures.push((name, self.info.type_of(rid).clone()));
        }

        ClosureInfo {
            id: cid,
            params: params.iter().map(|p| (p.name.name.clone(), p.ty)).collect(),
            ret: self.info.type_of(body).clone(),
            captures,
            body,
        }
    }

    /// Is `name` a top-level/global name (so a reference to it is *not* a capture)?
    fn is_global_name(&self, name: &str) -> bool {
        self.info.table.fns.contains_key(name)
            || self.info.table.consts.contains_key(name)
            || self.info.table.variants.contains_key(name)
            || self.info.table.type_index.contains_key(name)
            || self.generics.contains(name)
            // A name that collides across modules is table-keyed by its canonical
            // form, so its bare spelling misses the maps above — recognise it here.
            || self.info.dup_fns.contains(name)
            || self.info.dup_types.contains(name)
            || self.info.dup_variants.contains(name)
            || is_intrinsic(name)
    }

    /// Gather every value-name reference (and `self`) in a subtree, paired with
    /// the referencing expression id (so its inferred type is available).
    fn collect_refs(&self, id: ExprId, out: &mut Vec<(String, ExprId)>) {
        let ast = self.ast;
        match &ast.expr_at(id).kind {
            ExprKind::Name(n) => out.push((n.name.clone(), id)),
            ExprKind::Unary { rhs, .. } => self.collect_refs(*rhs, out),
            ExprKind::Binary { lhs, rhs, .. } => {
                self.collect_refs(*lhs, out);
                self.collect_refs(*rhs, out);
            }
            ExprKind::Assign { target, value, .. } => {
                self.collect_refs(*target, out);
                self.collect_refs(*value, out);
            }
            ExprKind::Call { callee, args } => {
                self.collect_refs(*callee, out);
                for a in args {
                    self.collect_refs(*a, out);
                }
            }
            ExprKind::Field { base, .. } => self.collect_refs(*base, out),
            ExprKind::Index { base, index } => {
                self.collect_refs(*base, out);
                self.collect_refs(*index, out);
            }
            ExprKind::Deref { base } | ExprKind::Try { base } => self.collect_refs(*base, out),
            ExprKind::Catch { base, fallback, .. } => {
                self.collect_refs(*base, out);
                self.collect_refs(*fallback, out);
            }
            ExprKind::StructLit { fields, spread, .. } => {
                for fi in fields {
                    self.collect_refs(fi.value, out);
                }
                if let Some(s) = spread {
                    self.collect_refs(*s, out);
                }
            }
            ExprKind::GenStructLit { fields, .. } => {
                for fi in fields {
                    self.collect_refs(fi.value, out);
                }
            }
            ExprKind::If { cond, then, els } => {
                self.collect_refs(*cond, out);
                self.collect_refs_block(then, out);
                if let Some(e) = els {
                    self.collect_refs(*e, out);
                }
            }
            ExprKind::Match { scrut, arms } => {
                self.collect_refs(*scrut, out);
                for a in arms {
                    if let Some(g) = a.guard {
                        self.collect_refs(g, out);
                    }
                    self.collect_refs(a.body, out);
                }
            }
            ExprKind::Block(b) | ExprKind::Unsafe(b) => self.collect_refs_block(b, out),
            ExprKind::Closure { body, .. } => self.collect_refs(*body, out),
            ExprKind::For { head, body, els, .. } => {
                match head {
                    ForHead::While(c) => self.collect_refs(*c, out),
                    ForHead::Iter { sources, .. } => {
                        for s in sources {
                            self.collect_refs(*s, out);
                        }
                    }
                    ForHead::Infinite => {}
                }
                self.collect_refs_block(body, out);
                if let Some(els) = els {
                    self.collect_refs_block(els, out);
                }
            }
            ExprKind::Invariant(e) | ExprKind::Variant(e) => self.collect_refs(*e, out),
            ExprKind::ParFor { iter, reduction, body, .. } => {
                self.collect_refs(*iter, out);
                self.collect_refs(*reduction, out);
                self.collect_refs(*body, out);
            }
            ExprKind::Select(arms) => {
                for arm in arms {
                    self.collect_refs(arm.chan, out);
                    self.collect_refs_block(&arm.body, out);
                }
            }
            _ => {}
        }
    }

    fn collect_refs_block(&self, b: &Block, out: &mut Vec<(String, ExprId)>) {
        for s in &b.stmts {
            match s {
                Stmt::Let { init: Some(e), .. } => self.collect_refs(*e, out),
                Stmt::Return { value: Some(v), .. } => self.collect_refs(*v, out),
                Stmt::Expr(e) => self.collect_refs(*e, out),
                _ => {}
            }
        }
    }

    /// Emit the environment struct and the closure (fn-ptr + env) struct for
    /// each lifted closure.
    fn closure_types(&mut self) {
        for ci in self.closures.clone() {
            // A closure coerced to a thin fn-pointer needs no environment struct
            // and no `{call, env}` closure struct — it is just a bare function.
            if self.closure_is_fn_ptr(&ci) {
                continue;
            }
            let n = ci.id.0;
            self.raw("typedef struct { ");
            if ci.captures.is_empty() {
                self.raw("char _unused; ");
            } else {
                for (cap, ty) in &ci.captures {
                    let cty = self.c_type(ty);
                    self.raw(format!("{cty} j_{cap}; "));
                }
            }
            self.raw(format!("}} JestyrEnv_{n};\n"));

            let ret = self.c_type(&ci.ret);
            let ptypes = self.closure_param_types(&ci);
            self.raw(format!(
                "typedef struct {{ {ret} (*call)(JestyrEnv_{n}*{ptypes}); JestyrEnv_{n} env; }} JestyrClosure_{n};\n"
            ));
        }
        self.raw("\n");
    }

    /// The closure parameter types as a leading-comma list (for the fn-ptr type).
    fn closure_param_types(&mut self, ci: &ClosureInfo) -> String {
        let mut s = String::new();
        for (_, ty) in &ci.params {
            let cty = match ty {
                Some(t) => self.c_ty_ast(*t),
                None => "int".to_string(),
            };
            let _ = write!(s, ", {cty}");
        }
        s
    }

    /// Emit one top-level function per closure: captures arrive through `j__env`,
    /// parameters as ordinary arguments.
    fn closure_fns(&mut self) {
        for ci in self.closures.clone() {
            let n = ci.id.0;
            // A coerced (thin) closure becomes a *bare* function — no env param —
            // whose C signature matches the fn-pointer typedef exactly.
            if self.closure_is_fn_ptr(&ci) {
                if ci.captures.is_empty() {
                    self.emit_thin_closure_fn(&ci);
                } else {
                    self.diag(
                        self.ast.expr_at(ci.id).span,
                        "a closure that captures its environment cannot coerce to a thin \
                         function pointer (only a non-capturing closure can)",
                    );
                }
                continue;
            }
            let ret = self.c_type(&ci.ret);
            let mut params = format!("JestyrEnv_{n}* j__env");
            for (pname, pty) in &ci.params {
                let cty = match pty {
                    Some(t) => self.c_ty_ast(*t),
                    None => "int".to_string(),
                };
                let _ = write!(params, ", {cty} j_{pname}");
            }

            self.subst.clear();
            self.ptr_params.clear();
            self.cur_result.clear();
            self.cur_ensures.clear();
            self.capture_set = ci.captures.iter().map(|(c, _)| c.clone()).collect();
            self.raw(format!("static {ret} jestyr_lam_{n}({params})\n"));
            self.emit_closure_body(ci.body, ret != "void");
            self.raw("\n");
            self.capture_set.clear();
        }
    }

    /// Emit a coerced non-capturing closure as a bare top-level function whose C
    /// signature is taken from the *expected* fn-pointer type (so it matches the
    /// `JestyrFn_…` typedef byte-for-byte). A `mut`/`out` parameter arrives by
    /// pointer, exactly as a real Jestyr function's would.
    fn emit_thin_closure_fn(&mut self, ci: &ClosureInfo) {
        let n = ci.id.0;
        let Ty::Fn { params, ret, .. } = self.info.type_of(ci.id).clone() else { return };
        let ret_c = self.c_type(&ret);
        let mut sig = String::new();
        let mut ptrs: HashSet<String> = HashSet::new();
        for (i, (conv, pty)) in params.iter().enumerate() {
            let base = self.c_type(pty);
            let pname = ci
                .params
                .get(i)
                .map(|(nm, _)| nm.clone())
                .unwrap_or_else(|| format!("_p{i}"));
            let cty = if matches!(conv, Conv::Mut | Conv::Out) {
                ptrs.insert(pname.clone());
                format!("{base}*")
            } else {
                base
            };
            let _ = write!(sig, "{}{} j_{}", if i > 0 { ", " } else { "" }, cty, pname);
        }
        if params.is_empty() {
            sig = "void".to_string();
        }
        self.subst.clear();
        self.ptr_params = ptrs;
        self.cur_result.clear();
        self.cur_ensures.clear();
        self.capture_set.clear();
        self.raw(format!("static {ret_c} jestyr_lam_{n}({sig})\n"));
        self.emit_closure_body(ci.body, ret_c != "void");
        self.raw("\n");
        self.ptr_params.clear();
    }

    fn emit_closure_body(&mut self, body: ExprId, returns_value: bool) {
        if let ExprKind::Block(b) | ExprKind::Unsafe(b) = &self.ast.expr_at(body).kind {
            let b = b.clone();
            self.emit_body(&b, returns_value);
            return;
        }
        self.line("{");
        self.depth += 1;
        if returns_value {
            self.emit_return(Some(body));
        } else {
            match &self.ast.expr_at(body).kind {
                ExprKind::If { .. } => self.emit_if(body, false),
                ExprKind::Match { .. } => self.emit_match(body, false),
                _ => {
                    let v = self.emit_expr(body);
                    self.line(format!("{v};"));
                }
            }
        }
        self.depth -= 1;
        self.line("}");
    }

    /// Emit the closure value at its creation site: a fn-ptr to the lifted
    /// function plus an environment populated from the enclosing scope.
    fn emit_closure_literal(&mut self, cid: ExprId) -> String {
        let Some(&idx) = self.closure_index.get(&cid) else {
            self.diag(self.ast.expr_at(cid).span, "the C backend does not support this closure yet");
            return "0".to_string();
        };
        let ci = self.closures[idx].clone();
        let n = ci.id.0;
        // Coerced to a thin fn-pointer: the value *is* the function's address.
        if self.closure_is_fn_ptr(&ci) {
            if !ci.captures.is_empty() {
                self.diag(
                    self.ast.expr_at(cid).span,
                    "a closure that captures its environment cannot coerce to a thin \
                     function pointer (only a non-capturing closure can)",
                );
                return "0".to_string();
            }
            return format!("(&jestyr_lam_{n})");
        }
        if ci.captures.is_empty() {
            return format!("(JestyrClosure_{n}){{ .call = jestyr_lam_{n}, .env = {{0}} }}");
        }
        let mut env = String::new();
        for (i, (cap, _)) in ci.captures.iter().enumerate() {
            if i > 0 {
                env.push_str(", ");
            }
            let v = self.emit_capture_value(cap);
            let _ = write!(env, ".j_{cap} = {v}");
        }
        format!("(JestyrClosure_{n}){{ .call = jestyr_lam_{n}, .env = {{ {env} }} }}")
    }

    /// Render a captured variable's value in the *enclosing* scope.
    fn emit_capture_value(&self, name: &str) -> String {
        if self.ptr_params.contains(name) {
            format!("(*j_{name})")
        } else {
            format!("j_{name}")
        }
    }

    /// Invoke a closure value: `f.call(&f.env, args)`.
    fn emit_closure_invoke(&mut self, callee: ExprId, args: &[ExprId]) -> String {
        let argv: Vec<String> = args.iter().map(|a| self.emit_expr(*a)).collect();
        let arglist = if argv.is_empty() { String::new() } else { format!(", {}", argv.join(", ")) };
        if matches!(&self.ast.expr_at(callee).kind, ExprKind::Name(_)) {
            let c = self.emit_expr(callee);
            format!("{c}.call(&{c}.env{arglist})")
        } else {
            // An inline / immediately-invoked closure: spill it so its env is
            // built exactly once. (GCC/Clang statement-expression.)
            let lit = self.emit_expr(callee);
            let n = callee.0;
            let tmp = format!("_f{}", self.tmp);
            self.tmp += 1;
            format!("({{ JestyrClosure_{n} {tmp} = {lit}; {tmp}.call(&{tmp}.env{arglist}); }})")
        }
    }

    /// Is `id` a value of closure type (so a call on it is an invocation)?
    fn is_closure_typed(&self, id: ExprId) -> bool {
        matches!(self.info.type_of(id), Ty::Opaque(s) if s == "closure")
    }

    /// Was this closure **coerced to a thin function pointer**? The type checker
    /// stamps a closure used where a `fn(...) -> R` is expected with that `Ty::Fn`
    /// (instead of the opaque fat-closure type). Such a closure lowers to a bare
    /// top-level function and `&`-of-it — not the `{call, env}` closure struct.
    fn closure_is_fn_ptr(&self, ci: &ClosureInfo) -> bool {
        matches!(self.info.type_of(ci.id), Ty::Fn { .. })
    }

    /// Emit an indirect call through a thin function-pointer value. The callee
    /// expression (a local `j_f`, a field `j_a.j_alloc_fn`, …) already holds the
    /// address, so we just `callee(args)`. A `mut`/`out` parameter — declared in
    /// the *pointer's type* — still takes its argument by `&`, matching the
    /// callee function's ABI exactly as a direct call would.
    fn emit_fn_ptr_invoke(&mut self, callee: ExprId, args: &[ExprId]) -> String {
        let convs: Vec<Conv> = match self.info.type_of(callee) {
            Ty::Fn { params, .. } => params.iter().map(|(c, _)| *c).collect(),
            _ => Vec::new(),
        };
        let c = self.emit_expr(callee);
        let mut parts = Vec::new();
        for (i, a) in args.iter().enumerate() {
            let e = if matches!(convs.get(i), Some(Conv::Mut) | Some(Conv::Out)) {
                self.emit_addr_arg(*a)
            } else {
                self.emit_expr(*a)
            };
            parts.push(e);
        }
        format!("{c}({})", parts.join(", "))
    }

    // --- generic-struct methods ---

    /// Find a method `method` on struct `ctor` (a generic-struct constructor or
    /// a plain struct declared with methods).
    fn find_struct_method_cg(&self, ctor: &str, method: &str) -> Option<&'a FnDecl> {
        if let Some(cf) = self.find_fn(ctor) {
            if let Some(body) = self.ctor_struct_body(cf) {
                for m in &body.members {
                    if let StructMember::Method(f) = m {
                        if f.name.name == method {
                            return Some(f);
                        }
                    }
                }
            }
        }
        for item in &self.ast.items {
            if let Item::Struct { name, body, .. } = item {
                if name.name == ctor {
                    for m in &body.members {
                        if let StructMember::Method(f) = m {
                            if f.name.name == method {
                                return Some(f);
                            }
                        }
                    }
                }
            }
        }
        None
    }

    /// The comptime type-parameter names of a generic struct's constructor
    /// (empty for a plain, non-generic struct).
    fn ctor_tp_names(&self, ctor: &str) -> Vec<String> {
        self.find_fn(ctor).map(|f| self.type_param_names(f)).unwrap_or_default()
    }

    /// The substitution binding a struct's type parameters to concrete arguments.
    fn method_subst(&self, ctor: &str, args: &[Ty]) -> HashMap<String, Ty> {
        self.ctor_tp_names(ctor).into_iter().zip(args.iter().cloned()).collect()
    }

    /// The C name of a monomorphized method, e.g. `jestyr_List__i32_push`.
    fn method_c_name(&self, ctor: &str, args: &[Ty], method: &str) -> String {
        if args.is_empty() {
            format!("jestyr_{ctor}_{method}")
        } else {
            let a: Vec<String> = args.iter().map(|t| self.ty_mangle(t)).collect();
            format!("jestyr_{ctor}__{}_{method}", a.join("_"))
        }
    }

    /// The C type of a method's `self` (the monomorphized receiver struct).
    fn method_self_cty(&self, ctor: &str, args: &[Ty]) -> String {
        if args.is_empty() {
            format!("Jestyr_{ctor}")
        } else {
            self.gen_struct_c_name(ctor, args)
        }
    }

    /// Like `params_str`, but renders `self` as a real parameter (`j_self`),
    /// by pointer for a `mut`/`out self`.
    fn method_params_str(&mut self, f: &FnDecl) -> String {
        let mut parts = Vec::new();
        for p in &f.params {
            if p.comptime {
                continue;
            }
            if p.is_self {
                // An exclusive (`mut`/`out`) self is non-aliasing → `restrict`.
                let suffix = if self.self_is_ptr { "* restrict" } else { "" };
                parts.push(format!("{}{suffix} j_self", self.self_cty));
                continue;
            }
            let base = match p.ty {
                Some(t) => self.c_ty_ast(t),
                None => "int".to_string(),
            };
            let cty = borrow_ptr_cty(&base, p.conv);
            parts.push(format!("{cty} j_{}", p.name.name));
        }
        if parts.is_empty() {
            "void".to_string()
        } else {
            parts.join(", ")
        }
    }

    /// Emit a monomorphized method as a top-level C function — a forward
    /// prototype (`body = false`) or a full definition (`body = true`).
    fn emit_method_decl(&mut self, ctor: &str, args: &[Ty], f: &FnDecl, body: bool) {
        self.subst = self.method_subst(ctor, args);
        self.self_cty = self.method_self_cty(ctor, args);
        let self_conv =
            f.params.iter().find(|p| p.is_self).map(|p| p.conv).unwrap_or(Conv::Default);
        self.self_is_ptr = matches!(self_conv, Conv::Mut | Conv::Out);
        self.ptr_params = f
            .params
            .iter()
            .filter(|p| !p.comptime && !p.is_self && matches!(p.conv, Conv::Mut | Conv::Out))
            .map(|p| p.name.name.clone())
            .collect();
        self.cur_ensures.clear();
        // `@no_panic`/`@inline`/`@cold`/… are honoured on methods too (they emit as
        // free C functions), so the attribute machinery must follow them here.
        self.cur_no_panic = f.no_panic;

        let prefix = self.fn_attr_prefix(f);
        // A fallible method returns its tagged result struct, exactly as a fallible
        // free function does — the ok type is the declared return lowered through
        // this INSTANCE's substitution, so a generic struct's method gets one result
        // type per instantiation. Setting `cur_result` is what makes `ok`/`err`/`?`
        // inside the body Just Work: they only ever consult it.
        let ret = if f.errors.is_some() {
            let ok = f.ret_ty.map(|t| self.ast_type_to_ty(t, &self.subst)).unwrap_or(Ty::Unit);
            self.result_c_name(&ok)
        } else {
            match f.ret_ty {
                Some(t) => self.c_ty_ast(t),
                None => "void".to_string(),
            }
        };
        self.cur_result = if f.errors.is_some() { ret.clone() } else { String::new() };
        let cname = self.method_c_name(ctor, args, &f.name.name);
        let params = self.method_params_str(f);
        if body {
            self.raw(format!("{prefix}{ret} {cname}({params})\n"));
            self.emit_body(&f.body, ret != "void");
            self.raw("\n");
        } else {
            self.raw(format!("{prefix}{ret} {cname}({params});\n"));
        }

        self.ptr_params.clear();
        self.self_cty.clear();
        self.self_is_ptr = false;
        self.subst.clear();
        self.cur_no_panic = false;
        self.cur_result.clear();
    }

    fn method_protos(&mut self) {
        for (ctor, args, method) in self.method_instances.clone() {
            if let Some(f) = self.find_struct_method_cg(&ctor, &method) {
                self.emit_method_decl(&ctor, &args, f, false);
            }
        }
        self.raw("\n");
    }

    fn method_defs(&mut self) {
        for (ctor, args, method) in self.method_instances.clone() {
            if let Some(f) = self.find_struct_method_cg(&ctor, &method) {
                self.emit_method_decl(&ctor, &args, f, true);
            }
        }
    }

    // --- trait static dispatch (traits, Stage C) ---

    /// The `FnDecl` of an `impl Trait for Type` method, located by the same
    /// `(trait, type-key, method)` triple the type checker recorded in
    /// [`ImplCall`]. Used at a call site to recover the receiver/argument
    /// conventions; `None` only if the impl/method has gone missing.
    fn find_impl_method(
        &self,
        trait_name: &str,
        type_key: &str,
        method: &str,
    ) -> Option<&'a FnDecl> {
        let empty = HashMap::new();
        for item in &self.ast.items {
            let Item::Impl(im) = item else { continue };
            if im.trait_name.name != trait_name {
                continue;
            }
            let target = self.ast_type_to_ty(im.ty, &empty);
            if self.info.table.ty_key(&target) != type_key {
                continue;
            }
            if let Some(f) = im.methods.iter().find(|f| f.name.name == method) {
                return Some(f);
            }
        }
        None
    }

    /// Emit one trait-`impl` method as a top-level C function — a forward
    /// prototype (`body = false`) or a full definition (`body = true`). An `impl`
    /// targets a *concrete* type, so there is no monomorphization: the method is
    /// essentially a free function whose first parameter is the receiver (`j_self`,
    /// taken by pointer for a `mut`/`out self`), reusing the struct-method
    /// machinery for `self`.
    fn emit_impl_method_decl(&mut self, im: &ImplDecl, f: &FnDecl, body: bool) {
        // A fallible impl method stays refused — and the real refusal now lives in
        // TYPECK, where it can explain itself. The reason is semantic, not an emission
        // gap: a call through the trait is typed by the TRAIT's signature, which
        // cannot declare an error set (there is no syntax for it), so a fallible impl
        // would be silently mistyped as infallible at every call site. This emission
        // guard is the backstop, kept so a future typeck regression degrades to a
        // diagnostic rather than to C that reads a result struct as its ok type.
        if f.errors.is_some() {
            if body {
                self.diag(
                    f.name.span,
                    "a trait-impl method cannot be fallible: calls are typed by the trait's signature, which has no error set",
                );
            }
            return;
        }
        let empty = HashMap::new();
        let target = self.ast_type_to_ty(im.ty, &empty);
        let type_key = self.info.table.ty_key(&target);

        self.subst.clear();
        self.self_cty = self.c_ty_ast(im.ty);
        let self_conv =
            f.params.iter().find(|p| p.is_self).map(|p| p.conv).unwrap_or(Conv::Default);
        self.self_is_ptr = matches!(self_conv, Conv::Mut | Conv::Out);
        self.ptr_params = f
            .params
            .iter()
            .filter(|p| !p.comptime && !p.is_self && matches!(p.conv, Conv::Mut | Conv::Out))
            .map(|p| p.name.name.clone())
            .collect();
        self.cur_ensures.clear();
        self.cur_no_panic = f.no_panic;

        let prefix = self.fn_attr_prefix(f);
        let ret = match f.ret_ty {
            Some(t) => self.c_ty_ast(t),
            None => "void".to_string(),
        };
        self.cur_result.clear();
        let cname = impl_method_c_name(&im.trait_name.name, &type_key, &f.name.name);
        let params = self.method_params_str(f);
        if body {
            self.raw(format!("{prefix}{ret} {cname}({params})\n"));
            self.emit_body(&f.body, ret != "void");
            self.raw("\n");
        } else {
            self.raw(format!("{prefix}{ret} {cname}({params});\n"));
        }

        self.ptr_params.clear();
        self.self_cty.clear();
        self.self_is_ptr = false;
        self.subst.clear();
        self.cur_no_panic = false;
        self.cur_result.clear();
    }

    fn impl_protos(&mut self) {
        let ast = self.ast;
        let mut any = false;
        for (i, item) in ast.items.iter().enumerate() {
            self.cur_mod = self.item_module(i);
            if let Item::Impl(im) = item {
                // A blanket `impl[T] …` is monomorphized per instance separately.
                if !im.generics.is_empty() {
                    continue;
                }
                for f in &im.methods {
                    self.emit_impl_method_decl(im, f, false);
                    any = true;
                }
            }
        }
        if self.emit_generic_drop_methods(false) {
            any = true;
        }
        if any {
            self.raw("\n");
        }
    }

    /// Per `dyn`-used trait, synthesize the **vtable struct** (one function-pointer
    /// field per method, receiver erased to `void*`) and the **fat-pointer typedef**
    /// `{ data, vtable }` — byte-compatible with a hand-written fn-pointer vtable
    /// (traits, Stage F). Emitted after the struct/result typedefs so a method's
    /// argument/return types are already named.
    fn dyn_typedefs(&mut self) {
        let ast = self.ast;
        let mut traits: Vec<&TraitDecl> = ast
            .items
            .iter()
            .filter_map(|it| match it {
                Item::Trait(t) if self.dyn_traits.contains(&t.name.name) => Some(t),
                _ => None,
            })
            .collect();
        traits.sort_by(|a, b| a.name.name.cmp(&b.name.name));
        for t in traits {
            let tname = t.name.name.clone();
            self.raw("typedef struct {\n".to_string());
            for m in &t.methods {
                let ret = m.ret_ty.map(|ty| self.c_ty_ast(ty)).unwrap_or_else(|| "void".to_string());
                let mut params = vec!["void* self".to_string()];
                for p in &m.params {
                    if p.is_self || p.comptime {
                        continue;
                    }
                    let base = p.ty.map(|ty| self.c_ty_ast(ty)).unwrap_or_else(|| "int".to_string());
                    params.push(format!("{} j_{}", borrow_ptr_cty(&base, p.conv), p.name.name));
                }
                self.raw(format!("    {ret} (*{})({});\n", m.name.name, params.join(", ")));
            }
            self.raw(format!("}} JestyrVtable_{tname};\n"));
            self.raw(format!(
                "typedef struct {{ void* data; const JestyrVtable_{tname}* vtable; }} JestyrDyn_{tname};\n"
            ));
        }
        if ast.items.iter().any(|it| matches!(it, Item::Trait(t) if self.dyn_traits.contains(&t.name.name))) {
            self.raw("\n");
        }
    }

    /// Per `impl` of a `dyn`-used trait, emit a **shim** for each method (adapting
    /// the erased `void* self` to the concrete receiver) and a `static const`
    /// **vtable instance** `jestyr_vt_<Trait>__<TypeKey>` wired to those shims, in
    /// trait-method order (traits, Stage F).
    fn dyn_vtables(&mut self) {
        let ast = self.ast;
        let empty = HashMap::new();
        for item in &ast.items {
            let Item::Impl(im) = item else { continue };
            let tname = im.trait_name.name.clone();
            if !self.dyn_traits.contains(&tname) {
                continue;
            }
            let target = self.ast_type_to_ty(im.ty, &empty);
            let key = self.info.table.ty_key(&target);
            let concrete = self.c_ty_ast(im.ty);

            // A shim per impl method: `void* self` is cast back to the concrete type
            // (deref'd for a by-value `read self`, kept as a pointer for `mut self`).
            for f in &im.methods {
                let ret = f.ret_ty.map(|ty| self.c_ty_ast(ty)).unwrap_or_else(|| "void".to_string());
                let self_is_ptr = f
                    .params
                    .iter()
                    .find(|p| p.is_self)
                    .map(|p| matches!(p.conv, Conv::Mut | Conv::Out))
                    .unwrap_or(false);
                let mut sig = vec!["void* self".to_string()];
                let mut call_args =
                    vec![if self_is_ptr { format!("({concrete}*)self") } else { format!("*({concrete}*)self") }];
                for p in &f.params {
                    if p.is_self || p.comptime {
                        continue;
                    }
                    let base = p.ty.map(|ty| self.c_ty_ast(ty)).unwrap_or_else(|| "int".to_string());
                    sig.push(format!("{} j_{}", borrow_ptr_cty(&base, p.conv), p.name.name));
                    call_args.push(format!("j_{}", p.name.name));
                }
                let shim = format!("jestyr_vtshim_{tname}__{key}__{}", f.name.name);
                let target_fn = impl_method_c_name(&tname, &key, &f.name.name);
                let ret_kw = if ret == "void" { "" } else { "return " };
                self.raw(format!(
                    "static {ret} {shim}({}) {{ {ret_kw}{target_fn}({}); }}\n",
                    sig.join(", "),
                    call_args.join(", ")
                ));
            }

            // The vtable instance: fields in *trait-method* order point at the shims.
            let methods: Vec<String> = ast
                .items
                .iter()
                .find_map(|it| match it {
                    Item::Trait(t) if t.name.name == tname => {
                        Some(t.methods.iter().map(|m| m.name.name.clone()).collect())
                    }
                    _ => None,
                })
                .unwrap_or_default();
            let inits: Vec<String> =
                methods.iter().map(|m| format!("jestyr_vtshim_{tname}__{key}__{m}")).collect();
            self.raw(format!(
                "static const JestyrVtable_{tname} jestyr_vt_{tname}__{key} = {{ {} }};\n",
                inits.join(", ")
            ));
        }
    }

    fn impl_defs(&mut self) {
        let ast = self.ast;
        for (i, item) in ast.items.iter().enumerate() {
            self.cur_mod = self.item_module(i);
            if let Item::Impl(im) = item {
                if !im.generics.is_empty() {
                    continue;
                }
                for f in &im.methods {
                    self.emit_impl_method_decl(im, f, true);
                }
            }
        }
        self.emit_generic_drop_methods(true);
    }

    /// A blanket `impl[T] Drop for <Ctor>(T)` covering every instantiation: detect it
    /// by constructor name. Returns the impl and the name of its single generic
    /// parameter, so a concrete instance can substitute it.
    fn generic_drop_impl(&self, ctor: &str) -> Option<(&'a ImplDecl, String)> {
        let empty = HashMap::new();
        for item in &self.ast.items {
            let Item::Impl(im) = item else { continue };
            if im.generics.is_empty() || im.trait_name.name != "Drop" {
                continue;
            }
            if let Ty::GenStruct { ctor: c, .. } = self.ast_type_to_ty(im.ty, &empty) {
                if c == ctor {
                    let g = im.generics.first().map(|g| g.name.name.clone()).unwrap_or_default();
                    return Some((im, g));
                }
            }
        }
        None
    }

    /// Is there a user-written *concrete* (non-generic) `impl Drop for <ty>`? Distinct
    /// from a bare `impl_index` lookup, which a blanket `impl[T] Drop for Ctor(T)` also
    /// populates under its generic-param key (colliding with an instance whose type
    /// argument is named like that param). Compares the lowered impl target by
    /// `ty_key`, matching only genuinely concrete overrides.
    fn has_concrete_drop_impl(&self, ty: &Ty) -> bool {
        let empty = HashMap::new();
        let key = self.info.table.ty_key(ty);
        let ast = self.ast;
        ast.items.iter().any(|it| {
            matches!(it, Item::Impl(im)
                if im.generics.is_empty()
                    && im.trait_name.name == "Drop"
                    && self.info.table.ty_key(&self.ast_type_to_ty(im.ty, &empty)) == key)
        })
    }

    /// Monomorphize a blanket `impl[T] Drop for Ctor(T)` once per concrete instance
    /// of `Ctor` actually used in the program (from `struct_instances`). Emits a
    /// prototype (`body = false`) or full definition, with the impl's generic
    /// parameter substituted to the instance's type argument — so the call site's
    /// `jestyr_impl_Drop__Ctor_C___drop(&j_x)` resolves. Returns whether anything
    /// was emitted (so the caller can manage trailing whitespace). An instance that
    /// already has a *concrete* `impl Drop for Ctor(C)` is skipped (no duplicate).
    fn emit_generic_drop_methods(&mut self, body: bool) -> bool {
        let mut emitted = false;
        for (ctor, args) in self.struct_instances.clone() {
            let Some((im, gparam)) = self.generic_drop_impl(&ctor) else {
                continue;
            };
            let concrete = Ty::GenStruct { ctor: ctor.clone(), args: args.clone() };
            let key = self.info.table.ty_key(&concrete);
            // Skip only if a *concrete* `impl Drop for <this instance>` overrides the
            // blanket (coherence — the concrete wins). A bare `impl_index` lookup is
            // wrong here: a blanket `impl[T] Drop for Ctor(T)` also occupies the index
            // under a key derived from its generic parameter `T`, which collides with
            // an instance whose type argument is *named* `T` (a user `struct T`) — and
            // would then wrongly skip that instance's drop glue.
            if self.has_concrete_drop_impl(&concrete) {
                continue;
            }
            let Some(f) = im.methods.iter().find(|m| m.name.name == "drop") else {
                continue;
            };
            let subst: HashMap<String, Ty> =
                std::iter::once((gparam.clone(), args.first().cloned().unwrap_or(Ty::Unknown))).collect();
            let cname = impl_method_c_name("Drop", &key, "drop");
            let self_cty = self.gen_struct_c_name(&ctor, &args);
            let self_conv =
                f.params.iter().find(|p| p.is_self).map(|p| p.conv).unwrap_or(Conv::Default);

            self.subst = subst;
            self.self_cty = self_cty;
            self.self_is_ptr = matches!(self_conv, Conv::Mut | Conv::Out);
            self.ptr_params = f
                .params
                .iter()
                .filter(|p| !p.comptime && !p.is_self && matches!(p.conv, Conv::Mut | Conv::Out))
                .map(|p| p.name.name.clone())
                .collect();
            self.cur_no_panic = f.no_panic;
            let mut moved = HashSet::new();
            self.collect_moved(&f.body, &mut moved);
            self.cur_moved = moved;
            let params = self.method_params_str(f);
            if body {
                self.raw(format!("void {cname}({params})\n"));
                self.emit_body(&f.body, false);
                self.raw("\n");
            } else {
                self.raw(format!("void {cname}({params});\n"));
            }
            self.subst.clear();
            self.self_cty.clear();
            self.self_is_ptr = false;
            self.ptr_params.clear();
            self.cur_no_panic = false;
            self.cur_moved.clear();
            emitted = true;
        }
        emitted
    }

    /// Lower a trait-method call `recv.m(args)` that resolved through an
    /// `impl Trait for <recv-type>` (recorded in `impl_calls`) to a **direct**
    /// call of the mangled impl-method function — the receiver threaded in as the
    /// first argument (by `&` for a `mut`/`out self`), `mut`/`out` arguments by
    /// address. Static dispatch: no vtable, the target is known at compile time.
    fn emit_impl_call(&mut self, callee: ExprId, ic: &ImplCall, args: &[ExprId]) -> String {
        let base = match &self.ast.expr_at(callee).kind {
            ExprKind::Field { base, .. } => *base,
            _ => return "0".to_string(),
        };
        let (self_conv, arg_convs): (Conv, Vec<Conv>) = self
            .find_impl_method(&ic.trait_name, &ic.type_key, &ic.method)
            .map(|f| {
                let sc =
                    f.params.iter().find(|p| p.is_self).map(|p| p.conv).unwrap_or(Conv::Default);
                let ac: Vec<Conv> =
                    f.params.iter().filter(|p| !p.comptime && !p.is_self).map(|p| p.conv).collect();
                (sc, ac)
            })
            .unwrap_or((Conv::Default, Vec::new()));

        let recv = if matches!(self_conv, Conv::Mut | Conv::Out) {
            self.emit_addr_arg(base)
        } else {
            self.emit_expr(base)
        };
        let mut parts = vec![recv];
        for (i, a) in args.iter().enumerate() {
            let e = if matches!(arg_convs.get(i), Some(Conv::Mut) | Some(Conv::Out)) {
                self.emit_addr_arg(*a)
            } else {
                self.emit_expr(*a)
            };
            parts.push(e);
        }
        let name = impl_method_c_name(&ic.trait_name, &ic.type_key, &ic.method);
        format!("{name}({})", parts.join(", "))
    }

    /// Lower an operator-trait binary op (Stage E) to a call of its impl method:
    /// `a + b` → `jestyr_impl_Add__<T>__add(a, b)`. The four base operators
    /// (`+`/`-`/`*`/`/`/`==`/`<`) call directly; the **derived** comparisons reuse
    /// one base method with a swap and/or negate: `!=` → `!eq(a,b)`, `>` →
    /// `lt(b,a)`, `<=` → `!lt(b,a)`, `>=` → `!lt(a,b)`. The receiver operand is
    /// taken by `&` for a `mut`/`out` `self` (operators are `read` by convention,
    /// so usually by value).
    fn emit_operator_call(&mut self, ic: &ImplCall, op: BinOp, lhs: ExprId, rhs: ExprId) -> String {
        use BinOp::*;
        // `>` and `<=` compare via `b < a`, so the receiver is the *right* operand.
        let swap = matches!(op, Gt | Le);
        // `!=`/`<=`/`>=` are the logical negation of their base comparison.
        let negate = matches!(op, Ne | Le | Ge);
        let (self_conv, rhs_conv) = self
            .find_impl_method(&ic.trait_name, &ic.type_key, &ic.method)
            .map(|f| {
                let sc =
                    f.params.iter().find(|p| p.is_self).map(|p| p.conv).unwrap_or(Conv::Default);
                let rc = f
                    .params
                    .iter()
                    .find(|p| !p.comptime && !p.is_self)
                    .map(|p| p.conv)
                    .unwrap_or(Conv::Default);
                (sc, rc)
            })
            .unwrap_or((Conv::Default, Conv::Default));
        let (recv_id, arg_id) = if swap { (rhs, lhs) } else { (lhs, rhs) };
        let recv = if matches!(self_conv, Conv::Mut | Conv::Out) {
            self.emit_addr_arg(recv_id)
        } else {
            self.emit_expr(recv_id)
        };
        let arg = if matches!(rhs_conv, Conv::Mut | Conv::Out) {
            self.emit_addr_arg(arg_id)
        } else {
            self.emit_expr(arg_id)
        };
        let name = impl_method_c_name(&ic.trait_name, &ic.type_key, &ic.method);
        let call = format!("{name}({recv}, {arg})");
        if negate {
            format!("(!{call})")
        } else {
            call
        }
    }

    /// Wrap a concrete value into a `dyn Trait` fat pointer (Stage F): build
    /// `{ &value, &<vtable for its type> }`. The data pointer must outlive the call
    /// the `dyn` is passed to — so a **scalar** is placed in a fresh compound
    /// literal `&((T){ v })` (automatic storage of the *enclosing block*, not a
    /// dangling statement-expression temp), while an aggregate's address is taken
    /// directly (its source is a local/field lvalue, or itself a compound literal).
    fn emit_dyn_coercion(&mut self, id: ExprId, trait_name: &str) -> String {
        let concrete = self.info.type_of(id).clone();
        let key = self.info.table.ty_key(&concrete);
        let cty = self.c_type(&concrete);
        self.dyn_guard.insert(id);
        let inner = self.emit_expr(id);
        self.dyn_guard.remove(&id);
        let data = if is_scalar_ty(&concrete) {
            format!("&(({cty}){{ {inner} }})")
        } else {
            format!("&({inner})")
        };
        format!("(JestyrDyn_{trait_name}){{ {data}, &jestyr_vt_{trait_name}__{key} }}")
    }

    // --- structured concurrency (`concurrent` / `spawn`) ---

    fn collect_spawns(&self) -> Vec<SpawnSite> {
        let mut out = Vec::new();
        for item in &self.ast.items {
            match item {
                Item::Fn(f) if !self.is_generic(f) => self.find_spawns_block(&f.body, &mut out),
                Item::Struct { body, .. } => {
                    for m in &body.members {
                        if let StructMember::Method(f) = m {
                            self.find_spawns_block(&f.body, &mut out);
                        }
                    }
                }
                _ => {}
            }
        }
        out
    }

    fn find_spawns_block(&self, b: &Block, out: &mut Vec<SpawnSite>) {
        for s in &b.stmts {
            match s {
                Stmt::Let { init: Some(e), .. } => self.find_spawns_expr(*e, out),
                Stmt::Return { value: Some(v), .. } => self.find_spawns_expr(*v, out),
                Stmt::Expr(e) => self.find_spawns_expr(*e, out),
                _ => {}
            }
        }
    }

    fn find_spawns_expr(&self, id: ExprId, out: &mut Vec<SpawnSite>) {
        match &self.ast.expr_at(id).kind {
            ExprKind::Concurrent(b) | ExprKind::Block(b) | ExprKind::Unsafe(b) => {
                self.find_spawns_block(b, out)
            }
            ExprKind::Spawn(inner) => {
                if let Some(site) = self.spawn_site(*inner) {
                    out.push(site);
                }
            }
            ExprKind::If { cond, then, els } => {
                self.find_spawns_expr(*cond, out);
                self.find_spawns_block(then, out);
                if let Some(e) = els {
                    self.find_spawns_expr(*e, out);
                }
            }
            ExprKind::Match { scrut, arms } => {
                self.find_spawns_expr(*scrut, out);
                for a in arms {
                    if let Some(g) = a.guard {
                        self.find_spawns_expr(g, out);
                    }
                    self.find_spawns_expr(a.body, out);
                }
            }
            ExprKind::For { body, els, .. } => {
                self.find_spawns_block(body, out);
                if let Some(els) = els {
                    self.find_spawns_block(els, out);
                }
            }
            ExprKind::Select(arms) => {
                for arm in arms {
                    self.find_spawns_expr(arm.chan, out);
                    self.find_spawns_block(&arm.body, out);
                }
            }
            _ => {}
        }
    }

    /// Extract `(call id, fn name, args)` from a `spawn`'s inner expression — a
    /// direct call `f(args)`.
    fn spawn_site(&self, inner: ExprId) -> Option<SpawnSite> {
        if let ExprKind::Call { callee, args } = &self.ast.expr_at(inner).kind {
            if let ExprKind::Name(n) = &self.ast.expr_at(*callee).kind {
                return Some(SpawnSite { call_id: inner, fn_name: n.name.clone(), args: args.clone() });
            }
        }
        None
    }

    /// Emit, for each spawn site, an argument struct (the task's captured args)
    /// and a `void*` trampoline that unpacks it and calls the target function.
    fn spawn_runtime(&mut self) {
        for site in self.spawn_sites.clone() {
            let Some(f) = self.find_fn(&site.fn_name) else {
                self.diag(
                    self.ast.expr_at(site.call_id).span,
                    format!("`spawn`: unknown function `{}`", site.fn_name),
                );
                continue;
            };
            let id = site.call_id.0;
            let ret_tid = f.ret_ty; // Option<TypeId> (Copy) — capture before &mut self use
            let runtime: Vec<&Param> = f.params.iter().filter(|p| !p.comptime && !p.is_self).collect();
            // A result-bearing task stores its return value in the arg struct's `ret`
            // field, which `await` reads after the join. `void` targets have no `ret`.
            let runtime_tys: Vec<String> = runtime
                .iter()
                .map(|p| match p.ty {
                    Some(t) => self.c_ty_ast(t),
                    None => "int".to_string(),
                })
                .collect();
            let ret_cty = ret_tid.map(|t| self.c_ty_ast(t)).filter(|c| c != "void");

            self.raw(format!("struct _jsp_{id} {{ "));
            if runtime.is_empty() && ret_cty.is_none() {
                self.raw("char _unused; ");
            }
            for (i, cty) in runtime_tys.iter().enumerate() {
                self.raw(format!("{cty} a{i}; "));
            }
            if let Some(rc) = &ret_cty {
                self.raw(format!("{rc} ret; "));
            }
            self.raw("};\n");

            let call_args: Vec<String> = (0..runtime.len()).map(|i| format!("_a->a{i}")).collect();
            let callee = self.c_fn_name(&site.fn_name); // honour `@no_mangle` spawn targets
            let call = format!("{callee}({})", call_args.join(", "));
            self.raw(format!("static void* jestyr_task_{id}(void* _vp) {{ "));
            self.raw(format!("struct _jsp_{id}* _a = (struct _jsp_{id}*)_vp; "));
            if ret_cty.is_some() {
                self.raw(format!("_a->ret = {call}; return NULL; }}\n"));
            } else {
                self.raw(format!("{call}; return NULL; }}\n"));
            }
        }
        if !self.spawn_sites.is_empty() {
            self.raw("\n");
        }
    }

    /// Lower a `par for v in xs reduce(r) { body }` to a statement-expression: map
    /// each element through `body` (serially) into a scratch `[]i64`, then run the
    /// **deterministic parallel reduction** `core.par_reduce` over it (the engine that
    /// guarantees bit-identical-to-serial for any thread schedule). The map is
    /// element-wise (always deterministic); the parallel, reassociation-sensitive part
    /// is `par_reduce`, whose reduction was already checked deterministic by typeck.
    /// Requires `import "core"` (the reduction value comes from there, so it always is).
    /// Lower `par for x in xs reduce(r) { … }`.
    ///
    /// The shape is map-then-reduce: run the body once per element into an `int64_t`
    /// buffer, then hand *that* to the deterministic engine. Which is why the SOURCE
    /// element type is free — `core.par_reduce` never sees the source slice — while the
    /// reduction stays `i64`, where the declared operators are exactly associative.
    ///
    /// The loop variable is declared with the element's own C type, so a body over
    /// `i32` computes in `i32`; only the per-element contribution is widened, once, on
    /// the way into the buffer. An `i64` source emits exactly what it always did, so
    /// every existing program's C is byte-identical.
    fn emit_par_for(&mut self, site: ExprId, var: &Ident, iter: ExprId, reduction: ExprId, body: ExprId) -> String {
        let n = self.tmp;
        self.tmp += 1;
        let elem = match apply_subst(&self.info.type_of(iter).clone(), &self.subst) {
            Ty::Slice(e) => (*e).clone(),
            Ty::Array { elem, .. } => (*elem).clone(),
            _ => Ty::Prim("i64"),
        };
        let elem = if matches!(&elem, Ty::Prim(p) if is_integer_c_prim(p)) {
            elem
        } else {
            Ty::Prim("i64")
        };
        let sl = self.slice_c_name(&elem);
        let i64sl = self.slice_c_name(&Ty::Prim("i64"));
        let ecty = self.c_type(&elem);
        let src = self.emit_expr(iter);
        let red = self.emit_expr(reduction);
        let vname = format!("j_{}", var.name);
        let bodyc = self.emit_expr(body); // references the loop var as `j_<var>`
        // The contribution cast is omitted when the body is already `i64`, so the
        // pre-existing lowering is reproduced character for character.
        let body_ty = apply_subst(&self.info.type_of(body).clone(), &self.subst);
        let contrib = if matches!(&body_ty, Ty::Prim("i64")) {
            bodyc
        } else {
            format!("(int64_t)({bodyc})")
        };
        let prc = self.c_fn_name("par_reduce");

        // The vector path, for a site an `@simd` function declared and the legality pass
        // certified. Shape: a lane-at-a-time head, then the SCALAR remainder — and the
        // remainder genuinely must be scalar, because a mask blend is not a conditional
        // and a lane comparison is not `0`/`1`. Results are stored lane by lane rather
        // than converted as a vector, which keeps the widening to `int64_t` exact for
        // every element width without reaching for `__builtin_convertvector`.
        if let Some(elem_v) = self.simd_sites.get(&site).cloned() {
            // The vector computes in the element's PROMOTED type, because the scalar
            // remainder below does — see `simd_compute_elem`. For an element that C does
            // not promote this is the element itself, so every program that vectorized
            // before emits exactly the C it emitted before.
            let cv = simd_compute_elem(&elem_v);
            let vt = self.simd_vec_name(&cv);
            let w = simd_lanes(&cv);
            // A promoted element is widened lane by lane on the way in; the conversion is
            // value-preserving and is the same one the remainder's promotion performs. An
            // unpromoted element is copied as raw bytes, which is both faster and the
            // byte-identical status quo.
            let load = if cv == elem_v {
                format!("memcpy(&_pv{n}, _pf{n}.ptr + _pi{n}, sizeof({vt}));")
            } else {
                format!(
                    "for (size_t _pl{n} = 0; _pl{n} < {w}; _pl{n}++) \
                     _pv{n}[_pl{n}] = _pf{n}.ptr[_pi{n} + _pl{n}];"
                )
            };
            let vbody = self.emit_expr_simd(body, &vt, &var.name, &format!("_pv{n}"));
            return format!(
                "({{ {sl} _pf{n} = {src}; \
                 int64_t* _pm{n} = (int64_t*)malloc(_pf{n}.len * sizeof(int64_t)); \
                 size_t _pi{n} = 0; \
                 for (; _pi{n} + {w} <= _pf{n}.len; _pi{n} += {w}) {{ \
                 {vt} _pv{n}; {load} \
                 {vt} _pw{n} = (({vt}){{0}}) + ({vbody}); \
                 for (size_t _pk{n} = 0; _pk{n} < {w}; _pk{n}++) \
                 _pm{n}[_pi{n} + _pk{n}] = (int64_t)_pw{n}[_pk{n}]; }} \
                 for (; _pi{n} < _pf{n}.len; _pi{n}++) {{ \
                 {ecty} {vname} = _pf{n}.ptr[_pi{n}]; _pm{n}[_pi{n}] = {contrib}; }} \
                 int64_t _pr{n} = {prc}(({i64sl}){{ _pm{n}, _pf{n}.len }}, {red}); \
                 free(_pm{n}); _pr{n}; }})"
            );
        }

        format!(
            "({{ {sl} _pf{n} = {src}; \
             int64_t* _pm{n} = (int64_t*)malloc(_pf{n}.len * sizeof(int64_t)); \
             for (size_t _pi{n} = 0; _pi{n} < _pf{n}.len; _pi{n}++) {{ \
             {ecty} {vname} = _pf{n}.ptr[_pi{n}]; _pm{n}[_pi{n}] = {contrib}; }} \
             int64_t _pr{n} = {prc}(({i64sl}){{ _pm{n}, _pf{n}.len }}, {red}); \
             free(_pm{n}); _pr{n}; }})"
        )
    }

    /// The C return type a `spawn` target stores in its task box (`None` for a
    /// `void` target). Mirrors the `ret` field decided in [`Cgen::spawn_runtime`].
    fn task_ret_cty(&mut self, fn_name: &str) -> Option<String> {
        let ret_tid = self.find_fn(fn_name).and_then(|f| f.ret_ty);
        ret_tid.map(|t| self.c_ty_ast(t)).filter(|c| c != "void")
    }

    /// Emit the thread-creation triple for one spawn at handle index `h`: declare
    /// the `pthread_t`, build the arg box (args zero-extend the `ret` field), and
    /// `pthread_create` the trampoline.
    fn emit_spawn_create(&mut self, site: &SpawnSite, h: usize) {
        let id = site.call_id.0;
        let vals: Vec<String> = site.args.iter().map(|a| self.emit_expr(*a)).collect();
        let init = if vals.is_empty() { "{0}".to_string() } else { format!("{{ {} }}", vals.join(", ")) };
        self.line(format!("pthread_t _jt{h};"));
        self.line(format!("struct _jsp_{id} _ja{h} = {init};"));
        self.line(format!("pthread_create(&_jt{h}, NULL, jestyr_task_{id}, &_ja{h});"));
    }

    /// Push one dynamic-N spawn onto the growable handle array `_dt`/`_da` (declared
    /// by `emit_concurrent`). Each task's arg box is heap-allocated (a stable address
    /// the thread reads — the arrays may `realloc`-move, but the boxes don't); the box
    /// is freed after its `pthread_join`. Used for a `spawn` inside a loop, where the
    /// worker count is a runtime value.
    fn emit_dyn_spawn(&mut self, inner: ExprId) {
        let Some(site) = self.spawn_site(inner) else {
            self.diag(self.ast.expr_at(inner).span, "`spawn` expects a direct function call");
            return;
        };
        let id = site.call_id.0;
        let vals: Vec<String> = site.args.iter().map(|a| self.emit_expr(*a)).collect();
        let init = if vals.is_empty() { "{0}".to_string() } else { format!("{{ {} }}", vals.join(", ")) };
        self.line("if (_dn == _dc) { _dc = _dc ? _dc * 2 : 8; _dt = (pthread_t*)realloc(_dt, _dc * sizeof(pthread_t)); _da = (void**)realloc(_da, _dc * sizeof(void*)); }");
        self.line(format!("struct _jsp_{id}* _dap = (struct _jsp_{id}*)malloc(sizeof(struct _jsp_{id})); *_dap = (struct _jsp_{id}){init};"));
        self.line(format!("_da[_dn] = _dap; pthread_create(&_dt[_dn], NULL, jestyr_task_{id}, _dap); _dn++;"));
    }

    /// Does `stmt` contain a `spawn` *not* at the concurrent block's top level (i.e.
    /// nested in a loop/if) — a dynamic-N spawn needing the growable handle array?
    /// Top-level fixed forms (a bare `spawn` statement, a `let h = spawn`) are handled
    /// by numbered handles and don't count.
    fn stmt_has_nested_spawn(&self, stmt: &Stmt) -> bool {
        let e = match stmt {
            Stmt::Expr(e) | Stmt::Let { init: Some(e), .. } => *e,
            _ => return false,
        };
        if matches!(self.ast.expr_at(e).kind, ExprKind::Spawn(_)) {
            return false; // a top-level fixed spawn
        }
        let mut v = Vec::new();
        self.find_spawns_expr(e, &mut v);
        !v.is_empty()
    }

    /// Lower a `concurrent { … }` nursery: each `spawn` creates a thread; the scope
    /// joins them all before it exits (structured concurrency). A `let h = spawn …`
    /// binds an awaitable task handle whose result is read back by `await h`; such a
    /// handle carries a `_jd` joined-flag so an `await` and the brace's safety-net
    /// join don't double-join. Bare `spawn` statements stay fire-and-forget. A `spawn`
    /// *inside a loop* is dynamic-N: it pushes onto a growable handle array joined at
    /// the brace (so the worker count can be a runtime value).
    fn emit_concurrent(&mut self, block: &Block) {
        let ast = self.ast;
        let saved_handles = std::mem::take(&mut self.task_handles);
        let saved_dyn = self.dyn_spawn_active;
        self.line("{");
        self.depth += 1;
        let mut handles = 0usize;
        // Declare the growable dynamic-spawn array iff some statement spawns in a loop.
        let needs_dyn = block.stmts.iter().any(|s| self.stmt_has_nested_spawn(s));
        if needs_dyn {
            self.line("pthread_t* _dt = NULL; void** _da = NULL; size_t _dn = 0, _dc = 0;");
        }
        self.dyn_spawn_active = needs_dyn;
        // (handle index, guarded?) — let-bound handles guard the brace-join with
        // their `_jd` flag (an `await` may have joined already); bare spawns don't.
        let mut joins: Vec<(usize, bool)> = Vec::new();

        for stmt in &block.stmts {
            // `let h = spawn f(args)` — a result task bound to an awaitable handle.
            if let Stmt::Let { name, init: Some(e), .. } = stmt {
                if let ExprKind::Spawn(inner) = &ast.expr_at(*e).kind {
                    if let Some(site) = self.spawn_site(*inner) {
                        let h = handles;
                        self.emit_spawn_create(&site, h);
                        self.line(format!("int _jd{h} = 0;"));
                        let ret_cty = self.task_ret_cty(&site.fn_name);
                        self.task_handles.insert(name.name.clone(), TaskHandle { idx: h, ret_cty });
                        joins.push((h, true));
                        handles += 1;
                        continue;
                    }
                    self.diag(ast.expr_at(*e).span, "`spawn` expects a direct function call");
                    continue;
                }
            }
            // Bare `spawn f(args)` — fire-and-forget, joined at the brace.
            if let Stmt::Expr(e) = stmt {
                if let ExprKind::Spawn(inner) = &ast.expr_at(*e).kind {
                    if let Some(site) = self.spawn_site(*inner) {
                        let h = handles;
                        self.emit_spawn_create(&site, h);
                        joins.push((h, false));
                        handles += 1;
                        continue;
                    }
                    self.diag(ast.expr_at(*e).span, "`spawn` expects a direct function call");
                    continue;
                }
            }
            self.emit_stmt(stmt);
        }
        for (h, guarded) in joins {
            if guarded {
                self.line(format!("if (!_jd{h}) pthread_join(_jt{h}, NULL);"));
            } else {
                self.line(format!("pthread_join(_jt{h}, NULL);"));
            }
        }
        // Join every dynamically-spawned task (any order — structured join), free each
        // task's arg box, then the arrays.
        if needs_dyn {
            self.line("for (size_t _dk = 0; _dk < _dn; _dk++) { pthread_join(_dt[_dk], NULL); free(_da[_dk]); }");
            self.line("free(_dt); free(_da);");
        }
        self.depth -= 1;
        self.line("}");
        self.task_handles = saved_handles;
        self.dyn_spawn_active = saved_dyn;
    }

    /// Lower a `select { recv(ch) => x { body } … }`: hoist each channel value, then
    /// spin, and the first arm whose channel has a buffered item receives it and runs
    /// (an `else if` chain so exactly one arm fires per pass). Single-consumer (the
    /// `len > 0` then `recv` is race-free when this is the only receiver). Reuses the
    /// non-generic `channel_len_i64`/`channel_recv_i64` wrappers from `std/sync.jtr`.
    fn emit_select(&mut self, arms: &[SelectArm]) {
        self.line("{");
        self.depth += 1;
        // Hoist each channel to a local so it is evaluated once, not per spin.
        let mut chvars = Vec::new();
        for (i, arm) in arms.iter().enumerate() {
            let cty = self.c_type(&self.info.type_of(arm.chan).clone());
            let v = self.emit_expr(arm.chan);
            self.line(format!("{cty} _sel{i} = {v};"));
            chvars.push(i);
        }
        self.line("int _seldone = 0;");
        self.line("while (!_seldone) {");
        self.depth += 1;
        for (i, arm) in arms.iter().enumerate() {
            let lead = if i == 0 { "if" } else { "else if" };
            self.line(format!("{lead} (jestyr_channel_len_i64(_sel{i}) > 0) {{"));
            self.depth += 1;
            self.line(format!("int64_t j_{} = jestyr_channel_recv_i64(_sel{i});", arm.bind.name));
            for stmt in &arm.body.stmts {
                self.emit_stmt(stmt);
            }
            self.line("_seldone = 1;");
            self.depth -= 1;
            self.line("}");
        }
        self.depth -= 1;
        self.line("}");
        self.depth -= 1;
        self.line("}");
    }

    /// Lower a `for` loop (see `docs/loops-spec.md`). The header selects the shape;
    /// a range loop also wires its index into the refinement proof so `xs[i]` in
    /// the body elides its bounds check. An optional `region` wraps the loop in a
    /// scratch arena that is reset (O(1)) each iteration and freed once at the end.
    fn emit_for(
        &mut self,
        label: Option<&Ident>,
        head: &ForHead,
        region: Option<&Ident>,
        body: &Block,
        els: Option<&Block>,
    ) {
        // Region-scoped loop: open the arena once, arm the per-iteration reset
        // (consumed by `emit_loop_body`), and free the arena after the loop.
        if let Some(r) = region {
            self.line("{");
            self.depth += 1;
            self.line(format!("JestyrArena j_{} = jestyr_arena_new(1u << 20);", r.name));
            self.scratch_reset = Some(r.name.clone());
        }
        // Hoist a tracker before the loop for each `variant` in the body, and arm
        // the continue-label target for the body.
        let saved_trackers = std::mem::take(&mut self.variant_trackers);
        self.hoist_variant_trackers(body);
        if let Some(l) = label {
            self.cont_label = Some(l.name.clone());
        }
        // A loop with an `else` needs its break target placed *after* the `else`,
        // so a `break` skips it. Reuse the user's label if present, else synthesize
        // a fresh one. (A label-only loop keeps its target right after the body —
        // there is no `else` in between.)
        let eff_label: Option<String> = match (label, els) {
            (Some(l), _) => Some(l.name.clone()),
            (None, Some(_)) => {
                let n = self.tmp;
                self.tmp += 1;
                Some(format!("_fe{n}"))
            }
            (None, None) => None,
        };
        // Arm plain-`break` rerouting for the body: only a loop that *has* an
        // `else` reroutes (to skip it). Setting `None` for an else-less loop
        // correctly models "the nearest loop" across nested loops.
        let saved_break = self.break_label.take();
        self.break_label = if els.is_some() { eff_label.clone() } else { None };
        self.emit_for_inner(head, body);
        self.break_label = saved_break;
        // The `else` runs on normal completion: emitted between the loop and the
        // break target, so falling out of the loop runs it while a `break` (now a
        // `goto <label>__break`) jumps past it.
        if let Some(els_blk) = els {
            self.line("{");
            self.depth += 1;
            for stmt in &els_blk.stmts {
                self.emit_stmt(stmt);
            }
            self.depth -= 1;
            self.line("}");
        }
        if let Some(name) = &eff_label {
            self.line(format!("{name}__break: ;")); // labeled- and/or else-break target
        }
        self.variant_trackers = saved_trackers;
        if let Some(r) = region {
            self.line(format!("jestyr_arena_free(&j_{});", r.name));
            self.depth -= 1;
            self.line("}");
        }
    }

    /// Declare one `int64_t` tracker (init to `INT64_MAX`) per `variant` statement
    /// directly in the loop body, recording the node→tracker mapping.
    fn hoist_variant_trackers(&mut self, body: &Block) {
        for stmt in &body.stmts {
            if let Stmt::Expr(e) = stmt {
                if matches!(&self.ast.expr_at(*e).kind, ExprKind::Variant(_)) {
                    let id = self.tmp;
                    self.tmp += 1;
                    self.variant_trackers.insert(*e, id);
                    self.line(format!("int64_t _vt{id} = INT64_MAX;"));
                }
            }
        }
    }

    /// A loop body that emits the armed per-iteration scratch reset (top), the body
    /// statements, and the labeled-continue target (bottom).
    fn emit_loop_body(&mut self, body: &Block) {
        self.line("{");
        self.depth += 1;
        self.drop_scope_enter();
        if let Some(name) = self.scratch_reset.take() {
            self.line(format!("j_{name}.off = 0;"));
        }
        for stmt in &body.stmts {
            self.emit_stmt(stmt);
        }
        // A droppable created inside the loop drops at the end of each iteration
        // (`break`/`continue` short-circuit it — leak-safe; precise per-path loop
        // drops are future work).
        if matches!(body.stmts.last(), Some(Stmt::Return { .. })) {
            self.drop_scope_exit_discard();
        } else {
            self.drop_scope_exit_emit();
        }
        if let Some(lbl) = self.cont_label.take() {
            self.line(format!("{lbl}__continue: ;"));
        }
        self.depth -= 1;
        self.line("}");
    }

    fn emit_for_inner(&mut self, head: &ForHead, body: &Block) {
        match head {
            ForHead::Infinite => {
                self.line("for (;;)");
                self.emit_loop_body(body);
            }
            ForHead::While(cond) => {
                let c = self.emit_expr(*cond);
                self.line(format!("while ({c})"));
                self.emit_loop_body(body);
            }
            ForHead::Iter { binds, sources, step } => {
                let step = *step;
                if sources.len() >= 2 {
                    // Lockstep zip: `for x, y in xs, ys` (length-checked).
                    self.emit_zip_for(binds, sources, body);
                    return;
                }
                let src = sources[0];
                let b0 = binds[0].clone();
                if let ExprKind::Range { lo, hi, inclusive } = &self.ast.expr_at(src).kind {
                    let (lo, hi, inclusive) = (*lo, *hi, *inclusive);
                    self.emit_range_for(&b0.name, src, lo, hi, inclusive, step, body);
                } else if let Some(s_arg) = self.codepoints_iter_arg(src) {
                    // `for cp in codepoints(s)` — O(n) UTF-8 decode (cost in the name);
                    // an optional second binding is the codepoint's byte offset (Go-style).
                    let off = binds.get(1).map(|b| b.name.clone());
                    self.emit_codepoints_for(&b0.name, off.as_ref(), s_arg, body);
                } else if let Some(g_arg) = self.graphemes_iter_arg(src) {
                    // `for g in graphemes(s)` — each `g` is a `str` grapheme cluster.
                    self.emit_graphemes_for(&b0.name, g_arg, body);
                } else if let Some((s_arg, sep_arg)) = self.split_iter_arg(src) {
                    // `for part in split(s, sep)` — each `part` is a `str` view.
                    self.emit_split_for(&b0.name, s_arg, sep_arg, body);
                } else if matches!(self.info.type_of(src), Ty::Prim("str")) {
                    // String iteration — byte by byte through the view.
                    let index = binds.get(1).map(|b| b.name.clone());
                    self.emit_str_for(&b0.name, index.as_ref(), src, body);
                } else if matches!(apply_subst(&self.info.type_of(src).clone(), &self.subst), Ty::Array { .. }) {
                    // Fixed-size array iteration over its inline `a[N]` field.
                    let index = binds.get(1).map(|b| b.name.clone());
                    self.emit_array_for(b0.conv, &b0.name, index.as_ref(), src, body);
                } else {
                    // Slice iteration, with an optional index binding (`for x, i in xs`).
                    let index = binds.get(1).map(|b| b.name.clone());
                    self.emit_slice_for(b0.conv, &b0.name, index.as_ref(), src, body);
                }
            }
        }
    }

    /// `for i in lo..hi { B }` → a counted C `for`, with `hi` snapshotted once.
    /// For an *exclusive* range whose index is named, the index is registered in
    /// `cur_refines` for the body so `s[i]` (where `hi == s.len`) becomes raw.
    fn emit_range_for(
        &mut self,
        binding: &Ident,
        iter: ExprId,
        lo: Option<ExprId>,
        hi: Option<ExprId>,
        inclusive: bool,
        step: Option<ExprId>,
        body: &Block,
    ) {
        let n = self.tmp;
        self.tmp += 1;
        let exposed = binding.name != "_";
        let ivar = if exposed { format!("j_{}", binding.name) } else { format!("_i{n}") };
        // A negative *literal* step descends: compare with `>`/`>=` and use a
        // signed index (so `size_t` underflow can't run the loop forever).
        let descending =
            step.is_some_and(|s| matches!(&self.ast.expr_at(s).kind, ExprKind::Unary { op: UnOp::Neg, .. }));
        let cty = if descending {
            "int64_t".to_string()
        } else {
            hi.map(|h| self.info.type_of(h).clone())
                .filter(crate::types::is_numeric)
                .map(|t| self.c_type(&t))
                .unwrap_or_else(|| "size_t".to_string())
        };
        let lo_c = lo.map(|e| self.emit_expr(e)).unwrap_or_else(|| "0".to_string());
        let hi_c = hi.map(|e| self.emit_expr(e)).unwrap_or_else(|| "0".to_string());
        let cmp = match (descending, inclusive) {
            (false, false) => "<",
            (false, true) => "<=",
            (true, false) => ">",
            (true, true) => ">=",
        };
        let incr = match step {
            Some(s) => {
                let sc = self.emit_expr(s);
                format!("{ivar} += ({sc})")
            }
            None => format!("{ivar}++"),
        };
        self.line(format!("{cty} _hi{n} = {hi_c};"));
        self.line(format!("for ({cty} {ivar} = {lo_c}; {ivar} {cmp} _hi{n}; {incr})"));

        // Bounds-check elision: an exclusive `0..s.len` index proves `i < s.len`,
        // so reuse the refinement machinery (`index_in_range`) for the body. A
        // stepped/descending index isn't a plain `0..len`, so it does not elide.
        let restore = if exposed && !inclusive && step.is_none() {
            Some(self.cur_refines.insert(binding.name.clone(), iter))
        } else {
            None
        };
        self.emit_loop_body(body);
        if let Some(prev) = restore {
            match prev {
                Some(p) => self.cur_refines.insert(binding.name.clone(), p),
                None => self.cur_refines.remove(&binding.name),
            };
        }
    }

    /// `for x in xs { B }` / `for mut x in xs { B }` / `for x, i in xs { B }` →
    /// iterate a slice. The slice is snapshotted; `read` binds the element by
    /// value, `mut` binds a pointer into the slice (registered in `ptr_params`,
    /// so uses render `(*j_x)`); an optional `index` binding gets the position.
    fn emit_slice_for(
        &mut self,
        conv: Conv,
        binding: &Ident,
        index: Option<&Ident>,
        iter: ExprId,
        body: &Block,
    ) {
        // Resolve through the active monomorphization subst so iterating a generic
        // `[]T` inside a generic function names `JestyrSlice_i32` / `int32_t`.
        let st = apply_subst(&self.info.type_of(iter).clone(), &self.subst);
        let elem = match &st {
            Ty::Slice(e) => (**e).clone(),
            _ => Ty::Unknown,
        };
        let ecty = self.c_type(&elem);
        let scty = self.c_type(&st);
        let s_iter = self.emit_expr(iter);
        let n = self.tmp;
        self.tmp += 1;
        let is_mut = matches!(conv, Conv::Mut);
        let exposed = binding.name != "_";

        self.line(format!("{scty} _s{n} = {s_iter};"));
        self.line(format!("for (size_t _k{n} = 0; _k{n} < _s{n}.len; _k{n}++)"));
        self.line("{");
        self.depth += 1;
        if let Some(name) = self.scratch_reset.take() {
            self.line(format!("j_{name}.off = 0;"));
        }
        if let Some(idx) = index {
            if idx.name != "_" {
                self.line(format!("size_t j_{} = _k{n};", idx.name));
            }
        }
        if exposed {
            if is_mut {
                self.line(format!("{ecty}* j_{} = &_s{n}.ptr[_k{n}];", binding.name));
            } else {
                self.line(format!("{ecty} j_{} = _s{n}.ptr[_k{n}];", binding.name));
            }
        }
        let added_ptr = is_mut && exposed;
        if added_ptr {
            self.ptr_params.insert(binding.name.clone());
        }
        for stmt in &body.stmts {
            self.emit_stmt(stmt);
        }
        if added_ptr {
            self.ptr_params.remove(&binding.name);
        }
        if let Some(lbl) = self.cont_label.take() {
            self.line(format!("{lbl}__continue: ;"));
        }
        self.depth -= 1;
        self.line("}");
    }

    /// `for x in arr { B }` → iterate a fixed-size array over its inline `a[N]`
    /// field. The array is iterated *in place* (by address), so there is no
    /// whole-array copy and a `mut x` binding writes back; an optional `index`
    /// binding gets the position. Mirrors [`Self::emit_slice_for`].
    fn emit_array_for(
        &mut self,
        conv: Conv,
        binding: &Ident,
        index: Option<&Ident>,
        iter: ExprId,
        body: &Block,
    ) {
        let at = apply_subst(&self.info.type_of(iter).clone(), &self.subst);
        let (elem, len) = match &at {
            Ty::Array { elem, len } => ((**elem).clone(), *len),
            _ => (Ty::Unknown, 0),
        };
        let ecty = self.c_type(&elem);
        let acty = self.c_type(&at);
        let a_iter = self.emit_expr(iter);
        let n = self.tmp;
        self.tmp += 1;
        let is_mut = matches!(conv, Conv::Mut);
        let exposed = binding.name != "_";

        self.line(format!("{acty}* _a{n} = &({a_iter});"));
        self.line(format!("for (size_t _k{n} = 0; _k{n} < {len}; _k{n}++)"));
        self.line("{");
        self.depth += 1;
        if let Some(name) = self.scratch_reset.take() {
            self.line(format!("j_{name}.off = 0;"));
        }
        if let Some(idx) = index {
            if idx.name != "_" {
                self.line(format!("size_t j_{} = _k{n};", idx.name));
            }
        }
        if exposed {
            if is_mut {
                self.line(format!("{ecty}* j_{} = &_a{n}->a[_k{n}];", binding.name));
            } else {
                self.line(format!("{ecty} j_{} = _a{n}->a[_k{n}];", binding.name));
            }
        }
        let added_ptr = is_mut && exposed;
        if added_ptr {
            self.ptr_params.insert(binding.name.clone());
        }
        for stmt in &body.stmts {
            self.emit_stmt(stmt);
        }
        if added_ptr {
            self.ptr_params.remove(&binding.name);
        }
        if let Some(lbl) = self.cont_label.take() {
            self.line(format!("{lbl}__continue: ;"));
        }
        self.depth -= 1;
        self.line("}");
    }

    /// `for c in text { B }` → iterate a string's bytes. The length is computed
    /// once with `strlen`; each `c` is the `u8` byte. (Byte iteration, not
    /// Unicode-aware — a real grapheme/codepoint iterator is future work.)
    /// Lower an f-string to a fresh owned `String`, built by appending each literal
    /// run and each interpolation (formatted per type). The result is a statement-
    /// expression so `f"…"` is an ordinary value.
    fn emit_fstring(&mut self, parts: &[String], exprs: &[ExprId]) -> String {
        let n = self.tmp;
        self.tmp += 1;
        let f = format!("_fs{n}");
        let mut s = format!("({{ JestyrString {f} = jestyr_rt_str_new(); ");
        for (i, part) in parts.iter().enumerate() {
            if !part.is_empty() {
                let _ = write!(s, "jestyr_rt_str_push(&{f}, JSTR(\"{part}\")); ");
            }
            if let Some(&e) = exprs.get(i) {
                let et = self.info.type_of(e).clone();
                let ec = self.emit_expr(e);
                match &et {
                    Ty::Prim("str") => {
                        let _ = write!(s, "jestyr_rt_str_push(&{f}, {ec}); ");
                    }
                    Ty::Prim("String") => {
                        let _ = write!(s, "jestyr_rt_str_push(&{f}, jestyr_rt_str_view(&{ec})); ");
                    }
                    Ty::Prim("bool") => {
                        let _ = write!(s, "jestyr_rt_str_push(&{f}, ({ec}) ? JSTR(\"true\") : JSTR(\"false\")); ");
                    }
                    // Integers (and, as a fallback, anything else) format as decimal.
                    _ => {
                        let _ = write!(s, "jestyr_rt_str_push_i64(&{f}, (int64_t)({ec})); ");
                    }
                }
            }
        }
        let _ = write!(s, "{f}; }})");
        s
    }

    /// If `e` is `codepoints(s)`, the string argument `s` — the marker for a
    /// codepoint-decoding `for` loop (this intrinsic is valid only in for-position).
    fn codepoints_iter_arg(&self, e: ExprId) -> Option<ExprId> {
        if let ExprKind::Call { callee, args } = &self.ast.expr_at(e).kind {
            if let ExprKind::Name(n) = &self.ast.expr_at(*callee).kind {
                if n.name == "codepoints" && args.len() == 1 {
                    return Some(args[0]);
                }
            }
        }
        None
    }

    /// `for cp in codepoints(s) { B }` — decode UTF-8 one codepoint at a time. Each
    /// `cp` is a `u32`; the loop advances by the codepoint's byte width. O(n), and
    /// the cost is right there in the name — never an implicit decode (the D lesson).
    fn emit_codepoints_for(
        &mut self,
        binding: &Ident,
        offset: Option<&Ident>,
        s_expr: ExprId,
        body: &Block,
    ) {
        let s = self.emit_expr(s_expr);
        let n = self.tmp;
        self.tmp += 1;
        self.line(format!("JestyrStr _str{n} = {s};"));
        self.line(format!("size_t _k{n} = 0;"));
        self.line(format!("while (_k{n} < _str{n}.len)"));
        self.line("{");
        self.depth += 1;
        if let Some(name) = self.scratch_reset.take() {
            self.line(format!("j_{name}.off = 0;"));
        }
        // The byte offset is `_k` *before* the decode advances it (Go's range-over-string).
        if let Some(off) = offset {
            if off.name != "_" {
                self.line(format!("size_t j_{} = _k{n};", off.name));
            }
        }
        if binding.name != "_" {
            self.line(format!(
                "uint32_t j_{} = jestyr_rt_decode_cp(_str{n}.ptr, _str{n}.len, &_k{n});",
                binding.name
            ));
        } else {
            self.line(format!("(void) jestyr_rt_decode_cp(_str{n}.ptr, _str{n}.len, &_k{n});"));
        }
        for stmt in &body.stmts {
            self.emit_stmt(stmt);
        }
        if let Some(lbl) = self.cont_label.take() {
            self.line(format!("{lbl}__continue: ;"));
        }
        self.depth -= 1;
        self.line("}");
    }

    fn graphemes_iter_arg(&self, e: ExprId) -> Option<ExprId> {
        if let ExprKind::Call { callee, args } = &self.ast.expr_at(e).kind {
            if let ExprKind::Name(nm) = &self.ast.expr_at(*callee).kind {
                if nm.name == "graphemes" && args.len() == 1 {
                    return Some(args[0]);
                }
            }
        }
        None
    }

    /// `for g in graphemes(s) { B }` — each `g` is a `str` view over one grapheme
    /// cluster (a base codepoint plus its following combining marks). Zero-copy.
    fn emit_graphemes_for(&mut self, binding: &Ident, s_expr: ExprId, body: &Block) {
        let s = self.emit_expr(s_expr);
        let n = self.tmp;
        self.tmp += 1;
        self.line(format!("JestyrStr _gs{n} = {s};"));
        self.line(format!("size_t _gk{n} = 0;"));
        self.line(format!("while (_gk{n} < _gs{n}.len)"));
        self.line("{");
        self.depth += 1;
        if let Some(name) = self.scratch_reset.take() {
            self.line(format!("j_{name}.off = 0;"));
        }
        self.line(format!("size_t _gstart{n} = _gk{n};"));
        self.line(format!("(void) jestyr_rt_decode_cp(_gs{n}.ptr, _gs{n}.len, &_gk{n});"));
        // Absorb following combining marks into the same cluster.
        self.line(format!("while (_gk{n} < _gs{n}.len) {{ size_t _gsave{n} = _gk{n}; uint32_t _gc{n} = jestyr_rt_decode_cp(_gs{n}.ptr, _gs{n}.len, &_gk{n}); if (!jestyr_rt_is_combining(_gc{n})) {{ _gk{n} = _gsave{n}; break; }} }}"));
        if binding.name != "_" {
            self.line(format!(
                "JestyrStr j_{} = (JestyrStr){{ _gs{n}.ptr + _gstart{n}, _gk{n} - _gstart{n} }};",
                binding.name
            ));
        }
        for stmt in &body.stmts {
            self.emit_stmt(stmt);
        }
        if let Some(lbl) = self.cont_label.take() {
            self.line(format!("{lbl}__continue: ;"));
        }
        self.depth -= 1;
        self.line("}");
    }

    fn split_iter_arg(&self, e: ExprId) -> Option<(ExprId, ExprId)> {
        if let ExprKind::Call { callee, args } = &self.ast.expr_at(e).kind {
            if let ExprKind::Name(nm) = &self.ast.expr_at(*callee).kind {
                if nm.name == "split" && args.len() == 2 {
                    return Some((args[0], args[1]));
                }
            }
        }
        None
    }

    /// `for part in split(s, sep) { B }` — yield each `str` view between separators
    /// (zero-copy; the last part is the remainder). An empty `sep` yields the whole
    /// string once.
    fn emit_split_for(&mut self, binding: &Ident, s_expr: ExprId, sep_expr: ExprId, body: &Block) {
        let s = self.emit_expr(s_expr);
        let sep = self.emit_expr(sep_expr);
        let n = self.tmp;
        self.tmp += 1;
        self.line(format!("JestyrStr _ss{n} = {s};"));
        self.line(format!("JestyrStr _sep{n} = {sep};"));
        self.line(format!("size_t _start{n} = 0;"));
        self.line(format!("int _go{n} = 1;"));
        self.line(format!("while (_go{n})"));
        self.line("{");
        self.depth += 1;
        if let Some(name) = self.scratch_reset.take() {
            self.line(format!("j_{name}.off = 0;"));
        }
        self.line(format!(
            "JestyrStr _rest{n} = (JestyrStr){{ _ss{n}.ptr + _start{n}, _ss{n}.len - _start{n} }};"
        ));
        self.line(format!("int64_t _hit{n} = _sep{n}.len ? jestyr_rt_find(_rest{n}, _sep{n}) : -1;"));
        if binding.name != "_" {
            self.line(format!(
                "JestyrStr j_{} = (_hit{n} < 0) ? _rest{n} : (JestyrStr){{ _rest{n}.ptr, (size_t)_hit{n} }};",
                binding.name
            ));
        }
        self.line(format!(
            "if (_hit{n} < 0) _go{n} = 0; else _start{n} += (size_t)_hit{n} + _sep{n}.len;"
        ));
        for stmt in &body.stmts {
            self.emit_stmt(stmt);
        }
        if let Some(lbl) = self.cont_label.take() {
            self.line(format!("{lbl}__continue: ;"));
        }
        self.depth -= 1;
        self.line("}");
    }

    fn emit_str_for(&mut self, binding: &Ident, index: Option<&Ident>, iter: ExprId, body: &Block) {
        let s = self.emit_expr(iter);
        let n = self.tmp;
        self.tmp += 1;
        self.line(format!("JestyrStr _str{n} = {s};"));
        self.line(format!("for (size_t _k{n} = 0; _k{n} < _str{n}.len; _k{n}++)"));
        self.line("{");
        self.depth += 1;
        if let Some(name) = self.scratch_reset.take() {
            self.line(format!("j_{name}.off = 0;"));
        }
        if let Some(idx) = index {
            if idx.name != "_" {
                self.line(format!("size_t j_{} = _k{n};", idx.name));
            }
        }
        if binding.name != "_" {
            self.line(format!("uint8_t j_{} = (uint8_t)_str{n}.ptr[_k{n}];", binding.name));
        }
        for stmt in &body.stmts {
            self.emit_stmt(stmt);
        }
        if let Some(lbl) = self.cont_label.take() {
            self.line(format!("{lbl}__continue: ;"));
        }
        self.depth -= 1;
        self.line("}");
    }

    /// `for x, y in xs, ys { B }` → lockstep iteration over several slices. Each
    /// slice is snapshotted; their lengths must be equal (a runtime `assert` now,
    /// a static `requires` once `@verified` lands); a single index drives them all.
    fn emit_zip_for(&mut self, binds: &[LoopBind], sources: &[ExprId], body: &Block) {
        let n = self.tmp;
        self.tmp += 1;
        let k = sources.len().min(binds.len());
        // Snapshot each source slice.
        for (i, s) in sources.iter().enumerate().take(k) {
            let st = self.info.type_of(*s).clone();
            let scty = self.c_type(&st);
            let sc = self.emit_expr(*s);
            self.line(format!("{scty} _z{n}_{i} = {sc};"));
        }
        // All lengths must match.
        let conds: Vec<String> = (1..k).map(|i| format!("_z{n}_0.len == _z{n}_{i}.len")).collect();
        if !conds.is_empty() {
            self.line(format!("assert({});", conds.join(" && ")));
        }
        self.line(format!("for (size_t _k{n} = 0; _k{n} < _z{n}_0.len; _k{n}++)"));
        self.line("{");
        self.depth += 1;
        if let Some(name) = self.scratch_reset.take() {
            self.line(format!("j_{name}.off = 0;"));
        }
        let mut added_ptrs = Vec::new();
        for (i, b) in binds.iter().enumerate().take(k) {
            let st = self.info.type_of(sources[i]).clone();
            let elem = match &st {
                Ty::Slice(e) => (**e).clone(),
                _ => Ty::Unknown,
            };
            let ecty = self.c_type(&elem);
            if b.name.name == "_" {
                continue;
            }
            if matches!(b.conv, Conv::Mut) {
                self.line(format!("{ecty}* j_{} = &_z{n}_{i}.ptr[_k{n}];", b.name.name));
                self.ptr_params.insert(b.name.name.clone());
                added_ptrs.push(b.name.name.clone());
            } else {
                self.line(format!("{ecty} j_{} = _z{n}_{i}.ptr[_k{n}];", b.name.name));
            }
        }
        for stmt in &body.stmts {
            self.emit_stmt(stmt);
        }
        for p in added_ptrs {
            self.ptr_params.remove(&p);
        }
        if let Some(lbl) = self.cont_label.take() {
            self.line(format!("{lbl}__continue: ;"));
        }
        self.depth -= 1;
        self.line("}");
    }

    /// Lower a `region r { … }` block: open a bump arena, run the body (whose
    /// `region_alloc(r, …)` calls allocate into it and hand out zero-cost
    /// `&[r]T` pointers), then free the whole arena in O(1) at the block's end.
    fn emit_region(&mut self, name: &str, body: &Block) {
        self.line("{");
        self.depth += 1;
        self.drop_scope_enter();
        self.line(format!("JestyrArena j_{name} = jestyr_arena_new(1u << 20);"));
        for stmt in &body.stmts {
            self.emit_stmt(stmt);
        }
        // Region-integrated bulk drop (design Phase 3): a value owned by the region
        // emits **no** per-value drop glue — the arena reclaims everything at once
        // when it is freed. The allocator/region *determines* the drop strategy, so
        // we discard the region's drop scope rather than emitting individual calls.
        self.drop_scope_exit_discard();
        self.line(format!("jestyr_arena_free(&j_{name});"));
        self.depth -= 1;
        self.line("}");
    }

    // --- type lowering ---

    /// Lower an AST type to its C spelling.
    fn c_ty_ast(&mut self, id: TypeId) -> String {
        let span = self.ast.type_at(id).span;
        match &self.ast.type_at(id).kind {
            TypeKind::Name(n) => {
                // a type parameter under the active monomorphization substitution
                if let Some(t) = self.subst.get(&n.name).cloned() {
                    return self.c_type(&t);
                }
                if let Some(p) = prim_c(&n.name) {
                    return p.to_string();
                }
                match self.info.table.type_index.get(&self.canon_type(&n.name)).copied() {
                    // A niche-optimized enum lowers to its bare pointer payload.
                    Some(i) if self.niche_enum_at(i).is_some() => {
                        let payload = self.niche_enum_at(i).unwrap().payload;
                        self.c_type(&payload)
                    }
                    // structs and enums both lower to a `Jestyr_<Name>` typedef
                    // (the decl's name is the canonical form).
                    Some(i) => format!("Jestyr_{}", self.info.table.types[i].name),
                    None => {
                        self.diag(span, format!("the C backend cannot lower the external type `{}` yet", n.name));
                        "int".to_string()
                    }
                }
            }
            TypeKind::TypeKw => {
                self.diag(span, "the C backend does not support `type` values yet");
                "int".to_string()
            }
            TypeKind::Ptr { mutbl, inner } => {
                let inner = self.c_ty_ast(*inner);
                match mutbl {
                    PtrMut::Const => format!("const {inner}*"),
                    _ => format!("{inner}*"),
                }
            }
            TypeKind::App { ctor, args } => {
                let subst = self.subst.clone();
                let aty: Vec<Ty> = args.iter().map(|a| self.ast_type_to_ty(*a, &subst)).collect();
                // Canon the ctor so a collided generic enum gets its own instance
                // symbol (bare for a generic struct / non-colliding name).
                let key = self.canon_type(&ctor.name);
                // A generic-enum instance may be niche-optimized to a bare pointer.
                if self.enum_is_generic(&key) {
                    if let Some(n) = self.niche_enum_instance(&key, &aty) {
                        return self.c_type(&n.payload);
                    }
                }
                self.gen_struct_c_name(&key, &aty)
            }
            TypeKind::Slice(inner) => {
                let subst = self.subst.clone();
                let elem = self.ast_type_to_ty(*inner, &subst);
                self.slice_c_name(&elem)
            }
            TypeKind::Array { len, elem } => {
                let subst = self.subst.clone();
                let et = self.ast_type_to_ty(*elem, &subst);
                self.array_c_name(&et, self.array_len(*len))
            }
            TypeKind::GenRef(inner) => {
                let subst = self.subst.clone();
                let elem = self.ast_type_to_ty(*inner, &subst);
                self.genref_c_name(&elem)
            }
            // a region reference is zero-cost: a plain pointer
            TypeKind::RegionRef { inner, .. } => {
                let i = self.c_ty_ast(*inner);
                format!("{i}*")
            }
            // a thin function pointer lowers to its `JestyrFn_<sig>` typedef
            // (emitted by `fn_type_typedefs`), so the *name* sits on the outside
            // of any declaration — sidestepping C's inside-out declarator syntax.
            TypeKind::Fn { .. } => {
                let subst = self.subst.clone();
                let ty = self.ast_type_to_ty(id, &subst);
                self.c_type(&ty)
            }
            // `dyn Trait` is the `{ data, vtable }` fat pointer typedef (Stage F).
            TypeKind::Dyn(n) => format!("JestyrDyn_{}", n.name),
            // A module-qualified type lowers through its resolved `Ty` (handles the
            // plain and generic cases uniformly; types are globally unique today).
            TypeKind::Path { .. } => {
                let subst = self.subst.clone();
                let ty = self.ast_type_to_ty(id, &subst);
                self.c_type(&ty)
            }
            TypeKind::Error => "int".to_string(),
        }
    }

    /// Lower an inferred `Ty` to its C spelling (used for `let` without an
    /// annotation).
    fn c_type(&mut self, t: &Ty) -> String {
        match t {
            Ty::Unit => "void".to_string(),
            Ty::Prim(n) => prim_c(n).unwrap_or("int").to_string(),
            Ty::Ptr { mutbl, inner } => {
                let i = self.c_type(inner);
                match mutbl {
                    PtrMut::Const => format!("const {i}*"),
                    _ => format!("{i}*"),
                }
            }
            Ty::Named(i) => {
                // A niche-optimized enum *is* its pointer payload.
                if let Some(n) = self.niche_enum_at(*i) {
                    return self.c_type(&n.payload);
                }
                format!("Jestyr_{}", self.info.table.types[*i].name)
            }
            // an inferred type parameter (e.g. `T`) under the active substitution
            Ty::Opaque(n) => {
                // `dyn Trait` (lowered to `Opaque("dyn <Trait>")`) is its fat-pointer
                // typedef; an ordinary opaque resolves through the active subst.
                if let Some(tr) = n.strip_prefix("dyn ") {
                    return format!("JestyrDyn_{tr}");
                }
                match self.subst.get(n).cloned() {
                    Some(t) => self.c_type(&t),
                    None => "int".to_string(),
                }
            }
            Ty::Result(ok) => self.result_c_name(ok),
            Ty::GenStruct { ctor, args } => self.gen_struct_c_name(ctor, args),
            Ty::GenEnum { ctor, args } => {
                // A niche-able instance is its bare pointer; else the tagged union.
                if let Some(n) = self.niche_enum_instance(ctor, args) {
                    return self.c_type(&n.payload);
                }
                self.gen_struct_c_name(ctor, args)
            }
            Ty::Slice(elem) => self.slice_c_name(elem),
            Ty::Array { elem, len } => self.array_c_name(elem, *len),
            Ty::GenRef(elem) => self.genref_c_name(elem),
            Ty::RegionRef(elem) => {
                let i = self.c_type(elem);
                format!("{i}*")
            }
            Ty::Fn { .. } => self.fn_type_c_name(t),
            _ => "int".to_string(),
        }
    }

    /// The C typedef name for a function-pointer type — a pure function of its
    /// signature mangle, so equal signatures share one `typedef` (see
    /// [`Cgen::fn_type_typedefs`]).
    fn fn_type_c_name(&self, t: &Ty) -> String {
        format!("JestyrFn_{}", self.ty_mangle(t))
    }

    /// The C struct name for a slice with the given element type.
    fn slice_c_name(&self, elem: &Ty) -> String {
        format!("JestyrSlice_{}", self.ty_mangle(elem))
    }

    /// The C struct name for a generational reference to the given element type.
    fn genref_c_name(&self, elem: &Ty) -> String {
        format!("JestyrRef_{}", self.ty_mangle(elem))
    }

    /// Every distinct slice element type used in the program — found by scanning
    /// the flat arenas for `[]T` annotations and `slice(T, …)` constructions.
    fn collect_slices(&self) -> Vec<Ty> {
        let mut seen: HashSet<String> = HashSet::new();
        let mut out = Vec::new();
        let empty = HashMap::new();
        for td in &self.ast.types {
            if let TypeKind::Slice(inner) = &td.kind {
                let elem = self.ast_type_to_ty(*inner, &empty);
                // A generic `[]T` *annotation* is a template, not an instance — its
                // concrete `JestyrSlice_<T>` comes from the monomorphized-instance
                // walk below (or a `slice(T, …)` construction), so skip the opaque
                // form rather than emitting a bogus `JestyrSlice_T`.
                if Self::is_concrete(&elem) && seen.insert(self.ty_mangle(&elem)) {
                    out.push(elem);
                }
            }
        }
        for ed in &self.ast.exprs {
            if let ExprKind::Call { callee, args } = &ed.kind {
                if let ExprKind::Name(n) = &self.ast.expr_at(*callee).kind {
                    if n.name == "slice" {
                        if let Some(&a0) = args.first() {
                            let elem = self.eval_type_arg(a0, &empty);
                            if seen.insert(self.ty_mangle(&elem)) {
                                out.push(elem);
                            }
                        }
                    }
                }
            }
        }
        // Each monomorphized generic function contributes the concrete element type
        // of any `[]T` parameter/return under its substitution — so a generic slice
        // algorithm's `JestyrSlice_<T>` is emitted even with no local `slice(T, …)`.
        for (name, args) in &self.instances {
            let Some(f) = self.find_fn(name) else { continue };
            let subst = self.make_subst(f, args);
            let sig_tys = f.params.iter().filter_map(|p| p.ty).chain(f.ret_ty);
            for ty in sig_tys {
                if let TypeKind::Slice(inner) = self.ast.type_at(ty).kind {
                    let elem = self.ast_type_to_ty(inner, &subst);
                    if Self::is_concrete(&elem) && seen.insert(self.ty_mangle(&elem)) {
                        out.push(elem);
                    }
                }
            }
        }
        out
    }



    /// The single tail expression of a block used as a value, or `None` if it carries
    /// statements. The same rule the value-position `Block` arm already enforces — kept in
    /// one place so the `if`-as-a-value lowering cannot drift from it.
    fn value_tail_of_block(b: &Block) -> Option<ExprId> {
        match b.stmts.as_slice() {
            [Stmt::Expr(e)] => Some(*e),
            _ => None,
        }
    }

    /// The same, for an `else` operand — which the parser stores as an expression. A
    /// plain `else { … }` is a block; `else if …` is another `If`, and recursing makes a
    /// whole else-if chain lower as nested conditionals for free.
    fn value_tail_of_expr(&self, e: ExprId) -> Option<ExprId> {
        match &self.ast.expr_at(e).kind {
            ExprKind::Block(b) => Self::value_tail_of_block(b),
            ExprKind::If { then, els, .. } => {
                // Only if the whole chain is expression-shaped; otherwise the caller
                // falls back to the diagnostic rather than emitting half a lowering.
                Self::value_tail_of_block(then)?;
                els.and_then(|x| self.value_tail_of_expr(x))?;
                Some(e)
            }
            _ => Some(e),
        }
    }

    /// The `par for` sites this run lowers to vector code: those inside a function that
    /// **declares** `@simd` and whose body `simd::classify` **certifies**.
    ///
    /// Opt-in per function, so a program that writes no `@simd` emits byte-identical C —
    /// the `@layout(auto)` discipline, and what keeps this increment off the corpus, the
    /// concatenated build, the seed and every attested hash.
    ///
    /// The verdict comes from the same `simd::classify` the attribute check uses, so a
    /// loop cgen vectorizes can never be one the compiler refused to certify.
    fn collect_simd_sites(&self) -> std::collections::HashMap<ExprId, Ty> {
        let mut out = std::collections::HashMap::new();
        for item in &self.ast.items {
            let Item::Fn(f) = item else { continue };
            if f.attr("simd").is_none() {
                continue;
            }
            for site in crate::simd::sites_in_span(self.ast, f.body.span) {
                if !site.verdict.is_legal() {
                    continue;
                }
                let ExprKind::ParFor { iter, .. } = &self.ast.expr_at(site.id).kind else { continue };
                let elem = match self.info.type_of(*iter).clone() {
                    Ty::Slice(e) => (*e).clone(),
                    Ty::Array { elem, .. } => (*elem).clone(),
                    _ => continue,
                };
                // Only an integer element has a vector form here; anything else was
                // already refused by the legality pass or by typeck.
                if !matches!(&elem, Ty::Prim(p) if is_integer_c_prim(p)) {
                    continue;
                }
                out.insert(site.id, elem);
            }
        }
        out
    }

    /// Emit one `typedef E JestyrVec_E __attribute__((vector_size(N)));` per element type
    /// a vectorized `par for` iterates.
    ///
    /// GCC vector extensions rather than an OpenMP pragma or an `-march` bump: the
    /// lowering has to be *chosen*, not begged for, or determinism sits at the
    /// optimizer's discretion. `CC_FLAGS` is untouched.
    fn simd_vector_defs(&mut self) {
        // Keyed on the COMPUTE element, so a `[]i8` site and a `[]i32` site ask for the
        // same `JestyrVec_i32` and the dedup below collapses them into one typedef.
        let mut elems: Vec<Ty> = self.simd_sites.values().map(simd_compute_elem).collect();
        elems.sort_by_key(|e| self.ty_mangle(e));
        elems.dedup_by_key(|e| self.ty_mangle(e));
        let any = !elems.is_empty();
        for elem in elems {
            let name = self.simd_vec_name(&elem);
            let ecty = self.c_type(&elem);
            let bytes = SIMD_VECTOR_BYTES;
            self.def_begin(name.clone(), Vec::new());
            self.raw(format!("typedef {ecty} {name} __attribute__((vector_size({bytes})));\n"));
            self.def_end();
        }
        if any {
            self.raw("\n");
        }
    }

    fn simd_vec_name(&self, elem: &Ty) -> String {
        format!("JestyrVec_{}", self.ty_mangle(elem))
    }

    /// Emit the certified body with the loop variable bound to a **vector**.
    ///
    /// Only the forms `simd::classify` certifies reach here, which is what lets this be a
    /// small, total function. Two lowerings differ from the scalar one, and both are
    /// forced by GNU vector semantics rather than chosen:
    ///
    /// * a comparison yields an all-ones/all-zeros **mask** per lane, not `0`/`1`, so
    ///   `and`/`or` become bitwise `and`/`or`;
    /// * the conditional operator is not defined on vectors at all, so `if c { a } else
    ///   { b }` becomes a mask blend.
    ///
    /// The scalar remainder loop must therefore keep the **scalar** forms — the bug
    /// Q-S1's oracle caught in its own harness, which is why an element count that is not
    /// a multiple of the lane width is this increment's first test.
    fn emit_expr_simd(&mut self, id: ExprId, vt: &str, loopvar: &str, vecvar: &str) -> String {
        let e = self.ast.expr_at(id);
        match &e.kind {
            ExprKind::Int(t) => t.replace('_', ""),
            // A bool literal has no scalar spelling that is a lane mask, so it is built
            // as one rather than written.
            ExprKind::Bool(b) => {
                if *b {
                    format!("(({vt}){{0}} == ({vt}){{0}})")
                } else {
                    format!("(({vt}){{0}} != ({vt}){{0}})")
                }
            }
            ExprKind::Name(n) => {
                if n.name == loopvar {
                    vecvar.to_string()
                } else {
                    // Loop-invariant: GCC broadcasts a scalar against a vector operand.
                    format!("j_{}", n.name)
                }
            }
            ExprKind::Unary { op, rhs } => {
                let r = self.emit_expr_simd(*rhs, vt, loopvar, vecvar);
                match op {
                    UnOp::Neg => format!("(-{r})"),
                    // `not` on a lane mask is a bitwise complement; scalar `!` is not
                    // defined on a vector.
                    UnOp::BitNot | UnOp::Not => format!("(~{r})"),
                    UnOp::Ref => "0".to_string(),
                }
            }
            ExprKind::Binary { op, lhs, rhs } => {
                let l = self.emit_expr_simd(*lhs, vt, loopvar, vecvar);
                let r = self.emit_expr_simd(*rhs, vt, loopvar, vecvar);
                let o = match op {
                    BinOp::Add => "+",
                    BinOp::Sub => "-",
                    BinOp::Mul => "*",
                    BinOp::Eq => "==",
                    BinOp::Ne => "!=",
                    BinOp::Lt => "<",
                    BinOp::Le => "<=",
                    BinOp::Gt => ">",
                    BinOp::Ge => ">=",
                    // Short-circuit becomes a blend: both sides are evaluated, which is
                    // value-preserving only because the certified subset is total.
                    BinOp::And | BinOp::BitAnd => "&",
                    BinOp::Or | BinOp::BitOr => "|",
                    BinOp::BitXor => "^",
                    BinOp::Shl => "<<",
                    BinOp::Shr => ">>",
                    // Never certified (`Reason::Trapping`), so unreachable in a real
                    // program; cgen stays total rather than panicking on one already
                    // rejected.
                    BinOp::Div | BinOp::Rem => "+",
                };
                format!("({l} {o} {r})")
            }
            ExprKind::If { cond, then, els } => {
                let c = self.emit_expr_simd(*cond, vt, loopvar, vecvar);
                let t = self.emit_block_simd(then, vt, loopvar, vecvar);
                let f = match els {
                    Some(e) => self.emit_expr_simd(*e, vt, loopvar, vecvar),
                    None => "0".to_string(),
                };
                format!("((({t}) & ({c})) | (({f}) & ~({c})))")
            }
            ExprKind::Block(b) => self.emit_block_simd(b, vt, loopvar, vecvar),
            _ => "0".to_string(),
        }
    }

    /// A certified block in vector position — its single tail expression.
    ///
    /// `simd::classify` certifies only that shape, precisely because the **scalar
    /// remainder** cannot lower a multi-statement block in value position (that needs
    /// drop-safe spilling). So the `_ => "0"` arm is unreachable in a certified program
    /// and exists only to keep cgen total, like every other path here.
    fn emit_block_simd(&mut self, b: &Block, vt: &str, loopvar: &str, vecvar: &str) -> String {
        match b.stmts.as_slice() {
            [Stmt::Expr(e)] => self.emit_expr_simd(*e, vt, loopvar, vecvar),
            _ => "0".to_string(),
        }
    }

    /// Emit `typedef struct { T* ptr; size_t len; } JestyrSlice_<T>;` per element.
    fn slice_struct_defs(&mut self) {
        for elem in self.slice_instances.clone() {
            let name = self.slice_c_name(&elem);
            let ecty = self.c_type(&elem);
            // A slice is `{ E* ptr; len }` — it embeds `E` only through a pointer, so
            // it has no by-value dependency (a forward declaration of `E` suffices).
            self.def_begin(name.clone(), Vec::new());
            self.raw(format!("typedef struct {{ {ecty}* ptr; size_t len; }} {name};\n"));
            self.def_end();
        }
        if !self.slice_instances.is_empty() {
            self.raw("\n");
        }
    }

    /// Every distinct fixed-size array `[N]T` the program uses — from inferred expr
    /// types, function signatures, and monomorphized generic-function signatures.
    fn collect_arrays(&self) -> Vec<Ty> {
        let mut seen: HashSet<String> = HashSet::new();
        let mut out: Vec<Ty> = Vec::new();
        let add = |t: Ty, seen: &mut HashSet<String>, out: &mut Vec<Ty>| {
            if matches!(&t, Ty::Array { .. }) && Self::is_concrete(&t) && seen.insert(self.ty_mangle(&t)) {
                out.push(t);
            }
        };
        for t in &self.info.expr_types {
            add(t.clone(), &mut seen, &mut out);
        }
        for sig in self.info.table.fns.values() {
            for p in &sig.params {
                add(p.ty.clone(), &mut seen, &mut out);
            }
            add(sig.ret.clone(), &mut seen, &mut out);
        }
        for (name, args) in &self.instances {
            let Some(f) = self.find_fn(name) else { continue };
            let subst = self.make_subst(f, args);
            let sig_tys = f.params.iter().filter_map(|p| p.ty).chain(f.ret_ty);
            for ty in sig_tys {
                if matches!(self.ast.type_at(ty).kind, TypeKind::Array { .. }) {
                    let t = self.ast_type_to_ty(ty, &subst);
                    add(t, &mut seen, &mut out);
                }
            }
        }
        out
    }

    /// Emit `typedef struct { T a[N]; } JestyrArr_<T>_<N>;` per array instance — a
    /// value type (the inline C array copies/returns by value via the struct).
    fn array_struct_defs(&mut self) {
        for t in self.array_instances.clone() {
            if let Ty::Array { elem, len } = &t {
                let name = self.array_c_name(elem, *len);
                let ecty = self.c_type(elem);
                // `{ E a[N]; }` embeds `E` *by value*, so it depends on `E`'s definition.
                let deps = Self::dep_of_cty(ecty.clone()).into_iter().collect();
                self.def_begin(name.clone(), deps);
                self.raw(format!("typedef struct {{ {ecty} a[{len}]; }} {name};\n"));
                self.def_end();
            }
        }
        if !self.array_instances.is_empty() {
            self.raw("\n");
        }
    }

    /// Every distinct generational-reference element type — `&T` annotations and
    /// `gen_new(T, …)` constructions across the flat arenas.
    fn collect_genrefs(&self) -> Vec<Ty> {
        let mut seen: HashSet<String> = HashSet::new();
        let mut out = Vec::new();
        let empty = HashMap::new();
        for td in &self.ast.types {
            if let TypeKind::GenRef(inner) = &td.kind {
                let elem = self.ast_type_to_ty(*inner, &empty);
                if seen.insert(self.ty_mangle(&elem)) {
                    out.push(elem);
                }
            }
        }
        for ed in &self.ast.exprs {
            if let ExprKind::Call { callee, args } = &ed.kind {
                if let ExprKind::Name(n) = &self.ast.expr_at(*callee).kind {
                    if n.name == "gen_new" {
                        if let Some(&a0) = args.first() {
                            let elem = self.eval_type_arg(a0, &empty);
                            if seen.insert(self.ty_mangle(&elem)) {
                                out.push(elem);
                            }
                        }
                    }
                }
            }
        }
        out
    }

    /// Every distinct function-pointer signature the program names — found by
    /// scanning the type arena for `TypeKind::Fn`. Nested fn-types occupy their
    /// own arena entries with *smaller* ids (a callee is parsed before the type
    /// that contains it), so arena order already emits inner typedefs first.
    fn collect_fn_types(&self) -> Vec<Ty> {
        let mut seen: HashSet<String> = HashSet::new();
        let mut out = Vec::new();
        let empty = HashMap::new();
        // Every fn-type written textually (struct field, signature, `let`, …).
        // Generic placeholders (`fn(T) -> T`, where `T` stays opaque) are skipped —
        // their *concrete* instances come from the generic-struct walk below.
        for (i, td) in self.ast.types.iter().enumerate() {
            if let TypeKind::Fn { .. } = &td.kind {
                let ty = self.ast_type_to_ty(TypeId(i as u32), &empty);
                if Self::is_concrete(&ty) && seen.insert(self.ty_mangle(&ty)) {
                    out.push(ty);
                }
            }
        }
        // A monomorphized generic struct contributes the *concrete* fn-pointer type
        // of each fn-pointer field under its substitution — e.g. `Container(i32)`'s
        // `op: fn(T) -> T` yields `fn(i32) -> i32`. Without this a generic vtable's
        // field would reference an un-emitted typedef.
        for (ctor, args) in &self.struct_instances {
            let Some(f) = self.find_fn(ctor) else { continue };
            let Some(body) = self.ctor_struct_body(f) else { continue };
            let names = self.type_param_names(f);
            let subst: HashMap<String, Ty> = names.into_iter().zip(args.iter().cloned()).collect();
            for m in &body.members {
                if let StructMember::Field { ty, .. } = m {
                    if matches!(self.ast.type_at(*ty).kind, TypeKind::Fn { .. }) {
                        let fty = self.ast_type_to_ty(*ty, &subst);
                        if Self::is_concrete(&fty) && seen.insert(self.ty_mangle(&fty)) {
                            out.push(fty);
                        }
                    }
                }
            }
        }
        // A monomorphized generic *function* contributes the concrete fn-pointer type
        // of each fn-pointer parameter (and return) under its substitution — e.g.
        // `opt_map(i32, i32)`'s `f: fn(T) -> U` yields `fn(i32) -> i32`. Without this a
        // higher-order generic combinator's signature references an un-emitted typedef.
        for (name, args) in &self.instances {
            let Some(f) = self.find_fn(name) else { continue };
            let subst = self.make_subst(f, args);
            let sig_tys = f.params.iter().filter_map(|p| p.ty).chain(f.ret_ty);
            for ty in sig_tys {
                if matches!(self.ast.type_at(ty).kind, TypeKind::Fn { .. }) {
                    let fty = self.ast_type_to_ty(ty, &subst);
                    if Self::is_concrete(&fty) && seen.insert(self.ty_mangle(&fty)) {
                        out.push(fty);
                    }
                }
            }
        }
        out
    }

    /// Emit `typedef R (*JestyrFn_<sig>)(T1, T2);` per distinct signature. The
    /// typedef puts the name on the *outside*, so every later use is a plain
    /// `JestyrFn_<sig> j_x` — no inside-out C declarator anywhere. A `mut`/`out`
    /// parameter lowers to `T*`, matching the ABI of a real Jestyr function
    /// (see `fn_signature`), so a fn-pointer can hold `&some_fn`.
    fn fn_type_typedefs(&mut self) {
        for t in self.fn_type_instances.clone() {
            let Ty::Fn { params, ret, .. } = &t else { continue };
            let name = self.fn_type_c_name(&t);
            let rc = self.c_type(ret);
            let ps: Vec<String> = params
                .iter()
                .map(|(c, pt)| {
                    let base = self.c_type(pt);
                    if matches!(c, Conv::Mut | Conv::Out) {
                        format!("{base}*")
                    } else {
                        base
                    }
                })
                .collect();
            let plist = if ps.is_empty() { "void".to_string() } else { ps.join(", ") };
            self.raw(format!("typedef {rc} (*{name})({plist});\n"));
        }
        if !self.fn_type_instances.is_empty() {
            self.raw("\n");
        }
    }

    /// Emit `typedef struct { T* ptr; uint64_t gen; } JestyrRef_<T>;` per element.
    /// A generational reference (§4.4) carries a snapshot of the allocation's
    /// generation; a stale deref (after `gen_free`) faults at runtime.
    fn genref_struct_defs(&mut self) {
        for elem in self.genref_instances.clone() {
            let name = self.genref_c_name(&elem);
            let ecty = self.c_type(&elem);
            // `{ E* ptr; gen }` embeds `E` only through a pointer — no by-value dep.
            self.def_begin(name.clone(), Vec::new());
            self.raw(format!("typedef struct {{ {ecty}* ptr; uint64_t gen; }} {name};\n"));
            self.def_end();
        }
        if !self.genref_instances.is_empty() {
            self.raw("\n");
        }
    }

    /// Does the index's refinement prove `index < base.len`, letting us elide the
    /// slice bounds check (design §7.2)? True when `base` is a slice named `s` and
    /// `index` is a parameter refined `in <lo>..s.len` (exclusive upper bound).
    fn index_in_range(&self, base: ExprId, index: ExprId) -> bool {
        let ast = self.ast;
        let ExprKind::Name(sname) = &ast.expr_at(base).kind else { return false };
        let ExprKind::Name(iname) = &ast.expr_at(index).kind else { return false };
        let Some(&refine) = self.cur_refines.get(&iname.name) else { return false };
        let ExprKind::Range { hi: Some(h), inclusive: false, .. } = &ast.expr_at(refine).kind else {
            return false;
        };
        let ExprKind::Field { base: fb, name: fname } = &ast.expr_at(*h).kind else { return false };
        fname.name == "len"
            && matches!(&ast.expr_at(*fb).kind, ExprKind::Name(n) if n.name == sname.name)
    }

    /// Does `emit_expr` lower `id` to a bounds-checked *statement expression* —
    /// a GNU `({ …; elem; })` that yields a **value**? Such a form is legal
    /// wherever a value is wanted and illegal in all three place positions: the
    /// left of `=`, the operand of `&`, and the base of another index.
    fn is_checked_index(&self, id: ExprId) -> bool {
        let ExprKind::Index { base, index } = &self.ast.expr_at(id).kind else { return false };
        let bt = apply_subst(&self.info.type_of(*base).clone(), &self.subst);
        match bt {
            // A fixed-size array always spills through `&base` and yields `_a->a[_ix]`.
            Ty::Array { .. } => true,
            // A slice spills only when the bounds check survives; a refinement-proved
            // index emits `(b).ptr[(i)]`, which is already an lvalue.
            Ty::Slice(_) => !self.index_in_range(*base, *index),
            _ => false,
        }
    }

    /// Is `id` a place expression that reaches *through* a checked index — e.g.
    /// `xs[i].f`, `m[i][j]`, `xs[i].f.g`? Those are exactly the places `emit_expr`
    /// cannot produce, so they must go through [`Self::emit_place`].
    ///
    /// A *directly* indexed target (`xs[i] = v`, `s[i] = v`) is **not** included:
    /// the `Assign` arm has always had its own lvalue lowering for that, and
    /// keeping it means the emitted C of every existing program is unchanged.
    fn place_through_checked_index(&self, id: ExprId) -> bool {
        match &self.ast.expr_at(id).kind {
            ExprKind::Field { base, .. } | ExprKind::Index { base, .. } => {
                self.is_checked_index(*base) || self.place_through_checked_index(*base)
            }
            _ => false,
        }
    }

    /// Emit `id` as a C **lvalue** — a place, not a value.
    ///
    /// Identical to `emit_expr` for every form except a checked index, which
    /// becomes the address-yielding `(*({ …; &elem; }))`: the statement
    /// expression produces a *pointer*, and dereferencing it gives back a place.
    /// That is what lets a projection chain continue through it, so `xs[i].f = v`
    /// assigns into the array and `m[i][j]` can take `&m[i]` for its own index.
    ///
    /// `write` selects the qualifier on the spilled base pointer. A read must keep
    /// `const` (as `emit_expr` does) so indexing a `static const` table does not
    /// discard the qualifier; an assignment target must not, or the write is
    /// through a pointer-to-const.
    fn emit_place(&mut self, id: ExprId, write: bool) -> String {
        match &self.ast.expr_at(id).kind {
            ExprKind::Field { base, name } => {
                let base = *base;
                let fname = name.name.clone();
                let bt = apply_subst(&self.info.type_of(base).clone(), &self.subst);
                // Only a struct-shaped base projects to an assignable field. A
                // slice's `.ptr`/`.len`, a `str`'s views and an array's constant
                // `.len` are computed, never places — leave them to `emit_expr`.
                let projectable = !matches!(
                    bt,
                    Ty::Slice(_) | Ty::Array { .. } | Ty::Prim("str") | Ty::Prim("String")
                ) && !self.info.qualified.contains_key(&id);
                if !projectable {
                    return self.emit_expr(id);
                }
                let b = self.emit_place(base, write);
                format!("{b}.j_{fname}")
            }
            ExprKind::Index { base, index } => {
                let (base, index) = (*base, *index);
                let bt = apply_subst(&self.info.type_of(base).clone(), &self.subst);
                if let Ty::Array { len, .. } = &bt {
                    let nlen = *len;
                    let aty = self.c_type(&bt);
                    let qual = if write { "" } else { "const " };
                    // The base is emitted as a *place* too, so an array of arrays
                    // keeps a real chain of addresses (`&m[i]` is legal because
                    // `m[i]` is itself an lvalue here, not a statement expression).
                    let b = self.emit_place(base, write);
                    let i = self.emit_expr(index);
                    let n = self.tmp;
                    self.tmp += 1;
                    return format!(
                        "(*({{ {qual}{aty}* _a{n} = &({b}); size_t _ix{n} = (size_t)({i}); assert(_ix{n} < {nlen}); &_a{n}->a[_ix{n}]; }}))"
                    );
                }
                if matches!(bt, Ty::Slice(_)) && !self.index_in_range(base, index) {
                    // The spilled `{ptr,len}` view is a copy, but the address taken
                    // points into the *buffer*, so it outlives the statement
                    // expression — a slice copy still names the same elements.
                    let sty = self.c_type(&bt);
                    let b = self.emit_expr(base);
                    let i = self.emit_expr(index);
                    let n = self.tmp;
                    self.tmp += 1;
                    return format!(
                        "(*({{ {sty} _s{n} = ({b}); size_t _ix{n} = (size_t)({i}); assert(_ix{n} < _s{n}.len); &_s{n}.ptr[_ix{n}]; }}))"
                    );
                }
                self.emit_expr(id)
            }
            _ => self.emit_expr(id),
        }
    }

    /// Emit `id` as an argument passed **by address** — a `mut`/`out` parameter, or
    /// a `mut`/`out self` receiver. Such an argument is a *place*, not a value, so
    /// it goes through [`Self::emit_place`]: `cs[i].bump()` otherwise takes the
    /// address of a bounds-checked statement expression and gcc reports "lvalue
    /// required as unary '&' operand". Every argument that is not reached through a
    /// checked index emits exactly as `&({expr})` always did.
    fn emit_addr_arg(&mut self, id: ExprId) -> String {
        let p = self.emit_place(id, true);
        format!("&({p})")
    }

    fn error_tag_of(&self, id: ExprId) -> Option<i64> {
        match &self.ast.expr_at(id).kind {
            ExprKind::Name(n) => self.error_tags.get(&n.name).copied(),
            _ => None,
        }
    }

    // --- monomorphization ---

    fn is_type_param(&self, p: &Param) -> bool {
        is_type_param_ast(self.ast, p)
    }

    /// A generic function has a `comptime <name>: type` parameter or a bracket-form
    /// `[T: Bound]` generic — either makes it a monomorphization template.
    fn is_generic(&self, f: &FnDecl) -> bool {
        is_generic_ast(self.ast, f)
    }

    /// The bracket-form generic parameter names of `f`, in declaration order.
    fn bracket_param_names(&self, f: &FnDecl) -> Vec<String> {
        f.generics.iter().map(|g| g.name.name.clone()).collect()
    }

    /// Infer a bracket-generic call's type arguments by unifying `f`'s declared
    /// parameter types against the actual argument types — the inference-based
    /// counterpart to a `comptime` generic's explicit type arguments. `subst` is
    /// the enclosing monomorphization substitution (for a call nested in another
    /// generic). Returns one `Ty` per bracket parameter, in declaration order.
    fn infer_bracket_args(
        &self,
        f: &FnDecl,
        args: &[ExprId],
        subst: &HashMap<String, Ty>,
    ) -> Vec<Ty> {
        if f.generics.is_empty() {
            return Vec::new();
        }
        let tps: HashSet<String> = f.generics.iter().map(|g| g.name.name.clone()).collect();
        let mut inferred: HashMap<String, Ty> = HashMap::new();
        for (i, p) in f.params.iter().enumerate() {
            let (Some(pt), Some(a)) = (p.ty, args.get(i)) else { continue };
            let param_ty = self.ast_type_to_ty(pt, subst);
            let arg_ty = apply_subst(&self.info.type_of(*a).clone(), subst);
            unify_tp(&param_ty, &arg_ty, &tps, &mut inferred);
        }
        f.generics
            .iter()
            .map(|g| inferred.get(&g.name.name).cloned().unwrap_or(Ty::Unknown))
            .collect()
    }

    /// The backend can emit a function if it has no `self` (methods) and no
    /// `comptime` *value* parameters (only `comptime` type parameters are ok).
    fn fn_supported(&self, f: &FnDecl) -> bool {
        fn_supported_ast(self.ast, f)
    }

    fn comptime_positions(&self, f: &FnDecl) -> Vec<usize> {
        f.params
            .iter()
            .enumerate()
            .filter(|(_, p)| self.is_type_param(p))
            .map(|(i, _)| i)
            .collect()
    }

    fn type_param_names(&self, f: &FnDecl) -> Vec<String> {
        f.params.iter().filter(|p| self.is_type_param(p)).map(|p| p.name.name.clone()).collect()
    }

    /// Find a top-level function by its *canonical* name. For a non-colliding
    /// name this is the bare name (callers passing a bare name are unaffected);
    /// for a duplicated name the caller passes the disambiguated `name__m<mod>`,
    /// selecting the right module's definition.
    fn find_fn(&self, name: &str) -> Option<&'a FnDecl> {
        let ast = self.ast;
        ast.items.iter().enumerate().find_map(|(i, it)| match it {
            Item::Fn(f) if self.canon_item(i, &f.name.name) == name => Some(f),
            _ => None,
        })
    }

    fn make_subst(&self, f: &FnDecl, args: &[Ty]) -> HashMap<String, Ty> {
        // Type parameters in mangle/instance order: `comptime` type params first,
        // then bracket-form `[T: Bound]` generics — matching how the instance's
        // `args` vector is assembled at every call site.
        let mut names = self.type_param_names(f);
        names.extend(self.bracket_param_names(f));
        names.into_iter().zip(args.iter().cloned()).collect()
    }

    fn mangle(&self, name: &str, args: &[Ty]) -> String {
        let parts: Vec<String> = args.iter().map(|t| self.ty_mangle(t)).collect();
        format!("{name}__{}", parts.join("_"))
    }

    fn ty_mangle(&self, t: &Ty) -> String {
        match t {
            Ty::Prim(n) => n.to_string(),
            Ty::Named(i) => self.info.table.types[*i].name.clone(),
            Ty::Ptr { inner, .. } => format!("ptr_{}", self.ty_mangle(inner)),
            Ty::Opaque(n) => n.clone(),
            Ty::Unit => "unit".to_string(),
            Ty::GenStruct { ctor, args } => {
                let a: Vec<String> = args.iter().map(|t| self.ty_mangle(t)).collect();
                format!("{ctor}__{}", a.join("_"))
            }
            // A generic enum instance mangles like a generic struct (so an
            // `Option(i32)`-returning fn-pointer gets a distinct typedef from an
            // `Option(f64)`-returning one — without this both collide on `x`).
            Ty::GenEnum { ctor, args } => {
                let a: Vec<String> = args.iter().map(|t| self.ty_mangle(t)).collect();
                format!("{ctor}__{}", a.join("_"))
            }
            Ty::Result(ok) => format!("result_{}", self.ty_mangle(ok)),
            Ty::Slice(elem) => format!("slice_{}", self.ty_mangle(elem)),
            Ty::Array { elem, len } => format!("arr_{}_{len}", self.ty_mangle(elem)),
            Ty::GenRef(elem) => format!("ref_{}", self.ty_mangle(elem)),
            Ty::RegionRef(elem) => format!("rref_{}", self.ty_mangle(elem)),
            // A fn-pointer mangle must vary with each parameter's *convention*
            // (a `mut`/`out` param lowers to `T*`, so it is a different C type),
            // hence the leading conv tag per parameter.
            Ty::Fn { params, ret, .. } => {
                let ps: Vec<String> = params
                    .iter()
                    .map(|(c, t)| {
                        let tag = match c {
                            Conv::Mut => "m",
                            Conv::Out => "o",
                            Conv::Take => "t",
                            Conv::Read => "r",
                            Conv::Default => "d",
                        };
                        format!("{tag}{}", self.ty_mangle(t))
                    })
                    .collect();
                format!("fn_{}_ret_{}", ps.join("_"), self.ty_mangle(ret))
            }
            _ => "x".to_string(),
        }
    }

    /// Evaluate a comptime type-argument expression (e.g. `i32`, or a type
    /// parameter `T` resolved through `subst`) to a concrete `Ty`.
    fn eval_type_arg(&self, id: ExprId, subst: &HashMap<String, Ty>) -> Ty {
        match &self.ast.expr_at(id).kind {
            ExprKind::Name(n) => {
                if let Some(t) = subst.get(&n.name) {
                    t.clone()
                } else if let Some(p) = prim_ty(&n.name) {
                    Ty::Prim(p)
                } else if let Some(&i) = self.info.table.type_index.get(&self.canon_type(&n.name)) {
                    Ty::Named(i)
                } else {
                    Ty::Opaque(n.name.clone())
                }
            }
            // A module-qualified type argument `mod.Type` (e.g. `tokens.Token`),
            // resolved in the target module via the import map — mirrors the
            // `TypeKind::Path` resolver (see `ast_type_to_ty`) so a generic
            // instantiated over an imported type mangles identically in the producer
            // and the consumer. Without this the element degrades to `Opaque("?")` and
            // the two modules disagree on the instance's C name (`Jestyr_List__?` in
            // the consumer vs `Jestyr_List__T` in the producer → invalid C).
            ExprKind::Field { base, name } => {
                if let ExprKind::Name(m) = &self.ast.expr_at(*base).kind {
                    let key = match self.path_target(&m.name) {
                        Some(t) => self.canon_type_in(t, &name.name),
                        None => self.canon_type(&name.name),
                    };
                    if let Some(&i) = self.info.table.type_index.get(&key) {
                        return Ty::Named(i);
                    }
                }
                Ty::Opaque("?".to_string())
            }
            _ => Ty::Opaque("?".to_string()),
        }
    }

    fn emit_generic_call(&mut self, name: &str, args: &[ExprId]) -> String {
        let Some(f) = self.find_fn(name) else { return "0".to_string() };
        let cpos = self.comptime_positions(f);
        let subst = self.subst.clone();
        // Type arguments in instance order: comptime (explicit) then bracket
        // (inferred from the value args). Must match `make_subst`'s ordering.
        let mut type_args: Vec<Ty> =
            cpos.iter().filter_map(|&p| args.get(p)).map(|a| self.eval_type_arg(*a, &subst)).collect();
        type_args.extend(self.infer_bracket_args(f, args, &subst));
        let mangled = self.mangle(name, &type_args);

        let mut parts = Vec::new();
        for (i, a) in args.iter().enumerate() {
            if cpos.contains(&i) {
                continue; // type argument — erased
            }
            let conv = f.params.get(i).map(|p| p.conv).unwrap_or(Conv::Default);
            let e = if matches!(conv, Conv::Mut | Conv::Out) {
                self.emit_addr_arg(*a)
            } else {
                self.emit_expr(*a)
            };
            parts.push(e);
        }
        format!("jestyr_{mangled}({})", parts.join(", "))
    }

    /// Walk all non-generic bodies (and, transitively, instantiated generic
    /// function *and* method bodies) collecting every concrete instantiation —
    /// both generic-function instances and generic-struct-method instances.
    /// A single worklist threads them because either can pull in the other.
    fn collect_all_instances(&self) -> (Vec<(String, Vec<Ty>)>, Vec<(String, Vec<Ty>, String)>) {
        let ast = self.ast;
        let mut seen: HashSet<String> = HashSet::new();
        let mut fn_order: Vec<(String, Vec<Ty>)> = Vec::new();
        let mut m_order: Vec<(String, Vec<Ty>, String)> = Vec::new();
        let mut work: Vec<Work> = Vec::new();
        let empty = HashMap::new();

        for item in &ast.items {
            if let Item::Fn(f) = item {
                if !self.is_generic(f) {
                    self.find_calls_block(&f.body, &empty, &mut work);
                }
            }
            // Trait-`impl` method bodies (Stage C) are emitted like free functions,
            // so a generic call inside one must instantiate its target too.
            if let Item::Impl(im) = item {
                for f in &im.methods {
                    self.find_calls_block(&f.body, &empty, &mut work);
                }
            }
        }
        while let Some(w) = work.pop() {
            match w {
                Work::Fn(name, args) => {
                    if !seen.insert(format!("fn:{}", self.mangle(&name, &args))) {
                        continue;
                    }
                    fn_order.push((name.clone(), args.clone()));
                    if let Some(gf) = self.find_fn(&name) {
                        let subst = self.make_subst(gf, &args);
                        self.find_calls_block(&gf.body, &subst, &mut work);
                    }
                }
                Work::Method(ctor, args, method) => {
                    if !seen.insert(format!("m:{}", self.method_c_name(&ctor, &args, &method))) {
                        continue;
                    }
                    m_order.push((ctor.clone(), args.clone(), method.clone()));
                    if let Some(mf) = self.find_struct_method_cg(&ctor, &method) {
                        let subst = self.method_subst(&ctor, &args);
                        self.find_calls_block(&mf.body, &subst, &mut work);
                    }
                }
            }
        }
        (fn_order, m_order)
    }

    fn find_calls_block(&self, b: &Block, subst: &HashMap<String, Ty>, work: &mut Vec<Work>) {
        for s in &b.stmts {
            match s {
                Stmt::Let { init: Some(e), .. } => self.find_calls_expr(*e, subst, work),
                Stmt::Return { value: Some(v), .. } => self.find_calls_expr(*v, subst, work),
                Stmt::Expr(e) => self.find_calls_expr(*e, subst, work),
                _ => {}
            }
        }
    }

    fn find_calls_expr(&self, id: ExprId, subst: &HashMap<String, Ty>, work: &mut Vec<Work>) {
        let ast = self.ast;
        match &ast.expr_at(id).kind {
            ExprKind::Call { callee, args } => {
                self.find_calls_expr(*callee, subst, work);
                for a in args {
                    self.find_calls_expr(*a, subst, work);
                }
                // A resolved method call instantiates its target — a struct
                // method (item C) or a generic free function (item A) — with
                // type arguments threaded through the current subst.
                if let Some(mr) = self.info.method_calls.get(&id) {
                    let targs: Vec<Ty> = mr.type_args.iter().map(|t| apply_subst(t, subst)).collect();
                    if let Some(ctor) = &mr.recv_ctor {
                        work.push(Work::Method(ctor.clone(), targs, mr.fn_name.clone()));
                    } else if self.generics.contains(&mr.fn_name) {
                        work.push(Work::Fn(mr.fn_name.clone(), targs));
                    }
                } else if let Some(qname) = self.info.qualified.get(&id) {
                    // A module-qualified call `mem.allocate(i32, …)` to a generic
                    // function instantiates it just like a bare generic call.
                    if self.generics.contains(qname) {
                        if let Some(gf) = self.find_fn(qname) {
                            let mut type_args: Vec<Ty> = self
                                .comptime_positions(gf)
                                .iter()
                                .filter_map(|&p| args.get(p))
                                .map(|a| self.eval_type_arg(*a, subst))
                                .collect();
                            type_args.extend(self.infer_bracket_args(gf, args, subst));
                            work.push(Work::Fn(qname.clone(), type_args));
                        }
                    }
                } else if let ExprKind::Name(n) = &ast.expr_at(*callee).kind {
                    // Canonical callee name for a within-module call to a
                    // possibly-colliding generic (bare when it doesn't collide).
                    let cname = self.info.call_sym.get(&id).cloned().unwrap_or_else(|| n.name.clone());
                    if self.generics.contains(&cname) {
                        if let Some(gf) = self.find_fn(&cname) {
                            let mut type_args: Vec<Ty> = self
                                .comptime_positions(gf)
                                .iter()
                                .filter_map(|&p| args.get(p))
                                .map(|a| self.eval_type_arg(*a, subst))
                                .collect();
                            type_args.extend(self.infer_bracket_args(gf, args, subst));
                            work.push(Work::Fn(cname, type_args));
                        }
                    }
                }
            }
            ExprKind::Binary { lhs, rhs, .. } => {
                self.find_calls_expr(*lhs, subst, work);
                self.find_calls_expr(*rhs, subst, work);
            }
            ExprKind::Unary { rhs, .. } => self.find_calls_expr(*rhs, subst, work),
            ExprKind::Assign { target, value, .. } => {
                self.find_calls_expr(*target, subst, work);
                self.find_calls_expr(*value, subst, work);
            }
            ExprKind::Range { lo, hi, .. } => {
                if let Some(l) = lo {
                    self.find_calls_expr(*l, subst, work);
                }
                if let Some(h) = hi {
                    self.find_calls_expr(*h, subst, work);
                }
            }
            ExprKind::Field { base, .. } => self.find_calls_expr(*base, subst, work),
            ExprKind::Index { base, index } => {
                self.find_calls_expr(*base, subst, work);
                self.find_calls_expr(*index, subst, work);
            }
            ExprKind::Deref { base } => self.find_calls_expr(*base, subst, work),
            ExprKind::Cast { expr, .. } => self.find_calls_expr(*expr, subst, work),
            ExprKind::Try { base } => self.find_calls_expr(*base, subst, work),
            // Both children, or a generic instantiated *only* in a fallback is never
            // monomorphized — a missing symbol at link time rather than a diagnostic.
            ExprKind::Catch { base, fallback, .. } => {
                self.find_calls_expr(*base, subst, work);
                self.find_calls_expr(*fallback, subst, work);
            }
            ExprKind::StructLit { fields, spread, .. } => {
                for f in fields {
                    self.find_calls_expr(f.value, subst, work);
                }
                if let Some(s) = spread {
                    self.find_calls_expr(*s, subst, work);
                }
            }
            ExprKind::GenStructLit { fields, .. } => {
                for f in fields {
                    self.find_calls_expr(f.value, subst, work);
                }
            }
            ExprKind::If { cond, then, els } => {
                self.find_calls_expr(*cond, subst, work);
                self.find_calls_block(then, subst, work);
                if let Some(e) = els {
                    self.find_calls_expr(*e, subst, work);
                }
            }
            ExprKind::Match { scrut, arms } => {
                self.find_calls_expr(*scrut, subst, work);
                for a in arms {
                    if let Some(g) = a.guard {
                        self.find_calls_expr(g, subst, work);
                    }
                    self.find_calls_expr(a.body, subst, work);
                }
            }
            ExprKind::Block(b) | ExprKind::Unsafe(b) | ExprKind::Concurrent(b) => {
                self.find_calls_block(b, subst, work)
            }
            ExprKind::Region { body, .. } => self.find_calls_block(body, subst, work),
            ExprKind::Closure { body, .. } => self.find_calls_expr(*body, subst, work),
            ExprKind::Spawn(inner) => self.find_calls_expr(*inner, subst, work),
            ExprKind::ParFor { iter, reduction, body, .. } => {
                // Descend so any (possibly generic) calls in the iterable, the
                // reduction, or the per-element body are monomorphized + emitted.
                self.find_calls_expr(*iter, subst, work);
                self.find_calls_expr(*reduction, subst, work);
                self.find_calls_expr(*body, subst, work);
            }
            ExprKind::Select(arms) => {
                for arm in arms {
                    self.find_calls_expr(arm.chan, subst, work);
                    self.find_calls_block(&arm.body, subst, work);
                }
            }
            ExprKind::For { head, body, els, .. } => {
                match head {
                    ForHead::While(c) => self.find_calls_expr(*c, subst, work),
                    ForHead::Iter { sources, .. } => {
                        for s in sources {
                            self.find_calls_expr(*s, subst, work);
                        }
                    }
                    ForHead::Infinite => {}
                }
                self.find_calls_block(body, subst, work);
                if let Some(els) = els {
                    self.find_calls_block(els, subst, work);
                }
            }
            ExprKind::Invariant(e) | ExprKind::Variant(e) => self.find_calls_expr(*e, subst, work),
            _ => {}
        }
    }
}

/// Does this expression lower to a C **lvalue** — something `&` may be applied to?
///
/// ## A Jestyr *place* is not automatically a C lvalue
/// The obvious rule — "the four place forms are lvalues" — is wrong, and the
/// `an_abi_ref_function_computes_the_same_answers` oracle rejected it immediately.
/// `xs[1]` is a place in Jestyr, but cgen lowers a **bounds-checked** index to a GNU
/// statement expression (`({ … ; _a->a[_i]; })`), which yields a *value*; `&` of it does
/// not compile. So lvalue-ness has to follow the **emission**, not the source form:
///
/// * `Name` — `j_x`, or `(*j_x)` for a pointer parameter. Both lvalues.
/// * `Field` — an lvalue exactly when its base is one, since it renders as `<base>.j_f`.
///   `xs[0].a` is therefore *not* one, because the base is that statement expression.
/// * `Deref` — always. Dereferencing any pointer *value* produces an lvalue in C, so
///   this holds however the operand was rendered.
/// * `Index` — never, per the above.
///
/// Being wrong toward "not an lvalue" costs one copy through the compound-literal path;
/// being wrong the other way does not compile at all, which is why the recursion is
/// worth the few lines.
fn is_c_lvalue(ast: &Ast, id: ExprId) -> bool {
    match &ast.expr_at(id).kind {
        ExprKind::Name(_) | ExprKind::Deref { .. } => true,
        ExprKind::Field { base, .. } => is_c_lvalue(ast, *base),
        _ => false,
    }
}

/// The C type of a runtime parameter given its passing convention.
///
/// A `mut`/`out` borrow is an *exclusive* (non-aliasing) reference — the same
/// guarantee Rust's `&mut` hands LLVM as `noalias`. We surface it to the C
/// optimizer as `restrict`, which is the cheapest place the ownership model pays
/// for itself in generated-code speed. (Soundness rests on the exclusivity the
/// escape checker enforces — passing the same object as two `mut` args would
/// violate it; the full aliasing guarantee lands with the reference model, §7 D.)
fn borrow_ptr_cty(base: &str, conv: Conv) -> String {
    if matches!(conv, Conv::Mut | Conv::Out) {
        format!("{base}* restrict")
    } else {
        base.to_string()
    }
}

/// Does control flow leave `block` via a `return` rather than falling off its
/// closing brace? When `ret` is set the caller emits the tail statement as a
/// `return` (so the block always diverges); otherwise only an explicit trailing
/// `Stmt::Return` diverges. Used to decide whether scope-exit drop glue runs at
/// the `}` (fall-through) or was already emitted before the `return`.
fn block_diverges(block: &Block, ret: bool) -> bool {
    ret || matches!(block.stmts.last(), Some(Stmt::Return { .. }))
}

/// The C symbol for a trait-`impl` method: `jestyr_impl_<Trait>__<TypeKey>__<method>`.
/// The type key (`i32`, `Point`, …) is sanitised to a C identifier so exotic
/// receiver types can't produce an invalid symbol. Both the definition and the
/// call site derive the name from the same `(trait, type-key, method)` triple, so
/// they always agree without a side table. (Coherence guarantees at most one impl
/// per `(trait, type)`, so distinct impls never collide on this name.)
fn impl_method_c_name(trait_name: &str, type_key: &str, method: &str) -> String {
    let safe: String =
        type_key.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
    format!("jestyr_impl_{trait_name}__{safe}__{method}")
}

/// Is `t` a C *scalar* (numeric/bool/char/pointer) rather than an aggregate? A
/// scalar may be placed in a single-value compound literal `(T){ v }`; an
/// aggregate (`str`/`String`, structs, slices, …) cannot, so its address is taken
/// directly. Used by the `dyn` coercion to give the erased data a valid address.
fn is_scalar_ty(t: &Ty) -> bool {
    match t {
        Ty::Prim(n) => !matches!(*n, "str" | "String" | "Builder"),
        Ty::Ptr { .. } => true,
        _ => false,
    }
}

/// Is `name` a backend intrinsic (a prelude stand-in for the stdlib / C interop)?
/// Used so a reference to one is not mistaken for a closure capture.
fn is_intrinsic(name: &str) -> bool {
    matches!(
        name,
        "print_int" | "print_float" | "print_str" | "print_bool"
            | "alloc" | "alloc_i32" | "realloc" | "realloc_i32" | "free_ptr" | "size_of" | "slice"
            // NOTE: the tier-3 reflection intrinsics are deliberately NOT listed here.
            // This list keeps a *bare value reference* to an intrinsic from being read
            // as a closure capture; reflection is only ever **called**, never referenced
            // as a value, so listing it would buy nothing — and would actively break any
            // program with a local named `field_count`, which the self-hosted compiler
            // itself has several of.
            | "align_of" | "offset_of" | "count_codepoints" | "codepoints" | "from_utf8" | "is_utf8"
            | "substr" | "str_eq" | "starts_with" | "ends_with" | "contains" | "find" | "trim"
            | "count_graphemes" | "graphemes" | "split" | "try_from_utf8" | "eq_fold"
            | "os_from_bytes" | "to_str_lossy"
            | "cow_borrow" | "cow_to_mut" | "cow_view" | "cow_is_owned" | "cow_free"
            | "string_new" | "string_from" | "string_push" | "string_view" | "string_free"
            | "builder_new" | "builder_push" | "builder_build" | "builder_free"
            | "region_str" | "region_concat" | "bytes"
            | "gen_new" | "gen_free" | "region_alloc" | "ok" | "err" | "is_err" | "unwrap"
            | "arena_open" | "arena_alloc" | "arena_close"
            | "read_file" | "try_read_file" | "write_file" | "file_exists" | "remove_file"
            | "run_command" | "eprint_str"
            | "arg_count" | "arg"
    )
}

/// Substitute type parameters (`Ty::Opaque`) throughout a `Ty` — used to push
/// a monomorphization substitution through method-call type arguments.
fn apply_subst(t: &Ty, subst: &HashMap<String, Ty>) -> Ty {
    match t {
        Ty::Opaque(n) => subst.get(n).cloned().unwrap_or_else(|| t.clone()),
        Ty::Ptr { mutbl, inner } => {
            Ty::Ptr { mutbl: *mutbl, inner: Box::new(apply_subst(inner, subst)) }
        }
        Ty::Result(ok) => Ty::Result(Box::new(apply_subst(ok, subst))),
        Ty::GenStruct { ctor, args } => {
            Ty::GenStruct { ctor: ctor.clone(), args: args.iter().map(|a| apply_subst(a, subst)).collect() }
        }
        Ty::GenEnum { ctor, args } => {
            Ty::GenEnum { ctor: ctor.clone(), args: args.iter().map(|a| apply_subst(a, subst)).collect() }
        }
        Ty::Slice(elem) => Ty::Slice(Box::new(apply_subst(elem, subst))),
        Ty::Array { elem, len } => Ty::Array { elem: Box::new(apply_subst(elem, subst)), len: *len },
        Ty::GenRef(elem) => Ty::GenRef(Box::new(apply_subst(elem, subst))),
        Ty::RegionRef(elem) => Ty::RegionRef(Box::new(apply_subst(elem, subst))),
        Ty::Fn { params, ret, ret_conv } => Ty::Fn {
            params: params.iter().map(|(c, t)| (*c, Box::new(apply_subst(t, subst)))).collect(),
            ret: Box::new(apply_subst(ret, subst)),
            ret_conv: *ret_conv,
        },
        _ => t.clone(),
    }
}

/// Re-render a Jestyr integer literal as valid C (strip `_`, convert binary).
/// A scalar comptime value as a C expression.
///
/// `Unit` and a nested `List` both render as `0`: neither can appear here in a
/// well-formed program (typeck refuses a unit-valued block, and a nested aggregate
/// needs an annotation it cannot have in this position), and cgen stays total rather
/// than panicking on a program that was already rejected.
fn c_comptime_scalar(v: &comptime::Value) -> String {
    match v {
        comptime::Value::Int(i) => i.to_string(),
        comptime::Value::Bool(b) => if *b { "true" } else { "false" }.to_string(),
        comptime::Value::Str(s) => format!("JSTR({})", c_string_literal(s)),
        comptime::Value::List(_) | comptime::Value::Unit => "0".to_string(),
    }
}

/// A comptime aggregate as a C **brace initializer** — `{ { 1, 2, 3 } }`, the outer
/// pair for the array-wrapper struct and the inner for its `a[]` member.
///
/// This is the form a `const` needs: a static initializer may not contain a
/// statement-expression, so the `({ … })` shape [`c_comptime_scalar`]'s caller builds
/// for an expression position would be invalid C at file scope.
fn c_comptime_brace(items: &[comptime::Value]) -> String {
    let parts: Vec<String> = items
        .iter()
        .map(|v| match v {
            comptime::Value::List(inner) => c_comptime_brace(inner),
            other => c_comptime_scalar(other),
        })
        .collect();
    format!("{{ {{ {} }} }}", parts.join(", "))
}

/// Encode a comptime-produced string as a C string literal.
///
/// A `Str` literal written in source is passed through verbatim (`JSTR({l})`) — its
/// escapes are already C's. A *computed* string has no source text, so it has to be
/// re-encoded, and two C rules make the naive encoder wrong:
///  * a hex escape is **maximal-munch** (`"\x41" "1"` reads as `\x411`), so
///    non-printables use three-digit octal, which has a fixed width;
///  * `-std=c11` still honours **trigraphs**, so a literal `?` is escaped rather
///    than left to turn `??/` into a backslash.
///
/// Bytes, not chars: a non-ASCII scalar emits its UTF-8 bytes, which is what `JSTR`'s
/// `sizeof(lit) - 1` length counts.
fn c_string_literal(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for b in s.bytes() {
        match b {
            b'"' => out.push_str("\\\""),
            b'\\' => out.push_str("\\\\"),
            b'\n' => out.push_str("\\n"),
            b'\r' => out.push_str("\\r"),
            b'\t' => out.push_str("\\t"),
            b'?' => out.push_str("\\?"),
            0x20..=0x7E => out.push(b as char),
            _ => {
                let _ = write!(out, "\\{b:03o}");
            }
        }
    }
    out.push('"');
    out
}

fn c_int_literal(lex: &str) -> String {
    let t: String = lex.chars().filter(|c| *c != '_').collect();
    if let Some(rest) = t.strip_prefix("0b").or_else(|| t.strip_prefix("0B")) {
        if let Ok(v) = u128::from_str_radix(rest, 2) {
            return v.to_string();
        }
    }
    t // decimal and `0x…` are already valid C
}

fn prim_c(name: &str) -> Option<&'static str> {
    Some(match name {
        "i8" => "int8_t",
        "i16" => "int16_t",
        "i32" => "int32_t",
        "i64" => "int64_t",
        "isize" => "intptr_t",
        "u8" => "uint8_t",
        "u16" => "uint16_t",
        "u32" => "uint32_t",
        "u64" => "uint64_t",
        "usize" => "size_t",
        "f32" => "float",
        "f64" => "double",
        "bool" => "bool",
        "char" => "uint32_t",
        "str" => "JestyrStr",
        "os_str" => "JestyrStr", // structurally a view, but unproven (possibly ill-formed)
        "cstr" => "const char*",
        "String" => "JestyrString",
        "Builder" => "JestyrBuilder",
        "Cow" => "JestyrCow",
        // The opaque error value a `catch |e|` binder carries. Runtime repr: the
        // result struct's `int err` tag. Opaque in the surface language, so a tag can
        // never be returned as a *success* value by accident.
        "error" => "int",
        _ => return None,
    })
}

fn binop_c(op: BinOp) -> &'static str {
    match op {
        BinOp::Add => "+",
        BinOp::Sub => "-",
        BinOp::Mul => "*",
        BinOp::Div => "/",
        BinOp::Rem => "%",
        BinOp::Eq => "==",
        BinOp::Ne => "!=",
        BinOp::Lt => "<",
        BinOp::Le => "<=",
        BinOp::Gt => ">",
        BinOp::Ge => ">=",
        BinOp::And => "&&",
        BinOp::Or => "||",
        BinOp::BitAnd => "&",
        BinOp::BitOr => "|",
        BinOp::BitXor => "^",
        BinOp::Shl => "<<",
        BinOp::Shr => ">>",
    }
}

fn unop_c(op: UnOp) -> &'static str {
    match op {
        UnOp::Neg => "-",
        UnOp::Not => "!",
        UnOp::BitNot => "~",
        UnOp::Ref => "&",
    }
}

fn assign_c(op: AssignOp) -> &'static str {
    match op {
        AssignOp::Assign => "=",
        AssignOp::Add => "+=",
        AssignOp::Sub => "-=",
        AssignOp::Mul => "*=",
        AssignOp::Div => "/=",
        AssignOp::Rem => "%=",
        AssignOp::BitAnd => "&=",
        AssignOp::BitOr => "|=",
        AssignOp::BitXor => "^=",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::Lexer;
    use crate::parser::Parser;

    fn gen(src: &str) -> (String, Vec<Diagnostic>) {
        let (tokens, ld) = Lexer::new(src).tokenize();
        assert!(ld.is_empty(), "lex: {:?}", ld);
        let (ast, pd) = Parser::new(src, tokens).parse();
        assert!(pd.is_empty(), "parse: {:?}", pd);
        let (info, _td) = crate::typeck::check(&ast);
        emit(&ast, &info)
    }

    /// Like [`gen`], but through [`emit_error_traces`] (`--error-traces`).
    fn gen_traced(src: &str) -> String {
        let (tokens, ld) = Lexer::new(src).tokenize();
        assert!(ld.is_empty(), "lex: {:?}", ld);
        let (ast, pd) = Parser::new(src, tokens).parse();
        assert!(pd.is_empty(), "parse: {:?}", pd);
        let (info, _td) = crate::typeck::check(&ast);
        let (c, d) = emit_error_traces(&ast, &info);
        assert!(d.is_empty(), "{d:?}");
        c
    }

    /// A fallible chain used by the error-trace shape tests: an origin, one `?` hop,
    /// and an unwrap.
    const TRACE_FIXTURE: &str = "fn deep(n: i32) -> i32 !{ Bad } { if n > 5 { return err(Bad) } return ok(n) } \
         fn mid(n: i32) -> i32 !{ Bad } { let v = deep(n)? return ok(v + 1) } \
         fn main() -> i32 { print_int(unwrap(mid(9)) as i64) return 0 }";

    /// **`--error-traces` instruments all three points, and only under the flag.**
    /// `err` is the origin (reset + push), `?` a propagation hop, unwrap-on-error the
    /// surfacing print. The flag-off arm is checked as an *absence*: one stray
    /// `jestyr_et_` in ordinary emission would be a corpus-wide golden diff.
    #[test]
    fn error_traces_instrument_err_try_and_unwrap() {
        let c = gen_traced(TRACE_FIXTURE);
        // The runtime is present…
        assert!(c.contains("static void jestyr_et_dump(void)"), "{c}");
        // …the origin resets then records…
        assert!(c.contains("jestyr_et_reset(); jestyr_et_push("), "origin: {c}");
        // …the `?` hop records inside its error branch, before the early return…
        assert!(c.contains("{ jestyr_et_push(\"<input>\", 0); return ("), "hop: {c}");
        // …and unwrap prints on error without changing what it yields. (Matched around
        // the temp number — it depends on how many temps preceded it.)
        assert!(c.contains(".is_err) jestyr_et_dump(); _uw"), "surface: {c}");

        // Flag off: not a byte of it — the emission is the pre-flag string exactly.
        let (plain, d) = gen(TRACE_FIXTURE);
        assert!(d.is_empty(), "{d:?}");
        assert!(!plain.contains("jestyr_et_"), "flag-off must be untouched: {plain}");
        // The `?` fast path keeps its original brace-free form: even a redundant
        // brace would diff every fallible corpus file against the port mirror.
        assert!(plain.contains(".is_err) return ("), "the untraced `?` string moved: {plain}");
    }

    /// Like [`gen`], but lowers in test-harness mode (`jestyrc test`).
    fn gen_tests(src: &str) -> (String, Vec<Diagnostic>) {
        let (tokens, ld) = Lexer::new(src).tokenize();
        assert!(ld.is_empty(), "lex: {:?}", ld);
        let (ast, pd) = Parser::new(src, tokens).parse();
        assert!(pd.is_empty(), "parse: {:?}", pd);
        let (info, _td) = crate::typeck::check(&ast);
        emit_tests(&ast, &info)
    }

    /// Like [`gen`], but with single-file debug-info populated (path `t.jtr`,
    /// base 0), so the backend emits `#line` directives — the loader path's
    /// behavior, reproduced without a temp file.
    fn gen_dbg(src: &str) -> (String, Vec<Diagnostic>) {
        let (tokens, ld) = Lexer::new(src).tokenize();
        assert!(ld.is_empty(), "lex: {:?}", ld);
        let (ast, pd) = Parser::new(src, tokens).parse();
        assert!(pd.is_empty(), "parse: {:?}", pd);
        let (mut info, _td) = crate::typeck::check(&ast);
        info.debug = crate::types::DebugInfo::new(
            vec!["t.jtr".to_string()],
            vec![src.to_string()],
            vec![0],
        );
        emit(&ast, &info)
    }

    // ── debug info: `#line` directives (workstream: debug info, increment a) ───

    /// Wiring: a function emits a `#line N "file"` directive with the *correct*
    /// line for its declaration, and gates only on populated source.
    #[test]
    fn emits_line_directives() {
        // `add` is declared on physical line 3 (1-based).
        let src = "\n\nfn add(a: i32, b: i32) -> i32 { a + b }\n";
        let (c, d) = gen_dbg(src);
        assert!(d.is_empty(), "{d:?}");
        assert!(c.contains("#line 3 \"t.jtr\""), "expected `#line 3 \"t.jtr\"` in:\n{c}");
        // The bare single-file path (empty debug) emits no `#line` — byte-identical.
        let (plain, _) = gen(src);
        assert!(!plain.contains("#line"), "no debug info ⇒ no `#line`:\n{plain}");
    }

    /// Wiring (multi-region): a span in an imported region resolves to *that*
    /// region's path + local line, not the root's. Simulated with two regions in
    /// the global buffer (the loader concatenates files with a `\n` separator).
    #[test]
    fn line_directive_points_at_the_imported_file() {
        // Region 0 ("root.jtr"): `fn root...` then a newline separator.
        // Region 1 ("dep.jtr"): a two-line file whose `fn dep` is local line 2.
        let root = "fn root() -> i32 { 0 }\n";
        let dep = "\nfn dep() -> i32 { 1 }\n";
        let src = format!("{root}{dep}");
        let (tokens, _) = Lexer::new(&src).tokenize();
        let (ast, _) = Parser::new(&src, tokens).parse();
        let (mut info, _td) = crate::typeck::check(&ast);
        info.debug = crate::types::DebugInfo::new(
            vec!["root.jtr".to_string(), "dep.jtr".to_string()],
            vec![root.to_string(), dep.to_string()],
            vec![0, root.len()],
        );
        let (c, _) = emit(&ast, &info);
        assert!(c.contains("#line 1 \"root.jtr\""), "root fn maps to root.jtr:1:\n{c}");
        assert!(c.contains("#line 2 \"dep.jtr\""), "dep fn maps to dep.jtr:2 (local line):\n{c}");
    }

    /// Unit: `span_to_file_line` on hand-chosen offsets — first byte, a newline
    /// boundary, the last byte, a second region at a nonzero base, and an
    /// out-of-range (synthesized) span. Region bases include the loader's `\n`
    /// separator (`module.rs` pushes one between files), so region b's base is
    /// `a.len() + 1`, not `a.len()` — there is no exact base collision in practice.
    #[test]
    fn span_to_file_line_maps_offsets() {
        use crate::span::Span;
        let a = "ab\ncd\n"; // bytes: a@0 b@1 \n@2 c@3 d@4 \n@5  (lines 1,2)
        let b = "xy\n"; // its own line 1
        let b_base = a.len() + 1; // +1 for the loader's region separator
        let dbg = crate::types::DebugInfo::new(
            vec!["a.jtr".to_string(), "b.jtr".to_string()],
            vec![a.to_string(), b.to_string()],
            vec![0, b_base],
        );
        let at = |o: usize| dbg.span_to_file_line(Span::new(o, o + 1));
        assert_eq!(at(0), Some(("a.jtr", 1))); // first byte
        assert_eq!(at(2), Some(("a.jtr", 1))); // the '\n' still closes line 1
        assert_eq!(at(3), Some(("a.jtr", 2))); // first byte after the newline
        assert_eq!(at(4), Some(("a.jtr", 2))); // last real byte of region a
        assert_eq!(at(b_base), Some(("b.jtr", 1))); // first byte of region b
        assert_eq!(at(b_base + 2), Some(("b.jtr", 1))); // its trailing '\n'
        // A span past every region (synthesized) resolves to nothing.
        assert_eq!(dbg.span_to_file_line(Span::new(999, 1000)), None);
        // Empty tables ⇒ no resolution (the single-file unit-test path).
        assert_eq!(
            crate::types::DebugInfo::default().span_to_file_line(Span::new(0, 1)),
            None
        );
    }

    /// Behavioral wiring: enabling `#line` changes the C *only* by adding `#line`
    /// lines — strip them and the output equals the no-debug build byte-for-byte.
    #[test]
    fn line_directives_are_purely_additive() {
        let src = "fn a() -> i32 { 1 }\nfn b(x: i32) -> i32 { x }\n";
        let (with, _) = gen_dbg(src);
        let (without, _) = gen(src);
        let stripped: String =
            with.lines().filter(|l| !l.starts_with("#line ")).map(|l| format!("{l}\n")).collect();
        assert_eq!(stripped, without, "removing `#line` lines must recover the plain C");
        // Windows-style backslash paths are normalized so they aren't C escapes
        // (`\p` / `\m` would otherwise be invalid/ambiguous escape sequences).
        let (tokens, _) = Lexer::new(src).tokenize();
        let (ast, _) = Parser::new(src, tokens).parse();
        let (mut info, _td) = crate::typeck::check(&ast);
        info.debug = crate::types::DebugInfo::new(
            vec!["C:\\proj\\m.jtr".to_string()],
            vec![src.to_string()],
            vec![0],
        );
        let (c, _) = emit(&ast, &info);
        assert!(c.contains("\"C:/proj/m.jtr\""), "backslashes normalized to forward slashes:\n{c}");
        assert!(!c.contains("C:\\proj"), "no raw backslashes in a C string literal:\n{c}");
    }

    /// Increment (b): each statement on its own line gets its own `#line`.
    #[test]
    fn per_statement_line_directives() {
        let src = "fn f() -> i32 {\n    let a = 1\n    let b = 2\n    return a + b\n}\n";
        let (c, d) = gen_dbg(src);
        assert!(d.is_empty(), "{d:?}");
        assert!(c.contains("#line 2 \"t.jtr\""), "`let a` on line 2:\n{c}");
        assert!(c.contains("#line 3 \"t.jtr\""), "`let b` on line 3:\n{c}");
        assert!(c.contains("#line 4 \"t.jtr\""), "`return` on line 4:\n{c}");
    }

    // ── B3: recoverable `try_read_file -> String !IoError` ────────────────────

    /// Wiring: `try_read_file` lowers to a tagged `JestyrResult_String`, gets its
    /// out-param runtime helper, and its err branch carries the `IoError` tag.
    /// **A fallible METHOD returns its tagged result struct, exactly as a fallible
    /// free function does.** The gate that refused this is gone; what replaced it:
    /// the method's C signature returns `JestyrResult_<ok>`, the typedef is emitted
    /// per *instance* (a generic struct's method has one ok type per instantiation),
    /// and `cur_result` is set during the body so `ok`/`err`/`?` inside it work
    /// unchanged.
    #[test]
    fn a_fallible_method_returns_its_result_struct() {
        let src = "struct A { b: i32 \
                     fn spend(mut self, n: i32) -> i32 !{ Insufficient } { \
                       if n > self.b { return err(Insufficient) } \
                       self.b = self.b - n return ok(self.b) } } \
                   fn main() -> i32 { var a: A = A { b: 100 } \
                     let l: i32 = a.spend(30) catch 0 - 1 print_int(l as i64) return 0 }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{d:?}");
        assert!(c.contains("typedef struct { bool is_err; int32_t ok; int err; } JestyrResult_i32;"), "{c}");
        assert!(c.contains("JestyrResult_i32 jestyr_A_spend("), "the method returns the result struct: {c}");
        // The body's `ok`/`err` construct THIS result type — cur_result was set.
        assert!(c.contains("(JestyrResult_i32){ .is_err = true"), "{c}");
    }

    /// A fallible impl method is refused at CHECK time with the reason: calls are
    /// typed by the trait's signature, which has no error-set syntax, so accepting it
    /// would mistype every call site as infallible.
    #[test]
    fn a_fallible_impl_method_is_refused_with_the_reason() {
        let src = "trait P { fn parse(read self) -> i32 } struct W { n: i32 } \
                   impl P for W { fn parse(read self) -> i32 !{ Bad } { return err(Bad) } } \
                   fn main() -> i32 { return 0 }";
        let (tokens, _) = crate::lexer::Lexer::new(src).tokenize();
        let (ast, _) = crate::parser::Parser::new(src, tokens).parse();
        let (_info, d) = crate::typeck::check(&ast);
        assert!(
            d.iter().any(|x| x.message.contains("a trait-impl method cannot be fallible")),
            "{d:?}"
        );
    }

    /// **`catch` recovers where `?` propagates**, and the difference shows in the C:
    /// `?` emits an early `return` of the error, `catch` emits a conditional. That is
    /// why `catch` is legal in an **infallible** function and `?` is not — recovering
    /// is exactly how a fallible call is made infallible.
    #[test]
    fn catch_lowers_to_a_conditional_not_an_early_return() {
        let src = "fn f(n: i32) -> i32 !{ Bad } { if n > 9 { return err(Bad) } return ok(n) } \
                   fn main() -> i32 { let a: i32 = f(1) catch 0 print_int(a as i64) return 0 }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{d:?}");
        assert!(
            c.contains("_ct0.is_err ? (0) : _ct0.ok"),
            "catch must lower to a conditional: {c}"
        );
        // No error is propagated out of `main` — that is `?`'s job, not `catch`'s.
        assert!(
            !c.contains("if (_ct0.is_err) return"),
            "catch must not early-return: {c}"
        );
    }

    /// **The three `catch` lowerings, each with its own shape** — and the binder-less
    /// one unchanged, since `error_catch.jtr` pins it against the port mirror.
    #[test]
    fn catch_binder_and_rethrow_lower_to_their_own_shapes() {
        // Rethrow ≡ `?`: early return, tag preserved.
        let src = "fn f() -> i32 !{ Bad } { return ok(1) } \
                   fn g() -> i32 !{ Bad } { let v: i32 = f() catch |e| return e return ok(v + 1) }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{d:?}");
        assert!(
            c.contains(".is_err) return (JestyrResult_i32){ .is_err = true, .err = _ct"),
            "rethrow must early-return with the tag preserved: {c}"
        );
        // Binder: a `const int j_e` scoped to the error branch; if/else over a result
        // variable, since `?:` cannot carry a declaration.
        let src = "fn f() -> i32 !{ Bad } { return ok(1) } \
                   fn g() -> i64 { return f() catch |e| (e as i64) }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{d:?}");
        assert!(c.contains("const int j_e = _ct"), "the binder must be declared: {c}");
        assert!(c.contains("(void)j_e;"), "an ignored binder must not warn: {c}");
        // Binder-less: the ORIGINAL `?:` string — rewriting it through the binder
        // shape would diff the corpus file against the port and the seed.
        let src = "fn f() -> i32 !{ Bad } { return ok(1) } \
                   fn g() -> i32 { return f() catch 0 }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{d:?}");
        assert!(c.contains(".is_err ? (0) : _ct"), "the binder-less `?:` moved: {c}");
        // Rethrow outside a fallible fn is `?`'s error, in `catch`'s words.
        let src = "fn f() -> i32 !{ Bad } { return ok(1) } \
                   fn g() -> i32 { return f() catch |e| return e }";
        let (_c, d) = gen(src);
        assert!(
            d.iter().any(|x| x.message.contains("`catch |e| return e` used outside a fallible function")),
            "{d:?}"
        );
    }

    /// The base is spilled to a temp so it is evaluated **once** — it is read twice
    /// (`.is_err` then `.ok`), and a call in base position would otherwise run twice.
    #[test]
    fn catch_evaluates_its_base_once() {
        let src = "fn f() -> i32 !{ Bad } { return ok(1) } \
                   fn main() -> i32 { let a: i32 = f() catch 0 print_int(a as i64) return 0 }";
        let (c, _) = gen(src);
        assert_eq!(c.matches("jestyr_f()").count(), 1, "base must be emitted once: {c}");
    }

    #[test]
    fn try_read_file_lowers_to_a_recoverable_result() {
        let src = "fn main() -> i32 { let r = try_read_file(\"x\") if is_err(r) { return 1 } return 0 }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "no diagnostics: {d:?}");
        assert!(c.contains("typedef struct { bool is_err; JestyrString ok; int err; } JestyrResult_String;"), "result typedef:\n{c}");
        assert!(c.contains("bool jestyr_rt_try_read_file(JestyrStr path, JestyrString* out)"), "runtime helper:\n{c}");
        assert!(c.contains(".is_err = true, .err = 1"), "err branch carries the IoError tag:\n{c}");
    }

    /// Byte-identity gate: a program that uses only `read_file` (not `try_read_file`)
    /// emits neither the result typedef nor the recoverable runtime helper — the
    /// feature is strictly additive, so unrelated programs are unchanged.
    #[test]
    fn try_read_gating_keeps_unrelated_programs_clean() {
        let src = "fn main() -> i32 { let s: String = read_file(\"x\") return s.len as i32 }";
        let (c, _) = gen(src);
        assert!(!c.contains("JestyrResult_String"), "no result typedef when unused:\n{c}");
        assert!(!c.contains("jestyr_rt_try_read_file"), "no recoverable helper when unused:\n{c}");
    }

    // ── B5: inline `slice(T, …)` typing in argument position ──────────────────

    /// Wiring: an *unannotated* `slice(u8, …)` fed straight into `from_utf8`
    /// gives its temp the slice type `JestyrSlice_u8`, not the old `int` fallback
    /// (which made the generated C fail to compile).
    #[test]
    fn inline_slice_into_from_utf8_types_as_a_slice() {
        let src = "fn f() -> i64 { var b: *mut u8 = alloc(u8, 4) let s: str = from_utf8(slice(u8, b, 4)) return s.len as i64 }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "no diagnostics: {d:?}");
        assert!(c.contains("JestyrSlice_u8 _u ="), "slice temp is typed []u8:\n{c}");
        assert!(!c.contains("int _u ="), "must NOT fall back to `int`:\n{c}");
    }

    /// The inline form lowers identically to the annotated-`let` workaround — the
    /// fix removes the need for the temporary binding, it doesn't change codegen.
    #[test]
    fn inline_slice_equals_the_annotated_workaround() {
        let inline = "fn f() -> i64 { var b: *mut u8 = alloc(u8, 4) let s: str = from_utf8(slice(u8, b, 4)) return s.len as i64 }";
        let annotated = "fn f() -> i64 { var b: *mut u8 = alloc(u8, 4) let vs: []u8 = slice(u8, b, 4) let s: str = from_utf8(vs) return s.len as i64 }";
        // The `from_utf8(_u)` statement-expression must be identical between forms.
        let (ci, _) = gen(inline);
        let (ca, _) = gen(annotated);
        assert!(ci.contains("JestyrSlice_u8 _u = (JestyrSlice_u8){ j_b, (size_t)(4) }"), "{ci}");
        assert!(ca.contains("JestyrSlice_u8 _u = j_vs"), "annotated form binds first:\n{ca}");
    }

    // ── B4: `unsafe`/block as a value (let/var initializer) ───────────────────

    /// Wiring: `unsafe { p.* }` as a `let` initializer lowers to the inner deref
    /// with no diagnostic and no statement-position rejection.
    #[test]
    fn unsafe_block_is_a_valid_value_initializer() {
        let src = "fn f() -> i64 { var d: *mut i64 = alloc(i64, 1) let y: i64 = unsafe { d.* } return y }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "no diagnostics for an unsafe initializer: {d:?}");
        assert!(c.contains("int64_t j_y = (*j_d);"), "unsafe init lowers to the deref:\n{c}");
        assert!(!c.contains("only supported in statement"), "no value-position rejection:\n{c}");
    }

    /// Metamorphic wiring: `unsafe { E }` in value position is byte-identical to
    /// bare `E` — `unsafe` is a compile-time marker with no runtime effect.
    #[test]
    fn unsafe_value_block_equals_the_bare_expression() {
        let wrapped = "fn f() -> i64 { let y: i64 = unsafe { 3 + 4 } return y }";
        let bare = "fn f() -> i64 { let y: i64 = 3 + 4 return y }";
        assert_eq!(gen(wrapped).0, gen(bare).0, "`unsafe {{ E }}` ≡ `E` as a value");
    }

    /// A plain `{ E }` block also yields its tail expression in value position.
    #[test]
    fn plain_block_yields_its_tail_as_a_value() {
        let block = "fn f() -> i64 { let y: i64 = { 5 + 6 } return y }";
        let bare = "fn f() -> i64 { let y: i64 = 5 + 6 return y }";
        assert_eq!(gen(block).0, gen(bare).0, "`{{ E }}` ≡ `E` as a value");
    }

    /// A value-position block with leading statements is still a clear error (the
    /// statement-expression form is future work, not silently miscompiled).
    #[test]
    fn multi_statement_value_block_is_rejected() {
        let src = "fn f() -> i64 { var d: *mut i64 = alloc(i64, 1) let y: i64 = unsafe { d.* = 1 d.* } return y }";
        let (_c, d) = gen(src);
        assert!(
            d.iter().any(|x| x.message.contains("single tail expression")),
            "a multi-statement value block must error: {d:?}"
        );
    }

    /// Increment (c): a contract's lowered `assert` is preceded by a `#line` at the
    /// `requires`/`ensures` clause, so a contract failure blames the `.jtr` source.
    #[test]
    fn contract_asserts_point_at_the_clause() {
        // `requires` on line 2, `ensures` on line 3.
        let src = "fn f(x: i32) -> i32\n    requires x >= 0\n    ensures result >= 0\n{\n    return x\n}\n";
        let (c, d) = gen_dbg(src);
        assert!(d.is_empty(), "{d:?}");
        assert!(c.contains("#line 2 \"t.jtr\""), "requires assert maps to line 2:\n{c}");
        assert!(c.contains("#line 3 \"t.jtr\""), "ensures assert maps to line 3:\n{c}");
    }

    /// Increment (b): a run of statements on *one* physical line costs a single
    /// directive, not one per statement (the dedup that keeps the C from bloating).
    #[test]
    fn line_directives_dedup_within_a_line() {
        let src = "fn f() -> i32 {\n    let a = 1 let b = 2 return a + b\n}\n";
        let (c, d) = gen_dbg(src);
        assert!(d.is_empty(), "{d:?}");
        assert_eq!(
            c.matches("#line 2 \"t.jtr\"").count(),
            1,
            "three statements share line 2 ⇒ exactly one directive:\n{c}"
        );
    }

    #[test]
    fn lowers_a_function_with_arithmetic_and_return() {
        let (c, d) = gen("fn add(a: i32, b: i32) -> i32 { a + b }");
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("int32_t jestyr_add(int32_t j_a, int32_t j_b)"), "{c}");
        assert!(c.contains("return (j_a + j_b);"), "{c}");
    }

    #[test]
    fn lowers_struct_and_compound_literal() {
        let (c, d) = gen("struct P { x: i32, y: i32 } fn mk() -> P { P{ x: 1, y: 2 } }");
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("typedef struct Jestyr_P Jestyr_P;"), "{c}");
        assert!(c.contains("int32_t j_x;"), "{c}");
        assert!(c.contains("(Jestyr_P){ .j_x = 1, .j_y = 2 }"), "{c}");
    }

    /// A struct embedding a generic-struct instance **by value** — the instance's C
    /// definition must precede the struct that embeds it, or C rejects the incomplete
    /// type (the reported gap: a `List(E)` field). The aggregate-definition emitter
    /// topologically orders definitions by their by-value field edges. Here `Holder`
    /// embeds `Box(Leaf)` which embeds `Leaf`, so the C order must be
    /// `Leaf` → `Box(Leaf)` → `Holder`.
    #[test]
    fn aggregate_defs_topologically_ordered_by_by_value_fields() {
        let src = "fn Box(comptime T: type) -> type { return struct { v: T } } \
                   struct Leaf { x: i32 } \
                   struct Holder { b: Box(Leaf), n: i32 } \
                   fn main() -> i32 { let h = Holder{ b: Box(Leaf){ v: Leaf{ x: 5 } }, n: 1 } return h.b.v.x }";
        let (c, d) = gen(src);
        assert!(!d.iter().any(|x| x.is_error()), "diags: {:?}", d);
        let leaf = c.find("struct Jestyr_Leaf {").expect("Leaf defined");
        let boxed = c.find("struct Jestyr_Box__Leaf {").expect("Box(Leaf) defined");
        let holder = c.find("struct Jestyr_Holder {").expect("Holder defined");
        // Each container's by-value field type is defined before the container.
        assert!(leaf < boxed, "Leaf must precede Box(Leaf):\n{c}");
        assert!(boxed < holder, "Box(Leaf) must precede Holder:\n{c}");
    }

    #[test]
    fn record_lowers_to_an_ordinary_struct() {
        // A `record` is immutable at the Jestyr level but representationally a
        // plain struct — zero runtime cost for the static guarantee.
        let (c, d) =
            gen("record Point { x: i32, y: i32 } fn mk() -> Point { Point{ x: 1, y: 2 } }");
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("typedef struct Jestyr_Point Jestyr_Point;"), "{c}");
        assert!(c.contains("(Jestyr_Point){ .j_x = 1, .j_y = 2 }"), "{c}");
    }

    #[test]
    fn lowers_if_in_return_position() {
        let (c, d) = gen("fn m(n: i32) -> i32 { if n <= 1 { return 1 } return n }");
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("if ((j_n <= 1))"), "{c}");
        assert!(c.contains("return 1;"), "{c}");
    }

    // --- Drop / RAII (design Phase 3) ---

    const DROP_PRELUDE: &str = "trait Drop { fn drop(mut self) } struct R { id: i32 } \
        impl Drop for R { fn drop(mut self) { print_int(self.id) } } ";

    #[test]
    fn droppable_local_emits_a_scope_exit_drop_call() {
        let (c, d) = gen(&format!("{DROP_PRELUDE} fn use_it() {{ let a = R{{ id: 1 }} }}"));
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("jestyr_impl_Drop__R__drop(&j_a);"), "{c}");
    }

    #[test]
    fn drops_run_in_reverse_declaration_order() {
        let (c, d) =
            gen(&format!("{DROP_PRELUDE} fn two() {{ let a = R{{ id: 1 }} let b = R{{ id: 2 }} }}"));
        assert!(d.is_empty(), "{:?}", d);
        let drop_b = c.find("jestyr_impl_Drop__R__drop(&j_b)").expect("b dropped");
        let drop_a = c.find("jestyr_impl_Drop__R__drop(&j_a)").expect("a dropped");
        assert!(drop_b < drop_a, "b must drop before a (reverse order):\n{c}");
    }

    #[test]
    fn moved_out_value_is_not_dropped_at_origin() {
        // The constructed value is returned (moved), so `make` emits no drop glue:
        // no drop *call site* (`__drop(&j_…`) exists anywhere in the program.
        let (c, d) = gen(&format!("{DROP_PRELUDE} fn make() -> R {{ return R{{ id: 9 }} }}"));
        assert!(d.is_empty(), "{:?}", d);
        assert!(!c.contains("__drop(&j_"), "make must not drop its returned value:\n{c}");
    }

    #[test]
    fn returned_local_moves_and_is_not_dropped() {
        // `r` is returned, so it is a move — dropped by the caller, not here.
        let (c, d) =
            gen(&format!("{DROP_PRELUDE} fn pass() -> R {{ let r = R{{ id: 3 }} return r }}"));
        assert!(d.is_empty(), "{:?}", d);
        assert!(!c.contains("jestyr_impl_Drop__R__drop(&j_r)"), "moved `r` must not drop:\n{c}");
    }

    #[test]
    fn show_drops_annotates_the_glue() {
        let src = format!("{DROP_PRELUDE} fn use_it() {{ let a = R{{ id: 1 }} }}");
        let (tokens, _) = Lexer::new(&src).tokenize();
        let (ast, _) = Parser::new(&src, tokens).parse();
        let (info, _) = crate::typeck::check(&ast);
        let (c, _d) = emit_show_drops(&ast, &info);
        assert!(c.contains("/* drop j_a : R */"), "show-drops comment missing:\n{c}");
    }

    #[test]
    fn region_owned_value_elides_per_value_drop_glue() {
        // Metamorphic: the *same* droppable emits one drop outside a region and
        // *zero* inside one (the arena reclaims it in bulk).
        let outside = format!("{DROP_PRELUDE} fn f() {{ let a = R{{ id: 1 }} }}");
        let inside = format!("{DROP_PRELUDE} fn f() {{ region r {{ let a = R{{ id: 1 }} }} }}");
        let (co, _) = gen(&outside);
        let (ci, _) = gen(&inside);
        assert_eq!(co.matches("__drop(&j_a)").count(), 1, "outside a region:\n{co}");
        assert_eq!(ci.matches("__drop(&j_a)").count(), 0, "region-owned value:\n{ci}");
    }

    #[test]
    fn mut_borrowed_droppable_still_drops_at_scope_exit() {
        // The RAII payoff (an allocator-owning Vec mutated via `mut` methods): a
        // droppable passed to a `mut`-borrow call is NOT moved, so it still drops
        // exactly once at scope exit. This is the take-vs-borrow seam in collect_moved.
        let src = "trait Drop { fn drop(mut self) } struct R { n: i32 } \
            impl Drop for R { fn drop(mut self) { print_int(self.n) } } \
            fn bump(mut r: R) { r.n = r.n + 1 } \
            fn f() { var r = R{ n: 1 } bump(r) }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert_eq!(
            c.matches("jestyr_impl_Drop__R__drop(&j_r)").count(),
            1,
            "a mut-borrowed droppable must drop exactly once:\n{c}"
        );
    }

    #[test]
    fn taken_droppable_is_not_dropped_by_caller() {
        // The complement: a `take` argument *consumes* — the callee owns it, so the
        // caller must NOT also drop it (that would double-free).
        let src = "trait Drop { fn drop(mut self) } struct R { n: i32 } \
            impl Drop for R { fn drop(mut self) { print_int(self.n) } } \
            fn consume(take r: R) {} \
            fn f() { var r = R{ n: 1 } consume(r) }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert_eq!(
            c.matches("jestyr_impl_Drop__R__drop(&j_r)").count(),
            0,
            "a taken droppable must not be dropped by the caller:\n{c}"
        );
    }

    #[test]
    fn generic_struct_instantiation_drops_at_scope_exit() {
        // A concrete instantiation of a generic struct (`Box(i32)`) with a `Drop`
        // impl is dropped at scope exit — the call and the impl definition agree on
        // the mangled name derived from the GenStruct's type key.
        let src = "trait Drop { fn drop(mut self) } \
            fn Box(comptime T: type) -> type { return struct { v: T } } \
            impl Drop for Box(i32) { fn drop(mut self) { print_int(self.v) } } \
            fn f() { var b = Box(i32){ v: 7 } }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("jestyr_impl_Drop__Box_i32___drop(&j_b)"), "drop call:\n{c}");
        assert!(
            c.contains("void jestyr_impl_Drop__Box_i32___drop(Jestyr_Box__i32"),
            "matching impl definition:\n{c}"
        );
    }

    #[test]
    fn blanket_generic_drop_impl_monomorphizes_per_instance() {
        // One `impl[T] Drop for Box(T)` covers every instantiation: cgen emits a
        // monomorphized `drop` per concrete element type, named by the instance's
        // type key so the scope-exit call site resolves.
        let src = "trait Drop { fn drop(mut self) } \
            fn Box(comptime T: type) -> type { return struct { v: T } } \
            impl[T] Drop for Box(T) { fn drop(mut self) { print_int(0) } } \
            fn f() { var a = Box(i32){ v: 1 } var b = Box(f64){ v: 2.0 } }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        // Both instances get a definition and a scope-exit drop call.
        assert!(c.contains("void jestyr_impl_Drop__Box_i32___drop(Jestyr_Box__i32"), "i32 def:\n{c}");
        assert!(c.contains("void jestyr_impl_Drop__Box_f64___drop(Jestyr_Box__f64"), "f64 def:\n{c}");
        assert!(c.contains("jestyr_impl_Drop__Box_i32___drop(&j_a)"), "drop a:\n{c}");
        assert!(c.contains("jestyr_impl_Drop__Box_f64___drop(&j_b)"), "drop b:\n{c}");
    }

    #[test]
    fn generic_call_borrow_arg_does_not_move_droppable() {
        // A droppable passed to a *generic* `mut`-borrow fn (a leading `comptime`
        // type argument occupies a slot) still drops exactly once — the args align
        // past the type arg, landing the value at a borrow, not a `take`.
        let src = "trait Drop { fn drop(mut self) } struct R { id: i32 } \
            impl Drop for R { fn drop(mut self) { print_int(self.id) } } \
            fn bump(comptime T: type, mut r: T) {} \
            fn f() { var x = R{ id: 1 } bump(R, x) }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert_eq!(
            c.matches("jestyr_impl_Drop__R__drop(&j_x)").count(),
            1,
            "a generic mut-borrow arg must not move the droppable:\n{c}"
        );
    }

    // --- B1: recursive drop of owned struct fields & enum payloads (design §2.8) ---

    #[test]
    fn nested_struct_field_drops_at_scope_exit() {
        // A struct with no `Drop` impl of its own but a droppable *field* must drop
        // that field at scope exit — RAII recurses into aggregates.
        let src = format!(
            "{DROP_PRELUDE} struct Holder {{ a: R, b: R }} \
             fn f() {{ let h = Holder{{ a: R{{ id: 1 }}, b: R{{ id: 2 }} }} }}"
        );
        let (c, d) = gen(&src);
        assert!(d.is_empty(), "{:?}", d);
        // Both fields drop, through the field accessor, and in reverse field order.
        let db = c.find("jestyr_impl_Drop__R__drop(&j_h.j_b)").expect("field b dropped");
        let da = c.find("jestyr_impl_Drop__R__drop(&j_h.j_a)").expect("field a dropped");
        assert!(db < da, "b must drop before a (reverse field order):\n{c}");
    }

    #[test]
    fn record_field_drops_at_scope_exit() {
        // A `record` (immutable struct) recurses into its droppable fields too.
        let src = format!(
            "{DROP_PRELUDE} record Wrap {{ inner: R }} \
             fn f() {{ let w = Wrap{{ inner: R{{ id: 5 }} }} }}"
        );
        let (c, d) = gen(&src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("jestyr_impl_Drop__R__drop(&j_w.j_inner)"), "record field dropped:\n{c}");
    }

    #[test]
    fn own_drop_runs_before_field_drops() {
        // A struct with *both* its own `Drop` impl and a droppable field runs its
        // own destructor first, then drops the field (Rust's outer-then-inner order).
        let src = "trait Drop { fn drop(mut self) } struct R { id: i32 } \
            impl Drop for R { fn drop(mut self) { print_int(self.id) } } \
            struct Outer { inner: R } \
            impl Drop for Outer { fn drop(mut self) { print_int(99) } } \
            fn f() { let o = Outer{ inner: R{ id: 1 } } }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        let own = c.find("jestyr_impl_Drop__Outer__drop(&j_o)").expect("own drop");
        let field = c.find("jestyr_impl_Drop__R__drop(&j_o.j_inner)").expect("field drop");
        assert!(own < field, "own destructor must run before field drops:\n{c}");
    }

    #[test]
    fn enum_payload_drops_only_for_the_live_variant() {
        // An enum payload drops under a `switch` on the tag — only the live
        // variant's owned payload is dropped (no blind drop of an inactive union arm).
        let src = format!(
            "{DROP_PRELUDE} enum Node {{ leaf, wrap(r: R) }} \
             fn f() {{ let n = wrap(R{{ id: 7 }}) }}"
        );
        let (c, d) = gen(&src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("switch (j_n.tag)"), "tag switch present:\n{c}");
        assert!(c.contains("case Jestyr_Node_wrap:"), "live variant case:\n{c}");
        assert!(c.contains("jestyr_impl_Drop__R__drop(&j_n.u.wrap.j_r)"), "payload dropped:\n{c}");
        // The nullary `leaf` variant carries nothing droppable, so it gets no case.
        assert!(!c.contains("case Jestyr_Node_leaf:"), "nullary variant needs no case:\n{c}");
    }

    #[test]
    fn non_droppable_aggregate_emits_no_drop_glue() {
        // Byte-identical guard: a struct/enum with no droppable field emits *no*
        // drop call at all — the new recursion is purely additive.
        let src = "struct Plain { x: i32, y: i32 } fn f() { let p = Plain{ x: 1, y: 2 } }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(!c.contains("__drop(&j_"), "a plain aggregate must not drop:\n{c}");
        assert!(!c.contains("switch (j_p.tag)"), "no spurious tag switch:\n{c}");
    }

    #[test]
    fn moved_out_field_owner_is_not_dropped() {
        // A nested-field owner that is *returned* (moved) drops neither itself nor
        // its fields here — the caller owns the whole aggregate. No double-drop.
        let src = format!(
            "{DROP_PRELUDE} struct Holder {{ a: R }} \
             fn make() -> Holder {{ let h = Holder{{ a: R{{ id: 1 }} }} return h }}"
        );
        let (c, d) = gen(&src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(!c.contains("__drop(&j_h"), "a moved aggregate must not drop its fields:\n{c}");
    }

    #[test]
    fn copy_aggregate_is_never_dropped() {
        // An `@copy` aggregate is duplicated, never owned at a single site — it must
        // never get drop glue even if structurally it could carry a droppable field.
        let src = "@copy struct Pt { x: i32, y: i32 } fn f() { let p = Pt{ x: 1, y: 2 } }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(!c.contains("__drop(&j_p"), "a @copy aggregate must not drop:\n{c}");
    }

    #[test]
    fn deeply_nested_fields_drop_recursively() {
        // Recursion is transitive: a struct holding a struct holding a droppable
        // reaches the leaf destructor through a chained field accessor.
        let src = format!(
            "{DROP_PRELUDE} struct Mid {{ r: R }} struct Top {{ m: Mid }} \
             fn f() {{ let t = Top{{ m: Mid{{ r: R{{ id: 1 }} }} }} }}"
        );
        let (c, d) = gen(&src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("jestyr_impl_Drop__R__drop(&j_t.j_m.j_r)"), "leaf dropped via chain:\n{c}");
    }

    #[test]
    fn allocator_value_routes_through_the_vtable_not_bare_malloc() {
        // The explicit-allocator interface (Phase 3): `a.alloc_fn(a.ctx, ly)` is an
        // *indirect* call through a struct fn-pointer field — the Zig vtable shape,
        // not a direct `malloc`.
        let src = "struct Layout { size: usize, align: usize } \
            struct Allocator { ctx: *mut u8, alloc_fn: fn(*mut u8, Layout) -> *mut u8 } \
            fn alloc_n(read a: Allocator, n: usize) -> *mut u8 { \
                return a.alloc_fn(a.ctx, Layout{ size: n, align: 8 }) }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("j_a.j_alloc_fn(j_a.j_ctx,"), "expected an indirect vtable call:\n{c}");
        // The fn-pointer field is a real typedef'd thin pointer, not a closure.
        assert!(c.contains("(*JestyrFn_"), "expected a fn-pointer typedef:\n{c}");
    }

    #[test]
    fn non_droppable_local_gets_no_glue() {
        // A plain struct without a `Drop` impl is never auto-dropped.
        let (c, d) = gen("struct P { x: i32 } fn f() { let p = P{ x: 1 } }");
        assert!(d.is_empty(), "{:?}", d);
        assert!(!c.contains("__drop"), "no Drop impl ⇒ no glue:\n{c}");
    }

    #[test]
    fn maps_print_intrinsic_and_emits_main_wrapper() {
        let (c, d) = gen("fn main() -> i32 { print_int(42) return 0 }");
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("jestyr_rt_print_int(42)"), "{c}");
        assert!(c.contains("int main(int argc, char** argv) { jestyr_rt_argc = argc; jestyr_rt_argv = argv; return (int) jestyr_main(); }"), "{c}");
    }

    #[test]
    fn normalizes_non_c_integer_literals() {
        assert_eq!(c_int_literal("1_000"), "1000");
        assert_eq!(c_int_literal("0b0010_0000"), "32");
        assert_eq!(c_int_literal("0x4000_C000"), "0x4000C000");
    }

    #[test]
    fn monomorphizes_a_generic_function() {
        // `pick` is instantiated at i32; the comptime type arg is erased.
        let src = "fn pick(comptime T: type, a: T, b: T) -> T { a } \
                   fn main() -> i32 { return pick(i32, 1, 2) }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("int32_t jestyr_pick__i32(int32_t j_a, int32_t j_b)"), "{c}");
        assert!(c.contains("jestyr_pick__i32(1, 2)"), "call site drops the type arg: {c}");
    }

    #[test]
    fn lowers_mut_borrow_by_pointer_with_raw_store() {
        let src = "struct L { ptr: *mut i32, len: i32 } \
                   fn push(mut l: L, x: i32) { unsafe { (l.ptr + l.len).* = x } l.len = l.len + 1 }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("void jestyr_push(Jestyr_L* restrict j_l, int32_t j_x)"), "mut → restrict pointer: {c}");
        assert!(c.contains("(*j_l).j_ptr"), "deref-field access: {c}");
        assert!(c.contains("(*j_l).j_len = ((*j_l).j_len + 1);"), "in-place mutation: {c}");
    }

    #[test]
    fn lowers_region_reference_zero_cost() {
        let src = "fn main() -> i32 { region r { var a: &[r]i32 = region_alloc(r, i32, 5) \
                       print_int(a.*) } return 0 }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("JestyrArena j_r = jestyr_arena_new"), "region opens an arena: {c}");
        assert!(c.contains("int32_t* j_a = "), "a region ref is a plain pointer: {c}");
        assert!(c.contains("jestyr_arena_alloc(&j_r, sizeof(int32_t))"), "bump allocation: {c}");
        assert!(c.contains("(*j_a)"), "deref is raw — zero-cost, no generation check: {c}");
        assert!(c.contains("jestyr_arena_free(&j_r)"), "arena freed at block end: {c}");
    }

    #[test]
    fn lowers_generational_reference_with_checked_deref() {
        let src =
            "fn main() -> i32 { var r: &i32 = gen_new(i32, 7) print_int(r.*) gen_free(r) return 0 }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(
            c.contains("typedef struct { int32_t* ptr; uint64_t gen; } JestyrRef_i32;"),
            "genref struct: {c}"
        );
        assert!(c.contains("malloc(8 + sizeof(int32_t))"), "alloc carries a generation header: {c}");
        assert!(c.contains(".ptr)[-1] == "), "deref checks the generation: {c}");
        assert!(c.contains(".ptr)[-1]++"), "gen_free bumps the generation: {c}");
    }

    // --- function-pointer types ---

    #[test]
    fn lowers_a_fn_pointer_type_to_a_c_typedef_and_indirect_call() {
        let (c, d) = gen("fn apply(f: fn(i32) -> i32, x: i32) -> i32 { return f(x) }");
        assert!(d.is_empty(), "{:?}", d);
        // The signature becomes a `typedef`, so the name sits on the outside.
        assert!(
            c.contains("typedef int32_t (*JestyrFn_fn_di32_ret_i32)(int32_t);"),
            "fn-pointer typedef: {c}"
        );
        assert!(c.contains("JestyrFn_fn_di32_ret_i32 j_f"), "the parameter uses the typedef: {c}");
        // The call through the pointer is *indirect* — `j_f`, not a mangled
        // `jestyr_f` (which would be a direct call to a non-existent function).
        assert!(c.contains("return j_f(j_x);"), "indirect call through the pointer: {c}");
    }

    #[test]
    fn lowers_a_vtable_struct_of_fn_pointers() {
        // The headline use case: a hand-written allocator vtable — a struct whose
        // fields are thin function pointers, the interface that must exist before
        // any trait system does.
        let src = "struct Allocator { alloc_fn: fn(i32) -> *mut u8, free_fn: fn(*mut u8) } \
                   fn pick(read a: Allocator) -> i32 { return 0 }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(
            c.contains("typedef uint8_t* (*JestyrFn_fn_di32_ret_ptr_u8)(int32_t);"),
            "alloc_fn typedef: {c}"
        );
        assert!(
            c.contains("typedef void (*JestyrFn_fn_dptr_u8_ret_unit)(uint8_t*);"),
            "free_fn typedef (unit return → void): {c}"
        );
        assert!(c.contains("JestyrFn_fn_di32_ret_ptr_u8 j_alloc_fn;"), "vtable field: {c}");
    }

    #[test]
    fn lowers_address_of_a_function_to_its_c_symbol() {
        let src = "fn dbl(x: i32) -> i32 { return x + x } \
                   fn main() -> i32 { let op: fn(i32) -> i32 = &dbl return op(21) }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("(&jestyr_dbl)"), "address-of-fn is the mangled C symbol: {c}");
        assert!(c.contains("j_op(21)"), "the call through `op` is indirect: {c}");
    }

    #[test]
    fn lowers_a_mut_parameter_in_a_fn_pointer_type_by_pointer() {
        // A `mut` parameter declared *in the pointer's type* lowers to `T*`,
        // matching the ABI of a real Jestyr `mut` parameter.
        let (c, d) = gen("fn f(g: fn(mut i32) -> i32) -> i32 { return 0 }");
        assert!(d.is_empty(), "{:?}", d);
        assert!(
            c.contains("typedef int32_t (*JestyrFn_fn_mi32_ret_i32)(int32_t*);"),
            "a `mut` parameter becomes a pointer in the typedef: {c}"
        );
    }

    #[test]
    fn lowers_a_field_call_through_a_vtable_pointer() {
        let src = "struct A { op: fn(i32) -> i32 } \
                   fn use_it(read a: A, n: i32) -> i32 { return a.op(n) }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("j_a.j_op(j_n)"), "indirect call through the struct field: {c}");
    }

    #[test]
    fn lowers_a_field_call_through_a_generic_vtable_pointer() {
        // The generic-struct counterpart of the test above: a fn-pointer field on
        // a *generic* vtable (`Box(i32)`), called method-style. Now that typeck
        // types the callee as `Ty::Fn`, codegen routes through the real fn-pointer
        // invoke path (not the generic tail), so the indirect call is emitted.
        let src = "fn Box(comptime T: type) -> type { return struct { op: fn(T) -> T } } \
                   fn use_it(n: i32) -> i32 { let b = Box(i32){ op: |x| x + 1 } return b.op(n) }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("j_b.j_op(j_n)"), "indirect call through the generic-struct field: {c}");
    }

    #[test]
    fn generic_vtable_field_call_takes_mut_arg_by_pointer() {
        // The strictly-more-correct payoff: a `mut` parameter declared in the
        // field's *pointer type* must be passed by `&`, matching the callee's ABI.
        // The old generic tail-fallthrough dropped the conv and passed by value;
        // routing through `emit_fn_ptr_invoke` reads the `Ty::Fn`'s per-param conv.
        let src = "fn Box(comptime T: type) -> type { return struct { op: fn(mut T) } } \
                   fn use_it(n: i32) { let b = Box(i32){ op: |x| { } } b.op(n) }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("j_b.j_op(&(j_n))"), "a `mut` field-pointer arg is passed by address: {c}");
    }

    // --- traits: static, monomorphized dispatch (Stage C) ---

    #[test]
    fn lowers_a_trait_impl_method_to_a_direct_static_call() {
        // `x.show()` resolved through `impl Show for i32` becomes a *direct* call
        // of the emitted, mangled impl-method function — no vtable, no indirection.
        let src = "trait Show { fn show(read self) -> i32 } \
                   impl Show for i32 { fn show(read self) -> i32 { return self + 1 } } \
                   fn use_it(read x: i32) -> i32 { return x.show() }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(
            c.contains("int32_t jestyr_impl_Show__i32__show(int32_t j_self)"),
            "impl method emitted with a value `self`: {c}"
        );
        assert!(c.contains("return (j_self + 1);"), "the body projects `self`: {c}");
        assert!(c.contains("jestyr_impl_Show__i32__show(j_x)"), "direct static-dispatch call: {c}");
    }

    #[test]
    fn lowers_a_trait_impl_for_a_struct_receiver_with_an_argument() {
        // A struct receiver (`self` projects fields) and an explicit argument
        // threaded after the receiver — the call site is fully positional.
        let src = "trait Area { fn scaled(read self, k: i32) -> i32 } \
                   struct Rect { w: i32, h: i32 } \
                   impl Area for Rect { fn scaled(read self, k: i32) -> i32 { return (self.w + self.h) * k } } \
                   fn use_it(read r: Rect, k: i32) -> i32 { return r.scaled(k) }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(
            c.contains("int32_t jestyr_impl_Area__Rect__scaled(Jestyr_Rect j_self, int32_t j_k)"),
            "struct `self` type + trailing arg: {c}"
        );
        assert!(
            c.contains("jestyr_impl_Area__Rect__scaled(j_r, j_k)"),
            "receiver then argument, positionally: {c}"
        );
    }

    #[test]
    fn trait_impl_mut_self_receiver_is_passed_by_pointer() {
        // A `mut self` receiver lowers to `T* restrict` and the call passes the
        // receiver by address — the same ABI as a hand-written `mut self` method.
        let src = "trait Bump { fn bump(mut self) } \
                   impl Bump for i32 { fn bump(mut self) { self = self + 1 } } \
                   fn use_it() { var x: i32 = 5 x.bump() }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(
            c.contains("void jestyr_impl_Bump__i32__bump(int32_t* restrict j_self)"),
            "mut self → restrict pointer: {c}"
        );
        assert!(c.contains("jestyr_impl_Bump__i32__bump(&(j_x))"), "mut self passed by address: {c}");
    }

    #[test]
    fn distinct_trait_impls_get_distinct_mangled_symbols() {
        // Two impls of one trait for different receiver types produce two distinct
        // C symbols, each selected at its call site by the receiver's type key —
        // the essence of static dispatch.
        let src = "trait G { fn g(read self) -> i32 } \
                   struct P { a: i32 } \
                   impl G for i32 { fn g(read self) -> i32 { return self } } \
                   impl G for P { fn g(read self) -> i32 { return self.a } } \
                   fn use_i(read n: i32) -> i32 { return n.g() } \
                   fn use_p(read p: P) -> i32 { return p.g() }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("jestyr_impl_G__i32__g(j_n)"), "i32 receiver picks the i32 impl: {c}");
        assert!(c.contains("jestyr_impl_G__P__g(j_p)"), "P receiver picks the P impl: {c}");
    }

    // --- traits: operator traits (Stage E) ---

    #[test]
    fn lowers_an_operator_to_its_impl_method_call() {
        // `a + b` and `a == b` on a user type lower to direct calls of the
        // `Add`/`Eq` impl methods (lhs receiver, rhs argument) — no native `+`/`==`.
        let src = "struct V { n: i32 } \
                   impl Add for V { fn add(read self, read rhs: V) -> V { return V{ n: self.n + rhs.n } } } \
                   impl Eq for V { fn eq(read self, read rhs: V) -> bool { return self.n == rhs.n } } \
                   fn use_it(read a: V, read b: V) -> bool { let s = a + b return a == b }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(
            c.contains("jestyr_impl_Add__V__add(j_a, j_b)"),
            "`+` lowers to the Add impl call: {c}"
        );
        assert!(
            c.contains("jestyr_impl_Eq__V__eq(j_a, j_b)"),
            "`==` lowers to the Eq impl call: {c}"
        );
        // The impl methods themselves are emitted (Stage C machinery).
        assert!(
            c.contains("Jestyr_V jestyr_impl_Add__V__add(Jestyr_V j_self, Jestyr_V j_rhs)"),
            "Add impl method definition: {c}"
        );
    }

    #[test]
    fn primitive_operator_keeps_native_lowering() {
        // A primitive `+` stays a native C `+` — no operator-trait dispatch.
        let (c, d) = gen("fn add(a: i32, b: i32) -> i32 { return a + b }");
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("(j_a + j_b)"), "primitive add is native: {c}");
        assert!(!c.contains("jestyr_impl_Add"), "no operator-trait dispatch for primitives: {c}");
    }

    #[test]
    fn lowers_subtraction_and_division_to_their_impls() {
        // `-`/`/` are their own primitive operator traits, like `+`/`*`.
        let src = "struct V { n: i32 } \
                   impl Sub for V { fn sub(read self, read rhs: V) -> V { return V{ n: self.n - rhs.n } } } \
                   impl Div for V { fn div(read self, read rhs: V) -> V { return V{ n: self.n / rhs.n } } } \
                   fn use_it(read a: V, read b: V) -> V { let d = a / b return a - b }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("jestyr_impl_Sub__V__sub(j_a, j_b)"), "`-` → Sub: {c}");
        assert!(c.contains("jestyr_impl_Div__V__div(j_a, j_b)"), "`/` → Div: {c}");
    }

    #[test]
    fn lowers_derived_comparisons_via_swap_and_negate() {
        // The four derived comparisons reuse `Eq::eq`/`Ord::lt` with a swap and/or
        // negate — no extra impls needed beyond `Eq` and `Ord`.
        let src = "struct V { n: i32 } \
                   impl Eq for V { fn eq(read self, read rhs: V) -> bool { return self.n == rhs.n } } \
                   impl Ord for V { fn lt(read self, read rhs: V) -> bool { return self.n < rhs.n } } \
                   fn ne(read a: V, read b: V) -> bool { return a != b } \
                   fn gt(read a: V, read b: V) -> bool { return a > b } \
                   fn le(read a: V, read b: V) -> bool { return a <= b } \
                   fn ge(read a: V, read b: V) -> bool { return a >= b }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("(!jestyr_impl_Eq__V__eq(j_a, j_b))"), "`!=` negates eq: {c}");
        assert!(c.contains("(!jestyr_impl_Ord__V__lt(j_b, j_a))"), "`<=` swaps + negates: {c}");
        assert!(c.contains("(!jestyr_impl_Ord__V__lt(j_a, j_b))"), "`>=` negates: {c}");
        // `>` swaps without negating — the bare (un-negated) swapped call appears
        // for `gt` (and inside `<=`'s negation, which is fine).
        assert!(
            c.contains("return jestyr_impl_Ord__V__lt(j_b, j_a);"),
            "`>` swaps operands without negating: {c}"
        );
    }

    // --- bracket-generic monomorphization (codegen side) ---

    #[test]
    fn monomorphizes_a_bracket_generic_from_the_argument_type() {
        // `dup[T]` is a template: its `T` is inferred from the call's value
        // argument and a mangled instance is emitted + called.
        let src = "fn dup[T](take x: T) -> T { return x } \
                   fn main() -> i32 { return dup(42) }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(
            c.contains("int32_t jestyr_dup__i32(int32_t j_x)"),
            "i32 instance with T erased to int32_t: {c}"
        );
        assert!(c.contains("jestyr_dup__i32(42)"), "call targets the instance: {c}");
    }

    #[test]
    fn a_bracket_generic_instantiates_once_per_concrete_type() {
        // Two calls at different types produce two distinct mangled instances —
        // each `T` recovered from that call's argument.
        let src = "fn dup[T](take x: T) -> T { return x } \
                   fn main() -> i32 { let a = dup(7) let b = dup(true) return a }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("jestyr_dup__i32("), "an i32 instance: {c}");
        assert!(c.contains("jestyr_dup__bool("), "a distinct bool instance: {c}");
    }

    #[test]
    fn a_multi_param_bracket_generic_mangles_each_type_arg() {
        // `[A, B]` recovers both parameters from the two arguments, in order.
        let src = "fn pair[A, B](read a: A, read b: B) -> i32 { return 0 } \
                   fn main() -> i32 { return pair(1, true) }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(
            c.contains("jestyr_pair__i32_bool(int32_t j_a, bool j_b)"),
            "both type args mangled, in declaration order: {c}"
        );
    }

    #[test]
    fn mixes_a_comptime_and_a_bracket_type_parameter() {
        // Both generic forms in one signature: an explicit `comptime T: type` and
        // a bracket `[U]` inferred from its value argument. The instance mangles
        // `comptime ++ bracket` (T then U) and erases the comptime type *argument*
        // from the value params — locking in the cross-cutting ordering invariant.
        let src = "fn mix[U](comptime T: type, take a: T, take b: U) -> i32 { return 0 } \
                   fn main() -> i32 { return mix(i32, 5, true) }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(
            c.contains("jestyr_mix__i32_bool(int32_t j_a, bool j_b)"),
            "T=i32 (comptime, erased from params) + U=bool (inferred), mangled in order: {c}"
        );
        assert!(c.contains("jestyr_mix__i32_bool(5, true)"), "the call drops the type argument: {c}");
    }

    #[test]
    fn a_bound_method_call_dispatches_per_monomorphized_instance() {
        // The "Zig fix" payoff: one generic body `x.show()` lowers to a *different*
        // impl per instantiation — the concrete type recovered from the active
        // monomorphization substitution.
        let src = "trait Show { fn show(read self) -> i32 } \
                   impl Show for i32 { fn show(read self) -> i32 { return self } } \
                   struct P { v: i32 } \
                   impl Show for P { fn show(read self) -> i32 { return self.v } } \
                   fn describe[T: Show](read x: T) -> i32 { return x.show() } \
                   fn main() -> i32 { return describe(1) + describe(P{ v: 2 }) }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        // Assert the *call* (receiver `j_x`), not the always-emitted impl method
        // definition (which uses `j_self`) — so this has teeth for the dispatch.
        assert!(
            c.contains("jestyr_impl_Show__i32__show(j_x)"),
            "the i32 instance body dispatches to the i32 impl: {c}"
        );
        assert!(
            c.contains("jestyr_impl_Show__P__show(j_x)"),
            "the P instance body dispatches to the P impl: {c}"
        );
    }

    // --- traits: `dyn Trait` dynamic dispatch (Stage F) ---

    #[test]
    fn lowers_dyn_to_a_vtable_fat_pointer_and_dispatches_through_it() {
        let src = "trait Show { fn show(read self) -> i32 } \
                   impl Show for i32 { fn show(read self) -> i32 { return self + 1 } } \
                   fn describe(read s: dyn Show) -> i32 { return s.show() } \
                   fn main() -> i32 { let n = 41 return describe(n) }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        // The synthesized vtable struct + fat-pointer typedef.
        assert!(c.contains("} JestyrVtable_Show;"), "vtable struct: {c}");
        assert!(
            c.contains("void* data; const JestyrVtable_Show* vtable;"),
            "fat-pointer typedef: {c}"
        );
        // A shim erasing the receiver + a static vtable instance.
        assert!(
            c.contains("jestyr_vtshim_Show__i32__show(void* self)")
                && c.contains("jestyr_impl_Show__i32__show(*(int32_t*)self)"),
            "the i32 shim casts the erased self back: {c}"
        );
        assert!(
            c.contains("static const JestyrVtable_Show jestyr_vt_Show__i32 = { jestyr_vtshim_Show__i32__show }"),
            "static vtable instance: {c}"
        );
        // Dispatch through the vtable slot, and the coercion to a fat pointer.
        assert!(c.contains("j_s.vtable->show(j_s.data)"), "dynamic dispatch: {c}");
        assert!(
            c.contains("&jestyr_vt_Show__i32"),
            "the argument coerces into a fat pointer with the i32 vtable: {c}"
        );
    }

    #[test]
    fn a_dyn_call_dispatches_the_same_function_to_distinct_impls() {
        // One `describe` (not monomorphized) dispatches to whichever impl the
        // value's runtime type provides — the vtable picks i32 vs P.
        let src = "trait Show { fn show(read self) -> i32 } \
                   impl Show for i32 { fn show(read self) -> i32 { return self } } \
                   struct P { v: i32 } \
                   impl Show for P { fn show(read self) -> i32 { return self.v } } \
                   fn describe(read s: dyn Show) -> i32 { return s.show() } \
                   fn main() -> i32 { let n = 1 let p = P{ v: 2 } return describe(n) + describe(p) }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        // A single describe function, but two vtables referenced at the call sites.
        assert_eq!(c.matches("int32_t jestyr_describe(").count(), 2, "one proto + one def: {c}");
        assert!(c.contains("&jestyr_vt_Show__i32"), "i32 call uses the i32 vtable: {c}");
        assert!(c.contains("&jestyr_vt_Show__P"), "P call uses the P vtable: {c}");
    }

    #[test]
    fn lowers_a_non_capturing_closure_coerced_to_a_fn_pointer() {
        // A non-capturing closure used where a `fn(...)` is expected becomes a
        // *bare* top-level function, and the value is its address — no fat
        // `{call, env}` closure struct.
        let src = "fn apply(f: fn(i32) -> i32, x: i32) -> i32 { return f(x) } \
                   fn main() -> i32 { return apply(|x| x + 1, 41) }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("static int32_t jestyr_lam_"), "a bare thin function: {c}");
        assert!(c.contains("(&jestyr_lam_"), "the value is the function's address: {c}");
        assert!(!c.contains("JestyrClosure_"), "no fat-closure struct for a coerced closure: {c}");
    }

    #[test]
    fn lowers_a_vtable_built_from_closure_literals() {
        // The full ergonomic: construct a vtable directly from closure literals in
        // the struct-literal fields. Each becomes a bare function; the field is
        // initialized with its address.
        let src = "struct V { op: fn(i32) -> i32 } \
                   fn run(read v: V, n: i32) -> i32 { return v.op(n) } \
                   fn main() -> i32 { let v = V{ op: |x| x + 1 } return run(v, 41) }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("static int32_t jestyr_lam_"), "closure field → bare function: {c}");
        assert!(
            c.contains(".j_op = (&jestyr_lam_"),
            "the struct field is initialized with the function's address: {c}"
        );
        assert!(!c.contains("JestyrClosure_"), "no fat-closure struct: {c}");
    }

    #[test]
    fn lowers_a_generic_vtable_field_under_substitution() {
        // A generic struct whose field's fn-pointer type uses the type parameter:
        // the monomorphized instance must emit the *substituted* concrete typedef
        // (and no opaque `…_dT_ret_T` placeholder), and the field closure coerces.
        let src = "fn Box(comptime T: type) -> type { return struct { op: fn(T) -> T } } \
                   fn run(comptime T: type, read b: Box(T), x: T) -> T { return b.op(x) } \
                   fn main() -> i32 { let b = Box(i32){ op: |x| x + 1 } return run(i32, b, 41) }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(
            c.contains("typedef int32_t (*JestyrFn_fn_di32_ret_i32)(int32_t);"),
            "concrete typedef under substitution: {c}"
        );
        assert!(c.contains("JestyrFn_fn_di32_ret_i32 j_op;"), "the monomorphized field uses it: {c}");
        assert!(c.contains("static int32_t jestyr_lam_"), "the field closure becomes a bare function: {c}");
        assert!(!c.contains("JestyrFn_fn_dT_ret_T"), "no opaque placeholder typedef: {c}");
    }

    #[test]
    fn rejects_a_capturing_closure_coerced_to_a_fn_pointer() {
        // A closure that captures its environment is not a thin pointer — coercing
        // it must be a clear error, not silently-wrong C.
        let src = "fn main() -> i32 { let k = 10 \
                   let bad: fn(i32) -> i32 = |x| x + k return bad(1) }";
        let (_c, d) = gen(src);
        assert!(
            d.iter().any(|m| m.message.contains("cannot coerce to a thin function pointer")),
            "expected a capture-coercion error, got {:?}",
            d
        );
    }

    #[test]
    fn refinement_elides_the_bounds_check() {
        // `at`'s index is refined `in 0..s.len`, so its `s[i]` is a raw access;
        // `chk`'s index is unconstrained, so its `s[i]` keeps the assert.
        let src = "fn at(s: []i32, i: usize in 0..s.len) -> i32 { s[i] } \
                   fn chk(s: []i32, i: usize) -> i32 { s[i] }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("return ((j_s).ptr[(j_i)]);"), "refined index elided: {c}");
        assert!(c.contains("assert(_ix0 < _s0.len)"), "unconstrained index checked: {c}");
    }

    #[test]
    fn lowers_volatile_fields_and_address_attr() {
        let src = "struct R { v: @volatile u32 } const BASE: *mut u32 = @address(0x4000_0000) \
                   fn main() -> i32 { var r = R{ v: 0 } return 0 }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("volatile uint32_t j_v;"), "volatile field qualifier: {c}");
        assert!(c.contains("((void*)(0x40000000))"), "fixed-address pointer: {c}");
    }

    #[test]
    fn lowers_slice_with_bounds_checked_index() {
        let src = "fn main() -> i32 { var p: *mut i32 = alloc_i32(2) var s: []i32 = slice(i32, p, 2) \
                       print_int(s[0]) print_int(s.len) free_ptr(p) return 0 }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(
            c.contains("typedef struct { int32_t* ptr; size_t len; } JestyrSlice_i32;"),
            "slice struct: {c}"
        );
        assert!(c.contains("(JestyrSlice_i32){ j_p, (size_t)(2) }"), "slice ctor: {c}");
        assert!(c.contains("assert(_ix0 < _s0.len)"), "bounds-checked index: {c}");
        assert!(c.contains("j_s.len"), "len accessor: {c}");
    }

    #[test]
    fn applies_layout_attributes_and_size_of() {
        let src = "@packed struct P { a: u8, b: i32 } @align(16) struct O { x: i32 } \
                   fn main() -> i32 { print_int(size_of(P)) print_int(size_of(O)) return 0 }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("struct __attribute__((packed)) Jestyr_P"), "packed: {c}");
        assert!(c.contains("struct __attribute__((aligned(16))) Jestyr_O"), "aligned: {c}");
        assert!(c.contains("sizeof(Jestyr_P)"), "size_of → sizeof: {c}");
    }

    /// `@layout(auto)` permutes the **declaration** and nothing else.
    ///
    /// The second half is the load-bearing one: construction stays a designated
    /// initializer in *source* order and every read stays by name, which is exactly why
    /// reordering the storage is safe. If cgen ever emitted a positional brace
    /// initializer, this assertion is what would catch it.
    #[test]
    fn layout_auto_reorders_the_declaration_only() {
        let src = "@layout(auto) struct T { a: u8, b: u64, c: i32 } \
                   fn main() -> i32 { let t = T { a: 1, b: 2, c: 3 } print_int(t.b as i64) return 0 }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        let body = c.split("struct Jestyr_T {").nth(1).expect("struct T emitted").split("};").next().unwrap();
        let order: Vec<&str> = ["j_a", "j_b", "j_c"]
            .iter()
            .map(|f| (f, body.find(f).unwrap_or(usize::MAX)))
            .filter(|(_, p)| *p != usize::MAX)
            .map(|(f, _)| *f)
            .collect();
        let mut by_pos = order.clone();
        by_pos.sort_by_key(|f| body.find(*f).unwrap());
        assert_eq!(by_pos, ["j_b", "j_c", "j_a"], "descending alignment: {body}");
        // Construction and access are by NAME, so neither moved.
        assert!(c.contains(".j_a = 1"), "designated initializer in source order: {c}");
        assert!(c.contains(".j_b = 2"), "designated initializer in source order: {c}");
        assert!(c.contains("j_t.j_b"), "field read unchanged: {c}");
    }

    /// The default is byte-identical. `@layout(c)` and no attribute at all must emit the
    /// same C as each other **and** as the compiler did before this feature existed —
    /// which is what lets the 140-file golden corpus, the concatenated build and the
    /// bootstrap seed stay untouched by an emission-changing increment.
    #[test]
    fn layout_c_is_byte_identical_to_no_attribute() {
        let base = "struct S { a: u8, b: u64, c: i32 } \
                    fn main() -> i32 { let s = S { a: 1, b: 2, c: 3 } print_int(s.b as i64) return 0 }";
        let (plain, _) = gen(base);
        let (explicit, _) = gen(&format!("@layout(c) {base}"));
        assert_eq!(plain, explicit, "`@layout(c)` must change nothing");
        // …and the annotated one really is different, so the comparison above is not
        // vacuously true because the attribute was dropped on the floor somewhere.
        let (auto, _) = gen(&format!("@layout(auto) {base}"));
        assert_ne!(plain, auto, "`@layout(auto)` must actually change the emission");
    }

    #[test]
    fn layout_reflection_align_of_and_offset_of() {
        let src = "struct M { a: u8, b: i32, c: u8 } \
                   fn main() -> i32 { print_int(align_of(M)) print_int(offset_of(M, b)) return 0 }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("_Alignof(Jestyr_M)"), "align_of → _Alignof: {c}");
        assert!(c.contains("offsetof(Jestyr_M, j_b)"), "offset_of → offsetof with j_ field: {c}");
    }

    /// **The two spellings emit different things, and that is the whole design.**
    ///
    /// `size_of(T)` is *C-deferred* — it lowers to `sizeof(Jestyr_T)` and the C compiler
    /// answers it. `@size_of(T)` is answered by **this** compiler from `layout.rs` and
    /// reaches the output as a literal. So the `@` forms can appear where a C expression
    /// cannot (a `const`, an array length), while every program written before them emits
    /// byte-identical C — which is what let this land without touching the 141-file
    /// corpus, the concat, the fixpoint or the seed.
    #[test]
    fn layout_queries_fold_while_the_bare_names_still_defer_to_c() {
        let src = "struct M { a: u8, b: i32, c: u8 } \
                   fn main() -> i32 { print_int(@size_of(M)) print_int(size_of(M)) \
                   print_int(@align_of(M)) print_int(@offset_of(M, b)) return 0 }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        // The `@` forms are literals: 12 bytes, align 4, `b` at offset 4.
        assert!(c.contains("jestyr_rt_print_int(12)"), "@size_of folds: {c}");
        assert!(c.contains("jestyr_rt_print_int(4)"), "@align_of / @offset_of fold: {c}");
        // …and the bare name is untouched.
        assert!(c.contains("sizeof(Jestyr_M)"), "bare size_of still defers to C: {c}");
        assert!(!c.contains("_Alignof"), "@align_of must not have deferred: {c}");
        assert!(!c.contains("offsetof("), "@offset_of must not have deferred: {c}");
    }

    /// `@abi(ref)` changes the **signature**, and only for the parameters it should.
    ///
    /// The three negative assertions carry as much weight as the positive one: a scalar
    /// and a small aggregate must stay by value (a pointer to them is strictly worse
    /// than the copy), and a function without the attribute must be untouched — which is
    /// what keeps every existing program byte-identical.
    #[test]
    fn abi_ref_passes_large_read_aggregates_by_const_pointer() {
        let src = "struct Big { a: i64, b: i64, c: i64, d: i64 } struct Small { x: i64 } \
                   @abi(ref) fn total(read v: Big, read s: Small, n: i64) -> i64 \
                   { return v.a + v.d + s.x + n } \
                   fn plain(read v: Big) -> i64 { return v.a } \
                   fn main() -> i32 { let b = Big { a: 1, b: 2, c: 3, d: 4 } \
                   let s = Small { x: 5 } print_int(total(b, s, 6)) print_int(plain(b)) return 0 }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        // 32 bytes → by `const*`; 8 bytes and a scalar → unchanged.
        assert!(
            c.contains("jestyr_total(const Jestyr_Big* j_v, Jestyr_Small j_s, int64_t j_n)"),
            "only the large aggregate goes by reference: {c}"
        );
        // The body dereferences through the existing `ptr_params` path.
        assert!(c.contains("(*j_v).j_a"), "field read through the pointer: {c}");
        // An lvalue argument takes its address — no copy, which is the entire point.
        assert!(c.contains("jestyr_total(&(j_b), j_s, 6)"), "lvalue arg by address: {c}");
        // A function that did not opt in is completely unchanged.
        assert!(c.contains("jestyr_plain(Jestyr_Big j_v)"), "non-users untouched: {c}");
        assert!(c.contains("jestyr_plain(j_b)"), "non-user call untouched: {c}");
    }

    /// An **rvalue** argument has no address, so `&(…)` would not compile and a GNU
    /// statement expression would hand back a dangling one. A compound literal of array
    /// type gives a `const T*` whose lifetime is the enclosing block.
    #[test]
    fn abi_ref_passes_a_temporary_through_a_compound_literal() {
        let src = "struct Big { a: i64, b: i64, c: i64, d: i64 } \
                   fn make() -> Big { return Big { a: 1, b: 2, c: 3, d: 4 } } \
                   @abi(ref) fn total(read v: Big) -> i64 { return v.a + v.d } \
                   fn main() -> i32 { print_int(total(make())) return 0 }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(
            c.contains("jestyr_total((const Jestyr_Big[1]){ jestyr_make() })"),
            "a temporary needs a compound literal, not `&`: {c}"
        );
        assert!(!c.contains("&(jestyr_make())"), "taking the address of an rvalue: {c}");
    }

    /// `@abi(value)` is the default said out loud, so it must emit exactly what no
    /// attribute at all emits — the property that lets the corpus stay byte-identical.
    #[test]
    fn abi_value_is_byte_identical_to_no_attribute() {
        let base = "struct Big { a: i64, b: i64, c: i64, d: i64 } \
                    fn total(read v: Big) -> i64 { return v.a } \
                    fn main() -> i32 { let b = Big { a: 1, b: 2, c: 3, d: 4 } print_int(total(b)) return 0 }";
        let (plain, _) = gen(base);
        let (explicit, _) = gen(&base.replace("fn total", "@abi(value) fn total"));
        assert_eq!(plain, explicit, "`@abi(value)` must change nothing");
        let (byref, _) = gen(&base.replace("fn total", "@abi(ref) fn total"));
        assert_ne!(plain, byref, "`@abi(ref)` must actually change the emission");
    }

    #[test]
    fn atomics_lower_to_gcc_atomic_builtins() {
        // Sequentially-consistent atomics over an `int64_t` cell — data-race-free
        // shared state, deterministic regardless of thread interleaving.
        let (c, d) = gen(
            "fn w(p: *mut i64) { atomic_add(p, 1) } \
             fn r(p: *mut i64) -> i64 { return atomic_load(p) }",
        );
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("__atomic_fetch_add((int64_t*)(j_p), (int64_t)(1), __ATOMIC_SEQ_CST)"), "{c}");
        assert!(c.contains("__atomic_load_n((int64_t*)(j_p), __ATOMIC_SEQ_CST)"), "{c}");
    }

    #[test]
    fn atomic_xchg_lowers_to_exchange_builtin() {
        // Test-and-set: `atomic_xchg(lock, 1)` stores 1 and returns the prior value
        // as one indivisible op — the single atom a spinlock (`std/sync.jtr`) needs.
        let (c, d) = gen("fn tas(p: *mut i64) -> i64 { return atomic_xchg(p, 1) }");
        assert!(d.is_empty(), "{:?}", d);
        assert!(
            c.contains("__atomic_exchange_n((int64_t*)(j_p), (int64_t)(1), __ATOMIC_SEQ_CST)"),
            "atomic_xchg must lower to __atomic_exchange_n: {c}"
        );
    }

    #[test]
    fn lowers_concurrent_spawn_to_pthreads() {
        let src = "fn w(p: *mut i32, i: i32) { unsafe { (p + i).* = i } } \
                   fn main() -> i32 { var b: *mut i32 = alloc_i32(2) \
                       concurrent { spawn w(b, 0) spawn w(b, 1) } free_ptr(b) return 0 }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("#include <pthread.h>"), "threads pull in pthread: {c}");
        assert!(c.contains("static void* jestyr_task_"), "a trampoline per spawn site: {c}");
        assert!(c.contains("pthread_create(&_jt0"), "spawns create threads: {c}");
        assert!(c.contains("pthread_join(_jt0"), "scope joins all tasks: {c}");
    }

    #[test]
    fn lowers_spawn_result_and_await_to_join_and_read() {
        // `let h = spawn f(x)` stores the result in the task box's `ret` field; the
        // trampoline writes it; `await h` joins-if-needed (guarded by a `_jd` flag so
        // the brace's safety-net join doesn't double-join) and reads `.ret`.
        let src = "fn sq(n: i64) -> i64 { return n * n } \
                   fn main() -> i32 { concurrent { let h = spawn sq(7) \
                       print_int(await h as i32) } return 0 }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("int64_t ret;"), "the task box carries a result field: {c}");
        assert!(c.contains("_a->ret = "), "the trampoline stores the result: {c}");
        assert!(c.contains("int _jd0 = 0;"), "an awaitable handle gets a joined-flag: {c}");
        assert!(
            c.contains("if (!_jd0) { pthread_join(_jt0, NULL); _jd0 = 1; }"),
            "await joins once: {c}"
        );
        assert!(c.contains("_ja0.ret"), "await reads the stored result: {c}");
        assert!(c.contains("if (!_jd0) pthread_join(_jt0, NULL);"), "brace-join is guarded: {c}");
    }

    #[test]
    fn lowers_dynamic_spawn_in_a_loop_to_a_growable_handle_array() {
        // A `spawn` inside a loop is dynamic-N: it pushes onto a growable `_dt`/`_da`
        // array (heap-allocated arg boxes for stable addresses), all joined + freed at
        // the brace — the worker count is a runtime value.
        let src = "fn w(p: *mut i64, i: i64) { unsafe { (p + (i as usize)).* = i } } \
                   fn main() -> i32 { var b: *mut i64 = alloc(i64, 4) \
                       concurrent { var k: i64 = 0 for k < 4 { spawn w(b, k) k = k + 1 } } \
                       free_ptr(b) return 0 }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("pthread_t* _dt = NULL"), "declares the growable handle array: {c}");
        assert!(c.contains("realloc(_dt"), "grows the array as tasks are spawned: {c}");
        assert!(c.contains("malloc(sizeof(struct _jsp_"), "heap-allocates each arg box: {c}");
        assert!(c.contains("pthread_create(&_dt[_dn]"), "each spawn pushes a thread: {c}");
        assert!(
            c.contains("for (size_t _dk = 0; _dk < _dn; _dk++) { pthread_join(_dt[_dk], NULL); free(_da[_dk]); }"),
            "the brace joins every dynamic task and frees its box: {c}"
        );
    }

    #[test]
    fn lowers_select_to_a_poll_loop() {
        // `select` hoists each channel, then spins: the first arm whose channel has a
        // value (via `channel_len_i64`) receives it (`channel_recv_i64`) and runs,
        // setting the done flag. (Single-source: the wrappers aren't defined here, but
        // the lowering shape is what we assert.)
        let src = "fn g(v: i64) {} fn f(c: i64) { select { recv(c) => x { g(x) } } }";
        let (c, _d) = gen(src);
        assert!(c.contains("int _seldone = 0;"), "a done flag: {c}");
        assert!(c.contains("while (!_seldone)"), "the wait loop: {c}");
        assert!(c.contains("jestyr_channel_len_i64("), "polls readiness via the i64 wrapper: {c}");
        assert!(c.contains("jestyr_channel_recv_i64("), "receives via the i64 wrapper: {c}");
        assert!(c.contains("_seldone = 1;"), "a fired arm ends the wait: {c}");
    }

    #[test]
    fn lowers_par_for_to_serial_map_plus_parallel_reduce() {
        // `par for x in s reduce(r) { x*x }` lowers to: a serial map of the body into a
        // scratch buffer, then a call to the deterministic engine `core.par_reduce`.
        let src = "fn sum_reduction() -> i64 { return 0 } \
                   fn main() -> i32 { var a: *mut i64 = alloc(i64, 4) let s: []i64 = slice(i64, a, 4) \
                       let r: i64 = par for x in s reduce(sum_reduction()) { x * x } return 0 }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("malloc("), "par for maps into a scratch buffer: {c}");
        assert!(c.contains("jestyr_par_reduce("), "par for reduces via the par_reduce engine: {c}");
        assert!(c.contains("(JestyrSlice_i64){ _pm"), "the mapped buffer is passed as a slice: {c}");
    }

    #[test]
    fn lowers_extern_c_to_bare_prototype_and_call() {
        let src = "extern \"c\" fn puts(s: cstr) -> i32 fn main() -> i32 { puts(\"hi\".cstr) return 0 }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("int32_t puts(const char* j_s);"), "extern prototype: {c}");
        assert!(c.contains("puts(JSTR(\"hi\").ptr)"), "bare call, str→cstr at the boundary: {c}");
        assert!(!c.contains("jestyr_puts"), "extern names are not mangled: {c}");
    }

    #[test]
    fn lowers_function_optimization_and_tooling_attributes() {
        // Each attribute lowers to a GNU declaration clause — a pure hint that
        // never changes what the function computes.
        let src = "@inline @hot fn sq(x: i32) -> i32 { return x * x } \
                   @cold fn slow(x: i32) -> i32 { return x } \
                   @must_use fn add(a: i32, b: i32) -> i32 { return a + b } \
                   @deprecated(\"use v2\") fn old(x: i32) -> i32 { return x }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(
            c.contains("static inline __attribute__((always_inline, hot)) int32_t jestyr_sq"),
            "inline+hot → static inline always_inline: {c}"
        );
        assert!(c.contains("__attribute__((cold)) int32_t jestyr_slow"), "cold: {c}");
        assert!(
            c.contains("__attribute__((warn_unused_result)) int32_t jestyr_add"),
            "must_use → warn_unused_result: {c}"
        );
        assert!(
            c.contains("__attribute__((deprecated(\"use v2\"))) int32_t jestyr_old"),
            "deprecated carries its message: {c}"
        );
    }

    #[test]
    fn no_mangle_emits_a_bare_symbol_and_calls_it_bare() {
        // `@no_mangle` exports the function under its bare C name (no `jestyr_`),
        // and every call site reaches it by that bare name too.
        let src = "@no_mangle fn entry(x: i32) -> i32 { return x * 2 } \
                   fn main() -> i32 { return entry(4) }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("int32_t entry(int32_t j_x)"), "bare definition: {c}");
        assert!(c.contains("return entry(4);"), "called by bare name: {c}");
        assert!(!c.contains("jestyr_entry"), "the export is not mangled: {c}");
    }

    #[test]
    fn section_attribute_places_a_function_in_a_named_section() {
        let (c, d) = gen("@section(\".boot\") fn reset() {}");
        assert!(d.is_empty(), "{:?}", d);
        assert!(
            c.contains("__attribute__((section(\".boot\"))) void jestyr_reset"),
            "section clause on the function: {c}"
        );
    }

    #[test]
    fn no_mangle_and_section_apply_to_a_const() {
        let (c, d) = gen(
            "@no_mangle const VERSION: i32 = 7 \
             @section(\".rodata.cfg\") const CFG: i32 = 3 \
             fn main() -> i32 { return VERSION + CFG }",
        );
        assert!(d.is_empty(), "{:?}", d);
        // `@no_mangle` → bare external global, referenced bare.
        assert!(c.contains("const int32_t VERSION = 7;"), "no_mangle const is bare+external: {c}");
        assert!(!c.contains("j_VERSION"), "no_mangle const reference is unmangled: {c}");
        // `@section` → the global carries the section attribute (still `j_`-named).
        assert!(
            c.contains("static const int32_t j_CFG __attribute__((section(\".rodata.cfg\"))) = 3;"),
            "section clause on the const: {c}"
        );
        assert!(c.contains("return (VERSION + j_CFG);"), "mixed references: {c}");
    }

    #[test]
    fn test_harness_runs_tests_and_times_benches() {
        let src = "@test fn t_ok() -> bool { return true } \
                   @bench fn b_work() { var s: i32 = 0 for i in 0..10 { s = s + i } }";
        let (c, d) = gen_tests(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("int main(void) {"), "harness emits its own main: {c}");
        assert!(c.contains("running 1 test(s)"), "test count: {c}");
        assert!(
            c.contains("if (jestyr_t_ok()) { printf(\"ok\\n\"); _passed++; }"),
            "runs the test and tallies: {c}"
        );
        assert!(c.contains("clock_t _s = clock(); jestyr_b_work();"), "times the bench: {c}");
        assert!(c.contains("return _failed == 0 ? 0 : 1;"), "exit reflects failures: {c}");
    }

    #[test]
    fn attributes_apply_to_a_generic_struct_method() {
        // Methods emit as free C functions through a separate path, so the
        // attribute prefix must follow them there too.
        let src = "fn List(comptime T: type) -> type { return struct { \
                       v: T, \
                       @inline fn val(read self) -> T { return self.v } \
                   } } \
                   fn mk(comptime T: type, x: T) -> List(T) { return List(T){ v: x } } \
                   fn main() -> i32 { var xs = mk(i32, 7) return xs.val() }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(
            c.contains("static inline __attribute__((always_inline)) int32_t jestyr_List__i32_val"),
            "an `@inline` method gets the same prefix as a free function: {c}"
        );
    }

    #[test]
    fn lowers_contracts_to_asserts() {
        // `requires` → assert on entry; `ensures` → spill `result`, assert before
        // each return (here the early `return 0 - x` *and* the tail `return x`).
        let src = "fn abs(x: i32) -> i32 ensures result >= 0 { if x < 0 { return 0 - x } return x } \
                   fn d(a: i32, b: i32) -> i32 requires b != 0 { return a / b }";
        let (c, dg) = gen(src);
        assert!(dg.is_empty(), "{:?}", dg);
        assert!(c.contains("assert((j_b != 0));"), "precondition asserted on entry: {c}");
        assert!(c.contains("int32_t j_result = (0 - j_x);"), "ensures spills `result`: {c}");
        assert!(c.contains("assert((j_result >= 0));"), "postcondition asserted: {c}");
    }

    #[test]
    fn emits_restrict_for_exclusive_borrows() {
        // A `mut` borrow is exclusive (non-aliasing) → `restrict`, giving the C
        // optimizer Rust-`noalias`-grade latitude.
        let src = "struct L { ptr: *mut i32, len: i32 } fn push(mut l: L, x: i32) { l.len = l.len + 1 }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(
            c.contains("jestyr_push(Jestyr_L* restrict j_l, int32_t j_x)"),
            "exclusive borrow → restrict pointer: {c}"
        );
    }

    #[test]
    fn lowers_alloc_intrinsics() {
        let (c, d) = gen("fn f() -> i32 { var p = alloc_i32(4) free_ptr(p) return 0 }");
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("malloc((size_t)(4) * sizeof(int32_t))"), "{c}");
        assert!(c.contains("free("), "{c}");
    }

    #[test]
    fn lowers_range_for_and_elides_the_bounds_check() {
        let src = "fn sum(xs: []i32) -> i32 { var t: i32 = 0 for i in 0..xs.len { t = t + xs[i] } return t }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("size_t _hi0 = j_xs.len;"), "bound snapshotted once: {c}");
        assert!(c.contains("for (size_t j_i = 0; j_i < _hi0; j_i++)"), "counted range loop: {c}");
        assert!(c.contains("(j_xs).ptr[(j_i)]"), "range index → raw access: {c}");
        assert!(!c.contains("assert(_ix"), "no bounds-check assert in the loop: {c}");
    }

    #[test]
    fn lowers_inclusive_range_without_elision() {
        // An inclusive index can equal len, so it must NOT elide.
        let src = "fn f(n: i32) -> i32 { var t: i32 = 0 for i in 0..=n { t = t + i } return t }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("j_i <= _hi0"), "inclusive uses `<=`: {c}");
    }

    #[test]
    fn lowers_a_cast_to_a_c_cast() {
        let (c, d) = gen("fn f(n: i32) -> usize { return n as usize }");
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("(size_t)(j_n)"), "cast → C cast: {c}");
    }

    #[test]
    fn lowers_pointer_cast() {
        let (c, d) = gen("fn f(p: *mut i32) -> *mut u8 { return p as *mut u8 }");
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("(uint8_t*)(j_p)"), "pointer cast: {c}");
    }

    #[test]
    fn region_string_allocates_in_the_arena() {
        let src = "fn f() -> i32 { var n: i32 = 0 region scratch { let g: str = region_concat(scratch, \"a\", \"b\") n = g.len as i32 } return n }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(
            c.contains("jestyr_arena_alloc(&j_scratch"),
            "region strings bump-allocate in the arena: {c}"
        );
    }

    #[test]
    fn bytes_exposes_a_strs_bytes_as_u8_slice() {
        let src = "fn f(read s: str) -> i32 { let b: []u8 = bytes(s) return b.len as i32 }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("(uint8_t*)"), "bytes views the str's bytes as u8: {c}");
    }

    #[test]
    fn fstring_builds_a_string_with_typed_interpolation() {
        let src = "fn f(read who: str) -> i32 { let n: i32 = 3 var m: String = f\"{who}: {n}\" let r: i32 = m.len as i32 string_free(m) return r }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("jestyr_rt_str_new()"), "an f-string builds a fresh String: {c}");
        assert!(c.contains("jestyr_rt_str_push_i64(&"), "an int interpolation formats as decimal: {c}");
    }

    #[test]
    fn builder_collects_fragments_and_flattens_once() {
        let src = "fn f() -> i32 { var b: Builder = builder_new() builder_push(b, \"x\") var s: String = builder_build(b) return s.len as i32 }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("JestyrBuilder j_b"), "Builder is the iolist type: {c}");
        assert!(c.contains("jestyr_rt_b_push(&"), "push stores a fragment view: {c}");
        assert!(c.contains("jestyr_rt_b_build(&"), "build flattens once: {c}");
    }

    #[test]
    fn owned_string_grows_and_views() {
        let src = "fn f() -> i32 { var s: String = string_from(\"hi\") string_push(s, \"!\") return s.len as i32 }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("JestyrString j_s"), "String is the owned heap type: {c}");
        assert!(c.contains("jestyr_rt_str_from("), "string_from copies into an owned buffer: {c}");
        assert!(c.contains("jestyr_rt_str_push(&"), "string_push takes the String by address: {c}");
        assert!(c.contains("j_s.len"), "String.len is an O(1) field: {c}");
    }

    #[test]
    fn string_slice_is_a_boundary_checked_view() {
        // `s[i..j]` (no annotation) types as `str`, so the `let` declares a
        // JestyrStr, and lowers to the bounds+boundary-checked substr helper.
        let src = "fn f(read s: str) -> i32 { let t = s[1..4] return t.len as i32 }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("JestyrStr j_t"), "a string slice types as str: {c}");
        assert!(c.contains("jestyr_rt_substr("), "via the boundary-checked helper: {c}");
    }

    #[test]
    fn named_substr_types_and_lowers() {
        let (c, d) = gen("fn f(read s: str) -> i32 { let t = substr(s, 1, 3) return t.len as i32 }");
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("JestyrStr j_t"), "substr(...) types as str: {c}");
        assert!(c.contains("jestyr_rt_substr("), "substr lowers to the helper: {c}");
    }

    #[test]
    fn split_grapheme_offset_iterators() {
        let src = "fn f(read s: str) -> i32 { var n: i32 = 0 \
            for p in split(s, \",\") { n = n + p.len as i32 } \
            for g in graphemes(s) { n = n + g.len as i32 } \
            for cp, off in codepoints(s) { n = n + off as i32 } \
            return (n + count_graphemes(s) as i32) }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("jestyr_rt_find(_rest"), "split scans with find: {c}");
        assert!(c.contains("jestyr_rt_is_combining("), "graphemes absorbs combining marks: {c}");
        assert!(c.contains("jestyr_rt_count_graphemes("), "count_graphemes: {c}");
        assert!(c.contains("size_t j_off = _k"), "(offset, codepoint) binds the byte offset: {c}");
    }

    #[test]
    fn string_operations_lower_to_helpers() {
        // The bare ops type via string_intrinsic_ret (no annotations needed):
        // eq/starts_with/contains → bool, find → isize, trim → str.
        let src = "fn f(read s: str) -> i32 { \
            let a = str_eq(s, \"x\") let b = starts_with(s, \"x\") let c = contains(s, \"x\") \
            let i = find(s, \"x\") let t = trim(s) return i as i32 }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("jestyr_rt_str_eq("), "str_eq: {c}");
        assert!(c.contains("jestyr_rt_find("), "find: {c}");
        assert!(c.contains("jestyr_rt_trim("), "trim: {c}");
        assert!(c.contains("JestyrStr j_t"), "trim yields a str view: {c}");
        assert!(c.contains("bool j_a"), "str_eq yields a bool: {c}");
    }

    #[test]
    fn cow_str_borrows_then_clones() {
        let src = "fn f(read s: str) -> i32 { var c: Cow = cow_borrow(s) let b = cow_is_owned(c) \
                   var o: Cow = cow_to_mut(c) let n = cow_view(o).len as i32 cow_free(o) return n }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("JestyrCow j_c"), "Cow-typed binding: {c}");
        assert!(c.contains("jestyr_rt_cow_borrow("), "borrow (no alloc): {c}");
        assert!(c.contains("jestyr_rt_cow_to_mut("), "copy-on-write point: {c}");
    }

    #[test]
    fn os_str_lossy_decode() {
        let src = "fn f(raw: []u8) -> i32 { let os: os_str = os_from_bytes(raw) var s: String = to_str_lossy(os) let n = s.len as i32 string_free(s) return n }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("JestyrStr j_os"), "os_str lowers to a view: {c}");
        assert!(c.contains("jestyr_rt_to_str_lossy("), "lossy decode to a proven String: {c}");
    }

    #[test]
    fn eq_fold_is_case_insensitive() {
        let (c, d) = gen("fn f() -> i32 { if eq_fold(\"Hi\", \"hi\") { return 1 } return 0 }");
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("jestyr_rt_eq_fold("), "eq_fold lowers to the fold compare: {c}");
    }

    #[test]
    fn try_from_utf8_returns_a_recoverable_result() {
        let src = "fn f(b: []u8) -> i32 { let r = try_from_utf8(b) if is_err(r) { return -1 } return unwrap(r).len as i32 }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("JestyrResult_str j_r"), "result-typed binding (no annotation): {c}");
        assert!(c.contains("(JestyrResult_str){ .is_err = false"), "ok construction: {c}");
        assert!(c.contains(".ok).len"), "unwrap(r).len projects the str length: {c}");
    }

    #[test]
    fn file_io_intrinsics_lower_to_runtime_calls() {
        // Exercises all four file-I/O intrinsics: read_file -> owned String,
        // write_file/file_exists/remove_file -> bool. No `and`/unused bindings (so
        // the program is diagnostic-clean) — each bool is consumed by an `if`.
        let src = "fn f() -> i32 { var s: String = read_file(\"p\") var n: i32 = s.len as i32 string_free(s) if write_file(\"p\", \"data\") { n = n + 1 } if file_exists(\"p\") { n = n + 1 } if remove_file(\"p\") { n = n + 1 } return n }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        // The runtime functions are defined in the prelude...
        assert!(c.contains("jestyr_rt_read_file(JestyrStr path)"), "read_file runtime defined: {c}");
        assert!(c.contains("jestyr_rt_write_file(JestyrStr path, JestyrStr data)"), "write_file runtime defined: {c}");
        // ...and each call site lowers to them; read_file yields an owned String.
        assert!(c.contains("JestyrString j_s = jestyr_rt_read_file("), "read_file binds an owned String: {c}");
        assert!(c.contains("jestyr_rt_write_file("), "write call lowered: {c}");
        assert!(c.contains("jestyr_rt_file_exists("), "exists call lowered: {c}");
        assert!(c.contains("jestyr_rt_remove_file("), "remove call lowered: {c}");
    }

    #[test]
    fn command_line_args_lower_to_runtime_accessors() {
        // arg_count() -> i32 and arg(i) -> str (a bounds-checked view of argv[i]).
        let src = "fn f() -> i32 { var n: i32 = arg_count() let p: str = arg(0) return n + (p.len as i32) }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("static int jestyr_rt_argc = 0;"), "argc global declared in the prelude: {c}");
        assert!(c.contains("jestyr_rt_arg_count()"), "arg_count lowers to the runtime accessor: {c}");
        assert!(c.contains("jestyr_rt_arg((int64_t)(0))"), "arg(i) lowers to the bounds-checked view: {c}");
    }

    #[test]
    fn from_utf8_validates_at_the_boundary() {
        let src = "fn f(b: []u8) -> i32 { let s: str = from_utf8(b) return s.len as i32 }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("jestyr_rt_valid_utf8("), "from_utf8 validates: {c}");
        assert!(c.contains("assert("), "validity is asserted at the boundary: {c}");
    }

    #[test]
    fn is_utf8_is_a_recoverable_check() {
        let (c, d) = gen("fn f(b: []u8) -> i32 { return is_utf8(b) as i32 }");
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("jestyr_rt_valid_utf8("), "is_utf8 → validity check: {c}");
    }

    #[test]
    fn count_codepoints_is_an_on_decode() {
        let (c, d) = gen("fn f(read s: str) -> i32 { return count_codepoints(s) as i32 }");
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("jestyr_rt_count_cp("), "count_codepoints → O(n) decode: {c}");
    }

    #[test]
    fn codepoints_for_decodes_utf8() {
        let src = "fn f(read s: str) -> i32 { var n: i32 = 0 for cp in codepoints(s) { n = n + 1 } return n }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("jestyr_rt_decode_cp("), "codepoint iteration decodes UTF-8: {c}");
        assert!(c.contains("uint32_t j_cp"), "each codepoint binds as a u32: {c}");
    }

    #[test]
    fn string_literal_is_a_length_carrying_view() {
        let (c, d) = gen("fn f() -> i32 { let s: str = \"hi\" return s.len as i32 }");
        assert!(d.is_empty(), "{:?}", d);
        assert!(
            c.contains("typedef struct { const char* ptr; size_t len; } JestyrStr;"),
            "the view type: {c}"
        );
        assert!(c.contains("JSTR(\"hi\")"), "a literal builds a view (length via sizeof): {c}");
    }

    #[test]
    fn cstr_is_the_distinct_ffi_type() {
        let (c, d) = gen("fn f(read s: str) -> cstr { return s.cstr }");
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("const char* jestyr_f(JestyrStr j_s)"), "str view in, cstr out: {c}");
        assert!(c.contains("j_s.ptr"), "`.cstr` bridges to the byte pointer: {c}");
    }

    #[test]
    fn lowers_string_iteration_over_the_view() {
        let src = "fn f(s: str) -> i32 { var t: i32 = 0 for c in s { t = t + (c as i32) } return t }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains(".len;"), "iterates to the view's length, not strlen: {c}");
        assert!(!c.contains("strlen("), "length is O(1) — no strlen: {c}");
        assert!(c.contains("uint8_t j_c = (uint8_t)"), "each byte binds as u8: {c}");
    }

    #[test]
    fn string_len_is_an_o1_field() {
        let (c, d) = gen("fn f(s: str) -> i32 { return s.len as i32 }");
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("j_s.len"), "str.len is an O(1) field read: {c}");
        assert!(!c.contains("strlen("), "no strlen: {c}");
    }

    #[test]
    fn lowers_element_plus_index_iteration() {
        let src = "fn f(xs: []i32) -> i32 { var s: i32 = 0 for x, i in xs { s = s + x } return s }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("size_t j_i = _k"), "index binding: {c}");
        assert!(c.contains("int32_t j_x = _s"), "element binding: {c}");
    }

    #[test]
    fn lowers_lockstep_zip_with_length_check() {
        let src = "fn f(xs: []i32, ys: []i32) -> i32 { var d: i32 = 0 for a, b in xs, ys { d = d + a * b } return d }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("assert(_z0_0.len == _z0_1.len)"), "lengths checked equal: {c}");
        assert!(c.contains("int32_t j_a = _z0_0"), "first zip binding: {c}");
        assert!(c.contains("int32_t j_b = _z0_1"), "second zip binding: {c}");
    }

    #[test]
    fn lowers_mut_slice_iteration_in_place() {
        let src = "fn f(mut xs: []i32) { for mut x in xs { x = x + 1 } }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("int32_t* j_x = &_s"), "mut element binds a pointer into the slice: {c}");
        assert!(c.contains("(*j_x) = ((*j_x) + 1)"), "writes through the pointer (in place): {c}");
    }

    #[test]
    fn lowers_conditional_infinite_and_break_continue() {
        let src = "fn f() { var k: i32 = 3 for k > 0 { k = k - 1 } for { continue break } }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("while ((j_k > 0))"), "conditional → while: {c}");
        assert!(c.contains("for (;;)"), "infinite → for(;;): {c}");
        assert!(c.contains("break;") && c.contains("continue;"), "break/continue: {c}");
    }

    #[test]
    fn lowers_loop_invariant_to_an_assert() {
        let src = "fn f(n: i32) { var t: i32 = 0 for i in 0..n { invariant t >= 0 t = t + i } }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("assert((j_t >= 0))"), "invariant → per-iteration assert: {c}");
    }

    #[test]
    fn no_panic_rejects_an_unprovable_index() {
        let src = "@no_panic fn at(xs: []i32, i: usize) -> i32 { return xs[i] }";
        let (_c, d) = gen(src);
        assert!(d.iter().any(|m| m.message.contains("@no_panic")), "should reject a faulting index: {:?}", d);
    }

    #[test]
    fn no_panic_allows_a_provably_in_range_loop_index() {
        let src = "@no_panic fn sum(xs: []i32) -> i32 { var t: i32 = 0 for i in 0..xs.len { t = t + xs[i] } return t }";
        let (_c, d) = gen(src);
        assert!(d.is_empty(), "an elided index is fault-free: {:?}", d);
    }

    #[test]
    fn lowers_labeled_break_and_continue_to_goto() {
        let src = "fn f(xs: []i32, ys: []i32) { for outer: i in 0..xs.len { for j in 0..ys.len { if xs[i] == 0 { break outer } continue outer } } }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("goto outer__break;"), "labeled break → goto: {c}");
        assert!(c.contains("outer__break: ;"), "break target after the loop: {c}");
        assert!(c.contains("goto outer__continue;"), "labeled continue → goto: {c}");
        assert!(c.contains("outer__continue: ;"), "continue target at body end: {c}");
    }

    #[test]
    fn lowers_loop_else_with_break_skipping_it() {
        // The `else` is emitted *after* the loop; a plain `break` becomes a
        // `goto …__break` whose target sits *after* the `else`, so it skips it.
        // Falling off the end of the slice runs the `else`.
        let src = "fn f(xs: []i32) -> i32 { var a: i32 = 0 \
                   for x in xs { if x == 0 { a = 1 break } } else { a = 0 - 1 } return a }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        // Plain break is rerouted to skip the else…
        assert!(c.contains("goto _fe0__break;"), "plain break skips the else via goto: {c}");
        // …and the else body precedes the break target.
        let els_at = c.find("j_a = (0 - 1)").expect("else body emitted");
        let tgt_at = c.find("_fe0__break: ;").expect("break target emitted");
        assert!(els_at < tgt_at, "else body comes before the break target: {c}");
    }

    #[test]
    fn lowers_labeled_loop_else_target_after_else() {
        // A *labeled* else-loop reuses the user's label for the skip target, and
        // that target still lands after the `else`.
        let src = "fn f(xs: []i32) -> i32 { var a: i32 = 0 \
                   for outer: x in xs { if x == 0 { break outer } } else { a = 9 } return a }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("goto outer__break;"), "labeled break → goto: {c}");
        let els_at = c.find("j_a = 9").expect("else body emitted");
        let tgt_at = c.find("outer__break: ;").expect("break target emitted");
        assert!(els_at < tgt_at, "labeled break target sits after the else: {c}");
    }

    #[test]
    fn loop_else_does_not_perturb_an_else_less_loop() {
        // No `else` ⇒ no synthetic label, and a plain `break` stays a C `break`.
        let (c, d) = gen("fn f() { for { break } }");
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("break;"), "else-less break is plain C break: {c}");
        assert!(!c.contains("__break"), "no break label synthesized: {c}");
    }

    #[test]
    fn lowers_step_and_descending_ranges() {
        let (c, d) = gen("fn f() { for i in 0..10 step 2 {} for j in 5..0 step -1 {} }");
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("j_i += (2)"), "ascending step: {c}");
        assert!(c.contains("int64_t j_j = 5"), "descending uses a signed index: {c}");
        assert!(c.contains("j_j > _hi"), "descending compares with `>`: {c}");
        assert!(c.contains("j_j += ((-1))"), "descending step: {c}");
    }

    #[test]
    fn lowers_variant_termination_measure() {
        let src = "fn f(n: i32) { var k: i32 = n for k > 0 { variant k k = k - 1 } }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("int64_t _vt"), "tracker hoisted before the loop: {c}");
        assert!(c.contains(">= 0)"), "bounded below: {c}");
        assert!(c.contains("< _vt"), "strictly decreasing each iteration: {c}");
    }

    #[test]
    fn lowers_region_scoped_loop_with_per_iteration_reset() {
        let src = "fn f() { for i in 0..3 region scratch { var p: &[scratch]i32 = region_alloc(scratch, i32, 1) unsafe { p.* = i } } }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert_eq!(c.matches("JestyrArena j_scratch = jestyr_arena_new").count(), 1, "arena opened once: {c}");
        assert!(c.contains("j_scratch.off = 0;"), "reset at top of each iteration: {c}");
        assert_eq!(c.matches("jestyr_arena_free(&j_scratch)").count(), 1, "freed once: {c}");
    }

    #[test]
    fn lowers_value_level_arena_allocator() {
        // The intrinsics that back the std arena allocator: `arena_open` heap-
        // allocates an arena handle, `arena_alloc` bump-allocates typed memory,
        // `arena_close` frees it in bulk.
        let src = "fn main() -> i32 { var h: *mut u8 = arena_open(64) \
                       var p: *mut i32 = arena_alloc(h, i32, 2) \
                       unsafe { (p + 0).* = 7 } arena_close(h) return 0 }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("typedef struct { char* buf; size_t off; size_t cap; } JestyrArena;"), "arena runtime: {c}");
        assert!(c.contains("(JestyrArena*)malloc(sizeof(JestyrArena))"), "arena_open: {c}");
        assert!(c.contains("(int32_t*) jestyr_arena_alloc((JestyrArena*)"), "typed bump: {c}");
        assert!(c.contains("jestyr_arena_free(_a"), "arena_close frees: {c}");
    }

    #[test]
    fn lowers_error_set_and_try_operator() {
        let src = "fn d(a: i32, b: i32) -> i32 !{ E } { if b == 0 { return err(E) } return ok(a) } \
                   fn c(a: i32, b: i32) -> i32 !{ E } { let x = d(a, b)? return ok(x) }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("typedef struct { bool is_err; int32_t ok; int err; } JestyrResult_i32;"), "{c}");
        assert!(c.contains(".is_err = true, .err = 1"), "err construction: {c}");
        assert!(c.contains("if (_q0.is_err) return"), "`?` early-return: {c}");
    }

    #[test]
    fn monomorphizes_a_generic_struct() {
        let src = "fn Box(comptime T: type) -> type { return struct { val: T } } \
                   fn mk(comptime T: type, x: T) -> Box(T) { return Box(T){ val: x } } \
                   fn main() -> i32 { var b = mk(i32, 7) return 0 }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("struct Jestyr_Box__i32"), "monomorphized struct: {c}");
        assert!(c.contains("int32_t j_val;"), "substituted field type: {c}");
        assert!(c.contains("(Jestyr_Box__i32){ .j_val ="), "generic struct literal: {c}");
    }

    #[test]
    fn monomorphizes_two_struct_instances_for_two_type_args() {
        let src = "fn Box(comptime T: type) -> type { return struct { val: T } } \
                   fn id(comptime T: type, b: Box(T)) -> Box(T) { return b } \
                   fn main() -> i32 { var a = id(i32, Box(i32){ val: 1 }) \
                                      var c = id(f64, Box(f64){ val: 2.0 }) return 0 }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("Jestyr_Box__i32"), "{c}");
        assert!(c.contains("Jestyr_Box__f64"), "{c}");
    }

    #[test]
    fn monomorphizes_one_instance_per_type_argument() {
        let src = "fn id(comptime T: type, x: T) -> T { x } \
                   fn main() -> i32 { print_int(id(i32, 1)) print_float(id(f64, 2.0)) return 0 }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("jestyr_id__i32"), "{c}");
        assert!(c.contains("jestyr_id__f64"), "{c}");
    }

    #[test]
    fn lowers_method_call_sugar_to_a_free_call() {
        // `xs.push(7)` resolves to `push`, recovering the type arg `i32` from the
        // receiver `xs : List(i32)`, and passes `&xs` for the `mut` receiver.
        let src = "fn List(comptime T: type) -> type { return struct { ptr: *mut T, len: i32 } } \
                   fn push(comptime T: type, mut l: List(T), x: T) { l.len = l.len + 1 } \
                   fn main() -> i32 { var xs = List(i32){ ptr: alloc(i32, 1), len: 0 } xs.push(7) return 0 }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("jestyr_push__i32(&(j_xs), 7)"), "method sugar → free call: {c}");
    }

    #[test]
    fn lowers_method_call_on_a_plain_struct() {
        // A non-generic receiver: `c.inc()` → `jestyr_inc(&c)` (no type args).
        let src = "struct Counter { n: i32 } \
                   fn inc(mut c: Counter) { c.n = c.n + 1 } \
                   fn main() -> i32 { var c = Counter{ n: 0 } c.inc() return c.n }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("jestyr_inc(&(j_c))"), "non-generic method: {c}");
    }

    #[test]
    fn monomorphizes_a_generic_struct_method() {
        // `get` is defined *inside* the struct `List(T)` returns; calling
        // `xs.get(0)` monomorphizes `jestyr_List__i32_get` with `self` by value.
        let src = "fn List(comptime T: type) -> type { return struct { ptr: *mut T, len: i32, \
                       fn get(read self, i: i32) -> T { unsafe { (self.ptr + i).* } } } } \
                   fn new(comptime T: type) -> List(T) { return List(T){ ptr: alloc(T, 1), len: 0 } } \
                   fn main() -> i32 { var xs = new(i32) return xs.get(0) }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(
            c.contains("int32_t jestyr_List__i32_get(Jestyr_List__i32 j_self, int32_t j_i)"),
            "monomorphized method with `self` by value: {c}"
        );
        assert!(c.contains("jestyr_List__i32_get(j_xs, 0)"), "call site: {c}");
    }

    #[test]
    fn lowers_mut_self_method_by_pointer() {
        // A method on a plain (non-generic) struct, with `mut self` by pointer.
        let src = "struct Counter { n: i32, fn inc(mut self) { self.n = self.n + 1 } } \
                   fn main() -> i32 { var c = Counter{ n: 0 } c.inc() return c.n }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("void jestyr_Counter_inc(Jestyr_Counter* restrict j_self)"), "mut self → restrict pointer: {c}");
        assert!(c.contains("(*j_self).j_n = ((*j_self).j_n + 1)"), "in-place mutation: {c}");
        assert!(c.contains("jestyr_Counter_inc(&(j_c))"), "call passes &c: {c}");
    }

    #[test]
    fn emits_a_method_inside_a_generic_struct() {
        // Methods defined *inside* the struct that `List(T)` returns become
        // `jestyr_List__i32_push` / `_get`; `mut self` lowers to a pointer.
        let src = "fn List(comptime T: type) -> type { return struct { ptr: *mut T, len: i32, \
                       fn push(mut self, x: T) { self.len = self.len + 1 } \
                       fn get(read self, i: i32) -> T { unsafe { (self.ptr + i).* } } } } \
                   fn new(comptime T: type) -> List(T) { return List(T){ ptr: alloc(T, 4), len: 0 } } \
                   fn main() -> i32 { var xs = new(i32) xs.push(7) return xs.get(0) }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(
            c.contains("void jestyr_List__i32_push(Jestyr_List__i32* restrict j_self, int32_t j_x)"),
            "`mut self` by restrict pointer: {c}"
        );
        assert!(
            c.contains("int32_t jestyr_List__i32_get(Jestyr_List__i32 j_self, int32_t j_i)"),
            "`read self` by value: {c}"
        );
        assert!(c.contains("jestyr_List__i32_push(&(j_xs), 7)"), "method-call site: {c}");
    }

    #[test]
    fn lowers_and_invokes_a_capturing_closure() {
        let src = "fn main() -> i32 { let b = 100 let add = |x: i32| x + b print_int(add(5)) return 0 }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("JestyrEnv_"), "an environment struct is emitted: {c}");
        assert!(c.contains("static int32_t jestyr_lam_"), "a lifted function is emitted: {c}");
        assert!(c.contains("j__env->j_b"), "the capture is read from the env: {c}");
        assert!(c.contains(".call(&"), "invocation lowers to a fn-ptr call: {c}");
    }

    #[test]
    fn lowers_a_non_capturing_closure_with_empty_env() {
        let src = "fn main() -> i32 { let t = |y: i32| y + y return t(21) }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("char _unused;"), "empty env gets a placeholder field: {c}");
        assert!(c.contains(".env = {0}"), "empty env is zero-initialized: {c}");
    }

    #[test]
    fn invokes_an_inline_closure_via_a_spill() {
        let src = "fn main() -> i32 { return (|x: i32| x + 1)(41) }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("JestyrClosure_"), "a closure struct is emitted: {c}");
        assert!(c.contains(".call(&"), "the inline closure is invoked: {c}");
    }

    #[test]
    fn lowers_enum_to_a_tagged_union() {
        let (c, d) = gen("enum E { a(x: i32), b }");
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("enum Jestyr_E_tag {"), "{c}");
        assert!(c.contains("Jestyr_E_a,"), "{c}");
        assert!(c.contains("struct { int32_t j_x; } a;"), "{c}");
        assert!(c.contains("} u;"), "{c}");
    }

    #[test]
    fn constructs_variants_with_and_without_payload() {
        let (c, d) = gen("enum E { a(x: i32), b } fn mk() -> E { a(7) } fn nul() -> E { b }");
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("(Jestyr_E){ .tag = Jestyr_E_a, .u.a = { 7 } }"), "{c}");
        assert!(c.contains("(Jestyr_E){ .tag = Jestyr_E_b }"), "{c}");
    }

    #[test]
    fn lowers_match_to_a_switch_with_payload_binding() {
        let src = "enum E { a(x: i32), b } fn f(read e: E) -> i32 { match e { a(v) => v, b => 0 } }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("switch ("), "{c}");
        assert!(c.contains("case Jestyr_E_a:"), "{c}");
        assert!(c.contains("int32_t j_v = "), "{c}"); // payload bound from the union
        assert!(c.contains("case Jestyr_E_b:"), "{c}");
        assert!(c.contains("__builtin_unreachable();"), "exhaustive match guard: {c}");
    }

    #[test]
    fn guarded_match_lowers_to_an_ordered_if_chain() {
        // Two arms share `a`, differing only by guard — impossible in a C `switch`,
        // so the match becomes an ordered if-chain on the tag.
        let src = "enum E { a(x: i32), b } \
                   fn f(read e: E) -> i32 { match e { a(v) if v > 0 => v, a(v) => 0 - v, b => 0 } }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(!c.contains("switch ("), "a guarded match uses an if-chain, not a switch: {c}");
        assert!(c.contains(".tag == Jestyr_E_a)"), "tag test per arm: {c}");
        assert!(c.contains("int32_t j_v = jm_0.u.a.j_x;"), "payload bound before the guard: {c}");
        assert!(c.contains("if ((j_v > 0))"), "the guard gates the arm: {c}");
        // Exhaustive via the unguarded `a(v)`/`b` arms → unreachable tail.
        assert!(c.contains("__builtin_unreachable();"), "{c}");
    }

    #[test]
    fn guarded_match_in_statement_position_jumps_to_an_end_label() {
        // In statement position (not the function tail) a fired arm `goto`s a shared
        // end label rather than returning.
        let src = "enum E { a(x: i32), b } \
                   fn f(read e: E) -> i32 { \
                       match e { a(v) if v > 0 => print_int(v), a(v) => print_int(0), b => print_int(1) } \
                       return 0 }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("goto jm_end_"), "a fired arm jumps to the end label: {c}");
        assert!(c.contains("jm_end_"), "the end label is emitted: {c}");
        // No unreachable in statement position — control falls past the label.
        assert!(!c.contains("__builtin_unreachable();"), "{c}");
    }

    #[test]
    fn guarded_niche_match_uses_an_ordered_null_test() {
        // A guarded niche match keeps the pointer representation but switches from
        // the simple two-way null branch to an ordered if-chain on the null test.
        let src = "enum Maybe { none, some(p: *mut i32) } \
                   fn get(m: Maybe, flag: i32) -> i32 { \
                       match m { some(p) if flag > 0 => 1, some(p) => 2, none => 0 - 1 } }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(!c.contains("switch ("), "niche match has no tag switch: {c}");
        assert!(c.contains("!= ((int32_t*)0)"), "`some` tested by non-null: {c}");
        assert!(c.contains("== ((int32_t*)0)"), "`none` tested by null: {c}");
        assert!(c.contains("if ((j_flag > 0))"), "the guard gates the `some` arm: {c}");
    }

    #[test]
    fn nested_variant_pattern_dispatches_via_deref() {
        // A nested variant pattern looks *through* the `indirect` (pointer) field.
        let src = "enum Tree { leaf, node(l: indirect Tree, r: indirect Tree) } \
                   fn f(read t: Tree) -> i32 { match t { leaf => 0, node(leaf, leaf) => 1, node(_, _) => 2 } }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "nested patterns now compile: {:?}", d);
        assert!(!c.contains("switch ("), "a nested match is an if-chain, not a switch: {c}");
        assert!(
            c.contains(".u.node.j_l).tag == Jestyr_Tree_leaf"),
            "left child is deref'd and tag-tested: {c}"
        );
        assert!(
            c.contains(".u.node.j_r).tag == Jestyr_Tree_leaf"),
            "right child too: {c}"
        );
    }

    #[test]
    fn bit_field_lowers_to_a_c_bit_field() {
        let src = "struct F { a: u8 : 1, mode: u8 : 3 } fn get(read f: F) -> i32 { return f.mode }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("uint8_t j_a : 1"), "1-bit field: {c}");
        assert!(c.contains("uint8_t j_mode : 3"), "3-bit field: {c}");
    }

    #[test]
    fn union_emits_a_c_union() {
        let src = "union U { a: i32, b: f32 } fn first(read u: U) -> i32 { return u.a }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("union Jestyr_U"), "emits a C union: {c}");
        assert!(c.contains("typedef union Jestyr_U"), "forward-declared as a union: {c}");
        assert!(!c.contains("struct Jestyr_U"), "a union is not a struct: {c}");
    }

    #[test]
    fn field_default_fills_omitted_fields() {
        let src = "struct C { x: i32 = 3, y: i32 = 7 } fn mk() -> C { C { y: 9 } }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains(".j_y = 9"), "explicit field stays: {c}");
        assert!(c.contains(".j_x = 3"), "omitted field filled from its default: {c}");
    }

    #[test]
    fn struct_spread_lowers_to_a_copy_and_override() {
        let src = "struct P { x: i32, y: i32 } fn f(read p: P) -> P { P { x: 9, ..p } }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("Jestyr_P jss_"), "copies the base into a temp: {c}");
        assert!(c.contains(".j_x = 9;"), "overrides the listed field: {c}");
    }

    #[test]
    fn struct_variant_construction_and_match() {
        let src = "enum S { circle(r: f64), dot } \
                   fn mk() -> S { circle { r: 2.0 } } \
                   fn area(read s: S) -> f64 { match s { circle { r } => r, dot => 0.0 } }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        // Named construction → a designated tagged-union initializer.
        assert!(
            c.contains(".tag = Jestyr_S_circle, .u.circle = { .j_r = "),
            "named construction: {c}"
        );
        // Named match → field projection by name.
        assert!(c.contains("jm_0.tag == Jestyr_S_circle"), "tag test: {c}");
        assert!(c.contains("double j_r = jm_0.u.circle.j_r;"), "named field binding: {c}");
    }

    #[test]
    fn nested_literal_pattern_tests_the_field() {
        // A non-pointer field needs no deref — the literal tests the value directly.
        let src = "enum E { v(x: i32), w } fn f(read e: E) -> i32 { match e { v(0) => 1, _ => 0 } }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains(".u.v.j_x == (0)"), "nested literal tests the field value: {c}");
    }

    #[test]
    fn flat_variant_match_still_uses_a_switch() {
        // Regression: a flat `node(l, r)` binding match keeps the optimized switch.
        let src = "enum Tree { leaf(v: i32), node(l: indirect Tree, r: indirect Tree) } \
                   fn f(read t: Tree) -> i32 { match t { leaf(v) => v, node(l, r) => 0 } }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("switch ("), "a flat match keeps its switch lowering: {c}");
    }

    #[test]
    fn rest_pattern_binds_only_named_fields() {
        let src = "enum E { c(x: i32, y: i32, z: i32) } \
                   fn f(read e: E) -> i32 { match e { c(x, ..) => x } }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("int32_t j_x = "), "binds the named field: {c}");
        assert!(!c.contains("j_y ="), "ignores the rest — no binding for y: {c}");
        assert!(!c.contains("j_z ="), "ignores the rest — no binding for z: {c}");
    }

    #[test]
    fn enum_or_pattern_stacks_case_labels() {
        let src = "enum C { red, green, blue, black } \
                   fn f(read c: C) -> i32 { match c { red | green | blue => 1, black => 0 } }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("switch ("), "an unguarded enum or-pattern keeps the switch: {c}");
        assert!(c.contains("case Jestyr_C_red:"), "{c}");
        assert!(c.contains("case Jestyr_C_green:"), "{c}");
        assert!(c.contains("case Jestyr_C_blue:"), "stacked case labels share one body: {c}");
    }

    #[test]
    fn scalar_or_pattern_ors_the_value_tests() {
        let src = "fn f(read n: i32) -> i32 { match n { 0 | 1 | 2 => 7, 10..=19 | 30..=39 => 8, _ => 0 } }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("(jm_0 == (0)) || (jm_0 == (1)) || (jm_0 == (2))"), "literal alts ORed: {c}");
        assert!(
            c.contains("(jm_0 >= (10) && jm_0 <= (19)) || (jm_0 >= (30) && jm_0 <= (39))"),
            "range alts ORed: {c}"
        );
    }

    #[test]
    fn scalar_match_lowers_to_a_value_if_chain() {
        let src = "fn f(read n: i32) -> i32 { match n { 0 => 0, 1..=9 => 1, 100..1000 => 2, _ => 9 } }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(!c.contains("switch ("), "a scalar match is an if-chain, not a switch: {c}");
        assert!(c.contains("== (0)"), "literal arm tests equality: {c}");
        assert!(c.contains(">= (1) && ") && c.contains("<= (9)"), "inclusive range bounds: {c}");
        assert!(c.contains("< (1000)"), "half-open upper bound is exclusive: {c}");
    }

    #[test]
    fn char_range_pattern_lowers_to_a_comparison() {
        // Char-literal bounds are integer comparisons in C.
        let src = "fn d(read c: u8) -> i32 { match c { '0'..='9' => 1, _ => 0 } }";
        let (c, diags) = gen(src);
        assert!(diags.is_empty(), "{:?}", diags);
        assert!(c.contains(">= ('0') && ") && c.contains("<= ('9')"), "char-range comparison: {c}");
    }

    #[test]
    fn scalar_match_guard_composes_with_a_binding_catch_all() {
        let src = "fn f(read n: i32) -> i32 { match n { 0 => 0, m if m < 0 => 0 - 1, m => 1 } }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("== (0)"), "literal arm: {c}");
        assert!(c.contains("int32_t j_m = "), "binding catch-all names the value: {c}");
        assert!(c.contains("if ((j_m < 0))"), "the guard gates the binding arm: {c}");
    }

    #[test]
    fn niche_optimizes_an_optional_pointer_to_a_bare_pointer() {
        // `none`/`some(*T)` collapses to the pointer itself — no tag, no struct.
        let src = "enum Maybe { none, some(p: *mut i32) } \
                   fn sz() -> i32 { size_of(Maybe) } \
                   fn mk(p: *mut i32) -> Maybe { some(p) } \
                   fn empty() -> Maybe { none } \
                   fn get(m: Maybe) -> i32 { match m { some(p) => unsafe { p.* }, none => 0 } }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        // No tagged-union struct or tag enum is emitted for the niche enum.
        assert!(!c.contains("Jestyr_Maybe"), "niche enum has no struct/tag: {c}");
        // It IS a pointer: size_of and the signature both show `int32_t*`.
        assert!(c.contains("sizeof(int32_t*)"), "size_of is the pointer size: {c}");
        assert!(c.contains("int32_t jestyr_get(int32_t* j_m"), "param is a bare pointer: {c}");
        // Construction: `some(p)` is the pointer; `none` is the null pointer.
        assert!(c.contains("return j_p;"), "some(p) lowers to the pointer: {c}");
        assert!(c.contains("return ((int32_t*)0);"), "none lowers to NULL: {c}");
        // Match lowers to a null test, not a tag switch.
        assert!(c.contains("!= ((int32_t*)0)"), "match dispatches on NULL: {c}");
        assert!(!c.contains("switch ("), "no tag switch for a niche enum: {c}");
    }

    #[test]
    fn generic_enum_template_is_not_emitted_until_instantiated() {
        // A generic enum is a template — declaring one (unused) emits no struct.
        let (c, d) =
            gen("enum Option(T) { none, some(x: T) } fn main() -> i32 { return 0 }");
        assert!(d.is_empty(), "declared-but-unused generic enum is clean: {:?}", d);
        assert!(!c.contains("Jestyr_Option"), "no template struct emitted when unused: {c}");
    }

    #[test]
    fn monomorphizes_a_generic_enum_instance_with_construction_and_match() {
        let src = "enum Option(T) { none, some(v: T) } \
                   fn get(o: Option(i32), d: i32) -> i32 { match o { some(v) => v, none => d } } \
                   fn main() -> i32 { var a: Option(i32) = some(7) var b: Option(i32) = none \
                                      return get(a, 0) + get(b, 1) }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        // The i32 instance is a tagged union with a mangled name.
        assert!(c.contains("struct Jestyr_Option__i32 {"), "instance struct: {c}");
        assert!(c.contains("Jestyr_Option__i32 j_o"), "param uses the instance: {c}");
        // Construction names the instance's tag/union.
        assert!(
            c.contains(".tag = Jestyr_Option__i32_some, .u.some = { 7 }"),
            "some(7) constructs the instance: {c}"
        );
        assert!(c.contains(".tag = Jestyr_Option__i32_none"), "none constructs the instance: {c}");
        // Match switches on the instance's tag and binds the substituted payload.
        assert!(c.contains("case Jestyr_Option__i32_some:"), "match case: {c}");
        assert!(c.contains("int32_t j_v = "), "payload bound at concrete type: {c}");
    }

    #[test]
    fn monomorphizes_a_generic_enum_inside_a_generic_function() {
        // A generic combinator that *constructs* and *matches* a generic enum:
        // inside `wrap`/`unwrap_or` the inferred type is `Option(U)`/`Option(T)`
        // with the parameter still opaque; the active monomorphization substitution
        // resolves it to the concrete instance for construction, the match tag
        // prefix, and the payload binding. The nullary `none` in `return`/tail
        // position inherits the function's return type. (Regression: previously
        // these emitted a "cannot infer the type arguments" diagnostic or named the
        // opaque `Option__U`/`Option__T`.)
        let src = "enum Option(T) { none, some(v: T) } \
                   fn wrap(comptime U: type, take v: U) -> Option(U) { return some(v) } \
                   fn unwrap_or(comptime T: type, take o: Option(T), take d: T) -> T { match o { some(v) => v, none => d } } \
                   fn main() -> i32 { return unwrap_or(i32, wrap(i32, 42), 0) }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "no 'cannot infer' diagnostic inside the generic fn: {:?}", d);
        assert!(c.contains(".tag = Jestyr_Option__i32_some"), "generic construction substituted: {c}");
        assert!(c.contains("case Jestyr_Option__i32_some:"), "generic match substituted: {c}");
        assert!(!c.contains("Option__U"), "the opaque param U was substituted away: {c}");
        assert!(!c.contains("Option__T_"), "the opaque param T was substituted away: {c}");
    }

    #[test]
    fn collects_fn_pointer_typedef_through_a_generic_signature() {
        // A higher-order generic combinator: `opt_map`'s `f: fn(T) -> U` parameter
        // must contribute the *concrete* fn-pointer typedef (`fn(i32) -> i32`) per
        // instance, or its monomorphized signature would reference an un-emitted
        // typedef. (Regression: fn-pointer typedefs were collected from struct
        // fields but not from generic function signatures.)
        let src = "enum Option(T) { none, some(v: T) } \
                   fn opt_map(comptime T: type, comptime U: type, take o: Option(T), f: fn(T) -> U) -> Option(U) { match o { some(v) => some(f(v)), none => none } } \
                   fn inc(x: i32) -> i32 { return x + 1 } \
                   fn main() -> i32 { var a: Option(i32) = some(41) var b = opt_map(i32, i32, a, &inc) return 0 }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("typedef int32_t (*JestyrFn_fn_di32_ret_i32)(int32_t);"), "fn-ptr typedef: {c}");
        assert!(
            c.contains("jestyr_opt_map__i32_i32(Jestyr_Option__i32 j_o, JestyrFn_fn_di32_ret_i32 j_f)"),
            "instance signature references the typedef: {c}"
        );
    }

    #[test]
    fn fn_pointer_returning_a_generic_enum_is_forward_declared_first() {
        // The monadic combinator shape: `f: fn(T) -> Option(U)` is a fn-pointer
        // returning a generic enum *by value*. Its typedef must follow a forward
        // declaration of the `Option(i32)` instance — emitted by `gen_forward_types`
        // before `fn_type_typedefs` — or the C is a forward-reference error. (Teeth:
        // moving the instance forward-typedefs back into `gen_enum_defs` reorders
        // them after the fn-pointer typedef and breaks this ordering.)
        let src = "enum Option(T) { none, some(v: T) } \
                   fn opt_and_then(comptime T: type, comptime U: type, take o: Option(T), f: fn(T) -> Option(U)) -> Option(U) { match o { some(v) => f(v), none => none } } \
                   fn step(x: i32) -> Option(i32) { return some(x + 1) } \
                   fn main() -> i32 { var a: Option(i32) = some(41) var b = opt_and_then(i32, i32, a, &step) return 0 }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        let fwd = c.find("typedef struct Jestyr_Option__i32 Jestyr_Option__i32;")
            .expect(&format!("forward typedef for the instance: {c}"));
        let fnp = c.find("(*JestyrFn_fn_di32_ret_Option__i32)")
            .expect(&format!("fn-pointer typedef returning the instance: {c}"));
        assert!(fwd < fnp, "the instance must be forward-declared before the fn-pointer typedef: {c}");
    }

    #[test]
    fn slice_index_assignment_lowers_to_an_lvalue() {
        // `buf[i] = v` must lower to a bounds-checked *lvalue* assignment through the
        // element pointer (`_s.ptr[_ix] = v`), not the rvalue statement-expression
        // `emit_expr` produces for an `Index` read (which is not assignable). This is
        // what lets the integer formatter write digits into a caller `[]u8`.
        let (c, d) = gen("fn setb(mut s: []u8, v: u8) { s[0] = v }");
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains(".ptr[_ix0] = j_v"), "lvalue element assignment: {c}");
        assert!(c.contains("assert(_ix0 < "), "with a bounds check: {c}");
    }

    #[test]
    fn a_field_through_an_array_index_assigns_through_the_element_address() {
        // `xs[i].f = v` — the target is a place reached *through* a checked index.
        // `emit_expr` lowers that index to a statement expression yielding a *value*,
        // so the old emission was `({ …; _a->a[_ix]; }).j_b = 9` and gcc reported
        // "lvalue required as left operand of assignment". The place form yields the
        // element's ADDRESS and derefs it, and the spilled pointer must not be
        // `const` — this is a write.
        let src = "struct T { a: u8, b: u64 } fn set() { var xs: [3]T = [T { a: 1, b: 2 }; 3] xs[1].b = 9 }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("&_a1->a[_ix1]; })).j_b = 9"), "field assigned through the element address: {c}");
        assert!(c.contains("assert(_ix1 < 3)"), "with the constant-length bounds check: {c}");
        assert!(!c.contains("const JestyrArr_T_3* _a1"), "the write path takes a non-const pointer: {c}");
    }

    #[test]
    fn a_field_through_a_slice_index_assigns_through_the_element_address() {
        // The same defect on the slice side: only a refinement-*proved* index emitted
        // an lvalue (`(s).ptr[(i)]`), so an unproved `s[i].f = v` needed the address
        // form. The `{ptr,len}` view is still spilled to a temp (so a side-effecting
        // base is evaluated once) — the address taken points into the buffer, not
        // into the copy, which is why it outlives the statement expression.
        let src = "struct T { a: u8, b: u64 } fn set(mut s: []T) { s[1].b = 9 }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("&_s0.ptr[_ix0]; })).j_b = 9"), "field assigned through the element address: {c}");
        assert!(c.contains("assert(_ix0 < _s0.len)"), "with the slice bounds check: {c}");
    }

    #[test]
    fn a_nested_array_index_is_a_place_on_both_the_read_and_the_write_path() {
        // `m[i][j]` — the *base* of the outer index is itself a checked index, and an
        // array index takes `&base`. So this shape was broken in BOTH directions:
        // `&({ … })` is "lvalue required as unary '&' operand", which means the read
        // failed to compile as well as the write. Emitting the base as a place fixes
        // both, and the read keeps its `const` qualifier while the write drops it.
        let src = "fn m2() -> i64 { var m: [2][3]i64 = [[0; 3]; 2] m[0][1] = 5 return m[0][1] }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("&_a3->a[_ix3]; })) = 5"), "the write assigns through the element address: {c}");
        assert!(c.contains("JestyrArr_i64_3* _a3 = &((*({ JestyrArr_arr_i64_3_2* _a2"), "the write's base is a non-const place: {c}");
        assert!(c.contains("const JestyrArr_i64_3* _a5 = &((*({ const JestyrArr_arr_i64_3_2* _a4"), "the read's base is a const place: {c}");
    }

    #[test]
    fn a_mut_receiver_through_an_index_takes_the_elements_own_address() {
        // `cs[i].bump()` on a `mut self` method is the SAME defect as `xs[i].f = v`
        // at a different call site: a `mut`/`out` receiver is passed by address, and
        // `&({ …; _a->a[_ix]; })` is "lvalue required as unary '&' operand". Routing
        // the receiver through `emit_place` hands over the element's own address, so
        // the method mutates the array rather than a temporary.
        let src = "struct C { n: i64, fn bump(mut self) { self.n = self.n + 1 } } \
                   fn go() { var cs: [2]C = [C { n: 0 }; 2] cs[1].bump() }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("&((*({ JestyrArr_C_2* _a1 = &(j_cs)"), "receiver is the element's address: {c}");
        assert!(c.contains("&_a1->a[_ix1]; })))"), "through the place form, non-const: {c}");
    }

    #[test]
    fn a_mut_argument_through_an_index_takes_the_elements_own_address() {
        // The argument half of the same rule: a `mut` parameter of a plain function.
        let src = "struct C { n: i64 } fn inc(mut c: C) { c.n = c.n + 1 } \
                   fn go() { var cs: [2]C = [C { n: 0 }; 2] inc(cs[1]) }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("jestyr_inc(&((*({ JestyrArr_C_2* _a1"), "argument is the element's address: {c}");
        assert!(c.contains("&_a1->a[_ix1]; })))"), "through the place form: {c}");
    }

    #[test]
    fn a_read_receiver_and_a_by_value_argument_are_unchanged() {
        // Only `mut`/`out` conveyances take an address, so a `read` receiver and a
        // by-value argument still emit the statement-expression *value* exactly as
        // before — which is what keeps every existing program byte-identical.
        let src = "struct C { n: i64, fn get(read self) -> i64 { return self.n } } \
                   fn by_value(c: C) -> i64 { return c.n } \
                   fn go() -> i64 { var cs: [2]C = [C { n: 0 }; 2] return cs[1].get() + by_value(cs[0]) }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(!c.contains("&_a"), "no address form for read/by-value conveyances: {c}");
        assert!(c.contains("_a1->a[_ix1]; })"), "the value form is unchanged: {c}");
    }

    #[test]
    fn a_directly_indexed_assignment_target_keeps_its_existing_lowering() {
        // The place form is deliberately NOT used for a target that is *itself* an
        // index: `xs[i] = v` and `s[i] = v` have had their own lvalue lowering since
        // the beginning, and reusing it is what keeps the emitted C of every existing
        // program byte-identical (140 corpus files, the concat, the seed and every
        // attested hash all key on this text).
        let (c, d) = gen("fn set() { var xs: [4]i64 = [0; 4] xs[2] = 11 }");
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("_a1->a[_ix1] = 11; })"), "the statement-expression form is unchanged: {c}");
        assert!(!c.contains("&_a1->a[_ix1]"), "no address form for a direct target: {c}");
    }

    #[test]
    fn fixed_array_lowers_to_a_value_struct_with_bounds_checked_indexing() {
        // `[N]T` is a value type: a C `struct { T a[N]; }`. `[v; N]` fills it, `a[i]`
        // is bounds-checked against the constant length (through the array's address,
        // no copy), `a.len` is that constant, and `for x in a` walks `.a[k]`.
        let src = "fn sum() -> i32 { var xs: [4]i32 = [0; 4] xs[2] = 9 var s: i32 = 0 for v in xs { s = s + v } return s + (xs.len as i32) }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("typedef struct { int32_t a[4]; } JestyrArr_i32_4;"), "value-struct typedef: {c}");
        assert!(c.contains("_ar0.a[_k0] = _v0"), "repeat literal fills the array: {c}");
        assert!(c.contains("assert(_ix") && c.contains("->a[_ix"), "bounds-checked element access: {c}");
        assert!(c.contains("((size_t)4)"), "`.len` is the constant length: {c}");
        assert!(c.contains("_a") && c.contains("->a[_k"), "for-loop iterates the inline field: {c}");
    }

    #[test]
    fn generic_slice_algorithm_monomorphizes_to_the_concrete_slice_type() {
        // A `for x in s` over a generic `[]T` inside a generic function must name the
        // concrete `JestyrSlice_i32` instance (resolved through the active subst), not
        // the opaque `JestyrSlice_T`. (Regression: the slice for-loop and index
        // lowering read the inferred type without applying the substitution.)
        let src = "fn sl_sum(comptime T: type, read s: []T, take z: T, f: fn(T, T) -> T) -> T { var acc: T = z for x in s { acc = f(acc, x) } return acc } \
                   fn add(a: i32, b: i32) -> i32 { return a + b } \
                   fn main() -> i32 { var p: *mut i32 = alloc_i32(1) unsafe { p.* = 5 } var s: []i32 = slice(i32, p, 1) return sl_sum(i32, s, 0, &add) }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("JestyrSlice_i32 _s"), "the for-loop names the concrete slice instance: {c}");
        assert!(!c.contains("JestyrSlice_T"), "no opaque slice type leaks: {c}");
    }

    #[test]
    fn generic_combinator_lowers_deterministically() {
        // The new collectors (fn-pointer typedefs through generic signatures; generic
        // enum instances) iterate ordered `Vec`s and a pure substitution, so the same
        // combinator-heavy source must emit byte-identical C every run — no iteration
        // -order leak (the `compilation_is_deterministic` discipline, applied here).
        let src = "enum Option(T) { none, some(v: T) } \
                   fn opt_map(comptime T: type, comptime U: type, take o: Option(T), f: fn(T) -> U) -> Option(U) { match o { some(v) => some(f(v)), none => none } } \
                   fn opt_unwrap_or(comptime T: type, take o: Option(T), take d: T) -> T { match o { some(v) => v, none => d } } \
                   fn inc(x: i32) -> i32 { return x + 1 } \
                   fn main() -> i32 { var a: Option(i32) = some(41) return opt_unwrap_or(i32, opt_map(i32, i32, a, &inc), 0) }";
        let (c1, d1) = gen(src);
        let (c2, _d2) = gen(src);
        assert!(d1.is_empty(), "{:?}", d1);
        assert_eq!(c1, c2, "combinator lowering must be byte-identical across runs");
    }

    #[test]
    fn infers_a_nullary_generic_variant_from_a_call_argument() {
        // `none` in argument position resolves to the parameter's instantiation.
        let src = "enum Option(T) { none, some(v: T) } \
                   fn or_else(o: Option(i32), d: i32) -> i32 { match o { some(v) => v, none => d } } \
                   fn main() -> i32 { return or_else(none, 5) }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        // `none` constructed the parameter's instance (not an unresolved template).
        assert!(c.contains("Jestyr_Option__i32_none"), "none → the param instance: {c}");
    }

    #[test]
    fn generic_enum_instance_inherits_niche_optimization() {
        // Option(*mut i32) is one nullary + one thin-pointer variant → bare pointer.
        let src = "enum Option(T) { none, some(v: T) } \
                   fn get(o: Option(*mut i32)) -> i32 { match o { some(p) => unsafe { p.* }, none => 0 } }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("int32_t jestyr_get(int32_t* j_o)"), "instance is a bare pointer: {c}");
        assert!(c.contains("!= ((int32_t*)0)"), "match is a null test: {c}");
        assert!(!c.contains("Jestyr_Option__pmut"), "no tagged union for the niche instance: {c}");
    }

    #[test]
    fn lowers_explicit_discriminants_and_reads_them_via_cast() {
        let src = "enum Color { red = 1, green = 2, blue = 4 } \
                   fn v(c: Color) -> i32 { c as i32 } \
                   fn main() -> i32 { return v(green) }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("Jestyr_Color_red = 1,"), "explicit discriminant in tag enum: {c}");
        assert!(c.contains("Jestyr_Color_blue = 4,"), "{c}");
        assert!(c.contains(").tag)"), "`c as i32` reads the discriminant (the tag): {c}");
    }

    #[test]
    fn distinct_lowers_to_a_zero_cost_typedef() {
        let src = "distinct UserId = i32 \
                   fn id(u: UserId) -> i32 { return u as i32 } \
                   fn main() -> i32 { return id(5 as UserId) }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("typedef int32_t Jestyr_UserId;"), "zero-cost typedef: {c}");
        assert!(c.contains("Jestyr_UserId j_u"), "param uses the distinct type: {c}");
    }

    #[test]
    fn a_non_niche_enum_keeps_its_tagged_union() {
        // Two payload fields ⇒ no single-pointer niche ⇒ ordinary tagged union.
        let two = gen("enum E { none, some(p: *mut i32, n: i32) }").0;
        assert!(two.contains("struct Jestyr_E {"), "two fields → tagged union: {two}");
        // Three variants ⇒ not the 2-variant niche shape.
        let three = gen("enum E { none, some(p: *mut i32), more(q: *mut i32) }").0;
        assert!(three.contains("struct Jestyr_E {"), "three variants → tagged union: {three}");
        // A fat generational ref `&T` has no null niche ⇒ tagged union.
        let fat = gen("enum E { none, some(p: &i32) }").0;
        assert!(fat.contains("struct Jestyr_E {"), "fat &T → tagged union: {fat}");
    }
}

/// Is `p` an integer primitive — the element types `par for` may iterate? Mirrors
/// `typeck::is_integer_prim`; kept local so cgen does not depend on typeck's privates.
/// The vector width every `@simd` lowering uses, in bytes. 256 bits — AVX2-shaped, and a
/// plain `vector_size` request that GCC lowers on any target. **Fixed and recorded rather
/// than probed**: a host-dependent width would make the emitted C depend on the build
/// machine, which the attestation hash would rightly flag as a different program.
const SIMD_VECTOR_BYTES: usize = 32;

/// The element type a vectorized `par for` **computes in** — the source element after C's
/// integer promotions.
///
/// This exists because `simd::classify` reasons in Jestyr's types and the emitted code runs
/// under C's. A `par for` emits a vector head *and* a scalar remainder from one source
/// expression, and in the remainder the loop variable is a real `int8_t`, so C promotes it:
/// `j_x * j_x` is computed in `int` and only then narrowed. GNU vector arithmetic has no
/// such rule — it is elementwise at the vector's own element type. Lower a `[]i8` body into
/// `int8_t` lanes and the two halves of the same loop compute in different widths, which is
/// exactly the silent divergence this function exists to remove: `33 * 33` is `1089` in the
/// remainder and `65` in the head.
///
/// So every element narrower than `int` promotes to `int32_t`, matching the remainder
/// exactly. All four of `i8`/`u8`/`i16`/`u16` promote to **signed** `int`, because `int` can
/// represent every value of each — including the unsigned two, which is why `~x` over
/// `[]u8` must yield `-34` and not `222`.
///
/// **The cost is density, and it is not optional.** A `[]i8` body computes in `int32_t`
/// lanes, so one vector iteration covers 8 elements, not 32 — the same lane count an `i32`
/// body gets. Narrow element types therefore buy load bandwidth, not lanes. The denser
/// lowering is only available to a language whose scalar arithmetic already truncates at the
/// element width, and Jestyr's does not: `(a * a) as i64` for `a: i8 = 33` is `1089` today,
/// not `65`, even though the type checker calls the product an `i8`. Until that is settled
/// one way or the other, agreeing with the remainder outranks filling the register — the
/// whole `@simd` guarantee is that scalar and every lane width compute the same bits.
fn simd_compute_elem(elem: &Ty) -> Ty {
    match elem {
        Ty::Prim("i8") | Ty::Prim("u8") | Ty::Prim("i16") | Ty::Prim("u16") => Ty::Prim("i32"),
        other => other.clone(),
    }
}

/// How many source elements one vector iteration covers.
///
/// Always called with a **compute** element ([`simd_compute_elem`]), never a raw source
/// element — a `[]i8` loop is 8 lanes wide, not 32, and reading a lane count straight off
/// the source width is the mistake that made a `[]i8` reduction return the wrong answer.
fn simd_lanes(elem: &Ty) -> usize {
    let sz = match elem {
        Ty::Prim("i32") | Ty::Prim("u32") => 4,
        _ => 8,
    };
    SIMD_VECTOR_BYTES / sz
}

fn is_integer_c_prim(p: &str) -> bool {
    matches!(
        p,
        "i8" | "i16" | "i32" | "i64" | "isize" | "u8" | "u16" | "u32" | "u64" | "usize"
    )
}
