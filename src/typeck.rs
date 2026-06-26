//! Stages ③ + ④: name resolution and type checking, interleaved.
//!
//! Real compilers usually fuse these two: resolving a name *is* looking up its
//! declaration, and you need that declaration to know its type. So this single
//! pass:
//!
//!   1. **builds the global table** (`build_table`) — two phases so that types
//!      can refer to each other regardless of source order (no forward decls);
//!   2. **checks every function body** (`check_items`) — walking expressions,
//!      resolving each name/field/variant to a declaration, inferring a `Ty` for
//!      every expression, and recording it in [`TypeInfo`].
//!
//! It reports the errors it can be *confident* about without a standard library:
//! unknown fields on known structs, call-arity mismatches on known functions,
//! and duplicate top-level definitions. Unknown bare names and external type
//! names are left opaque (deferred until there's a prelude to resolve them).
//!
//! Generics are handled leniently: a function's `comptime … : type` parameters
//! become in-scope *opaque* type parameters, so `Vec(comptime T: type)` and the
//! methods of the struct it returns type-check without monomorphization.

use std::collections::{HashMap, HashSet};

use crate::ast::*;
use crate::diag::Diagnostic;
use crate::module::{ModId, Modules};
use crate::span::Span;
use crate::types::*;

type Scope = Vec<HashMap<String, Ty>>;

/// Type-check a single-module program (the unit-test / single-file entry point;
/// the multi-file driver goes through [`check_program`]).
#[allow(dead_code)]
pub fn check(ast: &Ast) -> (TypeInfo, Vec<Diagnostic>) {
    check_program(ast, &Modules::single(ast))
}

/// Type-check a (possibly multi-module) program. `modules` says which module each
/// item belongs to, what each module imports, and what is `pub` — so the checker
/// can enforce visibility and resolve qualified access (`mem.allocate`).
pub fn check_program(ast: &Ast, modules: &Modules) -> (TypeInfo, Vec<Diagnostic>) {
    let owner = build_owner(ast, modules);
    let mut tc = TypeChecker {
        ast,
        modules,
        owner,
        cur_mod: 0,
        table: GlobalTable::default(),
        expr_types: vec![Ty::Unknown; ast.exprs.len()],
        method_calls: HashMap::new(),
        qualified: HashMap::new(),
        impl_calls: HashMap::new(),
        bound_method_calls: HashMap::new(),
        cur_type_param_bounds: HashMap::new(),
        cur_expected: None,
        cur_ret: None,
        diags: Vec::new(),
    };
    tc.build_table();
    tc.check_items();
    (
        TypeInfo {
            table: tc.table,
            expr_types: tc.expr_types,
            method_calls: tc.method_calls,
            qualified: tc.qualified,
            impl_calls: tc.impl_calls,
            bound_method_calls: tc.bound_method_calls,
        },
        tc.diags,
    )
}

/// The owning module and visibility of every named top-level item — the basis
/// for cross-module visibility checks and qualified-access resolution.
fn build_owner(ast: &Ast, modules: &Modules) -> HashMap<String, (ModId, bool)> {
    let mut owner = HashMap::new();
    for (i, item) in ast.items.iter().enumerate() {
        let m = *modules.item_mod.get(i).unwrap_or(&0);
        let is_pub = *modules.item_pub.get(i).unwrap_or(&true);
        let name = match item {
            Item::Fn(f) => Some(f.name.name.clone()),
            Item::Enum(e) => Some(e.name.name.clone()),
            Item::Const(c) => Some(c.name.name.clone()),
            Item::Struct { name, .. } => Some(name.name.clone()),
            Item::Distinct(d) => Some(d.name.name.clone()),
            Item::Extern(e) => Some(e.name.name.clone()),
            Item::Trait(t) => Some(t.name.name.clone()),
            Item::Impl(_) | Item::Import(_) => None,
        };
        if let Some(n) = name {
            owner.entry(n).or_insert((m, is_pub));
        }
    }
    owner
}

struct TypeChecker<'a> {
    ast: &'a Ast,
    modules: &'a Modules,
    /// name → (owning module, is_pub), for visibility + qualified resolution.
    owner: HashMap<String, (ModId, bool)>,
    /// The module whose item is currently being checked.
    cur_mod: ModId,
    table: GlobalTable,
    expr_types: Vec<Ty>,
    /// `Call`-expr id → method resolution (see [`MethodRes`]).
    method_calls: HashMap<ExprId, MethodRes>,
    /// Qualified access (`mem.allocate` / `mem.PAGE`) → the resolved bare name.
    qualified: HashMap<ExprId, String>,
    /// `Call`-expr id → trait-impl method resolution (traits, Stage B).
    impl_calls: HashMap<ExprId, ImplCall>,
    /// `Call`-expr id → a method call on a *bracket type parameter* resolved
    /// through its bound (the "Zig fix"). The concrete impl is selected per
    /// monomorphized instance in the backend, via the active type substitution.
    bound_method_calls: HashMap<ExprId, BoundMethodCall>,
    /// The bracket type parameters in scope for the function being checked, each
    /// mapped to its declared bound (`None` if unbounded). Drives the body-side
    /// "only the bound's methods are callable on a `T` value" check.
    cur_type_param_bounds: HashMap<String, Option<String>>,
    /// The type a sub-expression is *expected* to have (from a `let` annotation
    /// or a `return`), used to resolve an otherwise-ambiguous nullary generic
    /// variant like `none` to its instantiation (`Option(i32)`). A minimal,
    /// targeted bit of bidirectional inference — not a general expected-type pass.
    cur_expected: Option<Ty>,
    /// The return type of the function currently being checked — the expected type
    /// for a `return <expr>`.
    cur_ret: Option<Ty>,
    diags: Vec<Diagnostic>,
}

impl<'a> TypeChecker<'a> {
    fn error(&mut self, span: Span, message: impl Into<String>) {
        self.diags.push(Diagnostic::new(message, span));
    }

    /// A non-fatal warning (e.g. a redundant match arm) — reported but the build
    /// still succeeds.
    fn warn(&mut self, span: Span, message: impl Into<String>) {
        self.diags.push(Diagnostic::warning(message, span));
    }

    fn set(&mut self, id: ExprId, ty: Ty) -> Ty {
        self.expr_types[id.0 as usize] = ty.clone();
        ty
    }

    // --- building the global table ---

    fn build_table(&mut self) {
        let ast = self.ast;

        // Phase 1: register the names of all user types so they can be referred
        // to in any order.
        for item in &ast.items {
            match item {
                Item::Struct { name, is_record, attrs, .. } => {
                    let i = self.register_type(name, false);
                    self.table.types[i].is_record = *is_record;
                    // `@copy` opts a small aggregate into being freely copyable
                    // (design §2.8) — the escape checker then never treats it as a
                    // move/borrow that could escape.
                    self.table.types[i].is_copy = attrs.iter().any(|a| a.name == "copy");
                }
                Item::Enum(e) => {
                    let idx = self.register_type(&e.name, true);
                    self.table.types[idx].type_params =
                        e.type_params.iter().map(|p| p.name.clone()).collect();
                    for v in &e.variants {
                        self.table.variants.insert(v.name.name.clone(), idx);
                    }
                }
                Item::Distinct(d) => {
                    // Register the name now; the base type is lowered in phase 2.
                    if self.table.type_index.contains_key(&d.name.name) {
                        self.error(d.name.span, format!("duplicate definition of `{}`", d.name.name));
                    } else {
                        let idx = self.table.types.len();
                        self.table.types.push(TypeDecl {
                            name: d.name.name.clone(),
                            kind: TypeKindG::Distinct { base: Ty::Unknown },
                            is_copy: false,
                            is_record: false,
                            type_params: Vec::new(),
                        });
                        self.table.type_index.insert(d.name.name.clone(), idx);
                    }
                }
                _ => {}
            }
        }

        // Phase 2: lower field/variant/parameter/return types now that every
        // type name has an index.
        let empty = HashSet::new();
        for item in &ast.items {
            match item {
                Item::Struct { name, body, .. } => {
                    let self_idx = self.table.type_index.get(&name.name).copied();
                    let mut fields = Vec::new();
                    for m in &body.members {
                        if let StructMember::Field { name: fname, ty, .. } = m {
                            let fty = self.lower_type(&empty, *ty);
                            if let Some(si) = self_idx {
                                self.check_no_value_recursion(si, self.ast.type_at(*ty).span, &fty);
                            }
                            fields.push((fname.name.clone(), fty));
                        }
                    }
                    if let Some(&i) = self.table.type_index.get(&name.name) {
                        if let TypeKindG::Struct { fields: slot } = &mut self.table.types[i].kind {
                            *slot = fields;
                        }
                    }
                }
                Item::Enum(e) => {
                    // A generic enum's variant field types may mention its type
                    // parameters (`some(x: T)`), so lower them with those in scope.
                    let tp: HashSet<String> =
                        e.type_params.iter().map(|p| p.name.clone()).collect();
                    let self_idx = self.table.type_index.get(&e.name.name).copied();
                    let mut variants = Vec::new();
                    for v in &e.variants {
                        let mut ftys = Vec::new();
                        for (_, t) in &v.fields {
                            let fty = self.lower_type(&tp, *t);
                            if let Some(si) = self_idx {
                                self.check_no_value_recursion(si, self.ast.type_at(*t).span, &fty);
                            }
                            ftys.push(fty);
                        }
                        variants.push((v.name.name.clone(), ftys));
                    }
                    if let Some(&i) = self.table.type_index.get(&e.name.name) {
                        if let TypeKindG::Enum { variants: slot } = &mut self.table.types[i].kind {
                            *slot = variants;
                        }
                    }
                }
                Item::Distinct(d) => {
                    // Lower the base type; a distinct type is `Copy` iff its base is.
                    let base = self.lower_type(&empty, d.base);
                    let copy = base.is_copy(&self.table);
                    if let Some(&i) = self.table.type_index.get(&d.name.name) {
                        self.table.types[i].is_copy = copy;
                        if let TypeKindG::Distinct { base: slot } = &mut self.table.types[i].kind {
                            *slot = base;
                        }
                    }
                }
                Item::Fn(f) => {
                    let typ = self.fn_type_params(f, &empty);
                    let params: Vec<ParamSig> = f
                        .params
                        .iter()
                        .map(|p| {
                            let ty = if p.is_self {
                                Ty::Opaque("Self".to_string())
                            } else if let Some(t) = p.ty {
                                self.lower_type(&typ, t)
                            } else {
                                Ty::Unknown
                            };
                            ParamSig { name: p.name.name.clone(), conv: p.conv, ty }
                        })
                        .collect();
                    let ret = f.ret_ty.map(|t| self.lower_type(&typ, t)).unwrap_or(Ty::Unit);
                    if self.table.fns.contains_key(&f.name.name) {
                        self.error(f.name.span, format!("duplicate definition of `{}`", f.name.name));
                    }
                    self.table.fns.insert(
                        f.name.name.clone(),
                        FnSig { params, ret, ret_conv: f.ret_conv, fallible: f.errors.is_some() },
                    );
                }
                Item::Const(c) => {
                    let t = c.ty.map(|t| self.lower_type(&empty, t)).unwrap_or(Ty::Unknown);
                    self.table.consts.insert(c.name.name.clone(), t);
                }
                Item::Extern(e) => {
                    let params: Vec<ParamSig> = e
                        .params
                        .iter()
                        .map(|p| {
                            let ty = p.ty.map(|t| self.lower_type(&empty, t)).unwrap_or(Ty::Unknown);
                            ParamSig { name: p.name.name.clone(), conv: p.conv, ty }
                        })
                        .collect();
                    let ret = e.ret_ty.map(|t| self.lower_type(&empty, t)).unwrap_or(Ty::Unit);
                    if self.table.fns.contains_key(&e.name.name) {
                        self.error(e.name.span, format!("duplicate definition of `{}`", e.name.name));
                    }
                    self.table.fns.insert(
                        e.name.name.clone(),
                        FnSig { params, ret, ret_conv: e.ret_conv, fallible: false },
                    );
                }
                Item::Import(_) => {}
                // Traits and impls are registered in their own pass below (impls
                // need every type lowered first, to key by the target type).
                Item::Trait(_) | Item::Impl(_) => {}
            }
        }

        // Phase 3: built-in operator traits, then user traits, then impls
        // (coherence-checked against both). Operator traits are registered first
        // so a user `impl Add for T` resolves, and a user `trait Add` collides.
        self.register_operator_traits();
        self.register_traits();
        self.register_impls();
        // Phase 4: definition-site bounds (traits, Stage D) — every declared
        // bound `[T: Tr]` must name a registered trait (the per-call obligation
        // that a *concrete* type satisfies the bound is checked in `infer`).
        self.check_bound_traits_declared();
    }

    /// Register the built-in **operator traits** (traits, Stage E): `+`/`*`/`==`/`<`
    /// desugar to `Add`/`Mul`/`Eq`/`Ord` methods, so a user type opts into operator
    /// syntax by `impl`-ing the matching trait. These are synthetic (no AST
    /// `trait` item); a user type's `impl Add for T` then resolves and lowers
    /// through the Stage C static-dispatch path.
    fn register_operator_traits(&mut self) {
        for (name, method) in OPERATOR_TRAITS {
            self.table
                .traits
                .entry(name.to_string())
                .or_insert_with(|| TraitDef { methods: vec![(method.to_string(), true)] });
        }
    }

    /// Register each `trait` (name → its method set, flagged required/defaulted).
    fn register_traits(&mut self) {
        let ast = self.ast;
        for item in &ast.items {
            if let Item::Trait(t) = item {
                if self.table.traits.contains_key(&t.name.name) {
                    self.error(
                        t.name.span,
                        format!("duplicate definition of trait `{}`", t.name.name),
                    );
                    continue;
                }
                let methods = t
                    .methods
                    .iter()
                    .map(|m| (m.name.name.clone(), m.default_body.is_none()))
                    .collect();
                self.table.traits.insert(t.name.name.clone(), TraitDef { methods });
            }
        }
    }

    /// Register each `impl Trait for Type`, enforcing **coherence**: the trait must
    /// exist; at most one impl per `(trait, type)`; every required method present;
    /// no method that isn't a trait member. Records each provided method's return
    /// type (with `Self` resolved to the target) for resolution + the backend.
    fn register_impls(&mut self) {
        let ast = self.ast;
        let empty = HashSet::new();
        for item in &ast.items {
            let Item::Impl(im) = item else { continue };
            if !self.table.traits.contains_key(&im.trait_name.name) {
                self.error(
                    im.trait_name.span,
                    format!("unknown trait `{}` in `impl`", im.trait_name.name),
                );
                continue;
            }
            let target = self.lower_type(&empty, im.ty);
            let type_key = self.table.ty_key(&target);
            let pair = (im.trait_name.name.clone(), type_key.clone());
            if self.table.impl_index.contains_key(&pair) {
                self.error(
                    im.span,
                    format!(
                        "conflicting implementations of trait `{}` for `{}` (coherence: at most one)",
                        im.trait_name.name, type_key
                    ),
                );
                continue;
            }
            let self_subst: HashMap<String, Ty> =
                std::iter::once(("Self".to_string(), target.clone())).collect();
            let mut method_rets = HashMap::new();
            let mut provided: HashSet<String> = HashSet::new();
            for m in &im.methods {
                let tps = self.fn_type_params(m, &empty);
                let ret = m.ret_ty.map(|t| self.lower_type(&tps, t)).unwrap_or(Ty::Unit);
                let ret = subst_ty(&ret, &self_subst);
                let is_member = self.table.traits[&im.trait_name.name].has_method(&m.name.name);
                if !is_member {
                    self.error(
                        m.name.span,
                        format!(
                            "method `{}` is not a member of trait `{}`",
                            m.name.name, im.trait_name.name
                        ),
                    );
                }
                method_rets.insert(m.name.name.clone(), ret);
                provided.insert(m.name.name.clone());
            }
            let missing: Vec<String> = self.table.traits[&im.trait_name.name]
                .required()
                .filter(|r| !provided.contains(*r))
                .map(|s| s.to_string())
                .collect();
            for miss in missing {
                self.error(
                    im.span,
                    format!(
                        "missing method `{miss}` in `impl` of trait `{}` for `{type_key}`",
                        im.trait_name.name
                    ),
                );
            }
            let idx = self.table.impls.len();
            self.table.impls.push(ImplDef {
                trait_name: im.trait_name.name.clone(),
                type_key,
                method_rets,
            });
            self.table.impl_index.insert(pair, idx);
        }
    }

    /// Resolve `recv.method(args)` through an `impl Trait for <recv-type>`. Returns
    /// the method's return type and records the resolution for the backend.
    fn resolve_impl_method(&mut self, call_id: ExprId, method: &str, recv_ty: &Ty) -> Option<Ty> {
        let key = self.table.ty_key(recv_ty);
        let (trait_name, ret) = {
            let im = self
                .table
                .impls
                .iter()
                .find(|im| im.type_key == key && im.method_rets.contains_key(method))?;
            (im.trait_name.clone(), im.method_rets.get(method).cloned().unwrap_or(Ty::Unknown))
        };
        self.impl_calls.insert(call_id, ImplCall { trait_name, type_key: key, method: method.to_string() });
        Some(ret)
    }

    /// The "Zig fix" (design §8.2): inside a bracket-generic body `f[T: Tr]`, a
    /// method call `x.m()` on a value of the type parameter `T` resolves *through
    /// the bound* `Tr`. It type-checks iff `m` is one of `Tr`'s methods (typed by
    /// `m`'s declared return); a method **not** in the bound — or any method on an
    /// *unbounded* `[U]` — is a **definition-site** error ("blame the generic code,
    /// not the caller"). Records the resolution so the backend can dispatch to the
    /// concrete `impl` per monomorphized instance. `None` ⇒ the receiver isn't a
    /// bracket type parameter of the enclosing function (try other resolutions).
    fn resolve_bound_method(&mut self, call_id: ExprId, mname: &str, recv_ty: &Ty) -> Option<Ty> {
        let Ty::Opaque(tp) = recv_ty else { return None };
        let bound = self.cur_type_param_bounds.get(tp)?.clone();
        let span = self.ast.expr_at(call_id).span;
        let Some(tr) = bound else {
            self.error(
                span,
                format!("no method `{mname}` on unbounded type parameter `{tp}` — add a bound `[{tp}: Trait]`"),
            );
            return Some(Ty::Error);
        };
        if !self.table.traits.get(&tr).is_some_and(|t| t.has_method(mname)) {
            self.error(
                span,
                format!("no method `{mname}` on type parameter `{tp}`: its bound `{tr}` has no such method"),
            );
            return Some(Ty::Error);
        }
        self.bound_method_calls.insert(
            call_id,
            BoundMethodCall { trait_name: tr.clone(), method: mname.to_string(), type_param: tp.clone() },
        );
        Some(self.trait_method_ret(&tr, mname, recv_ty))
    }

