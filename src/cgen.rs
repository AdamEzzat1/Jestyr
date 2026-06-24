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
use crate::diag::Diagnostic;
use crate::span::Span;
use crate::types::{prim_ty, MethodRes, Ty, TypeInfo, TypeKindG};

/// Lower a program to C, ending with the ordinary entry-point wrapper around
/// the user's `main`.
pub fn emit(ast: &Ast, info: &TypeInfo) -> (String, Vec<Diagnostic>) {
    emit_program(ast, info, false)
}

/// Lower a program to C in *test* mode: instead of the `main` wrapper, emit a
/// harness `main` that runs every `@test` (reporting pass/fail) and times every
/// `@bench`. Drives `jestyrc test` (roadmap workstream O).
pub fn emit_tests(ast: &Ast, info: &TypeInfo) -> (String, Vec<Diagnostic>) {
    emit_program(ast, info, true)
}

fn emit_program(ast: &Ast, info: &TypeInfo, test_mode: bool) -> (String, Vec<Diagnostic>) {
    // Index every enum variant by name, so the backend can construct and match
    // on them by finding the owning enum and the variant's payload fields.
    let mut variants = HashMap::new();
    for item in &ast.items {
        if let Item::Enum(e) = item {
            for v in &e.variants {
                variants.insert(
                    v.name.name.clone(),
                    VariantInfo {
                        enum_name: e.name.name.clone(),
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
    for item in &ast.items {
        if let Item::Fn(f) = item {
            let is_gen = f.params.iter().any(|p| {
                p.comptime && p.ty.is_some_and(|t| matches!(ast.type_at(t).kind, TypeKind::TypeKw))
            });
            if is_gen {
                generics.insert(f.name.name.clone());
            }
            if let Some(es) = &f.errors {
                for name in &es.names {
                    let next = error_tags.len() as i64 + 1;
                    error_tags.entry(name.name.clone()).or_insert(next);
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
        spawn_sites: Vec::new(),
        slice_instances: Vec::new(),
        genref_instances: Vec::new(),
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
    };
    g.spawn_sites = g.collect_spawns();
    g.slice_instances = g.collect_slices();
    g.genref_instances = g.collect_genrefs();
    let (instances, method_instances) = g.collect_all_instances();
    g.instances = instances;
    g.method_instances = method_instances;
    g.struct_instances = g.collect_struct_instances();
    g.enum_instances = g.collect_enum_instances();
    let (closures, closure_index) = g.collect_closures();
    g.closures = closures;
    g.closure_index = closure_index;
    g.prelude();
    g.forward_types();
    g.struct_defs();
    g.enum_defs();
    g.gen_struct_defs();
    g.gen_enum_defs();
    g.slice_struct_defs();
    g.genref_struct_defs();
    g.result_defs();
    g.extern_protos();
    g.closure_types();
    g.fn_protos();
    g.method_protos();
    g.spawn_runtime();
    g.consts();
    g.closure_fns();
    g.fn_defs();
    g.method_defs();
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
    /// Names of the current function's by-pointer (`mut`/`out`) parameters, which
    /// must be dereferenced on use.
    ptr_params: HashSet<String>,
    /// variant name → its enum and payload field list.
    variants: HashMap<String, VariantInfo>,
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
    /// every `spawn` site, for emitting per-site arg structs + trampolines.
    spawn_sites: Vec<SpawnSite>,
    /// distinct slice element types, for emitting one `JestyrSlice_<T>` per type.
    slice_instances: Vec<Ty>,
    /// distinct generational-reference element types (one `JestyrRef_<T>` each).
    genref_instances: Vec<Ty>,
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
}

impl<'a> Cgen<'a> {
    fn diag(&mut self, span: Span, msg: impl Into<String>) {
        self.diags.push(Diagnostic::new(msg, span));
    }

    fn raw(&mut self, s: impl AsRef<str>) {
        self.out.push_str(s.as_ref());
    }

    fn line(&mut self, s: impl AsRef<str>) {
        let pad = "    ".repeat(self.depth);
        let _ = writeln!(self.out, "{pad}{}", s.as_ref());
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
        self.raw("static JestyrStr jestyr_rt_trim(JestyrStr s) { size_t a = 0, b = s.len; while (a < b) { char c = s.ptr[a]; if (c==' '||c=='\\t'||c=='\\n'||c=='\\r') a++; else break; } while (b > a) { char c = s.ptr[b-1]; if (c==' '||c=='\\t'||c=='\\n'||c=='\\r') b--; else break; } return (JestyrStr){ s.ptr + a, b - a }; }\n\n");
        self.raw("/* Jestyr owned String — a heap-owned, growable buffer (the owned half of the\n");
        self.raw("   owned/view split). `string_view` borrows it as a `str` view; no copy. */\n");
        self.raw("typedef struct { char* ptr; size_t len; size_t cap; } JestyrString;\n");
        self.raw("static JestyrString jestyr_rt_str_new(void) { JestyrString s; s.ptr = NULL; s.len = 0; s.cap = 0; return s; }\n");
        self.raw("static JestyrString jestyr_rt_str_from(JestyrStr v) { JestyrString s; s.cap = v.len ? v.len : 1; s.ptr = (char*)malloc(s.cap); memcpy(s.ptr, v.ptr, v.len); s.len = v.len; return s; }\n");
        self.raw("static void jestyr_rt_str_push(JestyrString* s, JestyrStr v) { if (s->len + v.len > s->cap) { size_t nc = s->cap ? s->cap * 2 : 8; while (nc < s->len + v.len) nc *= 2; s->ptr = (char*)realloc(s->ptr, nc); s->cap = nc; } memcpy(s->ptr + s->len, v.ptr, v.len); s->len += v.len; }\n");
        self.raw("static JestyrStr jestyr_rt_str_view(JestyrString* s) { return (JestyrStr){ s->ptr, s->len }; }\n");
        self.raw("static void jestyr_rt_str_free(JestyrString* s) { free(s->ptr); s->ptr = NULL; s->len = 0; s->cap = 0; }\n");
        self.raw("/* Append an integer's decimal digits (for f-string interpolation; copies). */\n");
        self.raw("static void jestyr_rt_str_push_i64(JestyrString* s, int64_t v) { char b[24]; int n = snprintf(b, sizeof(b), \"%lld\", (long long)v); if (n < 0) n = 0; jestyr_rt_str_push(s, (JestyrStr){ b, (size_t)n }); }\n\n");
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

    fn forward_types(&mut self) {
        let ast = self.ast;
        for item in &ast.items {
            match item {
                Item::Struct { name, is_union, .. } => {
                    let kw = if *is_union { "union" } else { "struct" };
                    self.raw(format!("typedef {kw} Jestyr_{0} Jestyr_{0};\n", name.name));
                }
                // `distinct UserId = u64` → a zero-cost C typedef of the base.
                Item::Distinct(dd) => {
                    let base = self.c_ty_ast(dd.base);
                    self.raw(format!("typedef {base} Jestyr_{};\n", dd.name.name));
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
                        .get(&e.name.name)
                        .is_some_and(|&i| self.niche_enum_at(i).is_some())
                    {
                        continue;
                    }
                    self.raw(format!("typedef struct Jestyr_{0} Jestyr_{0};\n", e.name.name));
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
    /// `Option(*T)`/`Option(&[r]T)` inherit the niche optimization automatically.
    fn niche_enum_instance(&self, ctor: &str, args: &[Ty]) -> Option<NicheInfo> {
        let e = self.ast.items.iter().find_map(|it| match it {
            Item::Enum(e) if e.name.name == ctor && e.is_generic() => Some(e),
            _ => None,
        })?;
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
        self.ast
            .items
            .iter()
            .find_map(|it| match it {
                Item::Enum(e) if e.name.name == ctor && e.is_generic() => Some(
                    e.type_params
                        .iter()
                        .map(|p| p.name.clone())
                        .zip(args.iter().cloned())
                        .collect(),
                ),
                _ => None,
            })
            .unwrap_or_default()
    }

    /// Lower each enum to a tagged union: a `tag` enum plus a `union` of the
    /// payload-carrying variants. Nullary variants contribute a tag constant but
    /// no union member. A niche-optimized enum is skipped (it has no struct).
    fn enum_defs(&mut self) {
        let ast = self.ast;
        for item in &ast.items {
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
                    .get(&e.name.name)
                    .is_some_and(|&i| self.niche_enum_at(i).is_some())
                {
                    continue;
                }
                let en = e.name.name.clone();
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

    /// Lower an AST type to a `Ty`, applying the given type-parameter substitution.
    fn ast_type_to_ty(&self, id: TypeId, subst: &HashMap<String, Ty>) -> Ty {
        match &self.ast.type_at(id).kind {
            TypeKind::Name(n) => {
                if let Some(t) = subst.get(&n.name) {
                    t.clone()
                } else if let Some(p) = prim_ty(&n.name) {
                    Ty::Prim(p)
                } else if let Some(&i) = self.info.table.type_index.get(&n.name) {
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
                if self.enum_is_generic(&ctor.name) {
                    Ty::GenEnum { ctor: ctor.name.clone(), args: aty }
                } else {
                    Ty::GenStruct { ctor: ctor.name.clone(), args: aty }
                }
            }
            TypeKind::Slice(inner) => Ty::Slice(Box::new(self.ast_type_to_ty(*inner, subst))),
            TypeKind::GenRef(inner) => Ty::GenRef(Box::new(self.ast_type_to_ty(*inner, subst))),
            TypeKind::RegionRef { inner, .. } => {
                Ty::RegionRef(Box::new(self.ast_type_to_ty(*inner, subst)))
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
        order
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
            _ => {}
        }
    }

    /// Emit a forward typedef and a definition for each monomorphized struct.
    fn gen_struct_defs(&mut self) {
        for (ctor, args) in self.struct_instances.clone() {
            let cname = self.gen_struct_c_name(&ctor, &args);
            self.raw(format!("typedef struct {cname} {cname};\n"));
        }
        self.raw("\n");
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
        self.raw(format!("struct {cname} {{\n"));
        for m in &body.members {
            if let StructMember::Field { name, ty, .. } = m {
                let fty = self.ast_type_to_ty(*ty, &subst);
                let fc = self.c_type(&fty);
                self.raw(format!("    {fc} j_{};\n", name.name));
            }
        }
        self.raw("};\n\n");
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
            _ => {}
        }
    }

    fn gen_enum_defs(&mut self) {
        // Forward typedefs for non-niche instances (a niche instance is a pointer).
        for (ctor, args) in self.enum_instances.clone() {
            if self.niche_enum_instance(&ctor, &args).is_some() {
                continue;
            }
            let cname = self.gen_struct_c_name(&ctor, &args);
            self.raw(format!("typedef struct {cname} {cname};\n"));
        }
        self.raw("\n");
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
        let Some(e) = self.ast.items.iter().find_map(|it| match it {
            Item::Enum(e) if e.name.name == ctor && e.is_generic() => Some(e.clone()),
            _ => None,
        }) else {
            return;
        };
        let subst: HashMap<String, Ty> = e
            .type_params
            .iter()
            .map(|p| p.name.clone())
            .zip(args.iter().cloned())
            .collect();
        let cname = self.gen_struct_c_name(ctor, args);
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
    }

    /// Emit one tagged result struct per distinct ok-type used by a fallible
    /// function: `{ bool is_err; <T> ok; int err; }`.
    fn result_defs(&mut self) {
        let ast = self.ast;
        let mut seen: HashSet<String> = HashSet::new();
        // `try_from_utf8(...) -> str !Utf8Error` is an *intrinsic*, so its result
        // type isn't discovered from a fn signature — emit it up front (and seed
        // `seen` so a user `str !E` function doesn't duplicate the typedef).
        self.raw("typedef struct { bool is_err; JestyrStr ok; int err; } JestyrResult_str;\n");
        seen.insert("JestyrResult_str".to_string());
        for item in &ast.items {
            if let Item::Fn(f) = item {
                if f.errors.is_none() {
                    continue;
                }
                let ok = self.info.table.fns.get(&f.name.name).map(|s| s.ret.clone()).unwrap_or(Ty::Unit);
                let cname = self.result_c_name(&ok);
                if !seen.insert(cname.clone()) {
                    continue;
                }
                self.raw(format!("typedef struct {{ bool is_err; "));
                if ok != Ty::Unit {
                    let okc = self.c_type(&ok);
                    self.raw(format!("{okc} ok; "));
                }
                self.raw(format!("int err; }} {cname};\n"));
            }
        }
        self.raw("\n");
    }

    fn struct_defs(&mut self) {
        let ast = self.ast;
        for item in &ast.items {
            if let Item::Struct { name, body, attrs, is_union, .. } = item {
                let attr = self.struct_attr(attrs);
                let kw = if *is_union { "union" } else { "struct" };
                self.raw(format!("{kw}{attr} Jestyr_{} {{\n", name.name));
                for m in &body.members {
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
            }
        }
    }

    /// Translate item attributes that affect struct layout into a GNU
    /// `__attribute__((…))` clause: `@packed` → `packed`, `@align(n)` →
    /// `aligned(n)`. `@layout(c)` is the default C layout (a no-op marker until
    /// field reordering lands); unknown attributes are ignored.
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
        for item in &ast.items {
            if let Item::Fn(f) = item {
                if self.is_generic(f) || !self.fn_supported(f) {
                    continue;
                }
                self.subst.clear();
                let cname = self.c_fn_name(&f.name.name);
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
        for item in &ast.items {
            if let Item::Const(c) = item {
                let cty = if let Some(t) = c.ty {
                    self.c_ty_ast(t)
                } else {
                    let t = self.info.type_of(c.value).clone();
                    self.c_type(&t)
                };
                let v = self.emit_expr(c.value);
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
        for item in &ast.items {
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
                let cname = self.c_fn_name(&f.name.name);
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
        self.ptr_params = f
            .params
            .iter()
            .filter(|p| !p.comptime && matches!(p.conv, Conv::Mut | Conv::Out))
            .map(|p| p.name.name.clone())
            .collect();
        self.cur_result = self.fn_result_type(f);
        self.cur_ensures = f.ensures.clone();
        self.cur_ret_cty = self.ret_type(f);
        self.cur_no_panic = f.no_panic;
        self.cur_refines =
            f.params.iter().filter_map(|p| p.refine.map(|r| (p.name.name.clone(), r))).collect();

        let sig = self.fn_signature(f, c_name);
        let returns_value = self.ret_type(f) != "void";
        self.raw(format!("{sig}\n"));
        self.emit_fn_body(&f.body, returns_value, &f.requires);
        self.raw("\n");
        self.ptr_params.clear();
        self.cur_result.clear();
        self.cur_ensures.clear();
        self.cur_ret_cty.clear();
        self.cur_refines.clear();
        self.cur_no_panic = false;
    }

    /// Like `emit_body`, but prefixed with the function's `requires`
    /// preconditions as `assert`s (active in debug, elided under `-DNDEBUG`).
    fn emit_fn_body(&mut self, block: &Block, ret: bool, requires: &[ExprId]) {
        self.line("{");
        self.depth += 1;
        for r in requires {
            let c = self.emit_expr(*r);
            self.line(format!("assert({c});"));
        }
        let n = block.stmts.len();
        for (i, stmt) in block.stmts.iter().enumerate() {
            let last = i + 1 == n;
            if last && ret {
                match stmt {
                    Stmt::Expr(e) => self.emit_return(Some(*e)),
                    Stmt::Return { value, .. } => self.emit_return(*value),
                    _ => self.emit_stmt(stmt),
                }
            } else {
                self.emit_stmt(stmt);
            }
        }
        self.depth -= 1;
        self.line("}");
    }

    /// Emit a value-returning `return`, checking any `ensures` postconditions
    /// first (with `result` — emitted as `j_result` — bound to the value).
    fn emit_value_return(&mut self, value: String) {
        if self.cur_ensures.is_empty() {
            self.line(format!("return {value};"));
            return;
        }
        let rty = self.cur_ret_cty.clone();
        self.line(format!("{rty} j_result = {value};"));
        for post in self.cur_ensures.clone() {
            let c = self.emit_expr(post);
            self.line(format!("assert({c});"));
        }
        self.line("return j_result;");
    }

    /// The C result-struct name if `f` is fallible, otherwise empty.
    fn fn_result_type(&self, f: &FnDecl) -> String {
        if f.errors.is_none() {
            return String::new();
        }
        let ok = self.info.table.fns.get(&f.name.name).map(|s| s.ret.clone()).unwrap_or(Ty::Unit);
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
            Some(true) => self.raw("int main(void) { return (int) jestyr_main(); }\n"),
            Some(false) => self.raw("int main(void) { jestyr_main(); return 0; }\n"),
            None => {}
        }
    }

    /// Emit the `jestyrc test` harness `main`: run each `@test` (a no-arg fn
    /// returning `bool`), tallying pass/fail; then time each `@bench`. Exits
    /// non-zero if any test fails. (User `main` is ignored in test mode.)
    fn test_main(&mut self) {
        let ast = self.ast;
        let runnable = |f: &FnDecl| !self.is_generic(f) && self.fn_supported(f);
        let tests: Vec<String> = ast
            .items
            .iter()
            .filter_map(|it| match it {
                Item::Fn(f) if f.has_attr("test") && runnable(f) => Some(f.name.name.clone()),
                _ => None,
            })
            .collect();
        let benches: Vec<String> = ast
            .items
            .iter()
            .filter_map(|it| match it {
                Item::Fn(f) if f.has_attr("bench") && runnable(f) => Some(f.name.name.clone()),
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
            let cty = borrow_ptr_cty(&base, p.conv);
            parts.push(format!("{cty} j_{}", p.name.name));
        }
        if parts.is_empty() {
            "void".to_string()
        } else {
            parts.join(", ")
        }
    }

    // --- statements ---

    fn emit_body(&mut self, block: &Block, ret: bool) {
        self.line("{");
        self.depth += 1;
        let n = block.stmts.len();
        for (i, stmt) in block.stmts.iter().enumerate() {
            let last = i + 1 == n;
            if last && ret {
                match stmt {
                    Stmt::Expr(e) => self.emit_return(Some(*e)),
                    Stmt::Return { value, .. } => self.emit_return(*value),
                    _ => self.emit_stmt(stmt),
                }
            } else {
                self.emit_stmt(stmt);
            }
        }
        self.depth -= 1;
        self.line("}");
    }

    fn emit_stmt(&mut self, stmt: &Stmt) {
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
                    ExprKind::Region { name, body } => self.emit_region(&name.name, body),
                    ExprKind::For { label, head, region, body, els } => {
                        self.emit_for(label.as_ref(), head, region.as_ref(), body, els.as_ref())
                    }
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

        let scrut_ty = self.info.type_of(scrut).clone();
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
                    if let Some(vi) = self.variants.get(&vname.name).cloned() {
                        for (i, sp) in subpats.iter().enumerate() {
                            match &ast.pat_at(*sp).kind {
                                // a plain binding → project the field
                                PatKind::Ident(bind)
                                    if !self.variants.contains_key(&bind.name) =>
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
                PatKind::Ident(vname) if self.variants.contains_key(&vname.name) => {
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
                    if let Some(vi) = self.variants.get(&vname.name).cloned() {
                        for (i, sp) in subpats.iter().enumerate() {
                            match &ast.pat_at(*sp).kind {
                                PatKind::Ident(bind)
                                    if !self.variants.contains_key(&bind.name) =>
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
                PatKind::Ident(vname) if self.variants.contains_key(&vname.name) => {
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
            PatKind::Ident(n) if self.variants.contains_key(&n.name) => Some(vec![n.name.clone()]),
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
            PatKind::Ident(n) => !self.variants.contains_key(&n.name), // a binding, not a variant
            _ => false,
        }
    }

    fn pat_is_constructor(&self, pat: PatId) -> bool {
        match &self.ast.pat_at(pat).kind {
            PatKind::Variant { .. }
            | PatKind::StructVariant { .. }
            | PatKind::Lit(_)
            | PatKind::Range { .. } => true,
            PatKind::Ident(n) => self.variants.contains_key(&n.name), // a nullary variant
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
        let vi = self.variants.get(vname)?.clone();
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
        let vi = self.variants.get(vname)?.clone();
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
            PatKind::Ident(n) if !self.variants.contains_key(&n.name) => {
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
        self.ast
            .items
            .iter()
            .any(|it| matches!(it, Item::Enum(e) if e.name.name == name && e.is_generic()))
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
        let vi = match self.variants.get(vname).cloned() {
            Some(v) => v,
            None => return "0".to_string(),
        };
        // A generic-enum instance: the instantiation comes from the inferred type.
        if let Ty::GenEnum { ctor, args } = self.info.type_of(id).clone() {
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
        if let Ty::GenEnum { ctor, args: targs } = self.info.type_of(construct_id).clone() {
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
        let ast = self.ast;
        let data = ast.expr_at(id);
        let span = data.span;
        match &data.kind {
            ExprKind::Int(l) => c_int_literal(l),
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
                if let Some(vi) = self.variants.get(&n.name).cloned() {
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
                let r = self.emit_expr(*rhs);
                format!("({}{r})", unop_c(*op))
            }
            ExprKind::Binary { op, lhs, rhs } => {
                let l = self.emit_expr(*lhs);
                let r = self.emit_expr(*rhs);
                format!("({l} {} {r})", binop_c(*op))
            }
            ExprKind::Assign { op, target, value } => {
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
                let bt = self.info.type_of(*base).clone();
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
                let bt = self.info.type_of(*base).clone();
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
                let b = self.emit_expr(*base);
                let i = self.emit_expr(*index);
                if matches!(bt, Ty::Prim("str")) {
                    // A string view indexes into its byte buffer.
                    format!("((uint8_t)({b}).ptr[({i})])")
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
                if self.variants.contains_key(&path.name) {
                    return self.emit_struct_variant_construct(id, &path.name, fields);
                }
                // `Point { x: 9, ..p }` — functional update: copy `p`, then assign the
                // listed fields. A GNU statement-expression keeps it an expression.
                if let Some(sp) = spread {
                    let base = self.emit_expr(*sp);
                    let tmp = format!("jss_{}", self.tmp);
                    self.tmp += 1;
                    let mut s = format!("({{ Jestyr_{} {tmp} = {base}; ", path.name);
                    for fi in fields {
                        let v = self.emit_expr(fi.value);
                        let _ = write!(s, "{tmp}.j_{} = {v}; ", fi.name.name);
                    }
                    let _ = write!(s, "{tmp}; }})");
                    return s;
                }
                let mut s = format!("(Jestyr_{}){{ ", path.name);
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
                format!(
                    "({{ {res_ty} {tmp} = {base_c}; if ({tmp}.is_err) return ({cur}){{ .is_err = true, .err = {tmp}.err }}; {tmp}.ok; }})"
                )
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
            ExprKind::Block(_) | ExprKind::If { .. } | ExprKind::Unsafe(_) => {
                self.diag(span, "this control-flow expression is only supported in statement or return position");
                "0".to_string()
            }
            ExprKind::Closure { .. } => self.emit_closure_literal(id),
            ExprKind::Concurrent(_) => {
                self.diag(span, "`concurrent` is only supported in statement position");
                "0".to_string()
            }
            ExprKind::Spawn(_) => {
                self.diag(span, "`spawn` may only appear inside a `concurrent` block");
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
        // Invoking a closure value (a local bound to one, or an inline closure).
        if self.is_closure_typed(callee) {
            return self.emit_closure_invoke(callee, args);
        }
        // `@address(0x…)` — a pointer at a fixed address (MMIO; design §16).
        if let ExprKind::Attr(n) = &ast.expr_at(callee).kind {
            if n.name == "address" {
                let addr = args.first().map(|a| self.emit_expr(*a)).unwrap_or_else(|| "0".to_string());
                return format!("((void*)({addr}))");
            }
        }
        if let ExprKind::Name(n) = &ast.expr_at(callee).kind {
            // enum-variant constructor with a payload, e.g. `circle(2.0)`
            if let Some(vi) = self.variants.get(&n.name).cloned() {
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
                    return format!("({}){{ .is_err = true, .err = {tag} }}", self.cur_result);
                }
                "is_err" => {
                    let v = args.first().map(|a| self.emit_expr(*a)).unwrap_or_else(|| "0".to_string());
                    return format!("(({v}).is_err)");
                }
                "unwrap" => {
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
                "starts_with" => return self.emit_str_binop("jestyr_rt_starts_with", args),
                "ends_with" => return self.emit_str_binop("jestyr_rt_ends_with", args),
                "contains" => return self.emit_str_binop("jestyr_rt_contains", args),
                "find" => return self.emit_str_binop("jestyr_rt_find", args),
                "trim" => {
                    let s = args.first().map(|a| self.emit_expr(*a)).unwrap_or_else(|| "(JestyrStr){0,0}".to_string());
                    return format!("jestyr_rt_trim({s})");
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
                    let e = self.emit_expr(*a);
                    if matches!(convs.get(i), Some(Conv::Mut) | Some(Conv::Out)) {
                        parts.push(format!("&({e})"));
                    } else {
                        parts.push(e);
                    }
                }
                return format!("{}({})", n.name, parts.join(", "));
            }

            // A generic function: pick (or already-collected) the monomorphized
            // instance for these type arguments and call it.
            if self.generics.contains(&n.name) {
                let name = n.name.clone();
                return self.emit_generic_call(&name, args);
            }

            // A known function: take `&arg` for `mut`/`out` parameters.
            let convs: Vec<Conv> = self
                .info
                .table
                .fns
                .get(&n.name)
                .map(|sig| sig.params.iter().map(|p| p.conv).collect())
                .unwrap_or_default();
            let mut parts = Vec::new();
            for (i, a) in args.iter().enumerate() {
                let e = self.emit_expr(*a);
                if matches!(convs.get(i), Some(Conv::Mut) | Some(Conv::Out)) {
                    parts.push(format!("&({e})"));
                } else {
                    parts.push(e);
                }
            }
            return format!("{}({})", self.c_fn_name(&n.name), parts.join(", "));
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
        let mut parts = Vec::new();
        for (i, a) in args.iter().enumerate() {
            let e = self.emit_expr(*a);
            if matches!(convs.get(i), Some(Conv::Mut) | Some(Conv::Out)) {
                parts.push(format!("&({e})"));
            } else {
                parts.push(e);
            }
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

        let recv = self.emit_expr(base);
        let recv = if matches!(mr.recv_conv, Conv::Mut | Conv::Out) {
            format!("&({recv})")
        } else {
            recv
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
            let e = self.emit_expr(*a);
            if matches!(arg_convs.get(i), Some(Conv::Mut) | Some(Conv::Out)) {
                parts.push(format!("&({e})"));
            } else {
                parts.push(e);
            }
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
        if f.errors.is_some() {
            if body {
                self.diag(
                    f.name.span,
                    "the C backend does not support fallible generic-struct methods yet",
                );
            }
            return;
        }
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
        self.cur_result.clear();
        self.cur_ensures.clear();
        // `@no_panic`/`@inline`/`@cold`/… are honoured on methods too (they emit as
        // free C functions), so the attribute machinery must follow them here.
        self.cur_no_panic = f.no_panic;

        let prefix = self.fn_attr_prefix(f);
        let ret = match f.ret_ty {
            Some(t) => self.c_ty_ast(t),
            None => "void".to_string(),
        };
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
            let runtime: Vec<&Param> = f.params.iter().filter(|p| !p.comptime && !p.is_self).collect();

            self.raw(format!("struct _jsp_{id} {{ "));
            if runtime.is_empty() {
                self.raw("char _unused; ");
            }
            for (i, p) in runtime.iter().enumerate() {
                let cty = match p.ty {
                    Some(t) => self.c_ty_ast(t),
                    None => "int".to_string(),
                };
                self.raw(format!("{cty} a{i}; "));
            }
            self.raw("};\n");

            let call_args: Vec<String> = (0..runtime.len()).map(|i| format!("_a->a{i}")).collect();
            let callee = self.c_fn_name(&site.fn_name); // honour `@no_mangle` spawn targets
            self.raw(format!("static void* jestyr_task_{id}(void* _vp) {{ "));
            self.raw(format!("struct _jsp_{id}* _a = (struct _jsp_{id}*)_vp; "));
            self.raw(format!("{callee}({}); return NULL; }}\n", call_args.join(", ")));
        }
        if !self.spawn_sites.is_empty() {
            self.raw("\n");
        }
    }

    /// Lower a `concurrent { … }` nursery: each `spawn` creates a thread; the
    /// scope joins them all before it exits (structured concurrency).
    fn emit_concurrent(&mut self, block: &Block) {
        let ast = self.ast;
        self.line("{");
        self.depth += 1;
        let mut handles = 0usize;
        for stmt in &block.stmts {
            if let Stmt::Expr(e) = stmt {
                if let ExprKind::Spawn(inner) = &ast.expr_at(*e).kind {
                    if let Some(site) = self.spawn_site(*inner) {
                        let id = site.call_id.0;
                        let h = handles;
                        let vals: Vec<String> = site.args.iter().map(|a| self.emit_expr(*a)).collect();
                        let init =
                            if vals.is_empty() { "{0}".to_string() } else { format!("{{ {} }}", vals.join(", ")) };
                        self.line(format!("pthread_t _jt{h};"));
                        self.line(format!("struct _jsp_{id} _ja{h} = {init};"));
                        self.line(format!("pthread_create(&_jt{h}, NULL, jestyr_task_{id}, &_ja{h});"));
                        handles += 1;
                        continue;
                    }
                    self.diag(ast.expr_at(*e).span, "`spawn` expects a direct function call");
                    continue;
                }
            }
            self.emit_stmt(stmt);
        }
        for h in 0..handles {
            self.line(format!("pthread_join(_jt{h}, NULL);"));
        }
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
        if let Some(name) = self.scratch_reset.take() {
            self.line(format!("j_{name}.off = 0;"));
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
        let st = self.info.type_of(iter).clone();
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
        self.line(format!("JestyrArena j_{name} = jestyr_arena_new(1u << 20);"));
        for stmt in &body.stmts {
            self.emit_stmt(stmt);
        }
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
                match self.info.table.type_index.get(&n.name).copied() {
                    // A niche-optimized enum lowers to its bare pointer payload.
                    Some(i) if self.niche_enum_at(i).is_some() => {
                        let payload = self.niche_enum_at(i).unwrap().payload;
                        self.c_type(&payload)
                    }
                    // structs and enums both lower to a `Jestyr_<Name>` typedef.
                    Some(_) => format!("Jestyr_{}", n.name),
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
                // A generic-enum instance may be niche-optimized to a bare pointer.
                if self.enum_is_generic(&ctor.name) {
                    if let Some(n) = self.niche_enum_instance(&ctor.name, &aty) {
                        return self.c_type(&n.payload);
                    }
                }
                self.gen_struct_c_name(&ctor.name, &aty)
            }
            TypeKind::Slice(inner) => {
                let subst = self.subst.clone();
                let elem = self.ast_type_to_ty(*inner, &subst);
                self.slice_c_name(&elem)
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
            Ty::Opaque(n) => match self.subst.get(n).cloned() {
                Some(t) => self.c_type(&t),
                None => "int".to_string(),
            },
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
            Ty::GenRef(elem) => self.genref_c_name(elem),
            Ty::RegionRef(elem) => {
                let i = self.c_type(elem);
                format!("{i}*")
            }
            _ => "int".to_string(),
        }
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
                if seen.insert(self.ty_mangle(&elem)) {
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
        out
    }

    /// Emit `typedef struct { T* ptr; size_t len; } JestyrSlice_<T>;` per element.
    fn slice_struct_defs(&mut self) {
        for elem in self.slice_instances.clone() {
            let name = self.slice_c_name(&elem);
            let ecty = self.c_type(&elem);
            self.raw(format!("typedef struct {{ {ecty}* ptr; size_t len; }} {name};\n"));
        }
        if !self.slice_instances.is_empty() {
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

    /// Emit `typedef struct { T* ptr; uint64_t gen; } JestyrRef_<T>;` per element.
    /// A generational reference (§4.4) carries a snapshot of the allocation's
    /// generation; a stale deref (after `gen_free`) faults at runtime.
    fn genref_struct_defs(&mut self) {
        for elem in self.genref_instances.clone() {
            let name = self.genref_c_name(&elem);
            let ecty = self.c_type(&elem);
            self.raw(format!("typedef struct {{ {ecty}* ptr; uint64_t gen; }} {name};\n"));
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

    fn error_tag_of(&self, id: ExprId) -> Option<i64> {
        match &self.ast.expr_at(id).kind {
            ExprKind::Name(n) => self.error_tags.get(&n.name).copied(),
            _ => None,
        }
    }

    // --- monomorphization ---

    fn is_type_param(&self, p: &Param) -> bool {
        p.comptime && p.ty.is_some_and(|t| matches!(self.ast.type_at(t).kind, TypeKind::TypeKw))
    }

    /// A generic function has at least one `comptime <name>: type` parameter.
    fn is_generic(&self, f: &FnDecl) -> bool {
        f.params.iter().any(|p| self.is_type_param(p))
    }

    /// The backend can emit a function if it has no `self` (methods) and no
    /// `comptime` *value* parameters (only `comptime` type parameters are ok).
    fn fn_supported(&self, f: &FnDecl) -> bool {
        f.params.iter().all(|p| !p.is_self && (!p.comptime || self.is_type_param(p)))
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

    fn find_fn(&self, name: &str) -> Option<&'a FnDecl> {
        let ast = self.ast;
        ast.items.iter().find_map(|it| match it {
            Item::Fn(f) if f.name.name == name => Some(f),
            _ => None,
        })
    }

    fn make_subst(&self, f: &FnDecl, args: &[Ty]) -> HashMap<String, Ty> {
        self.type_param_names(f).into_iter().zip(args.iter().cloned()).collect()
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
            Ty::Slice(elem) => format!("slice_{}", self.ty_mangle(elem)),
            Ty::GenRef(elem) => format!("ref_{}", self.ty_mangle(elem)),
            Ty::RegionRef(elem) => format!("rref_{}", self.ty_mangle(elem)),
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
                } else if let Some(&i) = self.info.table.type_index.get(&n.name) {
                    Ty::Named(i)
                } else {
                    Ty::Opaque(n.name.clone())
                }
            }
            _ => Ty::Opaque("?".to_string()),
        }
    }

    fn emit_generic_call(&mut self, name: &str, args: &[ExprId]) -> String {
        let Some(f) = self.find_fn(name) else { return "0".to_string() };
        let cpos = self.comptime_positions(f);
        let subst = self.subst.clone();
        let type_args: Vec<Ty> =
            cpos.iter().filter_map(|&p| args.get(p)).map(|a| self.eval_type_arg(*a, &subst)).collect();
        let mangled = self.mangle(name, &type_args);

        let mut parts = Vec::new();
        for (i, a) in args.iter().enumerate() {
            if cpos.contains(&i) {
                continue; // type argument — erased
            }
            let e = self.emit_expr(*a);
            let conv = f.params.get(i).map(|p| p.conv).unwrap_or(Conv::Default);
            if matches!(conv, Conv::Mut | Conv::Out) {
                parts.push(format!("&({e})"));
            } else {
                parts.push(e);
            }
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
                            let type_args: Vec<Ty> = self
                                .comptime_positions(gf)
                                .iter()
                                .filter_map(|&p| args.get(p))
                                .map(|a| self.eval_type_arg(*a, subst))
                                .collect();
                            work.push(Work::Fn(qname.clone(), type_args));
                        }
                    }
                } else if let ExprKind::Name(n) = &ast.expr_at(*callee).kind {
                    if self.generics.contains(&n.name) {
                        if let Some(gf) = self.find_fn(&n.name) {
                            let type_args: Vec<Ty> = self
                                .comptime_positions(gf)
                                .iter()
                                .filter_map(|&p| args.get(p))
                                .map(|a| self.eval_type_arg(*a, subst))
                                .collect();
                            work.push(Work::Fn(n.name.clone(), type_args));
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

/// Is `name` a backend intrinsic (a prelude stand-in for the stdlib / C interop)?
/// Used so a reference to one is not mistaken for a closure capture.
fn is_intrinsic(name: &str) -> bool {
    matches!(
        name,
        "print_int" | "print_float" | "print_str" | "print_bool"
            | "alloc" | "alloc_i32" | "realloc" | "realloc_i32" | "free_ptr" | "size_of" | "slice"
            | "align_of" | "offset_of" | "count_codepoints" | "codepoints" | "from_utf8" | "is_utf8"
            | "substr" | "str_eq" | "starts_with" | "ends_with" | "contains" | "find" | "trim"
            | "count_graphemes" | "graphemes" | "split" | "try_from_utf8"
            | "string_new" | "string_from" | "string_push" | "string_view" | "string_free"
            | "builder_new" | "builder_push" | "builder_build" | "builder_free"
            | "region_str" | "region_concat" | "bytes"
            | "gen_new" | "gen_free" | "region_alloc" | "ok" | "err" | "is_err" | "unwrap"
            | "arena_open" | "arena_alloc" | "arena_close"
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
        Ty::Slice(elem) => Ty::Slice(Box::new(apply_subst(elem, subst))),
        Ty::GenRef(elem) => Ty::GenRef(Box::new(apply_subst(elem, subst))),
        Ty::RegionRef(elem) => Ty::RegionRef(Box::new(apply_subst(elem, subst))),
        _ => t.clone(),
    }
}

/// Re-render a Jestyr integer literal as valid C (strip `_`, convert binary).
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
        "cstr" => "const char*",
        "String" => "JestyrString",
        "Builder" => "JestyrBuilder",
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

    /// Like [`gen`], but lowers in test-harness mode (`jestyrc test`).
    fn gen_tests(src: &str) -> (String, Vec<Diagnostic>) {
        let (tokens, ld) = Lexer::new(src).tokenize();
        assert!(ld.is_empty(), "lex: {:?}", ld);
        let (ast, pd) = Parser::new(src, tokens).parse();
        assert!(pd.is_empty(), "parse: {:?}", pd);
        let (info, _td) = crate::typeck::check(&ast);
        emit_tests(&ast, &info)
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

    #[test]
    fn maps_print_intrinsic_and_emits_main_wrapper() {
        let (c, d) = gen("fn main() -> i32 { print_int(42) return 0 }");
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("jestyr_rt_print_int(42)"), "{c}");
        assert!(c.contains("int main(void) { return (int) jestyr_main(); }"), "{c}");
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

    #[test]
    fn layout_reflection_align_of_and_offset_of() {
        let src = "struct M { a: u8, b: i32, c: u8 } \
                   fn main() -> i32 { print_int(align_of(M)) print_int(offset_of(M, b)) return 0 }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("_Alignof(Jestyr_M)"), "align_of → _Alignof: {c}");
        assert!(c.contains("offsetof(Jestyr_M, j_b)"), "offset_of → offsetof with j_ field: {c}");
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
    fn try_from_utf8_returns_a_recoverable_result() {
        let src = "fn f(b: []u8) -> i32 { let r = try_from_utf8(b) if is_err(r) { return -1 } return unwrap(r).len as i32 }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("JestyrResult_str j_r"), "result-typed binding (no annotation): {c}");
        assert!(c.contains("(JestyrResult_str){ .is_err = false"), "ok construction: {c}");
        assert!(c.contains(".ok).len"), "unwrap(r).len projects the str length: {c}");
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
