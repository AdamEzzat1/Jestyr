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

pub fn emit(ast: &Ast, info: &TypeInfo) -> (String, Vec<Diagnostic>) {
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
    };
    g.spawn_sites = g.collect_spawns();
    g.slice_instances = g.collect_slices();
    g.genref_instances = g.collect_genrefs();
    let (instances, method_instances) = g.collect_all_instances();
    g.instances = instances;
    g.method_instances = method_instances;
    g.struct_instances = g.collect_struct_instances();
    let (closures, closure_index) = g.collect_closures();
    g.closures = closures;
    g.closure_index = closure_index;
    g.prelude();
    g.forward_types();
    g.struct_defs();
    g.enum_defs();
    g.gen_struct_defs();
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
    g.main_wrapper();
    (g.out, g.diags)
}

#[derive(Clone)]
struct VariantInfo {
    enum_name: String,
    fields: Vec<(String, TypeId)>,
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
        self.raw("\n");
        self.raw("/* Jestyr runtime prelude — temporary print intrinsics (stand-in for a stdlib). */\n");
        self.raw("static void jestyr_rt_print_int(int64_t x) { printf(\"%lld\\n\", (long long) x); }\n");
        self.raw("static void jestyr_rt_print_float(double x) { printf(\"%g\\n\", x); }\n");
        self.raw("static void jestyr_rt_print_str(const char* s) { printf(\"%s\\n\", s); }\n");
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
                Item::Struct { name, .. } => {
                    self.raw(format!("typedef struct Jestyr_{0} Jestyr_{0};\n", name.name));
                }
                Item::Enum(e) => {
                    self.raw(format!("typedef struct Jestyr_{0} Jestyr_{0};\n", e.name.name));
                }
                _ => {}
            }
        }
        self.raw("\n");
    }

    /// Lower each enum to a tagged union: a `tag` enum plus a `union` of the
    /// payload-carrying variants. Nullary variants contribute a tag constant but
    /// no union member.
    fn enum_defs(&mut self) {
        let ast = self.ast;
        for item in &ast.items {
            if let Item::Enum(e) = item {
                let en = &e.name.name;
                self.raw(format!("enum Jestyr_{en}_tag {{\n"));
                for v in &e.variants {
                    self.raw(format!("    Jestyr_{en}_{},\n", v.name.name));
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
            TypeKind::App { ctor, args } => Ty::GenStruct {
                ctor: ctor.name.clone(),
                args: args.iter().map(|a| self.ast_type_to_ty(*a, subst)).collect(),
            },
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
            ExprKind::StructLit { fields, .. } => {
                for fi in fields {
                    self.collect_structs_in_expr(fi.value, subst, seen, order);
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

    /// Emit one tagged result struct per distinct ok-type used by a fallible
    /// function: `{ bool is_err; <T> ok; int err; }`.
    fn result_defs(&mut self) {
        let ast = self.ast;
        let mut seen: HashSet<String> = HashSet::new();
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
            if let Item::Struct { name, body, attrs, .. } = item {
                let attr = self.struct_attr(attrs);
                self.raw(format!("struct{attr} Jestyr_{} {{\n", name.name));
                for m in &body.members {
                    if let StructMember::Field { name: fname, ty, volatile, .. } = m {
                        let cty = self.c_ty_ast(*ty);
                        let vol = if *volatile { "volatile " } else { "" };
                        self.raw(format!("    {vol}{cty} j_{};\n", fname.name));
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
                let sig = self.fn_signature(f, &format!("jestyr_{}", f.name.name));
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
                self.raw(format!("static const {cty} j_{} = {v};\n", c.name.name));
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
                self.emit_fn(f, &format!("jestyr_{}", f.name.name));
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

    fn fn_signature(&mut self, f: &FnDecl, c_name: &str) -> String {
        let ret = self.ret_type(f);
        let params = self.params_str(f);
        format!("{ret} {c_name}({params})")
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

    /// Lower a `match` on an enum to a `switch` on the tag. The scrutinee is
    /// spilled to a temporary so it is evaluated exactly once.
    fn emit_match(&mut self, e: ExprId, ret: bool) {
        let ast = self.ast;
        let (scrut, arms) = match &ast.expr_at(e).kind {
            ExprKind::Match { scrut, arms } => (*scrut, arms),
            _ => return,
        };

        let scrut_ty = self.info.type_of(scrut).clone();
        let enum_name = match &scrut_ty {
            Ty::Named(i)
                if matches!(self.info.table.types[*i].kind, TypeKindG::Enum { .. }) =>
            {
                self.info.table.types[*i].name.clone()
            }
            _ => {
                self.diag(ast.expr_at(e).span, "the C backend only supports `match` on enum values");
                return;
            }
        };

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
                    self.line(format!("case Jestyr_{enum_name}_{}:", vname.name));
                    self.line("{");
                    self.depth += 1;
                    if let Some(vi) = self.variants.get(&vname.name).cloned() {
                        for (i, sp) in subpats.iter().enumerate() {
                            if let PatKind::Ident(bind) = &ast.pat_at(*sp).kind {
                                if let Some((fname, fty)) = vi.fields.get(i) {
                                    let fcty = self.c_ty_ast(*fty);
                                    self.line(format!(
                                        "{fcty} j_{} = {tmp}.u.{}.j_{fname};",
                                        bind.name, vname.name
                                    ));
                                }
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
                    self.line(format!("case Jestyr_{enum_name}_{}:", vname.name));
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

    fn emit_variant_construct(&mut self, vi: &VariantInfo, vname: &str, args: &[ExprId]) -> String {
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
            ExprKind::Str(l) => l.clone(),
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
                        return self.emit_variant_construct(&vi, &vname, &[]);
                    }
                }
                if self.ptr_params.contains(&n.name) {
                    format!("(*j_{})", n.name)
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
                } else if matches!(bt, Ty::Prim("str")) && name.name == "len" {
                    format!("strlen({b})") // a string's length is its strlen
                } else {
                    format!("{b}.j_{}", name.name)
                }
            }
            ExprKind::Index { base, index } => {
                let bt = self.info.type_of(*base).clone();
                let proven = matches!(bt, Ty::Slice(_)) && self.index_in_range(*base, *index);
                let b = self.emit_expr(*base);
                let i = self.emit_expr(*index);
                if !matches!(bt, Ty::Slice(_)) {
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
                let e = self.emit_expr(*expr);
                format!("({cty})({e})")
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
            ExprKind::StructLit { path, fields } => {
                if path.name == "Self" {
                    self.diag(span, "the C backend does not support `Self { .. }` (methods) yet");
                    return "0".to_string();
                }
                let mut s = format!("(Jestyr_{}){{ ", path.name);
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
                return self.emit_variant_construct(&vi, &vname, args);
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
            return format!("jestyr_{}({})", n.name, parts.join(", "));
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
            format!("jestyr_{}({})", name, parts.join(", "))
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
            let nm = if targs.is_empty() {
                format!("jestyr_{}", mr.fn_name)
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
            ExprKind::StructLit { fields, .. } | ExprKind::GenStructLit { fields, .. } => {
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
            ExprKind::StructLit { fields, .. } | ExprKind::GenStructLit { fields, .. } => {
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

        let ret = match f.ret_ty {
            Some(t) => self.c_ty_ast(t),
            None => "void".to_string(),
        };
        let cname = self.method_c_name(ctor, args, &f.name.name);
        let params = self.method_params_str(f);
        if body {
            self.raw(format!("{ret} {cname}({params})\n"));
            self.emit_body(&f.body, ret != "void");
            self.raw("\n");
        } else {
            self.raw(format!("{ret} {cname}({params});\n"));
        }

        self.ptr_params.clear();
        self.self_cty.clear();
        self.self_is_ptr = false;
        self.subst.clear();
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
            self.raw(format!("static void* jestyr_task_{id}(void* _vp) {{ "));
            self.raw(format!("struct _jsp_{id}* _a = (struct _jsp_{id}*)_vp; "));
            self.raw(format!("jestyr_{}({}); return NULL; }}\n", site.fn_name, call_args.join(", ")));
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
                } else if matches!(self.info.type_of(src), Ty::Prim("str")) {
                    // String iteration — byte by byte (via strlen).
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
    fn emit_str_for(&mut self, binding: &Ident, index: Option<&Ident>, iter: ExprId, body: &Block) {
        let s = self.emit_expr(iter);
        let n = self.tmp;
        self.tmp += 1;
        self.line(format!("const char* _str{n} = {s};"));
        self.line(format!("size_t _len{n} = strlen(_str{n});"));
        self.line(format!("for (size_t _k{n} = 0; _k{n} < _len{n}; _k{n}++)"));
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
            self.line(format!("uint8_t j_{} = (uint8_t)_str{n}[_k{n}];", binding.name));
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
            Ty::Named(i) => format!("Jestyr_{}", self.info.table.types[*i].name),
            // an inferred type parameter (e.g. `T`) under the active substitution
            Ty::Opaque(n) => match self.subst.get(n).cloned() {
                Some(t) => self.c_type(&t),
                None => "int".to_string(),
            },
            Ty::Result(ok) => self.result_c_name(ok),
            Ty::GenStruct { ctor, args } => self.gen_struct_c_name(ctor, args),
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
            ExprKind::StructLit { fields, .. } | ExprKind::GenStructLit { fields, .. } => {
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
        "str" => "const char*",
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
        let src = "extern \"c\" fn puts(s: str) -> i32 fn main() -> i32 { puts(\"hi\") return 0 }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("int32_t puts(const char* j_s);"), "extern prototype: {c}");
        assert!(c.contains("puts(\"hi\")"), "called by bare C name: {c}");
        assert!(!c.contains("jestyr_puts"), "extern names are not mangled: {c}");
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
    fn lowers_string_iteration_via_strlen() {
        let src = "fn f(s: str) -> i32 { var t: i32 = 0 for c in s { t = t + (c as i32) } return t }";
        let (c, d) = gen(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("strlen("), "length via strlen: {c}");
        assert!(c.contains("uint8_t j_c = (uint8_t)"), "each byte binds as u8: {c}");
    }

    #[test]
    fn lowers_string_len_to_strlen() {
        let (c, d) = gen("fn f(s: str) -> i32 { return s.len as i32 }");
        assert!(d.is_empty(), "{:?}", d);
        assert!(c.contains("strlen("), "str.len → strlen: {c}");
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
}