    /// The declared return type of trait `trait_name`'s method `mname`, with `Self`
    /// resolved to `self_ty` (here the opaque type parameter). `Unknown` for a
    /// synthetic (operator) trait or an absent method — best-effort typing.
    fn trait_method_ret(&self, trait_name: &str, mname: &str, self_ty: &Ty) -> Ty {
        for item in &self.ast.items {
            if let Item::Trait(t) = item {
                if t.name.name == trait_name {
                    for m in &t.methods {
                        if m.name.name == mname {
                            let r =
                                m.ret_ty.map(|ty| self.lower_type(&HashSet::new(), ty)).unwrap_or(Ty::Unit);
                            let subst: HashMap<String, Ty> =
                                std::iter::once(("Self".to_string(), self_ty.clone())).collect();
                            return subst_ty(&r, &subst);
                        }
                    }
                }
            }
        }
        Ty::Unknown
    }

    /// Definition-site bounds (traits, Stage D), the declaration half: every
    /// bracket-generic bound `[T: Tr]` must name a registered trait — a typo or
    /// undeclared trait is caught at the *definition*, not silently ignored (which
    /// is what would happen at the call site, where an unknown bound is skipped).
    /// Covers free functions and their `impl`/struct method counterparts.
    fn check_bound_traits_declared(&mut self) {
        let ast = self.ast;
        // Gather every function that can carry bracket-form generics (free fns,
        // `impl` methods, struct methods) — `&'a` borrows, independent of `self`.
        let mut fns: Vec<&FnDecl> = Vec::new();
        for item in &ast.items {
            match item {
                Item::Fn(f) => fns.push(f),
                Item::Impl(im) => fns.extend(im.methods.iter()),
                Item::Struct { body, .. } => {
                    for m in &body.members {
                        if let StructMember::Method(f) = m {
                            fns.push(f);
                        }
                    }
                }
                _ => {}
            }
        }
        let mut errs: Vec<(Span, String)> = Vec::new();
        for f in fns {
            for g in &f.generics {
                if let Some(b) = &g.bound {
                    if !self.table.traits.contains_key(&b.name) {
                        errs.push((
                            b.span,
                            format!(
                                "unknown trait `{}` in bound on type parameter `{}`",
                                b.name, g.name.name
                            ),
                        ));
                    }
                }
            }
        }
        for (sp, msg) in errs {
            self.error(sp, msg);
        }
    }

    /// Definition-site bounds (traits, Stage D), the call-site obligation: at a
    /// call to a bracket-generic `f[T: Tr](…)`, recover each bounded `T`'s
    /// concrete type by unifying `f`'s declared parameter types against the actual
    /// argument types, then require `impl Tr for <concrete>` (reusing
    /// `impl_index`). An unsatisfied bound errors *at the call site* — the concrete
    /// type is the caller's, but the obligation is the generic's contract. An
    /// unknown bound trait is left to [`Self::check_bound_traits_declared`]; an
    /// `Unknown`/opaque `T` (e.g. a call nested in another generic) is skipped
    /// rather than risk a false positive.
    fn check_call_bounds(&mut self, name: &str, arg_tys: &[Ty], span: Span) {
        let generics: Vec<(String, Option<String>)> = match self.find_fn_decl(name) {
            Some(f) if !f.generics.is_empty() => f
                .generics
                .iter()
                .map(|g| (g.name.name.clone(), g.bound.as_ref().map(|b| b.name.clone())))
                .collect(),
            _ => return,
        };
        let tps: HashSet<String> = generics.iter().map(|(n, _)| n.clone()).collect();
        let param_tys: Vec<Ty> = self
            .table
            .fns
            .get(name)
            .map(|s| s.params.iter().map(|p| p.ty.clone()).collect())
            .unwrap_or_default();
        let mut subst: HashMap<String, Ty> = HashMap::new();
        for (pt, at) in param_tys.iter().zip(arg_tys) {
            unify_tp(pt, at, &tps, &mut subst);
        }
        let mut violations: Vec<(String, String, String)> = Vec::new();
        for (gname, bound) in &generics {
            let Some(tr) = bound else { continue };
            if !self.table.traits.contains_key(tr) {
                continue; // unknown trait: reported at the definition site
            }
            let Some(concrete) = subst.get(gname) else { continue };
            if matches!(concrete, Ty::Opaque(_) | Ty::Unknown | Ty::Error) {
                continue; // unresolved `T` — don't risk a false positive
            }
            let key = self.table.ty_key(concrete);
            if !self.table.impl_index.contains_key(&(tr.clone(), key.clone())) {
                violations.push((key, tr.clone(), gname.clone()));
            }
        }
        for (ty, tr, param) in violations {
            self.error(
                span,
                format!(
                    "type `{ty}` does not implement trait `{tr}` required by bound `{param}: {tr}` on `{name}`"
                ),
            );
        }
    }

    /// Operator traits (traits, Stage E): a binary `a OP b` whose **left operand
    /// is a user type** resolves through `impl <OpTrait> for <lhs>` and is recorded
    /// for static dispatch (reusing `impl_calls`, keyed by the binary expr). Returns
    /// the result type — the impl method's return (`Add`/`Mul` → the type itself,
    /// `Eq`/`Ord` → `bool`). `None` means "not operator-overloaded" (a non-trait
    /// operator, or a primitive operand using native C ops). A user type used with
    /// a trait-backed operator but lacking the `impl` is an error.
    fn resolve_operator_trait(&mut self, id: ExprId, op: BinOp, lt: &Ty, span: Span) -> Option<Ty> {
        let (trait_name, method) = op_trait_method(op)?;
        // Only user types overload operators; primitives keep native semantics.
        if !matches!(lt, Ty::Named(_) | Ty::GenStruct { .. }) {
            return None;
        }
        let key = self.table.ty_key(lt);
        let resolved: Option<Ty> = self
            .table
            .impls
            .iter()
            .find(|im| {
                im.trait_name == trait_name && im.type_key == key && im.method_rets.contains_key(method)
            })
            .map(|im| im.method_rets.get(method).cloned().unwrap_or(Ty::Unknown));
        match resolved {
            Some(ret) => {
                self.impl_calls.insert(
                    id,
                    ImplCall {
                        trait_name: trait_name.to_string(),
                        type_key: key,
                        method: method.to_string(),
                    },
                );
                Some(ret)
            }
            None => {
                self.error(
                    span,
                    format!(
                        "type `{key}` does not implement `{trait_name}` (the `{}` operator)",
                        op_symbol(op)
                    ),
                );
                Some(Ty::Error)
            }
        }
    }

    fn register_type(&mut self, name: &Ident, is_enum: bool) -> usize {
        if let Some(&i) = self.table.type_index.get(&name.name) {
            self.error(name.span, format!("duplicate definition of `{}`", name.name));
            return i;
        }
        let idx = self.table.types.len();
        let kind = if is_enum {
            TypeKindG::Enum { variants: Vec::new() }
        } else {
            TypeKindG::Struct { fields: Vec::new() }
        };
        self.table.types.push(TypeDecl {
            name: name.name.clone(),
            kind,
            is_copy: false,
            is_record: false,
            type_params: Vec::new(),
        });
        self.table.type_index.insert(name.name.clone(), idx);
        idx
    }

    /// Reject a field that stores the enclosing type *by value* (`struct Node {
    /// next: Node }` / `enum List { cons(tail: List) }`) — it would be infinitely
    /// sized. The fix is an indirection (`indirect T`, `*T`, `&T`, `&[r]T`), which
    /// lowers to a pointer and breaks the cycle. (Catches direct self-reference;
    /// mutual / generic-by-value cycles are left to the C compiler for now.)
    fn check_no_value_recursion(&mut self, self_idx: usize, span: Span, field_ty: &Ty) {
        if matches!(field_ty, Ty::Named(i) if *i == self_idx) {
            let name = self.table.types[self_idx].name.clone();
            self.error(
                span,
                format!(
                    "recursive field would make `{name}` infinitely sized — store it behind an indirection (`indirect {name}` or `*{name}`)"
                ),
            );
        }
    }

    /// Is `t` a `distinct` nominal type?
    fn is_distinct(&self, t: &Ty) -> bool {
        matches!(t, Ty::Named(i) if matches!(self.table.types[*i].kind, TypeKindG::Distinct { .. }))
    }

    /// Should assigning `got` where `ann` is expected be rejected on `distinct`
    /// grounds? Only when a distinct type is involved and the two differ — and
    /// never when either side is loose (`Unknown`/`Opaque`/`Error`), to preserve
    /// the checker's leniency everywhere a distinct type is *not* involved.
    fn distinct_mismatch(&self, ann: &Ty, got: &Ty) -> bool {
        if !(self.is_distinct(ann) || self.is_distinct(got)) {
            return false;
        }
        let loose = |t: &Ty| matches!(t, Ty::Unknown | Ty::Opaque(_) | Ty::Error);
        !loose(ann) && !loose(got) && ann != got
    }

    /// Does `name` denote a generic enum (an `enum Name(T) { … }` template)?
    fn is_generic_enum(&self, name: &str) -> bool {
        self.table.type_index.get(name).is_some_and(|&i| {
            !self.table.types[i].type_params.is_empty()
                && matches!(self.table.types[i].kind, TypeKindG::Enum { .. })
        })
    }

    /// Type a variant construction `vname(args)` / bare `vname` whose owning enum
    /// is `ei`. For a plain enum this is `Named(ei)`; for a *generic* enum, recover
    /// the type arguments by unifying the actual arg types against the variant's
    /// template field types, falling back to the expected type for a nullary variant.
    fn variant_ctor_type(&self, ei: usize, vname: &str, arg_tys: &[Ty]) -> Ty {
        let decl = &self.table.types[ei];
        if decl.type_params.is_empty() {
            return Ty::Named(ei);
        }
        let tps: HashSet<String> = decl.type_params.iter().cloned().collect();
        let fields: Vec<Ty> = match &decl.kind {
            TypeKindG::Enum { variants } => variants
                .iter()
                .find(|(n, _)| n == vname)
                .map(|(_, f)| f.clone())
                .unwrap_or_default(),
            _ => Vec::new(),
        };
        let mut subst: HashMap<String, Ty> = HashMap::new();
        for (tmpl, actual) in fields.iter().zip(arg_tys) {
            unify_tp(tmpl, actual, &tps, &mut subst);
        }
        let inferred: Vec<Ty> = decl
            .type_params
            .iter()
            .map(|p| subst.get(p).cloned().unwrap_or(Ty::Unknown))
            .collect();
        // Args fully recovered from the call → use them.
        if inferred.iter().all(|t| *t != Ty::Unknown) {
            return Ty::GenEnum { ctor: decl.name.clone(), args: inferred };
        }
        // Otherwise (a nullary variant) adopt the expected instantiation if it matches.
        if let Some(Ty::GenEnum { ctor, args }) = &self.cur_expected {
            if *ctor == decl.name {
                return Ty::GenEnum { ctor: decl.name.clone(), args: args.clone() };
            }
        }
        Ty::GenEnum { ctor: decl.name.clone(), args: inferred }
    }

    /// If `t` (or what it points/refers to) is an immutable `record`, its name —
    /// used to reject `r.field = …`. Looks through one level of `*T`/`&T`/`&[r]T`
    /// so a record is immutable however it's reached.
    fn record_name(&self, t: &Ty) -> Option<String> {
        let inner = match t {
            Ty::Ptr { inner, .. } | Ty::GenRef(inner) | Ty::RegionRef(inner) => inner.as_ref(),
            other => other,
        };
        if let Ty::Named(i) = inner {
            let d = self.table.types.get(*i)?;
            if d.is_record {
                return Some(d.name.clone());
            }
        }
        None
    }

    /// The set of generic type-parameter names a function introduces — its
    /// `comptime <name>: type` parameters — unioned with any from an enclosing
    /// scope.
    fn fn_type_params(&self, f: &FnDecl, enclosing: &HashSet<String>) -> HashSet<String> {
        let mut set = enclosing.clone();
        for p in &f.params {
            if p.comptime
                && p.ty.is_some_and(|t| matches!(self.ast.type_at(t).kind, TypeKind::TypeKw))
            {
                set.insert(p.name.name.clone());
            }
        }
        set
    }

    fn lower_type(&self, ty_params: &HashSet<String>, id: TypeId) -> Ty {
        match &self.ast.type_at(id).kind {
            TypeKind::Name(n) => {
                if ty_params.contains(&n.name) {
                    Ty::Opaque(n.name.clone())
                } else if let Some(p) = prim_ty(&n.name) {
                    Ty::Prim(p)
                } else if let Some(&i) = self.table.type_index.get(&n.name) {
                    Ty::Named(i)
                } else {
                    Ty::Opaque(n.name.clone()) // external / not-yet-known: stay quiet
                }
            }
            TypeKind::TypeKw => Ty::TypeKw,
            TypeKind::Ptr { mutbl, inner } => {
                Ty::Ptr { mutbl: *mutbl, inner: Box::new(self.lower_type(ty_params, *inner)) }
            }
            TypeKind::Slice(inner) => Ty::Slice(Box::new(self.lower_type(ty_params, *inner))),
            TypeKind::GenRef(inner) => Ty::GenRef(Box::new(self.lower_type(ty_params, *inner))),
            TypeKind::RegionRef { inner, .. } => {
                Ty::RegionRef(Box::new(self.lower_type(ty_params, *inner)))
            }
            TypeKind::Fn { params, ret_conv, ret } => {
                let ps: Vec<(Conv, Box<Ty>)> = params
                    .iter()
                    .map(|p| (p.conv, Box::new(self.lower_type(ty_params, p.ty))))
                    .collect();
                let r = match ret {
                    Some(t) => self.lower_type(ty_params, *t),
                    None => Ty::Unit,
                };
                Ty::Fn { params: ps, ret: Box::new(r), ret_conv: *ret_conv }
            }
            // `dyn Trait` is opaque until trait resolution lands (Stage F gives it a
            // real fat-pointer representation); keep it quiet, not an error.
            TypeKind::Dyn(n) => Ty::Opaque(format!("dyn {}", n.name)),
            TypeKind::App { ctor, args } => {
                let aty: Vec<Ty> = args.iter().map(|a| self.lower_type(ty_params, *a)).collect();
                // `Ctor(args)` is a generic *enum* instance if `Ctor` names a
                // generic enum; otherwise a generic struct (the comptime-fn form).
                if self.is_generic_enum(&ctor.name) {
                    Ty::GenEnum { ctor: ctor.name.clone(), args: aty }
                } else {
                    Ty::GenStruct { ctor: ctor.name.clone(), args: aty }
                }
            }
            TypeKind::Error => Ty::Error,
        }
    }

    /// Resolve a type-valued expression (`i32`, a type parameter `T`) to a `Ty`.
    fn eval_type_expr(&self, ty_params: &HashSet<String>, id: ExprId) -> Ty {
        match &self.ast.expr_at(id).kind {
            ExprKind::Name(n) => {
                if ty_params.contains(&n.name) {
                    Ty::Opaque(n.name.clone())
                } else if let Some(p) = prim_ty(&n.name) {
                    Ty::Prim(p)
                } else if let Some(&i) = self.table.type_index.get(&n.name) {
                    Ty::Named(i)
                } else {
                    Ty::Opaque(n.name.clone())
                }
            }
            _ => Ty::Opaque("?".to_string()),
        }
    }

    fn find_fn_decl(&self, name: &str) -> Option<&'a FnDecl> {
        self.ast.items.iter().find_map(|it| match it {
            Item::Fn(f) if f.name.name == name => Some(f),
            _ => None,
        })
    }

    /// If `name` is generic, substitute its type arguments into `ret`. Handles
    /// both generic forms: a **comptime** `T: type` parameter takes its argument
    /// as an explicit type expression (`pick(i32, …)`), while a **bracket** `[T:
    /// Tr]` parameter is *inferred* from the value arguments' types (`sum(a, b)`
    /// with `a: i32` ⇒ `T = i32`) — so `sum(a, b) -> T` types as `i32`, not the
    /// bare type parameter.
    fn monomorphize_ret(&self, name: &str, args: &[ExprId], typ: &HashSet<String>, ret: Ty) -> Ty {
        let Some(f) = self.find_fn_decl(name) else { return ret };
        let mut subst = HashMap::new();
        for (i, p) in f.params.iter().enumerate() {
            let is_tp = p.comptime
                && p.ty.is_some_and(|t| matches!(self.ast.type_at(t).kind, TypeKind::TypeKw));
            if is_tp {
                if let Some(a) = args.get(i) {
                    subst.insert(p.name.name.clone(), self.eval_type_expr(typ, *a));
                }
            }
        }
        // Bracket-form generics: infer each `T` by unifying the declared parameter
        // types (`Ty::Opaque("T")`) against the actual argument types.
        if !f.generics.is_empty() {
            let tps: HashSet<String> = f.generics.iter().map(|g| g.name.name.clone()).collect();
            let param_tys: Vec<Ty> = self
                .table
                .fns
                .get(name)
                .map(|s| s.params.iter().map(|p| p.ty.clone()).collect())
                .unwrap_or_default();
            for (i, pt) in param_tys.iter().enumerate() {
                if let Some(a) = args.get(i) {
                    let at = self.expr_types[a.0 as usize].clone();
                    unify_tp(pt, &at, &tps, &mut subst);
                }
            }
        }
        if subst.is_empty() {
            ret
        } else {
            subst_ty(&ret, &subst)
        }
    }

    /// The comptime type-parameter names of `f`, in declaration order.
    fn comptime_tp_names(&self, f: &FnDecl) -> Vec<String> {
        f.params
            .iter()
            .filter(|p| {
                p.comptime
                    && p.ty.is_some_and(|t| matches!(self.ast.type_at(t).kind, TypeKind::TypeKw))
            })
            .map(|p| p.name.name.clone())
            .collect()
    }

    /// Resolve `base.mname(args)` to a free function whose first non-comptime
    /// parameter head-matches the receiver's type (backlog item A). Records the
    /// resolution in `method_calls` and returns the call's result type. `None`
    /// means "not a free-function method" (the caller tries a struct method).
    fn resolve_free_method(
        &mut self,
        call_id: ExprId,
        mname: &str,
        args: &[ExprId],
        recv_ty: &Ty,
        arg_tys: &[Ty],
    ) -> Option<Ty> {
        let f = self.find_fn_decl(mname)?;
        let recv_idx = f.params.iter().position(|p| !p.comptime)?; // first runtime param
        let tps = self.fn_type_params(f, &HashSet::new());

        // Take the owned parameter data we need, then release the table borrow.
        let (recv_conv, ret, fallible, param_tys, runtime_idx) = {
            let sig = self.table.fns.get(mname)?;
            let param_tys: Vec<Ty> = sig.params.iter().map(|p| p.ty.clone()).collect();
            let runtime_idx: Vec<usize> =
                f.params.iter().enumerate().filter(|(_, p)| !p.comptime).map(|(i, _)| i).collect();
            (sig.params[recv_idx].conv, sig.ret.clone(), sig.fallible, param_tys, runtime_idx)
        };

        // The receiver type must match for this to be a method call at all.
        if !head_matches(&param_tys[recv_idx], recv_ty) {
            return None;
        }

        // Recover the comptime type arguments by unifying params with actuals.
        let mut subst: HashMap<String, Ty> = HashMap::new();
        unify_tp(&param_tys[recv_idx], recv_ty, &tps, &mut subst);
        for (k, &pi) in runtime_idx.iter().skip(1).enumerate() {
            if let Some(aty) = arg_tys.get(k) {
                unify_tp(&param_tys[pi], aty, &tps, &mut subst);
            }
        }
        let type_args: Vec<Ty> = self
            .comptime_tp_names(f)
            .into_iter()
            .map(|n| subst.get(&n).cloned().unwrap_or(Ty::Unknown))
            .collect();

        let expected = runtime_idx.len() - 1; // minus the receiver
        if expected != args.len() {
            let span = self.ast.expr_at(call_id).span;
            self.error(
                span,
                format!("method `{mname}` expects {expected} argument(s), found {}", args.len()),
            );
        }

        self.method_calls.insert(
            call_id,
            MethodRes { fn_name: mname.to_string(), recv_ctor: None, type_args, recv_conv },
        );
        let ret = subst_ty(&ret, &subst);
        Some(if fallible { Ty::Result(Box::new(ret)) } else { ret })
    }

    /// The `struct { … }` body a generic-struct constructor function returns.
    fn ctor_struct_body(&self, f: &FnDecl) -> Option<&'a StructBody> {
        for stmt in &f.body.stmts {
            let e = match stmt {
                Stmt::Return { value: Some(e), .. } => *e,
                Stmt::Expr(e) => *e,
                _ => continue,
            };
            if let ExprKind::StructType(b) = &self.ast.expr_at(e).kind {
                return Some(b);
            }
        }
        None
    }

    /// Find a method named `mname` on a struct named `ctor`, returning the method
    /// declaration and the struct's type-parameter names (empty for a plain,
    /// non-generic struct).
    fn find_struct_method(&self, ctor: &str, mname: &str) -> Option<(&'a FnDecl, Vec<String>)> {
        // A generic struct: a constructor function returning `struct { … }`.
        if let Some(cf) = self.find_fn_decl(ctor) {
            if let Some(body) = self.ctor_struct_body(cf) {
                let tps = self.comptime_tp_names(cf);
                for m in &body.members {
                    if let StructMember::Method(f) = m {
                        if f.name.name == mname {
                            return Some((f, tps));
                        }
                    }
                }
            }
        }
        // A plain struct declared with methods.
        for item in &self.ast.items {
            if let Item::Struct { name, body, .. } = item {
                if name.name == ctor {
                    for m in &body.members {
                        if let StructMember::Method(f) = m {
                            if f.name.name == mname {
                                return Some((f, Vec::new()));
                            }
                        }
                    }
                }
            }
        }
        None
    }

    /// Resolve `base.mname(args)` to a method defined *inside* the receiver's
    /// struct (backlog item C). The struct's type parameters are bound from the
    /// receiver's concrete type arguments, so `List(i32).get` returns `i32`.
    fn resolve_struct_method(
        &mut self,
        call_id: ExprId,
        mname: &str,
        args: &[ExprId],
        recv_ty: &Ty,
    ) -> Option<Ty> {
        let (ctor, recv_args): (String, Vec<Ty>) = match recv_ty {
            Ty::GenStruct { ctor, args } => (ctor.clone(), args.clone()),
            Ty::Named(i) => (self.table.types[*i].name.clone(), Vec::new()),
            _ => return None,
        };
        let (method, tp_names) = self.find_struct_method(&ctor, mname)?;

        let recv_conv =
            method.params.iter().find(|p| p.is_self).map(|p| p.conv).unwrap_or(Conv::Default);
        let fallible = method.errors.is_some();

        // The struct's type parameters → the receiver's concrete arguments.
        let subst: HashMap<String, Ty> =
            tp_names.iter().cloned().zip(recv_args.iter().cloned()).collect();
        let tp_set: HashSet<String> = tp_names.iter().cloned().collect();
        let ret = method.ret_ty.map(|t| self.lower_type(&tp_set, t)).unwrap_or(Ty::Unit);
        let ret = subst_ty(&ret, &subst);

        let expected = method.params.iter().filter(|p| !p.is_self && !p.comptime).count();
        if expected != args.len() {
            let span = self.ast.expr_at(call_id).span;
            self.error(
                span,
                format!("method `{mname}` expects {expected} argument(s), found {}", args.len()),
            );
        }

        self.method_calls.insert(
            call_id,
            MethodRes {
                fn_name: mname.to_string(),
                recv_ctor: Some(ctor),
                type_args: recv_args,
                recv_conv,
            },
        );
        Some(if fallible { Ty::Result(Box::new(ret)) } else { ret })
    }

    /// Resolve a module-qualified call `binding.fname(args)` where `binding` is
    /// an import bound to `target_mod`. `fname` must be a `pub` item of that
    /// module. Records the resolution for the backend and returns the call type.
    #[allow(clippy::too_many_arguments)]
    fn resolve_qualified_call(
        &mut self,
        id: ExprId,
        binding: &str,
        target_mod: ModId,
        fname: &str,
        args: &[ExprId],
        scope: &mut Scope,
        typ: &HashSet<String>,
        self_ty: &Ty,
    ) -> Ty {
        let span = self.ast.expr_at(id).span;
        for a in args {
            self.infer(scope, typ, self_ty, *a);
        }
        match self.owner.get(fname).copied() {
            Some((owner, is_pub)) if owner == target_mod && is_pub => {
                self.qualified.insert(id, fname.to_string());
                if let Some(sig) = self.table.fns.get(fname) {
                    let ret = sig.ret.clone();
                    let fallible = sig.fallible;
                    let want = sig.params.len();
                    if want != args.len() {
                        self.error(
                            span,
                            format!("`{binding}.{fname}` expects {want} argument(s), found {}", args.len()),
                        );
                    }
                    let ret = self.monomorphize_ret(fname, args, typ, ret);
                    let t = if fallible { Ty::Result(Box::new(ret)) } else { ret };
                    self.set(id, t)
                } else {
                    self.set(id, Ty::Unknown)
                }
            }
            Some((owner, _)) if owner == target_mod => {
                self.error(span, format!("`{fname}` is private to module `{binding}`"));
                self.set(id, Ty::Unknown)
            }
            _ => {
                self.error(span, format!("module `{binding}` has no public function `{fname}`"));
                self.set(id, Ty::Unknown)
            }
        }
    }

    /// Resolve a module-qualified value access `binding.name` (a `pub` const of
    /// the bound module). Records the resolution and returns the const's type.
    fn resolve_qualified_const(
        &mut self,
        id: ExprId,
        binding: &str,
        target_mod: ModId,
        name: &str,
    ) -> Ty {
        let span = self.ast.expr_at(id).span;
        match self.owner.get(name).copied() {
            Some((owner, is_pub)) if owner == target_mod && is_pub => {
                self.qualified.insert(id, name.to_string());
                let t = self.table.consts.get(name).cloned().unwrap_or(Ty::Unknown);
                self.set(id, t)
            }
            Some((owner, _)) if owner == target_mod => {
                self.error(span, format!("`{name}` is private to module `{binding}`"));
                self.set(id, Ty::Unknown)
            }
            _ => {
                self.error(span, format!("module `{binding}` has no public item `{name}`"));
                self.set(id, Ty::Unknown)
            }
        }
    }

    // --- checking bodies ---

    /// The module an import `binding` refers to, in the current module's scope.
    fn binding_module(&self, binding: &str) -> Option<ModId> {
        self.modules.imports.get(self.cur_mod).and_then(|m| m.get(binding)).copied()
    }

    /// Error if `name` resolves to a top-level item in *another* module that is
    /// not `pub` — the bootstrap's visibility rule (design §9). A name local to
    /// the current module, or a builtin/unknown name, is fine.
    fn check_visibility(&mut self, name: &str, span: Span) {
        if let Some(&(owner, is_pub)) = self.owner.get(name) {
            if owner != self.cur_mod && !is_pub {
                self.error(
                    span,
                    format!("`{name}` is private to module `{}`", self.modules.names[owner]),
                );
            }
        }
    }

    fn check_items(&mut self) {
        let ast = self.ast;
        let empty = HashSet::new();
        for (i, item) in ast.items.iter().enumerate() {
            self.cur_mod = *self.modules.item_mod.get(i).unwrap_or(&0);
            match item {
                Item::Fn(f) => self.check_fn(f, &empty, &Ty::Unit),
                Item::Struct { name, body, .. } => {
                    let self_ty = self
                        .table
                        .type_index
                        .get(&name.name)
                        .map(|&i| Ty::Named(i))
                        .unwrap_or_else(|| Ty::Opaque(name.name.clone()));
                    for m in &body.members {
                        if let StructMember::Method(f) = m {
                            self.check_fn(f, &empty, &self_ty);
                        }
                    }
                }
                Item::Const(c) => {
                    let mut scope: Scope = vec![HashMap::new()];
                    self.infer(&mut scope, &empty, &Ty::Unit, c.value);
                }
                Item::Enum(_) | Item::Distinct(_) | Item::Extern(_) | Item::Import(_) => {}
                // Trait/impl method bodies are checked in Stage B (against the trait).
                Item::Trait(_) | Item::Impl(_) => {}
            }
        }
    }

    fn check_fn(&mut self, f: &FnDecl, enclosing: &HashSet<String>, self_ty: &Ty) {
        let typ = self.fn_type_params(f, enclosing);
        // The bracket type parameters in scope for this body (→ their bounds), for
        // the "Zig fix": only a bound's methods are callable on a `T` value.
        let prev_bounds = std::mem::replace(
            &mut self.cur_type_param_bounds,
            f.generics
                .iter()
                .map(|g| (g.name.name.clone(), g.bound.as_ref().map(|b| b.name.clone())))
                .collect(),
        );
        let mut scope: Scope = vec![HashMap::new()];
        for p in &f.params {
            let pty = if p.is_self {
                self_ty.clone()
            } else if let Some(t) = p.ty {
                self.lower_type(&typ, t)
            } else {
                Ty::Unknown
            };
            let name = if p.is_self { "self".to_string() } else { p.name.name.clone() };
            scope[0].insert(name, pty);
        }
        // The (ok) return type is the expected type for `return <expr>`.
        let prev_ret = self.cur_ret.take();
        self.cur_ret = f.ret_ty.map(|t| self.lower_type(&typ, t));
        self.infer_block(&mut scope, &typ, self_ty, &f.body);
        self.cur_ret = prev_ret;
        self.cur_type_param_bounds = prev_bounds;
    }

    fn infer_block(&mut self, scope: &mut Scope, typ: &HashSet<String>, self_ty: &Ty, block: &Block) -> Ty {
        scope.push(HashMap::new());
        let mut result = Ty::Unit;
        let n = block.stmts.len();
        for (i, stmt) in block.stmts.iter().enumerate() {
            match stmt {
                Stmt::Let { name, ty, init, .. } => {
                    // A type annotation is the expected type for the initializer
                    // (so `var m: Option(i32) = none` resolves `none`'s instantiation).
                    let expected = ty.map(|t| self.lower_type(typ, t));
                    let prev = self.cur_expected.take();
                    self.cur_expected = expected.clone();
                    let inferred = init.map(|e| self.infer(scope, typ, self_ty, e));
                    self.cur_expected = prev;
                    // A `distinct` type is *not* interchangeable with its base (or any
                    // other type): a mismatched initializer is an error suggesting `as`.
                    if let (Some(ann), Some(got)) = (&expected, &inferred) {
                        if self.distinct_mismatch(ann, got) {
                            self.error(
                                name.span,
                                format!(
                                    "expected `{}`, found `{}` — `distinct` types need an explicit `as`",
                                    ann.display(&self.table),
                                    got.display(&self.table)
                                ),
                            );
                        }
                    }
                    let bind = expected.unwrap_or_else(|| inferred.unwrap_or(Ty::Unknown));
                    scope.last_mut().unwrap().insert(name.name.clone(), bind);
                    result = Ty::Unit;
                }
                Stmt::Return { value, .. } => {
                    if let Some(v) = value {
                        let prev = self.cur_expected.take();
                        self.cur_expected = self.cur_ret.clone();
                        self.infer(scope, typ, self_ty, *v);
                        self.cur_expected = prev;
                    }
                    result = Ty::Unit;
                }
                Stmt::Expr(e) => {
                    let t = self.infer(scope, typ, self_ty, *e);
                    if i + 1 == n {
                        result = t;
                    }
                }
            }
        }
        scope.pop();
        result
    }

    fn infer(&mut self, scope: &mut Scope, typ: &HashSet<String>, self_ty: &Ty, id: ExprId) -> Ty {
        let ast = self.ast;
        let data = ast.expr_at(id);
        let span = data.span;
        let ty = match &data.kind {
            ExprKind::Int(_) => Ty::Prim("i32"),
            ExprKind::Float(_) => Ty::Prim("f64"),
            ExprKind::Str(_) => Ty::Prim("str"),
            ExprKind::Char(_) => Ty::Prim("char"),
            ExprKind::Bool(_) => Ty::Prim("bool"),
            ExprKind::Null => Ty::Ptr { mutbl: PtrMut::Default, inner: Box::new(Ty::Unknown) },

            ExprKind::Name(n) => {
                if let Some(t) = scope_lookup(scope, &n.name) {
                    t
                } else if let Some(t) = self.table.consts.get(&n.name) {
                    t.clone()
                } else if let Some(&i) = self.table.variants.get(&n.name) {
                    // A bare nullary variant, e.g. `none` — for a generic enum its
                    // instantiation comes from the expected type (`variant_ctor_type`).
                    self.variant_ctor_type(i, &n.name, &[])
                } else {
                    Ty::Unknown // a function name or external symbol: stay quiet
                }
            }
            ExprKind::SelfValue | ExprKind::SelfType => self_ty.clone(),
            ExprKind::Attr(_) => Ty::Unknown,

            ExprKind::Unary { op, rhs } => {
                let rt = self.infer(scope, typ, self_ty, *rhs);
                match op {
                    UnOp::Not => Ty::Prim("bool"),
                    UnOp::Neg | UnOp::BitNot => rt,
                    UnOp::Ref => Ty::Unknown,
                }
            }
            ExprKind::Binary { op, lhs, rhs } => {
                let lt = self.infer(scope, typ, self_ty, *lhs);
                let rt = self.infer(scope, typ, self_ty, *rhs);
                // Operator traits (Stage E): `a OP b` on a user type dispatches
                // through `impl <OpTrait> for <lhs>`; primitives fall through to
                // native semantics below.
                if let Some(t) = self.resolve_operator_trait(id, *op, &lt, span) {
                    t
                } else {
                    use BinOp::*;
                    match op {
                        Eq | Ne | Lt | Le | Gt | Ge | And | Or => Ty::Prim("bool"),
                        _ => {
                            if is_numeric(&lt) {
                                lt
                            } else if is_numeric(&rt) {
                                rt
                            } else {
                                Ty::Unknown
                            }
                        }
                    }
                }
            }
            ExprKind::Assign { target, value, .. } => {
                self.infer(scope, typ, self_ty, *target);
                self.infer(scope, typ, self_ty, *value);
                // A `record`'s fields are immutable: assigning one is a static
                // error (the whole binding may still be rebound, like `let`).
                if let ExprKind::Field { base, .. } = &ast.expr_at(*target).kind {
                    let bt = self.expr_types[base.0 as usize].clone();
                    if let Some(rname) = self.record_name(&bt) {
                        let sp = ast.expr_at(*target).span;
                        self.error(
                            sp,
                            format!("cannot assign to a field of immutable record `{rname}`"),
                        );
                    }
                }
                Ty::Unit
            }
            ExprKind::Range { lo, hi, .. } => {
                if let Some(l) = lo {
                    self.infer(scope, typ, self_ty, *l);
                }
                if let Some(h) = hi {
                    self.infer(scope, typ, self_ty, *h);
                }
                Ty::Unknown
            }
            ExprKind::Call { callee, args } => {
                // Method-call sugar: `base.name(args)` resolves either to a free
                // function whose first parameter is the receiver (item A), or to a
                // method defined inside the receiver's struct (item C).
                if let ExprKind::Field { base, name } = &ast.expr_at(*callee).kind {
                    let (base, name) = (*base, name.name.clone());
                    // Module-qualified call: `mod.func(args)` where `mod` is an
                    // import binding not shadowed by a local. Resolve `func`
                    // directly, bypassing method/receiver resolution (design §9).
                    if let ExprKind::Name(b) = &ast.expr_at(base).kind {
                        let b = b.name.clone();
                        if scope_lookup(scope, &b).is_none() {
                            if let Some(target) = self.binding_module(&b) {
                                return self.resolve_qualified_call(
                                    id, &b, target, &name, args, scope, typ, self_ty,
                                );
                            }
                        }
                    }
                    let recv_ty = self.infer(scope, typ, self_ty, base);
                    let arg_tys: Vec<Ty> =
                        args.iter().map(|a| self.infer(scope, typ, self_ty, *a)).collect();
                    // `a.f(x)` where `f` is a *function-pointer field* — an
                    // indirect call through the field, typed by its return type.
                    // Resolved by the field's *type*, ahead of method-call sugar:
                    // the "disambiguate by field type" rule (design discussion H).
                    if let Some(fty) = self.fn_ptr_field(&recv_ty, &name) {
                        if let Ty::Fn { ret, .. } = &fty {
                            let ret = (**ret).clone();
                            self.set(*callee, fty.clone()); // route cgen to an indirect call
                            return self.set(id, ret);
                        }
                    }
                    if let Some(ret) =
                        self.resolve_free_method(id, &name, args, &recv_ty, &arg_tys)
                    {
                        return self.set(id, ret);
                    }
                    if let Some(ret) = self.resolve_struct_method(id, &name, args, &recv_ty) {
                        return self.set(id, ret);
                    }
                    // `recv.m(args)` resolving through an `impl Trait for <recv>`
                    // (traits, Stage B) — a fallback after free-fn / struct methods.
                    if let Some(ret) = self.resolve_impl_method(id, &name, &recv_ty) {
                        return self.set(id, ret);
                    }
                    // `x.m(args)` where `x: T` is a **bracket type parameter** of the
                    // enclosing generic: resolve `m` *through the bound* `Tr` (the
                    // "Zig fix" — design §8.2). A method not in `Tr` is a
                    // definition-site error ("blame the generic code").
                    if let Some(ret) = self.resolve_bound_method(id, &name, &recv_ty) {
                        return self.set(id, ret);
                    }
                    return self.set(id, Ty::Unknown);
                }
                let callee_name = match &ast.expr_at(*callee).kind {
                    ExprKind::Name(n) => Some(n.name.clone()),
                    _ => None,
                };
                self.infer(scope, typ, self_ty, *callee);
                // Indirect call through a fn-pointer *local or parameter* (`op(x)`):
                // the callee is a value of type `fn(...) -> R`, so the call's type
                // is `R`. A bare top-level *function name* infers to `Unknown`
                // (not `Fn`), so this never intercepts an ordinary direct call.
                if let Ty::Fn { ret, .. } = self.expr_types[callee.0 as usize].clone() {
                    for a in args {
                        self.infer(scope, typ, self_ty, *a);
                    }
                    return self.set(id, *ret);
                }
                // Each argument is inferred with its parameter type as the expected
                // type, so a nullary generic variant resolves: `get(none)` where
                // `get(o: Option(i32), …)` types `none` as `Option(i32)`.
                let param_tys: Vec<Ty> = callee_name
                    .as_ref()
                    .and_then(|n| self.table.fns.get(n))
                    .map(|sig| sig.params.iter().map(|p| p.ty.clone()).collect())
                    .unwrap_or_default();
                for (i, a) in args.iter().enumerate() {
                    let prev = self.cur_expected.take();
                    self.cur_expected = param_tys.get(i).cloned();
                    self.infer(scope, typ, self_ty, *a);
                    self.cur_expected = prev;
                }
                if let Some(name) = callee_name {
                    self.check_visibility(&name, span);
                    // Definition-site bounds (Stage D): a bracket-generic call
                    // `f[T: Tr](…)` must instantiate `T` at a type that `impl`s
                    // `Tr` — checked here, where `T` is concrete (the args carry it).
                    let arg_tys: Vec<Ty> =
                        args.iter().map(|a| self.expr_types[a.0 as usize].clone()).collect();
                    self.check_call_bounds(&name, &arg_tys, span);
                    if let Some(sig) = self.table.fns.get(&name) {
                        let ret = sig.ret.clone();
                        let fallible = sig.fallible;
                        let want = sig.params.len();
                        if want != args.len() {
                            self.error(
                                span,
                                format!("`{name}` expects {want} argument(s), found {}", args.len()),
                            );
                        }
                        // For a generic call, resolve type parameters in the return.
                        let ret = self.monomorphize_ret(&name, args, typ, ret);
                        // A fallible call yields `T !E`; `?` later unwraps it.
                        if fallible {
                            Ty::Result(Box::new(ret))
                        } else {
                            ret
                        }
                    } else if let Some(&ei) = self.table.variants.get(&name) {
                        // An enum-variant constructor, e.g. `circle(2.0)`. For a
                        // generic enum, recover its type arguments from the args.
                        let arg_tys: Vec<Ty> =
                            args.iter().map(|a| self.expr_types[a.0 as usize].clone()).collect();
                        self.variant_ctor_type(ei, &name, &arg_tys)
                    } else if name == "unwrap" {
                        // `unwrap(r: T !E) -> T` — the ok type of the result argument.
                        match args.first().map(|a| self.expr_types[a.0 as usize].clone()) {
                            Some(Ty::Result(ok)) => *ok,
                            _ => Ty::Unknown,
                        }
                    } else if name == "is_err" {
                        Ty::Prim("bool")
                    } else if let Some(t) = string_intrinsic_ret(&name) {
                        // String intrinsics aren't declared functions; type their
                        // results so a `let` (without an annotation) gets the right C type.
                        t
                    } else {
                        Ty::Unknown
                    }
                } else {
                    Ty::Unknown
                }
            }
            ExprKind::Field { base, name } => {
                // Module-qualified value access: `mod.CONST` where `mod` is an
                // import binding not shadowed by a local.
                if let ExprKind::Name(b) = &ast.expr_at(*base).kind {
                    let b = b.name.clone();
                    if scope_lookup(scope, &b).is_none() {
                        if let Some(target) = self.binding_module(&b) {
                            return self.resolve_qualified_const(id, &b, target, &name.name);
                        }
                    }
                }
                let bt = self.infer(scope, typ, self_ty, *base);
                self.field_type(span, &bt, &name.name)
            }
            ExprKind::Index { base, index } => {
                let bt = self.infer(scope, typ, self_ty, *base);
                self.infer(scope, typ, self_ty, *index);
                // Indexing a slice yields its element type; a string yields a byte,
                // *except* `s[i..j]` (a range index) which slices a sub-`str`.
                match bt {
                    Ty::Slice(elem) => *elem,
                    Ty::Prim("str") => {
                        if matches!(ast.expr_at(*index).kind, ExprKind::Range { .. }) {
                            Ty::Prim("str")
                        } else {
                            Ty::Prim("u8")
                        }
                    }
                    _ => Ty::Unknown,
                }
            }
            ExprKind::Deref { base } => {
                let bt = self.infer(scope, typ, self_ty, *base);
                match bt {
                    Ty::Ptr { inner, .. } => *inner,
                    Ty::GenRef(elem) => *elem,    // `r.*` on a generational reference
                    Ty::RegionRef(elem) => *elem, // `r.*` on a region reference
                    _ => Ty::Unknown,
                }
            }
            ExprKind::Cast { expr, ty } => {
                self.infer(scope, typ, self_ty, *expr);
                self.lower_type(typ, *ty) // the cast's type is its target type
            }
            ExprKind::Try { base } => {
                // `e?` unwraps a `T !E` to its ok type `T`.
                let bt = self.infer(scope, typ, self_ty, *base);
                match bt {
                    Ty::Result(ok) => *ok,
                    _ => Ty::Unknown,
                }
            }
            ExprKind::StructLit { path, fields, spread } => {
                // For a plain named struct, resolve its index *first*, so each field
                // value is inferred against its **declared field type** as the
                // expected type. That is what lets a fn-pointer-typed field accept a
                // coercing closure literal — `Allocator{ alloc_fn: |n| … }`.
                let named_idx = if path.name != "Self" && !self.table.variants.contains_key(&path.name) {
                    self.table.type_index.get(&path.name).copied()
                } else {
                    None
                };
                for fi in fields {
                    let expected =
                        named_idx.and_then(|i| self.struct_field_decl_ty(i, &fi.name.name));
                    let prev = self.cur_expected.take();
                    self.cur_expected = expected;
                    self.infer(scope, typ, self_ty, fi.value);
                    self.cur_expected = prev;
                }
                if let Some(s) = spread {
                    self.infer(scope, typ, self_ty, *s);
                }
                if path.name == "Self" {
                    self_ty.clone()
                } else if let Some(&ei) = self.table.variants.get(&path.name) {
                    // `circle { r: 2.0 }` — a struct-variant construction (the path is
                    // an enum variant, not a struct type). Reuse the positional
                    // inference (source order is taken as field order).
                    let arg_tys: Vec<Ty> =
                        fields.iter().map(|fi| self.expr_types[fi.value.0 as usize].clone()).collect();
                    self.variant_ctor_type(ei, &path.name, &arg_tys)
                } else if let Some(i) = named_idx {
                    Ty::Named(i)
                } else {
                    Ty::Opaque(path.name.clone())
                }
            }
            ExprKind::GenStructLit { ctor, type_args, fields } => {
                // Evaluate the type arguments first, so each field value is inferred
                // against its declared type *under the substitution* — letting a
                // closure literal coerce in a generic vtable's fn-pointer field
                // (`Container(i32){ op: |x| x + 1 }`).
                let args: Vec<Ty> = type_args.iter().map(|a| self.eval_type_expr(typ, *a)).collect();
                for fi in fields {
                    let expected = self.gen_struct_field_decl_ty(&ctor.name, &args, &fi.name.name);
                    let prev = self.cur_expected.take();
                    self.cur_expected = expected;
                    self.infer(scope, typ, self_ty, fi.value);
                    self.cur_expected = prev;
                }
                Ty::GenStruct { ctor: ctor.name.clone(), args }
            }
            ExprKind::StructType(body) => {
                for m in &body.members {
                    if let StructMember::Method(f) = m {
                        self.check_fn(f, typ, &Ty::Opaque("Self".to_string()));
                    }
                }
                Ty::TypeKw
            }
            ExprKind::Block(b) => self.infer_block(scope, typ, self_ty, b),
            ExprKind::Unsafe(b) => self.infer_block(scope, typ, self_ty, b),
            ExprKind::If { cond, then, els } => {
                self.infer(scope, typ, self_ty, *cond);
                let t = self.infer_block(scope, typ, self_ty, then);
                if let Some(e) = els {
                    self.infer(scope, typ, self_ty, *e);
                }
                t
            }
            ExprKind::FString { exprs, .. } => {
                // Infer each interpolation (so names resolve and types are recorded
                // for cgen's per-type formatting). An f-string builds an owned String.
                for e in exprs {
                    self.infer(scope, typ, self_ty, *e);
                }
                Ty::Prim("String")
            }
            ExprKind::Match { scrut, arms } => {
                let st = self.infer(scope, typ, self_ty, *scrut);
                let mut result = Ty::Unknown;
                for (ai, arm) in arms.iter().enumerate() {
                    scope.push(HashMap::new());
                    self.bind_pattern_types(scope, &st, arm.pat);
                    // A guard is inferred with the pattern's bindings in scope (it
                    // may reference them: `circle(r) if r > 0.0`). We infer it for
                    // its side effects (recording expr types, resolving names);
                    // it's expected to be `bool` but the lenient checker doesn't
                    // enforce that.
                    if let Some(g) = arm.guard {
                        self.infer(scope, typ, self_ty, g);
                    }
                    let bt = self.infer(scope, typ, self_ty, arm.body);
                    scope.pop();
                    if ai == 0 {
                        result = bt;
                    }
                }
                self.check_exhaustive(span, &st, arms);
                result
            }
            ExprKind::Closure { params, body } => {
                // Check the body in a fresh scope with the parameters bound. If a
                // *function-pointer* type is expected here (a `let`/argument/return
                // annotated `fn(...) -> R`), this is a **closure → fn-pointer
                // coercion**: propagate the expected parameter types onto the
                // closure's (possibly unannotated) parameters, infer the body
                // against the expected return, and give the closure that `Ty::Fn`.
                // Otherwise it stays an opaque fat closure. (Whether the coercion
                // is *legal* — i.e. the closure captures nothing — is enforced by
                // codegen, which is where capture analysis lives.)
                let exp_fn = match self.cur_expected.clone() {
                    Some(Ty::Fn { params, ret, ret_conv }) => Some((params, ret, ret_conv)),
                    _ => None,
                };
                scope.push(HashMap::new());
                for (i, p) in params.iter().enumerate() {
                    let pty = if let Some(t) = p.ty {
                        self.lower_type(typ, t)
                    } else if let Some((ep, _, _)) = &exp_fn {
                        ep.get(i).map(|(_, t)| (**t).clone()).unwrap_or(Ty::Unknown)
                    } else {
                        Ty::Unknown
                    };
                    scope.last_mut().unwrap().insert(p.name.name.clone(), pty);
                }
                let prev = self.cur_expected.take();
                self.cur_expected = exp_fn.as_ref().map(|(_, r, _)| (**r).clone());
                self.infer(scope, typ, self_ty, *body);
                self.cur_expected = prev;
                scope.pop();
                match exp_fn {
                    Some((ep, r, rc)) => Ty::Fn { params: ep, ret: r, ret_conv: rc },
                    None => Ty::Opaque("closure".to_string()),
                }
            }
            ExprKind::Concurrent(b) => {
                self.infer_block(scope, typ, self_ty, b);
                Ty::Unit
            }
            ExprKind::Spawn(call) => {
                self.infer(scope, typ, self_ty, *call);
                Ty::Unit
            }
            ExprKind::Region { body, .. } => {
                self.infer_block(scope, typ, self_ty, body);
                Ty::Unit
            }
            ExprKind::For { head, body, els, .. } => {
                match head {
                    ForHead::Infinite => {
                        self.infer_block(scope, typ, self_ty, body);
                    }
                    ForHead::While(cond) => {
                        self.infer(scope, typ, self_ty, *cond);
                        self.infer_block(scope, typ, self_ty, body);
                    }
                    ForHead::Iter { binds, sources, .. } => {
                        let binds: Vec<(String, Conv)> =
                            binds.iter().map(|b| (b.name.name.clone(), b.conv)).collect();
                        let sources: Vec<ExprId> = sources.clone();
                        scope.push(HashMap::new());
                        if sources.len() <= 1 {
                            // Simple iteration (range / slice), plus an optional
                            // index binding (`for x, i in xs`).
                            let src = sources.first().copied();
                            let elem = src.map(|s| self.iter_elem_type(scope, typ, self_ty, s));
                            if let Some((n0, _)) = binds.first() {
                                if n0 != "_" {
                                    let t = elem.clone().unwrap_or(Ty::Unknown);
                                    scope.last_mut().unwrap().insert(n0.clone(), t);
                                }
                            }
                            if let Some((n1, _)) = binds.get(1) {
                                if n1 != "_" {
                                    scope.last_mut().unwrap().insert(n1.clone(), Ty::Prim("usize"));
                                }
                            }
                        } else {
                            // Lockstep zip: each binding ↔ its own source's element.
                            for (i, (name, _)) in binds.iter().enumerate() {
                                let elem = sources
                                    .get(i)
                                    .map(|s| self.iter_elem_type(scope, typ, self_ty, *s))
                                    .unwrap_or(Ty::Unknown);
                                if name != "_" {
                                    scope.last_mut().unwrap().insert(name.clone(), elem);
                                }
                            }
                        }
                        self.infer_block(scope, typ, self_ty, body);
                        scope.pop();
                    }
                }
                // The `else` block runs after the loop, in the *enclosing* scope —
                // the loop bindings are out of scope here.
                if let Some(els) = els {
                    self.infer_block(scope, typ, self_ty, els);
                }
                Ty::Unit
            }
            ExprKind::Break(_) | ExprKind::Continue(_) => Ty::Unit,
            ExprKind::Invariant(e) | ExprKind::Variant(e) => {
                self.infer(scope, typ, self_ty, *e);
                Ty::Unit
            }
            ExprKind::Error => Ty::Error,
        };
        self.set(id, ty)
    }

    /// The element type a `for` binding gets when iterating `src`: a range yields
    /// its bounds' numeric type (default `usize`); a slice yields its element.
    fn iter_elem_type(&mut self, scope: &mut Scope, typ: &HashSet<String>, self_ty: &Ty, src: ExprId) -> Ty {
        if let ExprKind::Range { lo, hi, .. } = &self.ast.expr_at(src).kind {
            let (lo, hi) = (*lo, *hi);
            let mut t = Ty::Prim("usize");
            if let Some(h) = hi {
                let ht = self.infer(scope, typ, self_ty, h);
                if is_numeric(&ht) {
                    t = ht;
                }
            }
            if let Some(l) = lo {
                let lt = self.infer(scope, typ, self_ty, l);
                if !is_numeric(&t) && is_numeric(&lt) {
                    t = lt;
                }
            }
            self.set(src, Ty::Unknown);
            t
        } else {
            let t = self.infer(scope, typ, self_ty, src);
            // String iterators (recognized by their callee): `split`/`graphemes`
            // yield a `str` view per element, `codepoints` yields a codepoint.
            if let ExprKind::Call { callee, .. } = &self.ast.expr_at(src).kind {
                if let ExprKind::Name(n) = &self.ast.expr_at(*callee).kind {
                    match n.name.as_str() {
                        "split" | "graphemes" => return Ty::Prim("str"),
                        "codepoints" => return Ty::Prim("u32"),
                        _ => {}
                    }
                }
            }
            match t {
                Ty::Slice(e) => *e,
                Ty::Prim("str") => Ty::Prim("u8"), // iterating a string yields bytes
                _ => Ty::Unknown,
            }
        }
    }

    /// If `base` is a struct with a field `fname` of *function-pointer* type,
    /// return that `Ty::Fn`. Diagnostic-free (unlike [`Self::field_type`]), so it
    /// can probe a method-call's receiver field without emitting a spurious "no
    /// field" error when the call is really a method.
    ///
    /// Handles both shapes of vtable receiver:
    /// - a **plain** struct (`Ty::Named`) — read the field's declared type directly;
    /// - a **generic** struct (`Ty::GenStruct`) — resolve the field's declared type
    ///   *under the receiver's type arguments* (mirroring
    ///   [`Self::gen_struct_field_decl_ty`]), so `gen.op(n)` on `Box(i32)` sees
    ///   `op: fn(T) -> T` as the concrete `fn(i32) -> i32` and the call is typed by
    ///   its substituted return rather than falling through to `Unknown`.
    fn fn_ptr_field(&self, base: &Ty, fname: &str) -> Option<Ty> {
        let t = match base {
            Ty::Named(i) => {
                let TypeKindG::Struct { fields } = &self.table.types.get(*i)?.kind else {
                    return None;
                };
                fields.iter().find(|(n, _)| n == fname).map(|(_, t)| t.clone())?
            }
            Ty::GenStruct { ctor, args } => self.gen_struct_field_decl_ty(ctor, args, fname)?,
            _ => return None,
        };
        matches!(t, Ty::Fn { .. }).then_some(t)
    }

    /// The declared type of field `fname` on the struct at table index `i` (the
    /// *expected* type when inferring that field's value in a struct literal —
    /// notably letting a fn-pointer field coerce a closure literal). `None` for an
    /// enum/distinct, or an absent field.
    fn struct_field_decl_ty(&self, i: usize, fname: &str) -> Option<Ty> {
        let TypeKindG::Struct { fields } = &self.table.types.get(i)?.kind else { return None };
        fields.iter().find(|(n, _)| n == fname).map(|(_, t)| t.clone())
    }

    /// The declared type of field `fname` of generic struct `ctor` *under the
    /// concrete type arguments* `args` — e.g. `Container(i32)`'s `op: fn(T)->T`
    /// resolves to `fn(i32)->i32`. This is the expected type that lets a closure
    /// literal coerce inside a generic-struct literal's fn-pointer field.
    fn gen_struct_field_decl_ty(&self, ctor: &str, args: &[Ty], fname: &str) -> Option<Ty> {
        let cf = self.find_fn_decl(ctor)?;
        let body = self.ctor_struct_body(cf)?;
        let tp_names = self.comptime_tp_names(cf);
        let tp_set: HashSet<String> = tp_names.iter().cloned().collect();
        let subst: HashMap<String, Ty> = tp_names.into_iter().zip(args.iter().cloned()).collect();
        for m in &body.members {
            if let StructMember::Field { name, ty, .. } = m {
                if name.name == fname {
                    return Some(subst_ty(&self.lower_type(&tp_set, *ty), &subst));
                }
            }
        }
        None
    }

    fn field_type(&mut self, span: Span, base: &Ty, fname: &str) -> Ty {
        if let Ty::Slice(elem) = base {
            // A slice exposes `ptr` (the data pointer) and `len` (the length).
            return match fname {
                "len" => Ty::Prim("usize"),
                "ptr" => Ty::Ptr { mutbl: PtrMut::Default, inner: elem.clone() },
                _ => Ty::Unknown,
            };
        }
        if matches!(base, Ty::Prim("str")) {
            // A string view exposes its byte length (O(1)) and the underlying
            // bytes — `.cstr` is the null-terminated C-interop pointer.
            return match fname {
                "len" => Ty::Prim("usize"),
                "ptr" | "cstr" => Ty::Prim("cstr"),
                _ => Ty::Unknown,
            };
        }
        if matches!(base, Ty::Prim("String")) && fname == "len" {
            return Ty::Prim("usize"); // an owned String's byte length
        }
        if let Ty::GenStruct { ctor, args } = base {
            // A field *read* on a generic-struct value resolves under the
            // receiver's type arguments — the same substitution the field-*call*
            // path (`fn_ptr_field`) uses, so `let f = gen.op` on `Box(i32)` reads
            // `op: fn(T) -> T` as the concrete `fn(i32) -> i32` instead of typing
            // as `Unknown`. (A subsequent `f(n)` is then a typed indirect call.)
            return match self.gen_struct_field_decl_ty(ctor, args, fname) {
                Some(t) => t,
                None => {
                    let shown = base.display(&self.table);
                    self.error(span, format!("no field `{fname}` on `{shown}`"));
                    Ty::Error
                }
            };
        }
        if let Ty::Named(i) = base {
            // Read what we need, dropping the table borrow before any diagnostic.
            let (found, sname, is_struct) = {
                let decl = &self.table.types[*i];
                let is_struct = matches!(decl.kind, TypeKindG::Struct { .. });
                let found = match &decl.kind {
                    TypeKindG::Struct { fields } => {
                        fields.iter().find(|(n, _)| n == fname).map(|(_, t)| t.clone())
                    }
                    // Enums project payloads via `match`; a distinct type has no
                    // fields of its own — neither has a directly-accessible field.
                    TypeKindG::Enum { .. } | TypeKindG::Distinct { .. } => Some(Ty::Unknown),
                };
                (found, decl.name.clone(), is_struct)
            };
            match found {
                Some(t) => {
                    // Per-field visibility: a non-`pub` struct field is private to
                    // its defining module (design §2.8). Same-module access is free.
                    if is_struct {
                        if let Some(&(owner, _)) = self.owner.get(&sname) {
                            if owner != self.cur_mod && !self.field_is_pub(&sname, fname) {
                                let m = self.modules.names[owner].clone();
                                self.error(
                                    span,
                                    format!("field `{fname}` is private to module `{m}`"),
                                );
                            }
                        }
                    }
                    t
                }
                None => {
                    self.error(span, format!("no field `{fname}` on struct `{sname}`"));
                    Ty::Error
                }
            }
        } else {
            Ty::Unknown
        }
    }

    /// Is `field_name` a `pub` field of struct `struct_name`? (Fields are private
    /// to their module by default; `pub` exposes them.) Unknown struct/field →
    /// `true` (lenient — don't restrict what we can't resolve).
    fn field_is_pub(&self, struct_name: &str, field_name: &str) -> bool {
        for item in &self.ast.items {
            if let Item::Struct { name, body, .. } = item {
                if name.name == struct_name {
                    for m in &body.members {
                        if let StructMember::Field { name: fname, is_pub, .. } = m {
                            if fname.name == field_name {
                                return *is_pub;
                            }
                        }
                    }
                }
            }
        }
        true
    }

    /// Bind a pattern's variables, projecting an enum variant's payload field
    /// types onto its sub-patterns (so `circle(r)` types `r` as `f64`, not
    /// `Unknown`).
    fn bind_pattern_types(&mut self, scope: &mut Scope, scrut: &Ty, pat: PatId) {
        let ast = self.ast;
        match &ast.pat_at(pat).kind {
            PatKind::Ident(n) => {
                // A nullary variant (`none`) binds nothing; a plain identifier is
                // a catch-all binding the whole scrutinee.
                if !self.table.variants.contains_key(&n.name) {
                    scope.last_mut().unwrap().insert(n.name.clone(), scrut.clone());
                }
            }
            PatKind::Variant { name, subpats } => {
                let ftys = self.variant_field_types_in(scrut, &name.name);
                for (i, sp) in subpats.iter().enumerate() {
                    let fty = ftys.get(i).cloned().unwrap_or(Ty::Unknown);
                    self.bind_pattern_types(scope, &fty, *sp);
                }
            }
            PatKind::StructVariant { fields, .. } => {
                // The table doesn't carry enum-variant field *names*, so the named
                // bindings are typed leniently (`Unknown`) — cgen still projects the
                // concrete field type from the variant declaration.
                for (_, sp) in fields {
                    self.bind_pattern_types(scope, &Ty::Unknown, *sp);
                }
            }
            // Scalar patterns and `..` rest bind nothing.
            PatKind::Lit(_) | PatKind::Range { .. } | PatKind::Rest => {}
            PatKind::Or(alts) => {
                // Alternatives should bind the same names; bind each so any binding
                // is in scope for the body (the bootstrap doesn't check consistency).
                for sp in alts {
                    self.bind_pattern_types(scope, scrut, *sp);
                }
            }
            PatKind::Wildcard | PatKind::Error => {}
        }
    }

    /// The payload field types of an enum variant, in order.
    fn variant_field_types(&self, vname: &str) -> Vec<Ty> {
        if let Some(&ei) = self.table.variants.get(vname) {
            if let TypeKindG::Enum { variants } = &self.table.types[ei].kind {
                if let Some((_, ftys)) = variants.iter().find(|(n, _)| n == vname) {
                    return ftys.clone();
                }
            }
        }
        Vec::new()
    }

    /// Variant field types projected onto a *specific* scrutinee — for a generic
    /// enum instance `Option(i32)`, the template's `T` becomes `i32` so a pattern
    /// `some(p)` binds `p` to the concrete payload type.
    fn variant_field_types_in(&self, scrut: &Ty, vname: &str) -> Vec<Ty> {
        let base = self.variant_field_types(vname);
        if let Ty::GenEnum { ctor, args } = scrut {
            if let Some(&ei) = self.table.type_index.get(ctor) {
                let subst: HashMap<String, Ty> = self.table.types[ei]
                    .type_params
                    .iter()
                    .cloned()
                    .zip(args.iter().cloned())
                    .collect();
                return base.iter().map(|t| subst_ty(t, &subst)).collect();
            }
        }
        base
    }

    /// Check a `match` for exhaustiveness (a hard error) and redundant/unreachable
    /// arms (a warning), using Maranget's *usefulness* algorithm over enum
    /// patterns and an interval analysis over scalar ones. Guarded arms are
    /// excluded — a guard may be false, so a guarded arm neither proves coverage
    /// nor can be deemed unreachable.
    fn check_exhaustive(&mut self, span: Span, scrut: &Ty, arms: &[MatchArm]) {
        if let Ty::Prim(p) = scrut {
            if is_scalar_match_ty(p) {
                self.check_scalar_match(span, p, arms);
            }
            return;
        }
        let ei = match scrut {
            Ty::Named(i) => *i,
            Ty::GenEnum { ctor, .. } => match self.table.type_index.get(ctor) {
                Some(&i) => i,
                None => return,
            },
            _ => return,
        };
        if !matches!(self.table.types[ei].kind, TypeKindG::Enum { .. }) {
            return; // matching a struct, not an enum
        }
        self.check_enum_match(span, arms);
    }

    /// Maranget usefulness over enum patterns: nested-pattern exhaustiveness plus
    /// redundant-arm detection.
    fn check_enum_match(&mut self, span: Span, arms: &[MatchArm]) {
        let mut matrix: Vec<Vec<Pat>> = Vec::new();
        for arm in arms {
            if arm.guard.is_some() {
                continue;
            }
            let row = vec![self.lower_pat(arm.pat)];
            // An arm useless against the rows above it is unreachable.
            if !self.useful(&matrix, &row) {
                let sp = self.ast.pat_at(arm.pat).span;
                self.warn(sp, "unreachable match arm: already covered by an earlier arm");
            }
            matrix.push(row);
        }
        // Exhaustive iff the all-wildcard vector is *not* useful against the matrix.
        if self.useful(&matrix, &[Pat::Wild]) {
            let msg = self.non_exhaustive_message(&matrix);
            self.error(span, msg);
        }
    }

    /// Scalar `match` (integer/char/bool): exhaustiveness via interval coverage of
    /// the type's domain, and redundancy when an arm's value-set is already covered.
    fn check_scalar_match(&mut self, span: Span, prim: &str, arms: &[MatchArm]) {
        let bounds = scalar_bounds(prim);
        let mut covered: Vec<(i128, i128)> = Vec::new();
        let mut full = false; // has an unguarded wildcard/binding been seen?
        for arm in arms {
            if arm.guard.is_some() {
                continue;
            }
            let mut my: Vec<(i128, i128)> = Vec::new();
            let mut my_full = false;
            self.collect_scalar_intervals(arm.pat, &mut my, &mut my_full);
            let redundant = if my_full {
                full // a catch-all after a catch-all
            } else {
                !my.is_empty() && my.iter().all(|(lo, hi)| full || intervals_cover(*lo, *hi, &covered))
            };
            if redundant {
                let sp = self.ast.pat_at(arm.pat).span;
                self.warn(sp, "unreachable match arm: already covered by an earlier arm");
            }
            if my_full {
                full = true;
            }
            covered.extend(my);
        }
        let exhaustive = full
            || match bounds {
                Some((lo, hi)) => intervals_cover(lo, hi, &covered),
                None => false, // platform-width type → require a catch-all
            };
        if !exhaustive {
            self.error(
                span,
                "non-exhaustive `match`: a scalar `match` needs a `_`/binding catch-all (or full coverage)"
                    .to_string(),
            );
        }
    }

    /// Build a helpful non-exhaustive message — naming missing top-level variants
    /// when possible, else a generic "some cases aren't covered".
    fn non_exhaustive_message(&self, matrix: &[Vec<Pat>]) -> String {
        let mut present: HashSet<String> = HashSet::new();
        for row in matrix {
            Self::collect_head_variants(&row[0], &mut present);
        }
        if let Some(name) = present.iter().next() {
            if let Some(vs) = self.enum_variants_of(name) {
                let missing: Vec<String> =
                    vs.into_iter().map(|(n, _)| n).filter(|v| !present.contains(v)).collect();
                if !missing.is_empty() {
                    return format!("non-exhaustive `match`: missing `{}`", missing.join("`, `"));
                }
            }
        }
        "non-exhaustive `match`: some cases aren't covered".to_string()
    }

    // --- Maranget usefulness ---

    /// Is `q` useful w.r.t. the pattern `matrix` — i.e. does it match some value no
    /// row already matches? (Maranget, "Warnings for pattern matching", 2007.)
    fn useful(&self, matrix: &[Vec<Pat>], q: &[Pat]) -> bool {
        let q0 = match q.first() {
            None => return matrix.is_empty(), // base case: useful iff no rows remain
            Some(p) => p,
        };
        let qrest = &q[1..];
        match q0 {
            Pat::Or(alts) => alts.iter().any(|a| {
                let mut nq = vec![a.clone()];
                nq.extend_from_slice(qrest);
                self.useful(matrix, &nq)
            }),
            Pat::Var(name, args) => {
                let spec = self.specialize_var(matrix, name, args.len());
                let mut nq = args.clone();
                nq.extend_from_slice(qrest);
                self.useful(&spec, &nq)
            }
            Pat::Int(v) => {
                let spec = self.specialize_value(matrix, *v);
                self.useful(&spec, qrest)
            }
            Pat::Range(lo, hi) => self.useful_range(matrix, *lo, *hi, qrest),
            Pat::Wild => match self.col_kind(matrix) {
                ColKind::Enum(variants) => {
                    let present = self.head_variants(matrix);
                    let complete = variants.iter().all(|(n, _)| present.contains(n));
                    if complete {
                        // The signature is covered; q is useful iff useful under some
                        // constructor (a witness picks one and recurses on its fields).
                        variants.iter().any(|(n, ar)| {
                            let spec = self.specialize_var(matrix, n, *ar);
                            let mut nq = vec![Pat::Wild; *ar];
                            nq.extend_from_slice(qrest);
                            self.useful(&spec, &nq)
                        })
                    } else {
                        // A missing constructor witnesses usefulness — recurse on the
                        // default (wildcard) matrix.
                        let def = self.default_matrix(matrix);
                        self.useful(&def, qrest)
                    }
                }
                // Scalar columns are treated as never complete (a wildcard sibling is
                // required); interval coverage of *finite* types is handled by the
                // dedicated scalar-match check, not the matrix.
                ColKind::Scalar | ColKind::WildOnly => {
                    let def = self.default_matrix(matrix);
                    self.useful(&def, qrest)
                }
            },
        }
    }

    /// The default matrix `D(P)`: rows whose first pattern matches *any* value,
    /// with the first column removed (or-patterns split first).
    fn default_matrix(&self, matrix: &[Vec<Pat>]) -> Vec<Vec<Pat>> {
        let mut out = Vec::new();
        for row in matrix {
            Self::default_row(row, &mut out);
        }
        out
    }

    fn default_row(row: &[Pat], out: &mut Vec<Vec<Pat>>) {
        let (head, rest) = row.split_first().expect("row has a column");
        match head {
            Pat::Wild => out.push(rest.to_vec()),
            Pat::Or(alts) => {
                for a in alts {
                    let mut nr = vec![a.clone()];
                    nr.extend_from_slice(rest);
                    Self::default_row(&nr, out);
                }
            }
            _ => {} // a constructor row is dropped
        }
    }

    /// The specialized matrix `S(c, P)` for an enum constructor of the given arity.
    fn specialize_var(&self, matrix: &[Vec<Pat>], ctor: &str, arity: usize) -> Vec<Vec<Pat>> {
        let mut out = Vec::new();
        for row in matrix {
            Self::spec_var_row(row, ctor, arity, &mut out);
        }
        out
    }

    fn spec_var_row(row: &[Pat], ctor: &str, arity: usize, out: &mut Vec<Vec<Pat>>) {
        let (head, rest) = row.split_first().expect("row has a column");
        match head {
            Pat::Wild => {
                let mut r = vec![Pat::Wild; arity];
                r.extend_from_slice(rest);
                out.push(r);
            }
            Pat::Var(n, args) if n == ctor => {
                let mut r = args.clone();
                r.extend_from_slice(rest);
                out.push(r);
            }
            Pat::Or(alts) => {
                for a in alts {
                    let mut nr = vec![a.clone()];
                    nr.extend_from_slice(rest);
                    Self::spec_var_row(&nr, ctor, arity, out);
                }
            }
            _ => {} // a different constructor (or a scalar) is dropped
        }
    }

    /// Specialize a scalar column by a concrete value: keep rows whose head pattern
    /// matches `v`, dropping the column.
    fn specialize_value(&self, matrix: &[Vec<Pat>], v: i128) -> Vec<Vec<Pat>> {
        let mut out = Vec::new();
        for row in matrix {
            Self::spec_val_row(row, v, &mut out);
        }
        out
    }

    fn spec_val_row(row: &[Pat], v: i128, out: &mut Vec<Vec<Pat>>) {
        let (head, rest) = row.split_first().expect("row has a column");
        match head {
            Pat::Wild => out.push(rest.to_vec()),
            Pat::Int(w) if *w == v => out.push(rest.to_vec()),
            Pat::Range(lo, hi) if *lo <= v && v <= *hi => out.push(rest.to_vec()),
            Pat::Or(alts) => {
                for a in alts {
                    let mut nr = vec![a.clone()];
                    nr.extend_from_slice(rest);
                    Self::spec_val_row(&nr, v, out);
                }
            }
            _ => {}
        }
    }

    /// Usefulness of a range pattern — precise for a single scalar column (the
    /// common case), conservative (assumed useful) for nested positions.
    fn useful_range(&self, matrix: &[Vec<Pat>], lo: i128, hi: i128, qrest: &[Pat]) -> bool {
        if !qrest.is_empty() {
            return true;
        }
        let mut covered: Vec<(i128, i128)> = Vec::new();
        let mut full = false;
        for row in matrix {
            Self::collect_pat_intervals(&row[0], &mut covered, &mut full);
        }
        if full {
            return false;
        }
        !intervals_cover(lo, hi, &covered)
    }

    fn collect_pat_intervals(p: &Pat, out: &mut Vec<(i128, i128)>, full: &mut bool) {
        match p {
            Pat::Wild => *full = true,
            Pat::Int(v) => out.push((*v, *v)),
            Pat::Range(a, b) => out.push((*a, *b)),
            Pat::Or(alts) => {
                for a in alts {
                    Self::collect_pat_intervals(a, out, full);
                }
            }
            Pat::Var(_, _) => {}
        }
    }

    /// The kind of a matrix's first column — an enum (with its full variant set),
    /// a scalar, or wildcards only.
    fn col_kind(&self, matrix: &[Vec<Pat>]) -> ColKind {
        let mut variant: Option<String> = None;
        let mut scalar = false;
        for row in matrix {
            Self::scan_head(&row[0], &mut variant, &mut scalar);
        }
        if let Some(n) = variant {
            if let Some(vs) = self.enum_variants_of(&n) {
                return ColKind::Enum(vs);
            }
        }
        if scalar {
            ColKind::Scalar
        } else {
            ColKind::WildOnly
        }
    }

    fn scan_head(p: &Pat, variant: &mut Option<String>, scalar: &mut bool) {
        match p {
            Pat::Var(n, _) => {
                if variant.is_none() {
                    *variant = Some(n.clone());
                }
            }
            Pat::Int(_) | Pat::Range(_, _) => *scalar = true,
            Pat::Or(alts) => {
                for a in alts {
                    Self::scan_head(a, variant, scalar);
                }
            }
            Pat::Wild => {}
        }
    }

    fn head_variants(&self, matrix: &[Vec<Pat>]) -> HashSet<String> {
        let mut out = HashSet::new();
        for row in matrix {
            Self::collect_head_variants(&row[0], &mut out);
        }
        out
    }

    fn collect_head_variants(p: &Pat, out: &mut HashSet<String>) {
        match p {
            Pat::Var(n, _) => {
                out.insert(n.clone());
            }
            Pat::Or(alts) => {
                for a in alts {
                    Self::collect_head_variants(a, out);
                }
            }
            _ => {}
        }
    }

    /// The full variant set (name + arity) of the enum that owns `vname`.
    fn enum_variants_of(&self, vname: &str) -> Option<Vec<(String, usize)>> {
        let &ei = self.table.variants.get(vname)?;
        if let TypeKindG::Enum { variants } = &self.table.types[ei].kind {
            Some(variants.iter().map(|(n, ftys)| (n.clone(), ftys.len())).collect())
        } else {
            None
        }
    }

    // --- pattern lowering (AST → the usefulness IR) ---

    fn lower_pat(&self, pat: PatId) -> Pat {
        match &self.ast.pat_at(pat).kind {
            PatKind::Wildcard | PatKind::Rest | PatKind::Error => Pat::Wild,
            PatKind::Ident(n) => {
                if self.table.variants.contains_key(&n.name) {
                    Pat::Var(n.name.clone(), vec![])
                } else {
                    Pat::Wild // a binding matches anything
                }
            }
            PatKind::Variant { name, subpats } => {
                let arity = self.variant_field_types(&name.name).len();
                let mut args = Vec::new();
                for sp in subpats {
                    if matches!(self.ast.pat_at(*sp).kind, PatKind::Rest) {
                        break; // trailing `..` → the remaining fields are wildcards
                    }
                    args.push(self.lower_pat(*sp));
                }
                while args.len() < arity {
                    args.push(Pat::Wild);
                }
                Pat::Var(name.name.clone(), args)
            }
            PatKind::StructVariant { name, .. } => {
                // For usefulness, a struct-variant covers its variant; the named
                // field patterns are treated as wildcards (named-field dispatch
                // lives in cgen; nested constructors-in-named are rare and lenient).
                let arity = self.variant_field_types(&name.name).len();
                Pat::Var(name.name.clone(), vec![Pat::Wild; arity])
            }
            PatKind::Lit(e) => match self.eval_pat_int(*e) {
                Some(v) => Pat::Int(v),
                None => Pat::Wild, // un-evaluable literal: treat conservatively
            },
            PatKind::Range { lo, hi, inclusive } => {
                match (self.eval_pat_int(*lo), self.eval_pat_int(*hi)) {
                    (Some(a), Some(b)) => {
                        let b = if *inclusive { b } else { b - 1 };
                        Pat::Range(a, b)
                    }
                    _ => Pat::Wild,
                }
            }
            PatKind::Or(alts) => Pat::Or(alts.iter().map(|p| self.lower_pat(*p)).collect()),
        }
    }

    /// Accumulate the value-intervals an (unguarded) scalar pattern covers, and
    /// whether it is a catch-all.
    fn collect_scalar_intervals(&self, pat: PatId, out: &mut Vec<(i128, i128)>, full: &mut bool) {
        match &self.ast.pat_at(pat).kind {
            PatKind::Wildcard | PatKind::Rest => *full = true,
            PatKind::Ident(n) => {
                if !self.table.variants.contains_key(&n.name) {
                    *full = true; // a binding catches everything
                }
            }
            PatKind::Lit(e) => {
                if let Some(v) = self.eval_pat_int(*e) {
                    out.push((v, v));
                }
            }
            PatKind::Range { lo, hi, inclusive } => {
                if let (Some(a), Some(b)) = (self.eval_pat_int(*lo), self.eval_pat_int(*hi)) {
                    let b = if *inclusive { b } else { b - 1 };
                    if a <= b {
                        out.push((a, b));
                    }
                }
            }
            PatKind::Or(alts) => {
                for p in alts {
                    self.collect_scalar_intervals(*p, out, full);
                }
            }
            PatKind::Variant { .. } | PatKind::StructVariant { .. } | PatKind::Error => {}
        }
    }

    /// Evaluate a literal pattern expression to an integer value (covers integer,
    /// `char`, and `bool` literals, and a leading unary minus).
    fn eval_pat_int(&self, e: ExprId) -> Option<i128> {
        match &self.ast.expr_at(e).kind {
            ExprKind::Int(t) => parse_int_lit(t),
            ExprKind::Bool(b) => Some(if *b { 1 } else { 0 }),
            ExprKind::Char(t) => parse_char_lit(t),
            ExprKind::Unary { op: UnOp::Neg, rhs } => self.eval_pat_int(*rhs).map(|v| -v),
            _ => None,
        }
    }
}

/// The usefulness-algorithm IR a match pattern lowers to (a structural view that
/// drops bindings and source spans — see [`TypeChecker::lower_pat`]).
#[derive(Clone, Debug)]
enum Pat {
    /// `_`, a binding, `..` rest, or an un-evaluable literal — matches anything.
    Wild,
    /// An enum variant constructor with its (positionally lowered) field patterns.
    Var(String, Vec<Pat>),
    /// A concrete scalar value (integer/char/bool literal).
    Int(i128),
    /// An inclusive integer range.
    Range(i128, i128),
    /// An or-pattern — matches if any alternative does.
    Or(Vec<Pat>),
}

/// What a usefulness matrix's first column ranges over.
enum ColKind {
    /// An enum, with its full `(variant, arity)` signature.
    Enum(Vec<(String, usize)>),
    /// A scalar (integer/char/bool) — treated as never fully covered by the matrix.
    Scalar,
    /// Wildcards only — no constructors to split on.
    WildOnly,
}

/// Parse an integer literal's source text to a value (handles `_` separators and
/// `0x`/`0b`/`0o` radices).
fn parse_int_lit(text: &str) -> Option<i128> {
    let t: String = text.chars().filter(|c| *c != '_').collect();
    let t = t.trim();
    if let Some(h) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        i128::from_str_radix(h, 16).ok()
    } else if let Some(b) = t.strip_prefix("0b").or_else(|| t.strip_prefix("0B")) {
        i128::from_str_radix(b, 2).ok()
    } else if let Some(o) = t.strip_prefix("0o").or_else(|| t.strip_prefix("0O")) {
        i128::from_str_radix(o, 8).ok()
    } else {
        t.parse::<i128>().ok()
    }
}

/// Parse a char literal's source text (`'a'`, `'\n'`, …) to its codepoint value.
fn parse_char_lit(text: &str) -> Option<i128> {
    let inner = text.strip_prefix('\'')?.strip_suffix('\'')?;
    let mut chars = inner.chars();
    let c = chars.next()?;
    let value = if c == '\\' {
        let e = chars.next()?;
        match e {
            'n' => '\n',
            't' => '\t',
            'r' => '\r',
            '0' => '\0',
            '\\' => '\\',
            '\'' => '\'',
            '"' => '"',
            _ => return None,
        }
    } else {
        c
    };
    if chars.next().is_some() {
        return None; // more than one char/escape — not a simple char literal
    }
    Some(value as i128)
}

/// The inclusive `[min, max]` value range of a finite scalar type, or `None` for a
/// platform-width type (`usize`/`isize`) whose domain can't be enumerated here.
fn scalar_bounds(p: &str) -> Option<(i128, i128)> {
    let r = match p {
        "bool" => (0, 1),
        "u8" => (0, 255),
        "i8" => (-128, 127),
        "u16" => (0, 65_535),
        "i16" => (-32_768, 32_767),
        "char" => (0, 0x10_FFFF),
        "u32" => (0, 4_294_967_295),
        "i32" => (-2_147_483_648, 2_147_483_647),
        "u64" => (0, u64::MAX as i128),
        "i64" => (i64::MIN as i128, i64::MAX as i128),
        _ => return None,
    };
    Some(r)
}

/// Do the `covered` intervals fully cover the inclusive range `[lo, hi]`?
fn intervals_cover(lo: i128, hi: i128, covered: &[(i128, i128)]) -> bool {
    if lo > hi {
        return true;
    }
    let mut iv: Vec<(i128, i128)> = covered.iter().copied().filter(|(a, b)| a <= b).collect();
    iv.sort_unstable();
    let mut cur = lo;
    for (a, b) in iv {
        if a > cur {
            return false; // a gap before this interval
        }
        if b >= cur {
            if b >= hi {
                return true;
            }
            cur = b + 1;
        }
    }
    false
}

/// The scalar types a `match` can dispatch on by value (integer/char/bool) — i.e.
/// where literal/range patterns are meaningful. Floats are excluded (equality is a
/// footgun).
pub(crate) fn is_scalar_match_ty(p: &str) -> bool {
    matches!(
        p,
        "i8" | "i16" | "i32" | "i64" | "u8" | "u16" | "u32" | "u64" | "usize" | "isize" | "char"
            | "bool"
    )
}

/// The return type of a string-library intrinsic (which isn't a declared
/// function), so a `let` bound to one gets the right C type without an explicit
/// annotation. `from_utf8` traps on invalid input today, so it yields `str`
/// directly (the recoverable `str !Utf8Error` form is a future refinement).
fn string_intrinsic_ret(name: &str) -> Option<Ty> {
    Some(match name {
        "substr" | "from_utf8" | "trim" => Ty::Prim("str"),
        "os_from_bytes" => Ty::Prim("os_str"),
        "to_str_lossy" => Ty::Prim("String"),
        "cow_borrow" | "cow_to_mut" => Ty::Prim("Cow"),
        "cow_view" => Ty::Prim("str"),
        "cow_is_owned" => Ty::Prim("bool"),
        // Recoverable: yields a Result so `is_err`/`unwrap`/`?` compose.
        "try_from_utf8" => Ty::Result(Box::new(Ty::Prim("str"))),
        "count_codepoints" | "count_graphemes" => Ty::Prim("usize"),
        "find" => Ty::Prim("isize"),
        "is_utf8" | "str_eq" | "eq_fold" | "starts_with" | "ends_with" | "contains" => {
            Ty::Prim("bool")
        }
        _ => return None,
    })
}

/// Does the parameter type's head constructor match the receiver's? Confirms
/// that `base.name(...)` really is a method on `base`'s type (and not a typo
/// that happens to share a name with some unrelated function).
/// The built-in operator traits (traits, Stage E): `(trait name, method name)`.
/// Four "primitive" operator methods a user type implements directly —
/// `+`→`Add::add`, `-`→`Sub::sub`, `*`→`Mul::mul`, `/`→`Div::div`, `==`→`Eq::eq`,
/// `<`→`Ord::lt`. The remaining comparisons are *derived* from `Eq`/`Ord` (see
/// [`op_trait_method`]), so a user type opts into the full operator set by
/// `impl`-ing just these six.
const OPERATOR_TRAITS: [(&str, &str); 6] = [
    ("Add", "add"),
    ("Sub", "sub"),
    ("Mul", "mul"),
    ("Div", "div"),
    ("Eq", "eq"),
    ("Ord", "lt"),
];

/// The `(trait, method)` a trait-backed binary operator desugars to, or `None`
/// for an operator with native-only semantics (`%`, `&&`, bit-ops, …). The
/// *derived* comparisons reuse one base method and are completed by a swap/negate
/// at lowering time ([`crate::cgen`]): `!=`→`Eq::eq` (negated), `>`→`Ord::lt`
/// (swapped), `<=`→`Ord::lt` (swapped+negated), `>=`→`Ord::lt` (negated).
fn op_trait_method(op: BinOp) -> Option<(&'static str, &'static str)> {
    use BinOp::*;
    Some(match op {
        Add => ("Add", "add"),
        Sub => ("Sub", "sub"),
        Mul => ("Mul", "mul"),
        Div => ("Div", "div"),
        Eq | Ne => ("Eq", "eq"),
        Lt | Gt | Le | Ge => ("Ord", "lt"),
        _ => return None,
    })
}

/// The source spelling of a trait-backed operator, for diagnostics.
fn op_symbol(op: BinOp) -> &'static str {
    use BinOp::*;
    match op {
        Add => "+",
        Sub => "-",
        Mul => "*",
        Div => "/",
        Eq => "==",
        Ne => "!=",
        Lt => "<",
        Gt => ">",
        Le => "<=",
        Ge => ">=",
        _ => "?",
    }
}

fn head_matches(param: &Ty, recv: &Ty) -> bool {
    match (param, recv) {
        (Ty::GenStruct { ctor: a, .. }, Ty::GenStruct { ctor: b, .. }) => a == b,
        (Ty::Named(a), Ty::Named(b)) => a == b,
        _ => false,
    }
}

/// Unify a parameter type against an actual type, binding any type parameters
/// it mentions (names in `tps`) into `subst`. A one-directional match — enough
/// to recover `T = i32` from `List(T)` vs `List(i32)`.
pub(crate) fn unify_tp(param: &Ty, actual: &Ty, tps: &HashSet<String>, subst: &mut HashMap<String, Ty>) {
    match (param, actual) {
        (Ty::Opaque(n), a) if tps.contains(n) => {
            subst.entry(n.clone()).or_insert_with(|| a.clone());
        }
        (Ty::GenStruct { ctor: c1, args: a1 }, Ty::GenStruct { ctor: c2, args: a2 })
        | (Ty::GenEnum { ctor: c1, args: a1 }, Ty::GenEnum { ctor: c2, args: a2 })
            if c1 == c2 =>
        {
            for (p, x) in a1.iter().zip(a2) {
                unify_tp(p, x, tps, subst);
            }
        }
        (Ty::Ptr { inner: i1, .. }, Ty::Ptr { inner: i2, .. }) => unify_tp(i1, i2, tps, subst),
        (Ty::Result(o1), Ty::Result(o2)) => unify_tp(o1, o2, tps, subst),
        (Ty::Slice(e1), Ty::Slice(e2)) => unify_tp(e1, e2, tps, subst),
        (Ty::GenRef(e1), Ty::GenRef(e2)) => unify_tp(e1, e2, tps, subst),
        (Ty::RegionRef(e1), Ty::RegionRef(e2)) => unify_tp(e1, e2, tps, subst),
        _ => {}
    }
}

/// Substitute type parameters (`Ty::Opaque(name)`) throughout a type.
fn subst_ty(ty: &Ty, subst: &HashMap<String, Ty>) -> Ty {
    match ty {
        Ty::Opaque(n) => subst.get(n).cloned().unwrap_or_else(|| ty.clone()),
        Ty::Ptr { mutbl, inner } => Ty::Ptr { mutbl: *mutbl, inner: Box::new(subst_ty(inner, subst)) },
        Ty::Result(ok) => Ty::Result(Box::new(subst_ty(ok, subst))),
        Ty::GenStruct { ctor, args } => Ty::GenStruct {
            ctor: ctor.clone(),
            args: args.iter().map(|a| subst_ty(a, subst)).collect(),
        },
        Ty::GenEnum { ctor, args } => Ty::GenEnum {
            ctor: ctor.clone(),
            args: args.iter().map(|a| subst_ty(a, subst)).collect(),
        },
        Ty::Slice(elem) => Ty::Slice(Box::new(subst_ty(elem, subst))),
        Ty::GenRef(elem) => Ty::GenRef(Box::new(subst_ty(elem, subst))),
        Ty::RegionRef(elem) => Ty::RegionRef(Box::new(subst_ty(elem, subst))),
        Ty::Fn { params, ret, ret_conv } => Ty::Fn {
            params: params.iter().map(|(c, t)| (*c, Box::new(subst_ty(t, subst)))).collect(),
            ret: Box::new(subst_ty(ret, subst)),
            ret_conv: *ret_conv,
        },
        _ => ty.clone(),
    }
}

fn scope_lookup(scope: &Scope, name: &str) -> Option<Ty> {
    for s in scope.iter().rev() {
        if let Some(t) = s.get(name) {
            return Some(t.clone());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::Lexer;
    use crate::parser::Parser;

    fn analyze(src: &str) -> (TypeInfo, Vec<Diagnostic>) {
        let (tokens, ld) = Lexer::new(src).tokenize();
        assert!(ld.is_empty(), "lex: {:?}", ld);
        let (ast, pd) = Parser::new(src, tokens).parse();
        assert!(pd.is_empty(), "parse: {:?}", pd);
        check(&ast)
    }

    /// Like [`analyze`] but keeps the AST too, so a test can locate a specific
    /// expression and assert its inferred type.
    fn analyze_full(src: &str) -> (Ast, TypeInfo) {
        let (tokens, ld) = Lexer::new(src).tokenize();
        assert!(ld.is_empty(), "lex: {:?}", ld);
        let (ast, pd) = Parser::new(src, tokens).parse();
        assert!(pd.is_empty(), "parse: {:?}", pd);
        let (info, _d) = check(&ast);
        (ast, info)
    }

    // --- function-pointer types ---

    #[test]
    fn a_fn_pointer_type_is_copy_and_first_class() {
        // The escape checker keys off `is_copy`: a thin fn-pointer captures
        // nothing, so it is Copy — and therefore escapes freely.
        let tbl = GlobalTable::default();
        let fp = Ty::Fn {
            params: vec![(Conv::Read, Box::new(Ty::Prim("i32")))],
            ret: Box::new(Ty::Prim("i32")),
            ret_conv: Conv::Default,
        };
        assert!(fp.is_copy(&tbl), "a thin fn-pointer must be Copy / first-class");
    }

    #[test]
    fn indirect_call_through_a_parameter_is_typed_by_the_pointer_return() {
        // `f(x)` where `f: fn(i32) -> i64` types as i64 — distinct enough that a
        // coincidental i64 elsewhere can't make this pass by accident.
        let (ast, info) =
            analyze_full("fn apply(f: fn(i32) -> i64, x: i32) -> i64 { return f(x) }");
        let call = ast
            .exprs
            .iter()
            .enumerate()
            .find_map(|(i, e)| matches!(e.kind, ExprKind::Call { .. }).then_some(ExprId(i as u32)))
            .expect("the `f(x)` call");
        assert_eq!(info.type_of(call), &Ty::Prim("i64"), "indirect call types as the pointer return");
    }

    #[test]
    fn fn_pointer_field_call_is_not_mistaken_for_a_method() {
        // `a.alloc_fn(n)` on a fn-pointer *field* must resolve by the field's
        // type — no spurious "no field" / method-resolution diagnostic.
        let src = "struct A { alloc_fn: fn(i32) -> i32 } \
                   fn use_it(read a: A, n: i32) -> i32 { return a.alloc_fn(n) }";
        let (_i, d) = analyze(src);
        assert!(d.is_empty(), "a fn-pointer field call should typecheck cleanly: {:?}", d);
    }

    #[test]
    fn fn_pointer_field_call_types_by_the_field_return() {
        let (ast, info) = analyze_full(
            "struct A { op: fn(i32) -> i64 } \
             fn use_it(read a: A, n: i32) -> i64 { return a.op(n) }",
        );
        let call = ast
            .exprs
            .iter()
            .enumerate()
            .find_map(|(i, e)| matches!(e.kind, ExprKind::Call { .. }).then_some(ExprId(i as u32)))
            .expect("the `a.op(n)` call");
        assert_eq!(info.type_of(call), &Ty::Prim("i64"));
    }

    #[test]
    fn a_non_capturing_closure_coerces_to_a_fn_pointer_type() {
        // With a `fn(i32) -> i32` expected, an unannotated closure picks up the
        // parameter type and is itself typed as that function pointer.
        let (ast, info) = analyze_full("fn f() { let op: fn(i32) -> i32 = |x| x + 1 }");
        let clo = ast
            .exprs
            .iter()
            .enumerate()
            .find_map(|(i, e)| matches!(e.kind, ExprKind::Closure { .. }).then_some(ExprId(i as u32)))
            .expect("the closure");
        assert!(
            matches!(info.type_of(clo), Ty::Fn { .. }),
            "closure should coerce to a fn-pointer type, got {:?}",
            info.type_of(clo)
        );
    }

    #[test]
    fn a_closure_in_a_struct_field_coerces_to_the_field_fn_pointer() {
        // A vtable built directly from closure literals: the field's declared
        // fn-pointer type flows in as the expected type, so the closure coerces.
        let (ast, info) = analyze_full(
            "struct V { op: fn(i32) -> i32 } fn f() { let v = V{ op: |x| x + 1 } }",
        );
        let clo = ast
            .exprs
            .iter()
            .enumerate()
            .find_map(|(i, e)| matches!(e.kind, ExprKind::Closure { .. }).then_some(ExprId(i as u32)))
            .expect("the closure");
        assert!(
            matches!(info.type_of(clo), Ty::Fn { .. }),
            "a closure in a fn-pointer field should coerce, got {:?}",
            info.type_of(clo)
        );
    }

    #[test]
    fn a_closure_in_a_generic_struct_field_coerces_under_substitution() {
        // `Box(i32)`'s field `op: fn(T) -> T` resolves to `fn(i32) -> i32` under
        // T = i32, so the closure coerces to that concrete pointer type.
        let src = "fn Box(comptime T: type) -> type { return struct { op: fn(T) -> T } } \
                   fn f() { let b = Box(i32){ op: |x| x + 1 } }";
        let (ast, info) = analyze_full(src);
        let clo = ast
            .exprs
            .iter()
            .enumerate()
            .find_map(|(i, e)| matches!(e.kind, ExprKind::Closure { .. }).then_some(ExprId(i as u32)))
            .expect("the closure");
        match info.type_of(clo) {
            Ty::Fn { params, ret, .. } => {
                assert_eq!(params.len(), 1);
                assert_eq!(&*params[0].1, &Ty::Prim("i32"), "parameter substituted to i32");
                assert_eq!(&**ret, &Ty::Prim("i32"), "return substituted to i32");
            }
            other => panic!("expected fn(i32) -> i32, got {other:?}"),
        }
    }

    #[test]
    fn fn_pointer_field_call_on_a_generic_struct_types_by_the_substituted_return() {
        // The Main Objective: a fn-pointer-field call *method-style* on a
        // **generic-struct receiver**. `b: Box(i32)` has `op: fn(T) -> T`, so
        // `b.op(n)` must infer the *substituted* return `i32` — not `Unknown`
        // from the generic fallthrough. The teeth: a distinct return type (`i64`)
        // that can only be right if substitution under T = i64 actually ran.
        let src = "fn Box(comptime T: type) -> type { return struct { op: fn(T) -> T } } \
                   fn use_it(n: i64) -> i64 { let b = Box(i64){ op: |x| x + 1 } return b.op(n) }";
        let (ast, info) = analyze_full(src);
        let call = ast
            .exprs
            .iter()
            .enumerate()
            .find_map(|(i, e)| matches!(e.kind, ExprKind::Call { .. }).then_some(ExprId(i as u32)))
            .expect("the `b.op(n)` call");
        assert_eq!(
            info.type_of(call),
            &Ty::Prim("i64"),
            "generic-vtable field call types by the substituted return, not Unknown"
        );
    }

    #[test]
    fn fn_pointer_field_call_on_a_generic_struct_is_not_mistaken_for_a_method() {
        // Companion to the plain-struct diagnostic test: probing the generic
        // receiver's fn-pointer field must stay diagnostic-free (no spurious "no
        // field" / failed method-resolution error).
        let src = "fn Box(comptime T: type) -> type { return struct { op: fn(T) -> T } } \
                   fn use_it(n: i32) -> i32 { let b = Box(i32){ op: |x| x + 1 } return b.op(n) }";
        let (_i, d) = analyze(src);
        assert!(d.is_empty(), "a generic-struct fn-pointer field call should typecheck cleanly: {:?}", d);
    }

    #[test]
    fn bare_fn_ptr_field_read_on_a_generic_struct_types_under_substitution() {
        // The adjacent gap: *reading* (not calling) a fn-pointer field on a
        // generic-struct value. `let f = b.op` where `b: Box(i32)` must type `f`
        // as the substituted `fn(i32) -> i32` (was `Unknown`), so the later
        // `f(n)` is a typed indirect call. Teeth: the field-read expr is
        // `fn(i32) -> i32`, and `f(n)` infers `i32`.
        let src = "fn Box(comptime T: type) -> type { return struct { op: fn(T) -> T } } \
                   fn use_it(n: i32) -> i32 { let b = Box(i32){ op: |x| x + 1 } let f = b.op return f(n) }";
        let (ast, info) = analyze_full(src);
        let field = ast
            .exprs
            .iter()
            .enumerate()
            .find_map(|(i, e)| matches!(e.kind, ExprKind::Field { .. }).then_some(ExprId(i as u32)))
            .expect("the `b.op` field read");
        match info.type_of(field) {
            Ty::Fn { params, ret, .. } => {
                assert_eq!(&*params[0].1, &Ty::Prim("i32"), "field-read param substituted to i32");
                assert_eq!(&**ret, &Ty::Prim("i32"), "field-read return substituted to i32");
            }
            other => panic!("expected fn(i32) -> i32, got {other:?}"),
        }
        let call = ast
            .exprs
            .iter()
            .enumerate()
            .find_map(|(i, e)| matches!(e.kind, ExprKind::Call { .. }).then_some(ExprId(i as u32)))
            .expect("the `f(n)` call");
        assert_eq!(info.type_of(call), &Ty::Prim("i32"), "call through the read field types as i32");
    }

    #[test]
    fn unknown_field_on_a_generic_struct_is_reported() {
        // The new GenStruct arm of `field_type` still diagnoses a genuinely
        // missing field (not silently `Unknown`), matching the plain-struct path.
        let src = "fn Box(comptime T: type) -> type { return struct { op: fn(T) -> T } } \
                   fn use_it() { let b = Box(i32){ op: |x| x + 1 } let z = b.nope }";
        let (_i, d) = analyze(src);
        assert!(d.iter().any(|m| m.message.contains("no field `nope`")), "{:?}", d);
    }

    // --- traits: resolve + coherence (Stage B) ---

    #[test]
    fn resolves_a_trait_method_call_to_the_impl_return() {
        let (ast, info) = analyze_full(
            "trait Show { fn show(read self) -> i64 } \
             impl Show for i32 { fn show(read self) -> i64 { return 1 } } \
             fn use_it(read x: i32) -> i64 { return x.show() }",
        );
        let call = ast
            .exprs
            .iter()
            .enumerate()
            .find_map(|(i, e)| matches!(e.kind, ExprKind::Call { .. }).then_some(ExprId(i as u32)))
            .expect("the x.show() call");
        assert_eq!(info.type_of(call), &Ty::Prim("i64"), "call types as the impl method's return");
        assert!(info.impl_calls.contains_key(&call), "the resolution is recorded for the backend");
    }

    #[test]
    fn duplicate_impl_is_a_coherence_error() {
        let (_i, d) = analyze(
            "trait T { fn m(read self) -> i32 } \
             impl T for i32 { fn m(read self) -> i32 { return 1 } } \
             impl T for i32 { fn m(read self) -> i32 { return 2 } }",
        );
        assert!(
            d.iter().any(|m| m.message.contains("conflicting implementations")),
            "{:?}",
            d
        );
    }

    #[test]
    fn impl_missing_a_required_method_is_an_error() {
        let (_i, d) = analyze("trait T { fn need(read self) -> i32 } impl T for i32 { }");
        assert!(d.iter().any(|m| m.message.contains("missing method `need`")), "{:?}", d);
    }

    #[test]
    fn impl_of_an_unknown_trait_is_an_error() {
        let (_i, d) = analyze("impl Nope for i32 { fn m(read self) -> i32 { return 1 } }");
        assert!(d.iter().any(|m| m.message.contains("unknown trait `Nope`")), "{:?}", d);
    }

    #[test]
    fn impl_method_not_in_the_trait_is_an_error() {
        let (_i, d) = analyze(
            "trait T { fn m(read self) -> i32 } \
             impl T for i32 { fn m(read self) -> i32 { return 1 } \
                              fn extra(read self) -> i32 { return 2 } }",
        );
        assert!(d.iter().any(|m| m.message.contains("not a member of trait `T`")), "{:?}", d);
    }

    #[test]
    fn impl_may_omit_a_defaulted_method() {
        let (_i, d) = analyze(
            "trait T { fn show(read self) -> i32  fn label(read self) -> i32 { return 0 } } \
             impl T for i32 { fn show(read self) -> i32 { return 1 } }",
        );
        assert!(d.is_empty(), "a defaulted method may be omitted: {:?}", d);
    }

    // --- traits: definition-site bounds (Stage D) ---

    #[test]
    fn unsatisfied_definition_site_bound_is_an_error() {
        // `xuse[T: Show]` is instantiated at `bool`, which has no `impl Show` — a
        // call-site obligation failure ("blame the generic code, but the caller's
        // type must satisfy the contract").
        let (_i, d) = analyze(
            "trait Show { fn show(read self) -> i32 } \
             impl Show for i32 { fn show(read self) -> i32 { return 1 } } \
             fn xuse[T: Show](read x: T) -> i32 { return 0 } \
             fn caller(read b: bool) -> i32 { return xuse(b) }",
        );
        assert!(
            d.iter().any(|m| m.message.contains("does not implement trait `Show`")),
            "expected an unsatisfied-bound error: {:?}",
            d
        );
    }

    #[test]
    fn satisfied_definition_site_bound_is_accepted() {
        // Instantiated at `i32`, which *does* `impl Show` — no bound error.
        let (_i, d) = analyze(
            "trait Show { fn show(read self) -> i32 } \
             impl Show for i32 { fn show(read self) -> i32 { return 1 } } \
             fn xuse[T: Show](read x: T) -> i32 { return 0 } \
             fn caller(read n: i32) -> i32 { return xuse(n) }",
        );
        assert!(d.is_empty(), "a satisfied bound should type-check cleanly: {:?}", d);
    }

    #[test]
    fn unknown_trait_in_a_bound_is_a_definition_site_error() {
        // The declaration half: a bound naming an undeclared trait is caught at
        // the definition, not silently ignored.
        let (_i, d) = analyze("fn f[T: Bogus](read x: T) -> i32 { return 0 }");
        assert!(
            d.iter().any(|m| m.message.contains("unknown trait `Bogus`")),
            "expected an unknown-trait-in-bound error: {:?}",
            d
        );
    }

    #[test]
    fn a_struct_receiver_satisfies_a_bound_via_its_impl() {
        // The concrete type can be a user struct: the bound is keyed by the
        // struct's name, so its `impl` satisfies the obligation.
        let (_i, d) = analyze(
            "trait Show { fn show(read self) -> i32 } \
             struct P { a: i32 } \
             impl Show for P { fn show(read self) -> i32 { return self.a } } \
             fn xuse[T: Show](read x: T) -> i32 { return 0 } \
             fn caller(read p: P) -> i32 { return xuse(p) }",
        );
        assert!(d.is_empty(), "a struct with the impl satisfies the bound: {:?}", d);
    }

    #[test]
    fn an_unbounded_generic_param_is_never_a_bound_error() {
        // `[U]` (no bound) imposes no obligation — any instantiation is fine.
        let (_i, d) = analyze(
            "fn xid[U](read x: U) -> i32 { return 0 } \
             fn caller(read b: bool) -> i32 { return xid(b) }",
        );
        assert!(d.is_empty(), "an unbounded param accepts any type: {:?}", d);
    }

    // --- traits: operator traits (Stage E) ---

    #[test]
    fn an_arithmetic_operator_on_a_user_type_resolves_through_its_impl() {
        // `a + b` on a type with `impl Add` types as the impl's return (the type
        // itself) and is recorded for static dispatch.
        let (ast, info) = analyze_full(
            "struct V { n: i32 } \
             impl Add for V { fn add(read self, read rhs: V) -> V { return V{ n: self.n + rhs.n } } } \
             fn use_it(read a: V, read b: V) -> V { return a + b }",
        );
        // Find the operator-*dispatched* `a + b` (the impl body's own `self.n +
        // rhs.n` is native i32 with no `impl_calls` entry, so filter by that).
        let add = ast
            .exprs
            .iter()
            .enumerate()
            .find_map(|(i, e)| {
                let id = ExprId(i as u32);
                (matches!(e.kind, ExprKind::Binary { op: BinOp::Add, .. })
                    && info.impl_calls.contains_key(&id))
                .then_some(id)
            })
            .expect("the dispatched `a + b` expr");
        assert!(matches!(info.type_of(add), Ty::Named(_)), "operator result is the user type");
    }

    #[test]
    fn a_comparison_operator_on_a_user_type_resolves_to_bool() {
        // `a == b` on a type with `impl Eq` types as the impl's `bool` return.
        let (ast, info) = analyze_full(
            "struct V { n: i32 } \
             impl Eq for V { fn eq(read self, read rhs: V) -> bool { return self.n == rhs.n } } \
             fn use_it(read a: V, read b: V) -> bool { return a == b }",
        );
        let eq = ast
            .exprs
            .iter()
            .enumerate()
            .find_map(|(i, e)| {
                let id = ExprId(i as u32);
                (matches!(e.kind, ExprKind::Binary { op: BinOp::Eq, .. })
                    && info.impl_calls.contains_key(&id))
                .then_some(id)
            })
            .expect("the dispatched `a == b` expr");
        assert_eq!(info.type_of(eq), &Ty::Prim("bool"), "Eq::eq returns bool");
    }

    #[test]
    fn an_operator_on_a_user_type_without_the_impl_is_an_error() {
        // A user type used with `+` but lacking `impl Add` is rejected — clearer
        // than silently typing `Unknown` and emitting invalid C.
        let (_i, d) = analyze(
            "struct V { n: i32 } \
             fn use_it(read a: V, read b: V) -> V { return a + b }",
        );
        assert!(
            d.iter().any(|m| m.message.contains("does not implement `Add`")),
            "expected a missing-operator-impl error: {:?}",
            d
        );
    }

    #[test]
    fn primitive_arithmetic_is_not_routed_through_operator_traits() {
        // Primitives keep native semantics — no operator-trait resolution, no
        // `impl_calls` entry, just the usual numeric result.
        let (ast, info) = analyze_full("fn add(a: i32, b: i32) -> i32 { return a + b }");
        let plus = ast
            .exprs
            .iter()
            .enumerate()
            .find_map(|(i, e)| {
                matches!(e.kind, ExprKind::Binary { op: BinOp::Add, .. })
                    .then_some(ExprId(i as u32))
            })
            .expect("the `a + b` expr");
        assert_eq!(info.type_of(plus), &Ty::Prim("i32"));
        assert!(!info.impl_calls.contains_key(&plus), "primitives don't dispatch operators");
    }

    #[test]
    fn a_user_trait_named_like_an_operator_trait_collides() {
        // The built-in operator traits are reserved: a user `trait Add` conflicts
        // with the pre-registered one.
        let (_i, d) = analyze("trait Add { fn add(read self) -> i32 }");
        assert!(
            d.iter().any(|m| m.message.contains("duplicate definition of trait `Add`")),
            "{:?}",
            d
        );
    }

    #[test]
    fn subtraction_on_a_user_type_resolves_through_sub() {
        // `-` is its own primitive operator trait `Sub` (like `Add`/`Mul`/`Div`).
        let (ast, info) = analyze_full(
            "struct V { n: i32 } \
             impl Sub for V { fn sub(read self, read rhs: V) -> V { return V{ n: self.n - rhs.n } } } \
             fn use_it(read a: V, read b: V) -> V { return a - b }",
        );
        let sub = ast
            .exprs
            .iter()
            .enumerate()
            .find_map(|(i, e)| {
                let id = ExprId(i as u32);
                (matches!(e.kind, ExprKind::Binary { op: BinOp::Sub, .. })
                    && info.impl_calls.contains_key(&id))
                .then_some(id)
            })
            .expect("the dispatched `a - b`");
        assert!(matches!(info.type_of(sub), Ty::Named(_)), "Sub::sub returns the type");
    }

    #[test]
    fn derived_comparison_operators_resolve_through_their_base_trait() {
        // `>` and `!=` need only `Ord`/`Eq`: they reuse `lt`/`eq` (a swap/negate is
        // applied at lowering), and type as `bool`.
        let (ast, info) = analyze_full(
            "struct V { n: i32 } \
             impl Eq for V { fn eq(read self, read rhs: V) -> bool { return self.n == rhs.n } } \
             impl Ord for V { fn lt(read self, read rhs: V) -> bool { return self.n < rhs.n } } \
             fn use_it(read a: V, read b: V) -> bool { let p = a > b return a != b }",
        );
        for want in [BinOp::Gt, BinOp::Ne] {
            let e = ast
                .exprs
                .iter()
                .enumerate()
                .find_map(|(i, ex)| {
                    matches!(&ex.kind, ExprKind::Binary { op, .. } if *op == want)
                        .then_some(ExprId(i as u32))
                })
                .expect("the derived comparison");
            assert_eq!(info.type_of(e), &Ty::Prim("bool"), "{want:?} types as bool");
            assert!(info.impl_calls.contains_key(&e), "{want:?} resolved through its base trait");
        }
    }

    // --- bracket-generic monomorphization (typeck side) ---

    #[test]
    fn a_bracket_generic_call_infers_its_return_type() {
        // `dup[T](x: T) -> T` called at `5` infers `T = i32` from the value
        // argument, so the call types as `i32` (not the bare type parameter).
        let (ast, info) = analyze_full(
            "fn dup[T](take x: T) -> T { return x } \
             fn use_it() -> i32 { return dup(5) }",
        );
        let call = ast
            .exprs
            .iter()
            .enumerate()
            .find_map(|(i, e)| matches!(e.kind, ExprKind::Call { .. }).then_some(ExprId(i as u32)))
            .expect("the dup(5) call");
        assert_eq!(
            info.type_of(call),
            &Ty::Prim("i32"),
            "bracket-generic return inferred from the argument type"
        );
    }

    // --- body-side bound enforcement: the "Zig fix" ---

    #[test]
    fn a_bound_method_call_resolves_through_the_bound() {
        // Inside `describe[T: Show]`, `x.show()` resolves through `Show` and types
        // as `Show::show`'s return — and is recorded for per-instance dispatch.
        let (ast, info) = analyze_full(
            "trait Show { fn show(read self) -> i64 } \
             impl Show for i32 { fn show(read self) -> i64 { return 1 } } \
             fn describe[T: Show](read x: T) -> i64 { return x.show() }",
        );
        let call = ast
            .exprs
            .iter()
            .enumerate()
            .find_map(|(i, e)| {
                let id = ExprId(i as u32);
                (matches!(e.kind, ExprKind::Call { .. })
                    && info.bound_method_calls.contains_key(&id))
                .then_some(id)
            })
            .expect("the x.show() bound-method call");
        assert_eq!(info.type_of(call), &Ty::Prim("i64"), "typed by the bound method's return");
    }

    #[test]
    fn calling_a_non_bound_method_on_a_type_param_is_an_error() {
        // The headline "blame the generic code" check: a method the bound doesn't
        // provide is rejected at the generic's *definition*, not at a call site.
        let (_i, d) = analyze(
            "trait Show { fn show(read self) -> i32 } \
             fn describe[T: Show](read x: T) -> i32 { return x.other() }",
        );
        assert!(
            d.iter().any(|m| m.message.contains("its bound `Show` has no such method")),
            "{:?}",
            d
        );
    }

    #[test]
    fn calling_a_method_on_an_unbounded_type_param_is_an_error() {
        // No bound ⇒ no methods are available on the value at all.
        let (_i, d) = analyze("fn f[U](read x: U) -> i32 { return x.anything() }");
        assert!(
            d.iter().any(|m| m.message.contains("unbounded type parameter `U`")),
            "{:?}",
            d
        );
    }

    #[test]
    fn a_closure_without_an_expected_fn_pointer_stays_opaque() {
        // No coercion pressure ⇒ the closure keeps its fat opaque type (no
        // accidental change to existing closure behaviour).
        let (ast, info) = analyze_full("fn f() { let c = |x| x + 1 }");
        let clo = ast
            .exprs
            .iter()
            .enumerate()
            .find_map(|(i, e)| matches!(e.kind, ExprKind::Closure { .. }).then_some(ExprId(i as u32)))
            .expect("the closure");
        assert!(matches!(info.type_of(clo), Ty::Opaque(s) if s == "closure"));
    }

    #[test]
    fn types_the_vec_example_without_errors() {
        let src = include_str!("../examples/vec.jtr");
        let (_info, diags) = analyze(src);
        assert!(diags.is_empty(), "unexpected type errors: {:?}", diags);
    }

    #[test]
    fn reports_unknown_field_on_known_struct() {
        let (_i, d) = analyze("struct Point { x: i32, y: i32 } fn f(read p: Point) -> i32 { p.z }");
        assert_eq!(d.len(), 1, "{:?}", d);
        assert!(d[0].message.contains("no field `z` on struct `Point`"));
    }

    #[test]
    fn same_module_private_field_access_is_fine() {
        // Per-field visibility only restricts *cross-module* access; within a
        // module a private field is freely readable (no false positive).
        let (_i, d) = analyze("struct P { y: i32 } fn f(read p: P) -> i32 { return p.y }");
        assert!(d.is_empty(), "same-module access is free: {:?}", d);
    }

    #[test]
    fn reports_call_arity_mismatch() {
        let (_i, d) = analyze("fn g(a: i32) {} fn h() { g() }");
        assert_eq!(d.len(), 1, "{:?}", d);
        assert!(d[0].message.contains("expects 1 argument"));
    }

    #[test]
    fn reports_duplicate_definition() {
        let (_i, d) = analyze("fn dup() {} fn dup() {}");
        assert_eq!(d.len(), 1, "{:?}", d);
        assert!(d[0].message.contains("duplicate definition of `dup`"));
    }

    #[test]
    fn record_field_mutation_is_a_static_error() {
        let (_i, d) =
            analyze("record P { x: i32 } fn f(mut p: P) { p.x = 9 }");
        assert_eq!(d.len(), 1, "{:?}", d);
        assert!(
            d[0].message.contains("cannot assign to a field of immutable record `P`"),
            "{:?}",
            d
        );
    }

    #[test]
    fn record_reads_and_construction_are_fine() {
        // Constructing and reading a record is allowed; only field assignment isn't.
        let (_i, d) = analyze(
            "record P { x: i32, y: i32 } fn f() -> i32 { let p = P { x: 1, y: 2 } p.x }",
        );
        assert!(d.is_empty(), "unexpected errors: {:?}", d);
    }

    #[test]
    fn a_plain_struct_field_is_still_mutable() {
        // The record rule must not leak onto ordinary structs.
        let (_i, d) = analyze("struct S { x: i32 } fn f(mut s: S) { s.x = 9 }");
        assert!(d.is_empty(), "struct mutation should be allowed: {:?}", d);
    }

    #[test]
    fn reports_non_exhaustive_match() {
        let (_i, d) =
            analyze("enum E { a, b, c } fn f(read e: E) -> i32 { match e { a => 0, b => 1 } }");
        assert_eq!(d.len(), 1, "{:?}", d);
        assert!(d[0].message.contains("non-exhaustive"), "{:?}", d);
        assert!(d[0].message.contains('c'), "names the missing variant: {:?}", d);
    }

    #[test]
    fn rejects_by_value_recursive_field_but_allows_indirect() {
        // By-value self-reference is infinitely sized → error.
        let (_i, d) = analyze("enum List { nil, cons(tail: List) }");
        assert!(d.iter().any(|m| m.message.contains("infinitely sized")), "{:?}", d);
        let (_s, ds) = analyze("struct Node { next: Node }");
        assert!(ds.iter().any(|m| m.message.contains("infinitely sized")), "{:?}", ds);
        // Behind an indirection it's fine.
        let (_i2, d2) = analyze("enum List { nil, cons(tail: indirect List) }");
        assert!(d2.is_empty(), "indirect breaks the cycle: {:?}", d2);
        let (_i3, d3) = analyze("struct Node { next: *Node }");
        assert!(d3.is_empty(), "a pointer breaks the cycle: {:?}", d3);
    }

    #[test]
    fn distinct_types_are_not_interchangeable_with_their_base() {
        // Assigning the base type to a distinct binding (no cast) is an error.
        let (_i, d) =
            analyze("distinct UserId = i32 fn main() -> i32 { var x: UserId = 5 return 0 }");
        assert!(d.iter().any(|m| m.message.contains("distinct")), "{:?}", d);
        // An explicit `as` conversion is accepted.
        let (_i2, d2) =
            analyze("distinct UserId = i32 fn main() -> i32 { var x: UserId = 5 as UserId return 0 }");
        assert!(d2.is_empty(), "explicit `as` is fine: {:?}", d2);
    }

    #[test]
    fn wildcard_makes_match_exhaustive() {
        let (_i, d) =
            analyze("enum E { a, b, c } fn f(read e: E) -> i32 { match e { a => 0, _ => 1 } }");
        assert!(d.is_empty(), "a wildcard covers the rest: {:?}", d);
    }

    #[test]
    fn guarded_arm_does_not_satisfy_exhaustiveness() {
        // `a if cond` may not fire (the guard can be false), so it does NOT cover
        // `a`: the match is still missing `a`'s unguarded case.
        let src = "enum E { a(x: i32), b } \
                   fn f(read e: E) -> i32 { match e { a(v) if v > 0 => 1, b => 0 } }";
        let (_i, d) = analyze(src);
        assert_eq!(d.len(), 1, "{:?}", d);
        assert!(d[0].message.contains("non-exhaustive"), "{:?}", d);
        assert!(d[0].message.contains('a'), "names the still-uncovered variant: {:?}", d);
    }

    #[test]
    fn guarded_wildcard_does_not_make_a_match_exhaustive() {
        // Even a guarded catch-all doesn't prove coverage.
        let src = "enum E { a, b, c } \
                   fn f(read e: E) -> i32 { match e { a => 0, _ if other() => 1 } }";
        let (_i, d) = analyze(src);
        assert_eq!(d.len(), 1, "a guarded `_` is not a catch-all: {:?}", d);
        assert!(d[0].message.contains("non-exhaustive"), "{:?}", d);
    }

    #[test]
    fn or_pattern_covers_each_alternative_for_exhaustiveness() {
        // `red | green | blue` covers all three variants — exhaustive, no catch-all.
        let src = "enum C { red, green, blue } \
                   fn f(read c: C) -> i32 { match c { red | green | blue => 1 } }";
        let (_i, d) = analyze(src);
        assert!(d.is_empty(), "an or-pattern of all variants is exhaustive: {:?}", d);
    }

    #[test]
    fn or_pattern_missing_an_alternative_is_non_exhaustive() {
        let src = "enum C { red, green, blue } \
                   fn f(read c: C) -> i32 { match c { red | green => 1 } }";
        let (_i, d) = analyze(src);
        assert_eq!(d.len(), 1, "{:?}", d);
        assert!(d[0].message.contains("non-exhaustive"), "{:?}", d);
        assert!(d[0].message.contains("blue"), "names the uncovered variant: {:?}", d);
    }

    #[test]
    fn nested_pattern_exhaustiveness_via_maranget() {
        // `node(leaf, leaf)` covers only one shape of `node` — non-exhaustive.
        let src = "enum Tree { leaf, node(l: indirect Tree, r: indirect Tree) } \
                   fn f(read t: Tree) -> i32 { match t { leaf => 0, node(leaf, leaf) => 1 } }";
        let (_i, d) = analyze(src);
        assert!(
            d.iter().any(|x| x.is_error() && x.message.contains("non-exhaustive")),
            "nested gap should be caught: {:?}",
            d
        );
    }

    #[test]
    fn nested_wildcard_makes_it_exhaustive() {
        let src = "enum Tree { leaf, node(l: indirect Tree, r: indirect Tree) } \
                   fn f(read t: Tree) -> i32 { match t { leaf => 0, node(_, _) => 1 } }";
        let (_i, d) = analyze(src);
        assert!(d.iter().all(|x| !x.is_error()), "node(_, _) covers all nodes: {:?}", d);
    }

    #[test]
    fn redundant_enum_arm_is_a_warning() {
        // Exhaustive, but the second `red` is unreachable — a warning, not an error.
        let src = "enum C { red, green } \
                   fn f(read c: C) -> i32 { match c { red => 0, green => 1, red => 2 } }";
        let (_i, d) = analyze(src);
        assert!(d.iter().all(|x| !x.is_error()), "no error: {:?}", d);
        assert!(
            d.iter().any(|x| !x.is_error() && x.message.contains("unreachable")),
            "the duplicate arm warns: {:?}",
            d
        );
    }

    #[test]
    fn arm_after_a_wildcard_is_unreachable() {
        let src = "enum C { red, green } \
                   fn f(read c: C) -> i32 { match c { _ => 0, red => 1 } }";
        let (_i, d) = analyze(src);
        assert!(
            d.iter().any(|x| !x.is_error() && x.message.contains("unreachable")),
            "an arm after `_` warns: {:?}",
            d
        );
    }

    #[test]
    fn redundant_scalar_arm_is_a_warning() {
        // `5` is already covered by the earlier `0..=9`.
        let src = "fn f(read n: i32) -> i32 { match n { 0..=9 => 0, 5 => 1, _ => 2 } }";
        let (_i, d) = analyze(src);
        assert!(
            d.iter().any(|x| !x.is_error() && x.message.contains("unreachable")),
            "the subsumed literal warns: {:?}",
            d
        );
    }

    #[test]
    fn bool_match_is_exhaustive_without_a_catch_all() {
        // `true | false` covers `bool` — interval coverage the old name-set check missed.
        let src = "fn f(read b: bool) -> i32 { match b { true => 1, false => 0 } }";
        let (_i, d) = analyze(src);
        assert!(d.iter().all(|x| !x.is_error()), "true/false covers bool: {:?}", d);
    }

    #[test]
    fn full_range_makes_a_bounded_scalar_match_exhaustive() {
        let src = "fn f(read x: u8) -> i32 { match x { 0..=255 => 1 } }";
        let (_i, d) = analyze(src);
        assert!(d.iter().all(|x| !x.is_error()), "0..=255 covers u8: {:?}", d);
    }

    #[test]
    fn struct_variant_match_is_exhaustive_by_variant() {
        let src = "enum S { a(x: i32), b } \
                   fn f(read s: S) -> i32 { match s { a { x } => x, b => 0 } }";
        let (_i, d) = analyze(src);
        assert!(d.iter().all(|x| !x.is_error()), "named arms cover both variants: {:?}", d);
    }

    #[test]
    fn struct_variant_match_missing_variant_is_non_exhaustive() {
        let src = "enum S { a(x: i32), b } \
                   fn f(read s: S) -> i32 { match s { a { x } => x } }";
        let (_i, d) = analyze(src);
        assert!(
            d.iter().any(|x| x.is_error() && x.message.contains("non-exhaustive")),
            "{:?}",
            d
        );
    }

    #[test]
    fn scalar_match_without_catch_all_is_non_exhaustive() {
        // Literal/range arms can't enumerate the integer domain — a catch-all is
        // required.
        let (_i, d) = analyze("fn f(read n: i32) -> i32 { match n { 0 => 0, 1..=9 => 1 } }");
        assert_eq!(d.len(), 1, "{:?}", d);
        assert!(d[0].message.contains("non-exhaustive"), "{:?}", d);
        assert!(d[0].message.contains("catch-all"), "{:?}", d);
    }

    #[test]
    fn scalar_match_with_catch_all_is_exhaustive() {
        let (_i, d) = analyze("fn f(read n: i32) -> i32 { match n { 0 => 0, _ => 1 } }");
        assert!(d.is_empty(), "a `_` covers the rest: {:?}", d);
    }

    #[test]
    fn an_unguarded_fallback_after_guarded_arms_is_exhaustive() {
        // Guarded arms plus an unguarded fallback covering every variant is fine.
        let src = "enum E { a(x: i32), b } \
                   fn f(read e: E) -> i32 { match e { a(v) if v > 0 => 1, a(v) => 0 - v, b => 0 } }";
        let (_i, d) = analyze(src);
        assert!(d.is_empty(), "unguarded `a(v)`/`b` cover everything: {:?}", d);
    }

    #[test]
    fn projects_enum_payload_types() {
        // `circle(r)` binds `r : f64`; without projection `r` would be Unknown and
        // no `f64` would appear in the inferred types of this body.
        let src = "enum S { circle(r: f64), none } \
                   fn f(read s: S) -> f64 { match s { circle(r) => r, none => other() } }";
        let (info, d) = analyze(src);
        assert!(d.is_empty(), "{:?}", d);
        assert!(
            info.expr_types.iter().any(|t| *t == Ty::Prim("f64")),
            "payload `r` projected to f64"
        );
    }

    #[test]
    fn resolves_method_call_and_recovers_type_argument() {
        // `xs.get(0)` resolves to `get`, recovering T = i32 from the receiver.
        let src = "fn List(comptime T: type) -> type { return struct { ptr: *mut T, len: i32 } } \
                   fn get(comptime T: type, l: List(T), i: i32) -> T { unsafe { (l.ptr + i).* } } \
                   fn main() -> i32 { var xs = List(i32){ ptr: alloc(i32, 1), len: 0 } return xs.get(0) }";
        let (info, d) = analyze(src);
        assert!(d.is_empty(), "{:?}", d);
        let mr = info.method_calls.values().next().expect("a method call was recorded");
        assert_eq!(mr.fn_name, "get");
        assert_eq!(mr.type_args, vec![Ty::Prim("i32")], "type arg recovered from receiver");
    }

    #[test]
    fn resolves_a_struct_method_to_its_constructor() {
        // `xs.get(0)` resolves to the method inside `List(T)`, recording the
        // constructor `List` and the concrete type argument `i32`.
        let src = "fn List(comptime T: type) -> type { return struct { ptr: *mut T, len: i32, \
                       fn get(read self, i: i32) -> T { unsafe { (self.ptr + i).* } } } } \
                   fn new(comptime T: type) -> List(T) { return List(T){ ptr: alloc(T, 1), len: 0 } } \
                   fn main() -> i32 { var xs = new(i32) return xs.get(0) }";
        let (info, d) = analyze(src);
        assert!(d.is_empty(), "{:?}", d);
        let mr = info.method_calls.values().next().expect("a method call was recorded");
        assert_eq!(mr.fn_name, "get");
        assert_eq!(mr.recv_ctor.as_deref(), Some("List"));
        assert_eq!(mr.type_args, vec![Ty::Prim("i32")]);
    }

    #[test]
    fn resolves_a_generic_struct_method() {
        let src = "fn List(comptime T: type) -> type { return struct { ptr: *mut T, len: i32, \
                       fn get(read self, i: i32) -> T { unsafe { (self.ptr + i).* } } } } \
                   fn new(comptime T: type) -> List(T) { return List(T){ ptr: alloc(T, 4), len: 0 } } \
                   fn main() -> i32 { var xs = new(i32) return xs.get(0) }";
        let (info, d) = analyze(src);
        assert!(d.is_empty(), "{:?}", d);
        let mr = info.method_calls.values().next().expect("a method call was recorded");
        assert_eq!(mr.fn_name, "get");
        assert_eq!(mr.recv_ctor.as_deref(), Some("List"), "resolved to a struct method");
        assert_eq!(mr.type_args, vec![Ty::Prim("i32")]);
    }

    #[test]
    fn infers_field_and_primitive_types() {
        // `p.value` has the field's declared type; escape will use this for Copy.
        let (info, d) = analyze("struct Box { value: i32 } fn f(read b: Box) -> i32 { b.value }");
        assert!(d.is_empty(), "{:?}", d);
        // the tail expression `b.value` should be typed as i32 (Copy).
        let copyish = info.expr_types.iter().any(|t| *t == Ty::Prim("i32"));
        assert!(copyish, "expected an i32 somewhere in the inferred types");
    }
}
