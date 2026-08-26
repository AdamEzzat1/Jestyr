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
use crate::comptime;
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
    let Owners { owner, extern_owned, name_mods, dup, dup_types, dup_variants } =
        build_owner(ast, modules);
    let item_mod: Vec<ModId> =
        (0..ast.items.len()).map(|i| *modules.item_mod.get(i).unwrap_or(&0)).collect();
    let mut tc = TypeChecker {
        ast,
        modules,
        owner,
        extern_owned,
        name_mods,
        dup,
        dup_types,
        dup_variants,
        cur_mod: 0,
        table: GlobalTable::default(),
        expr_types: vec![Ty::Unknown; ast.exprs.len()],
        variant_field_names: HashMap::new(),
        resolved: vec![None; ast.exprs.len()],
        must_use_call: vec![false; ast.exprs.len()],
        cur_type_param_bounds: HashMap::new(),
        cur_expected: None,
        cur_ret: None,
        cur_errs: None,
        err_payloads: std::collections::BTreeMap::new(),
        cur_type_fn: None,
        diags: Vec::new(),
    };
    tc.build_table();
    tc.collect_err_payloads();
    tc.audit_type_paths();
    tc.check_items();
    (
        TypeInfo {
            table: tc.table,
            expr_types: tc.expr_types,
            // Carry the per-region source tables so the backend can emit `#line`
            // directives mapping generated C back to `.jtr`. `Modules::single`
            // (the unit-test path) leaves `srcs` empty ⇒ no `#line` ⇒ that path's
            // emitted C is byte-identical.
            debug: DebugInfo::new(
                modules.paths.clone(),
                modules.srcs.clone(),
                modules.bases.clone(),
            ),
            item_mod,
            dup_fns: tc.dup,
            dup_types: tc.dup_types,
            dup_variants: tc.dup_variants,
            imports: modules.imports.clone(),
            resolved: tc.resolved,
            err_payloads: tc.err_payloads,
        },
        tc.diags,
    )
}

/// The result of the ownership pass: who owns what, and which names collide.
struct Owners {
    /// `(owning module, item name)` → is it `pub`. The basis for cross-module
    /// visibility and qualified-access resolution; keyed on the module so two
    /// modules may own the same bare name independently (the namespace fix).
    owner: HashMap<(ModId, String), bool>,
    /// `(module, name)` pairs declared by an `extern`. An extern's name is a C SYMBOL,
    /// so it must never be canonicalized — see `TypeChecker::canon_in`.
    extern_owned: HashSet<(ModId, String)>,
    /// fn/const name → the modules that define it. Drives `dup` and the
    /// "defined in another module — call it qualified" diagnostic.
    name_mods: HashMap<String, Vec<ModId>>,
    /// fn/const names defined in more than one module (see [`crate::types::canon`]).
    dup: HashSet<String>,
    /// non-generic type names (struct/enum/distinct) defined in more than one
    /// module — drives `canon` for the `Jestyr_<type>` C symbol.
    dup_types: HashSet<String>,
    /// enum variant names defined in more than one module — drives `canon` for the
    /// variant→enum lookup.
    dup_variants: HashSet<String>,
}

/// The owning module and visibility of every named top-level item — the basis
/// for cross-module visibility checks, namespace isolation, and qualified-access
/// resolution. Unlike the v1 flat pool, ownership is keyed on `(module, name)`,
/// so two modules each defining `make` are distinct entries, not a collision.
fn build_owner(ast: &Ast, modules: &Modules) -> Owners {
    let mut owner: HashMap<(ModId, String), bool> = HashMap::new();
    let mut extern_owned: HashSet<(ModId, String)> = HashSet::new();
    let mut name_mods: HashMap<String, Vec<ModId>> = HashMap::new();
    // Per-module-set trackers for the two type-side namespaces (non-generic type
    // names and enum variant names), so two modules can each define `Slot` or a
    // variant `red` and get distinct C symbols.
    let mut type_mods: HashMap<String, Vec<ModId>> = HashMap::new();
    let mut variant_mods: HashMap<String, Vec<ModId>> = HashMap::new();
    let note = |map: &mut HashMap<String, Vec<ModId>>, n: String, m: ModId| {
        let v = map.entry(n).or_default();
        if !v.contains(&m) {
            v.push(m);
        }
    };
    for (i, item) in ast.items.iter().enumerate() {
        let m = *modules.item_mod.get(i).unwrap_or(&0);
        let is_pub = *modules.item_pub.get(i).unwrap_or(&true);
        // `namespaced` names participate in *function/const* collision
        // disambiguation (increment 1); types and variants have their own dup
        // sets (collidable types); externs keep their bare linker name and traits
        // still resolve globally.
        let (name, namespaced) = match item {
            Item::Fn(f) => (Some(f.name.name.clone()), true),
            Item::Const(c) => (Some(c.name.name.clone()), true),
            Item::Enum(e) => {
                // Both plain and *generic* enums are collidable: the type name is
                // canon-keyed, and a generic enum's monomorphized instance mangling
                // (`Jestyr_<ctor>__<args>`) disambiguates via the canon ctor.
                note(&mut type_mods, e.name.name.clone(), m);
                for v in &e.variants {
                    note(&mut variant_mods, v.name.name.clone(), m);
                }
                (Some(e.name.name.clone()), false)
            }
            Item::Struct { name, .. } => {
                note(&mut type_mods, name.name.clone(), m);
                (Some(name.name.clone()), false)
            }
            Item::Distinct(d) => {
                note(&mut type_mods, d.name.name.clone(), m);
                (Some(d.name.name.clone()), false)
            }
            Item::Extern(e) => {
                extern_owned.insert((m, e.name.name.clone()));
                (Some(e.name.name.clone()), false)
            }
            Item::Trait(t) => (Some(t.name.name.clone()), false),
            Item::Impl(_) | Item::Import(_) => (None, false),
        };
        if let Some(n) = name {
            owner.entry((m, n.clone())).or_insert(is_pub);
            if namespaced {
                note(&mut name_mods, n, m);
            }
        }
    }
    let dups = |map: &HashMap<String, Vec<ModId>>| -> HashSet<String> {
        map.iter().filter(|(_, ms)| ms.len() > 1).map(|(n, _)| n.clone()).collect()
    };
    let dup = dups(&name_mods);
    let dup_types = dups(&type_mods);
    let dup_variants = dups(&variant_mods);
    Owners { owner, extern_owned, name_mods, dup, dup_types, dup_variants }
}

struct TypeChecker<'a> {
    ast: &'a Ast,
    modules: &'a Modules,
    /// `(module, name)` → is_pub, for visibility + namespace-isolated resolution.
    owner: HashMap<(ModId, String), bool>,
    /// `(module, name)` pairs declared by an `extern` — never canonicalized (`canon_in`).
    extern_owned: HashSet<(ModId, String)>,
    /// fn/const name → the modules defining it (cross-module diagnostics).
    name_mods: HashMap<String, Vec<ModId>>,
    /// fn/const names defined in more than one module (drives `canon`).
    dup: HashSet<String>,
    /// non-generic type names defined in more than one module (drives the type
    /// `canon` — two modules may each define `Slot`).
    dup_types: HashSet<String>,
    /// enum variant names defined in more than one module (drives the variant
    /// `canon`).
    dup_variants: HashSet<String>,
    /// The module whose item is currently being checked.
    cur_mod: ModId,
    table: GlobalTable,
    expr_types: Vec<Ty>,
    /// `(enum's index in `table.types`, bare variant name)` → its payload field
    /// names, in declaration order.
    ///
    /// [`GlobalTable`] stores a variant's field *types* positionally and drops the
    /// names, which is all the positional pattern `rect(w, h)` needs. The named
    /// form `rect { w, h }` needs the name→position map to type its bindings, so
    /// it is recorded here rather than widening the table (and with it every
    /// `TypeKindG::Enum` reader in layout, doc, attest and the backend). Keyed on
    /// the type index, not the canonical name, so two modules' same-named variants
    /// stay distinct.
    variant_field_names: HashMap<(usize, String), Vec<String>>,
    /// Expr id → every resolution recorded for it (see [`Resolved`]) — the row-wise
    /// HIR handed to `escape` and `cgen` verbatim as [`TypeInfo::resolved`]. Sized
    /// with `expr_types` and indexed the same way. Written only through the
    /// `record_*` helpers below; never read back during checking.
    resolved: Vec<Option<Box<Resolved>>>,
    /// Expr id → "this call resolved to a `@must_use` function".
    ///
    /// A **dense** side table sized with `expr_types`, deliberately, and not a
    /// `HashMap<ExprId, _>`: an expr id is a position in one `Ast`'s arena, so a map
    /// keyed by one is a map keyed by an index that only means anything next to the
    /// arena it came from. `expr_types` and `resolved` are both dense for the same
    /// reason and this rides beside them.
    ///
    /// Written by the four call-resolution paths — unqualified, `mod.f(…)`, the UFCS
    /// method form, and a struct-body method — and read once, at the discarded-
    /// statement seam. Recording it at resolution rather than re-resolving at the
    /// seam is what keeps the rule from growing a *fifth* answer to "which function
    /// does this call name?".
    ///
    /// That there were four and not three was **measured, not reasoned**: the rule
    /// was written against the three `FnSig` paths, and a probe of the two method
    /// forms the attribute advertises (`Target::Method`) found `@must_use fn peek`
    /// inside a struct body silently doing nothing. Trait methods remain uncovered
    /// on purpose — see `resolve_struct_method` for why that is an AST change.
    must_use_call: Vec<bool>,
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
    /// The declared error set of the function currently being checked (sorted),
    /// `None` when it is infallible — what `err(E)` membership and `?`/rethrow
    /// inclusion are checked against (error-payloads E2).
    cur_errs: Option<Vec<String>>,
    /// Error names that carry a payload → the payload's type (error-payloads E3).
    /// Whole-program by decision D1: every declaring site must agree, checked in
    /// [`Checker::collect_err_payloads`]. Only payload-carrying names appear.
    err_payloads: std::collections::BTreeMap<String, Ty>,
    /// The enclosing **comptime type-fn** while its body is being checked — the
    /// (canonical ctor name, comptime type-param names) of `fn Box(comptime T:
    /// type) -> type { … }`. This is what lets the `return struct { … }` arm type
    /// a ctor-body method's `self` as the real generic-struct type
    /// (`Box(T)` with `T` opaque) rather than an opaque `Self` — so `self.field`
    /// resolves through the template and the escape checker judges it precisely
    /// instead of refusing via the `Unknown` finalization. `None` outside a
    /// type-fn, where an anonymous `struct { … }` keeps the `Self` placeholder.
    cur_type_fn: Option<(String, Vec<String>)>,
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

    // --- recording resolutions (the HIR write side) ---
    //
    // One writer per `Resolved` field. Each creates the expression's row on first
    // write and fills in its own slot, so recording two kinds of resolution for
    // one expression composes rather than clobbering — the shape the checker
    // relied on when these were seven independent maps.

    /// The row for `id`, created empty if this is its first resolution.
    fn row(&mut self, id: ExprId) -> &mut Resolved {
        self.resolved[id.0 as usize].get_or_insert_with(Default::default)
    }

    fn record_call_sym(&mut self, id: ExprId, sym: String) {
        self.row(id).call_sym = Some(sym);
    }

    fn record_method(&mut self, id: ExprId, m: MethodRes) {
        self.row(id).method = Some(m);
    }

    fn record_qualified(&mut self, id: ExprId, sym: String) {
        self.row(id).qualified = Some(sym);
    }

    fn record_impl_call(&mut self, id: ExprId, c: ImplCall) {
        self.row(id).impl_call = Some(c);
    }

    fn record_bound_method(&mut self, id: ExprId, c: BoundMethodCall) {
        self.row(id).bound_method = Some(c);
    }

    fn record_dyn_coercion(&mut self, id: ExprId, trait_name: String) {
        self.row(id).dyn_coercion = Some(trait_name);
    }

    /// Note that call `id` resolved to a `@must_use` function. Deliberately NOT a
    /// `Resolved` field: that struct is the HIR handed to `escape` and `cgen`, and
    /// this fact is consumed entirely inside the checker — putting it there would
    /// widen the backend's input for a rule the backend has no part in.
    fn record_must_use(&mut self, id: ExprId, must_use: bool) {
        if must_use {
            self.must_use_call[id.0 as usize] = true;
        }
    }

    fn record_dyn_call(&mut self, id: ExprId, method: String) {
        self.row(id).dyn_call = Some(method);
    }

    // --- per-module namespacing ---

    /// The canonical symbol name of `name` as owned by module `m` (the global
    /// table's key and the backend's C symbol — bare unless the name collides).
    fn canon_in(&self, m: ModId, name: &str) -> String {
        // **An `extern` name is a C symbol and is never canonicalized.** It is registered
        // in `table.fns` under its BARE name, because that is the symbol the linker will
        // look for — so canonicalizing it here would look up `close__m7` and find nothing,
        // and the call would then be reported as an unresolved cross-module name.
        //
        // That is not hypothetical: `std/file` and `std/sysdir` both declare a Jestyr
        // `close`, which puts `close` in `dup`; `std/sysnet` then binds POSIX's `close(2)`
        // to shut a socket, and every call to it inside its own module failed with
        // *cannot find `close` in this module; it is defined in module `sysdir`*.
        //
        // Keyed on `(module, name)` rather than on the name alone, and that matters: making
        // every `close` bare would give `file.close` and `sysdir.close` the same C symbol.
        // Only the module that declared the extern gets the bare spelling.
        if self.extern_owned.contains(&(m, name.to_string())) {
            return name.to_string();
        }
        crate::types::canon(m, name, &self.dup)
    }

    /// The canonical name of `name` resolved from the *current* module.
    fn canon_cur(&self, name: &str) -> String {
        self.canon_in(self.cur_mod, name)
    }

    /// The canonical *type* name owned by module `m` — the `type_index` key and the
    /// backend's `Jestyr_<type>` symbol (bare unless the type name collides).
    fn canon_type_in(&self, m: ModId, name: &str) -> String {
        crate::types::canon(m, name, &self.dup_types)
    }

    /// The canonical type name resolved from the current module.
    fn canon_type_cur(&self, name: &str) -> String {
        self.canon_type_in(self.cur_mod, name)
    }

    /// The canonical *variant* name owned by module `m` (the key of `variants`).
    fn canon_variant_in(&self, m: ModId, name: &str) -> String {
        crate::types::canon(m, name, &self.dup_variants)
    }

    /// Does the current module itself define a top-level item called `name`?
    /// (An unqualified name resolves only against its own module — namespace
    /// isolation; cross-module access must be qualified.)
    fn owns_local(&self, name: &str) -> bool {
        self.owner.contains_key(&(self.cur_mod, name.to_string()))
    }

    /// The module that defines a top-level `name`, if exactly one does — used for
    /// non-namespaced kinds (types) where the name is unique program-wide.
    fn defining_module(&self, name: &str) -> Option<ModId> {
        self.owner.keys().find(|(_, n)| n == name).map(|(m, _)| *m)
    }

    // --- building the global table ---

    fn build_table(&mut self) {
        let ast = self.ast;

        // The `@cfg` platform each registered function name was declared under, so a
        // second declaration of the same name can be judged. Two items may share a name
        // when their platforms are DISJOINT — that is the entire point of `@cfg`:
        // `@cfg(posix) fn dir_open` and `@cfg(windows) fn dir_open` are one API with two
        // implementations, and only one of them survives the C preprocessor.
        //
        // Everything else stays a duplicate-definition error. In particular two items on
        // the SAME platform still collide, and an unguarded item collides with anything,
        // because it is live everywhere — which is what keeps this a narrow relaxation
        // rather than a hole in redefinition checking.
        let mut fn_cfg: HashMap<String, Option<String>> = HashMap::new();

        // Phase 1: register the names of all user types so they can be referred
        // to in any order. `cur_mod` is tracked so a re-registration can tell a
        // same-module redefinition from a cross-module type-name collision.
        for (i, item) in ast.items.iter().enumerate() {
            self.cur_mod = *self.modules.item_mod.get(i).unwrap_or(&0);
            match item {
                Item::Struct { name, is_record, attrs, .. } => {
                    let i = self.register_type(name, false);
                    self.table.types[i].is_record = *is_record;
                    // `@copy` opts a small aggregate into being freely copyable
                    // (design §2.8) — the escape checker then never treats it as a
                    // move/borrow that could escape.
                    self.table.types[i].is_copy = attrs.iter().any(|a| a.name == "copy");
                    // `@move` is `@copy`'s opposite: the aggregate is a RESOURCE, so the
                    // ownership rules treat it the way they treat a droppable even though
                    // it has no `Drop`. Set here beside `is_copy` because the two are one
                    // decision about the same type, and `attrs::validate_struct` has
                    // already refused a struct that claims both.
                    self.table.types[i].is_move = attrs.iter().any(|a| a.name == "move");
                }
                Item::Enum(e) => {
                    let idx = self.register_type(&e.name, true);
                    self.table.types[idx].type_params =
                        e.type_params.iter().map(|p| p.name.clone()).collect();
                    // `@copy` opts a PLAIN enum into Copy (the niche-Link-over-genref
                    // case) — payload copy-ness is VALIDATED in phase 2, once payload
                    // types are lowered; a generic enum never (its instances are
                    // `GenEnum`, which stays non-Copy).
                    self.table.types[idx].is_copy = e.type_params.is_empty()
                        && e.attrs.iter().any(|a| a.name == "copy");
                    for v in &e.variants {
                        let vkey = self.canon_variant_in(self.cur_mod, &v.name.name);
                        // Variant names resolve by bare name within a module, so a
                        // duplicate — in this enum or another same-module enum — would
                        // silently shadow the earlier one (a bare `err(name, …)` or
                        // pattern would bind against the wrong enum). First-wins +
                        // error, never last-wins.
                        if let Some(&prev) = self.table.variants.get(&vkey) {
                            let owner = self.table.types[prev].name.clone();
                            self.error(
                                v.name.span,
                                format!(
                                    "duplicate variant name `{}`: enum `{}` already declares it in this module",
                                    v.name.name, owner
                                ),
                            );
                        } else {
                            self.table.variants.insert(vkey, idx);
                        }
                    }
                }
                Item::Distinct(d) => {
                    // Register the name now (by canonical key); the base type is
                    // lowered in phase 2.
                    let key = self.canon_type_cur(&d.name.name);
                    if self.table.type_index.contains_key(&key) {
                        self.error(d.name.span, format!("duplicate definition of `{}`", d.name.name));
                    } else {
                        let idx = self.table.types.len();
                        self.table.types.push(TypeDecl {
                            name: key.clone(),
                            kind: TypeKindG::Distinct { base: Ty::Unknown },
                            is_copy: false,
                            is_move: false,
                            is_record: false,
                            type_params: Vec::new(),
                        });
                        self.table.type_index.insert(key, idx);
                    }
                }
                _ => {}
            }
        }

        // Phase 2: lower field/variant/parameter/return types now that every
        // type name has an index.
        let empty = HashSet::new();
        for (item_ix, item) in ast.items.iter().enumerate() {
            let item_m = *self.modules.item_mod.get(item_ix).unwrap_or(&0);
            // Lower this item's types *from its own module's view*, so an
            // unqualified type name resolves current-module-first (collidable types).
            self.cur_mod = item_m;
            match item {
                Item::Struct { name, body, .. } => {
                    let key = self.canon_type_cur(&name.name);
                    let self_idx = self.table.type_index.get(&key).copied();
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
                    if let Some(i) = self_idx {
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
                    let self_idx = self.table.type_index.get(&self.canon_type_cur(&e.name.name)).copied();
                    let mut variants = Vec::new();
                    let declared_copy =
                        self_idx.is_some_and(|si| self.table.types[si].is_copy);
                    for v in &e.variants {
                        let mut ftys = Vec::new();
                        for (_, t) in &v.fields {
                            let fty = self.lower_type(&tp, *t);
                            if let Some(si) = self_idx {
                                self.check_no_value_recursion(si, self.ast.type_at(*t).span, &fty);
                            }
                            // The `@copy` contract is checked, not trusted: a copy of a
                            // droppable payload would drop twice. (Copy-ness of a Named
                            // payload reads the phase-1 flag, so declaration order does
                            // not matter.)
                            if declared_copy && !fty.is_copy(&self.table) {
                                let shown = fty.display(&self.table);
                                self.error(
                                    self.ast.type_at(*t).span,
                                    format!(
                                        "`@copy` enum `{}` carries a non-Copy payload `{shown}` in variant `{}` — a copy would double-drop it; only Copy payloads may ride a `@copy` enum",
                                        e.name.name, v.name.name
                                    ),
                                );
                            }
                            ftys.push(fty);
                        }
                        // `variants` keeps field types *positionally*; a struct-variant
                        // pattern (`rect { w, h }`) addresses them by name, so keep the
                        // name→position map too. See `variant_field_names`.
                        if let Some(si) = self_idx {
                            self.variant_field_names.insert(
                                (si, v.name.name.clone()),
                                v.fields.iter().map(|(n, _)| n.name.clone()).collect(),
                            );
                        }
                        variants.push((v.name.name.clone(), ftys));
                    }
                    if let Some(i) = self_idx {
                        if let TypeKindG::Enum { variants: slot } = &mut self.table.types[i].kind {
                            *slot = variants;
                        }
                    }
                }
                Item::Distinct(d) => {
                    // Lower the base type; a distinct type is `Copy` iff its base is.
                    let base = self.lower_type(&empty, d.base);
                    let copy = base.is_copy(&self.table);
                    if let Some(&i) = self.table.type_index.get(&self.canon_type_cur(&d.name.name)) {
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
                    // Key on the canonical name so two modules may each define
                    // `make`; a clash here is a *same-module* redefinition.
                    // **A function named like an intrinsic is refused.**
                    //
                    // cgen dispatches intrinsics by NAME before it looks at user
                    // functions, so a module defining `fn arg_count(read r: Args)` had
                    // every unqualified call to it emitted as `jestyr_rt_arg_count()` —
                    // the runtime's argc — **with the argument silently discarded**. The
                    // program compiled without a warning from Jestyr or from gcc and
                    // returned the process's argument count instead of the caller's data.
                    //
                    // That is worse than the `int`-fallback miscompiles already recorded,
                    // because there is no wrong TYPE to notice: the shadowing intrinsic
                    // has a plausible signature and C is perfectly happy. The only signal
                    // was a wrong answer at runtime, in a module (`std/cli`) whose whole
                    // job is counting arguments — which is how it got caught at all.
                    //
                    // **A WARNING, not an error, and the corpus is why.** Two existing
                    // modules shadow an intrinsic — `lexer.str_eq` and `set.contains` —
                    // and both work today, because `str_eq`'s semantics happen to match
                    // the intrinsic's and `contains` is only ever called qualified. An
                    // error would refuse working code to catch a hazard those two have
                    // not yet been bitten by; a warning names the hazard and leaves them
                    // alone. They are still traps: change `lexer.str_eq`'s behaviour and
                    // the change is silently ignored at every unqualified call.
                    //
                    // **The real fix is in cgen — prefer the user's function when the
                    // program defines one** — and it is deferred because it is an
                    // emission change: it would rename `str_eq`'s call sites in a closure
                    // module, so it owes a port mirror, a reseed and a golden churn. This
                    // warning is what makes that a known debt rather than a latent one.
                    if crate::cgen::is_intrinsic(&f.name.name) {
                        self.warn(
                            f.name.span,
                            format!(
                                "`{}` shadows a compiler intrinsic: an unqualified call emits the intrinsic, not this function",
                                f.name.name
                            ),
                        );
                        self.diags.last_mut().unwrap().help = Some(
                            "a qualified call (`mod.name(..)`) reaches this definition and an \
                             unqualified one does not, so the two spellings disagree silently — \
                             rename it unless the semantics are identical"
                                .to_string(),
                        );
                    }
                    let key = self.canon_in(item_m, &f.name.name);
                    let cfg = crate::attrs::cfg_of(ast, &f.attrs);
                    if self.table.fns.contains_key(&key)
                        && !crate::attrs::cfgs_are_disjoint(
                            fn_cfg.get(&key).unwrap_or(&None),
                            &cfg,
                        )
                    {
                        self.error(f.name.span, format!("duplicate definition of `{}`", f.name.name));
                    }
                    fn_cfg.insert(key.clone(), cfg);
                    self.table.fns.insert(
                        key,
                        FnSig {
                            params,
                            ret,
                            ret_conv: f.ret_conv,
                            errs: errs_of(&f.errors),
                            must_use: f.attr("must_use").is_some(),
                        },
                    );
                }
                Item::Const(c) => {
                    let t = c.ty.map(|t| self.lower_type(&empty, t)).unwrap_or(Ty::Unknown);
                    self.table.consts.insert(self.canon_in(item_m, &c.name.name), t);
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
                    let cfg = crate::attrs::cfg_of(ast, &e.attrs);
                    if self.table.fns.contains_key(&e.name.name)
                        && !crate::attrs::cfgs_are_disjoint(
                            fn_cfg.get(&e.name.name).unwrap_or(&None),
                            &cfg,
                        )
                    {
                        self.error(e.name.span, format!("duplicate definition of `{}`", e.name.name));
                    }
                    fn_cfg.insert(e.name.name.clone(), cfg);
                    self.table.fns.insert(
                        e.name.name.clone(),
                        // Never `@must_use`: the attribute's target list in `attrs.rs`
                        // is `Fn`/`Method`, so `validate` already refuses it on an
                        // `extern`. Hard-coded rather than read from `e.attrs` so the
                        // two facts cannot drift apart silently.
                        FnSig { params, ret, ret_conv: e.ret_conv, errs: None, must_use: false },
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
            self.table.traits.entry(name.to_string()).or_insert_with(|| TraitDef {
                methods: vec![(method.to_string(), true)],
                method_errs: HashMap::new(),
            });
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
                // Trait-errors T1: a method's declared set is the CONTRACT calls
                // are typed by. A default body on a fallible trait method is
                // refused for now — it would need cur-set plumbing through the
                // per-impl copy, and no design has asked for it yet.
                let mut method_errs = HashMap::new();
                for m in &t.methods {
                    if let Some(errs) = errs_of(&m.errors) {
                        if m.default_body.is_some() {
                            self.error(
                                m.name.span,
                                format!(
                                    "a fallible trait method cannot have a default body — \
                                     declare the set and implement `{}` in each impl",
                                    m.name.name
                                ),
                            );
                        }
                        method_errs.insert(m.name.name.clone(), errs);
                    }
                }
                self.table.traits.insert(t.name.name.clone(), TraitDef { methods, method_errs });
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
            // Fallibility conformance (trait-errors T1). A call through the trait is
            // typed by the TRAIT's signature, so the trait's declared set is the
            // contract and an impl must agree with it:
            //  * trait declares no set, impl does → refused (the original rule —
            //    the call sites would be silently mistyped as infallible);
            //  * trait declares a set, impl declares none → refused (the ABI
            //    returns the tagged result struct, so the body must construct it
            //    through `ok`/`err` — an infallible body cannot);
            //  * both declare → the impl's set must be a SUBSET of the trait's
            //    (E2's inclusion, the same relation `?` enforces).
            // Payload agreement needs no rule of its own: a payload is a property
            // of the NAME (D1), checked whole-program by `collect_err_payloads`.
            for f in &im.methods {
                let trait_errs = self
                    .table
                    .traits
                    .get(&im.trait_name.name)
                    .and_then(|t| t.method_errs.get(&f.name.name).cloned());
                match (&f.errors, &trait_errs) {
                    (Some(es), None) => self.error(
                        es.span,
                        format!(
                            "a trait-impl method cannot be fallible: calls to `{}` are typed by \
                             trait `{}`'s signature, which declares no error set",
                            f.name.name, im.trait_name.name
                        ),
                    ),
                    (None, Some(te)) => self.error(
                        f.name.span,
                        format!(
                            "`{}` must declare an error set: trait `{}` declares `!{{ {} }}`, and a \
                             call through the trait returns its tagged result (declare a subset)",
                            f.name.name,
                            im.trait_name.name,
                            te.join(", ")
                        ),
                    ),
                    (Some(es), Some(te)) => {
                        // A blanket impl's fallible method is deferred: the
                        // per-instance emission path has not been taught the
                        // result-struct ABI yet, and a silent wrong lowering is
                        // worse than a refusal with the reason.
                        if !im.generics.is_empty() {
                            self.error(
                                es.span,
                                format!(
                                    "a fallible method in a blanket `impl[…]` is not yet supported \
                                     (implement `{}` per concrete type)",
                                    f.name.name
                                ),
                            );
                        }
                        let ie = errs_of(&f.errors).unwrap_or_default();
                        let missing: Vec<&str> =
                            ie.iter().filter(|e| !te.contains(e)).map(String::as_str).collect();
                        if !missing.is_empty() {
                            self.error(
                                es.span,
                                format!(
                                    "impl `{}` declares {{ {} }} beyond trait `{}`'s set {{ {} }} — \
                                     an impl's errors must be a subset of the trait's",
                                    f.name.name,
                                    missing.join(", "),
                                    im.trait_name.name,
                                    te.join(", ")
                                ),
                            );
                        }
                    }
                    (None, None) => {}
                }
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
        self.record_impl_call(call_id, ImplCall { trait_name: trait_name.clone(), type_key: key, method: method.to_string() });
        Some(self.wrap_trait_ret(&trait_name, method, ret))
    }

    /// Trait-errors T1: a call through a trait whose method declares an error set
    /// yields `T !{ set }` — the TRAIT's set, whichever impl answers, because the
    /// trait's signature is the contract every call site is typed by.
    fn wrap_trait_ret(&self, trait_name: &str, method: &str, ret: Ty) -> Ty {
        match self.table.traits.get(trait_name).and_then(|t| t.method_errs.get(method)) {
            Some(errs) => Ty::Result(Box::new(ret), errs.clone()),
            None => ret,
        }
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
        self.record_bound_method(
            call_id,
            BoundMethodCall { trait_name: tr.clone(), method: mname.to_string(), type_param: tp.clone() },
        );
        let ret = self.trait_method_ret(&tr, mname, recv_ty);
        Some(self.wrap_trait_ret(&tr, mname, ret))
    }

    /// `recv.m(args)` where `recv: dyn Trait` (traits, Stage F): a **dynamic**
    /// dispatch. `m` must be one of `Trait`'s methods; the call types by the trait
    /// method's declared return and is recorded so the backend lowers it to a
    /// vtable call. `None` ⇒ the receiver isn't a `dyn` value.
    fn resolve_dyn_method(&mut self, call_id: ExprId, mname: &str, recv_ty: &Ty) -> Option<Ty> {
        let tr = dyn_trait_of(recv_ty)?.to_string();
        let span = self.ast.expr_at(call_id).span;
        if !self.table.traits.get(&tr).is_some_and(|t| t.has_method(mname)) {
            self.error(span, format!("no method `{mname}` on `dyn {tr}`: not a method of the trait"));
            return Some(Ty::Error);
        }
        self.record_dyn_call(call_id, mname.to_string());
        let ret = self.trait_method_ret(&tr, mname, recv_ty);
        Some(self.wrap_trait_ret(&tr, mname, ret))
    }

    /// If `expected` is a `dyn Trait` and `expr` has a *concrete* type that `impl`s
    /// that trait, record the coercion so the backend wraps the value into a
    /// `{ data, vtable }` fat pointer (Stage F). A concrete type that does **not**
    /// implement the trait is an error; a value that is already `dyn Trait` (or a
    /// non-`dyn` expected type) is left untouched.
    fn check_dyn_coercion(&mut self, expr: ExprId, expected: &Ty) {
        let Some(tr) = dyn_trait_of(expected) else { return };
        let actual = self.expr_types[expr.0 as usize].clone();
        // Already a `dyn` value (pass-through), or unresolved — nothing to wrap.
        if dyn_trait_of(&actual).is_some() || matches!(actual, Ty::Unknown | Ty::Error) {
            return;
        }
        // Trait-errors T1: dyn dispatch of a fallible method is NOT yet supported —
        // the vtable machinery has not been taught the result-struct ABI — so a
        // trait with any fallible method cannot be erased to `dyn`. Static impl
        // calls and bracket-bound generic calls carry fallibility fine (both lower
        // to direct calls of the result-returning impl fn).
        if let Some(t) = self.table.traits.get(tr) {
            if let Some(m) = t.method_errs.keys().next() {
                let span = self.ast.expr_at(expr).span;
                self.error(
                    span,
                    format!(
                        "cannot coerce to `dyn {tr}`: its method `{m}` is fallible, and \
                         fallible dynamic dispatch is not yet supported (call it statically)"
                    ),
                );
                return;
            }
        }
        let key = self.table.ty_key(&actual);
        if self.table.impl_index.contains_key(&(tr.to_string(), key)) {
            self.record_dyn_coercion(expr, tr.to_string());
        } else {
            let span = self.ast.expr_at(expr).span;
            let shown = actual.display(&self.table);
            self.error(
                span,
                format!("type `{shown}` does not implement `{tr}`, so it cannot coerce to `dyn {tr}`"),
            );
        }
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
        let (trait_name, _) = op_trait_method(op)?;
        // Only user types overload operators; primitives keep native semantics.
        if !matches!(lt, Ty::Named(_) | Ty::GenStruct { .. }) {
            return None;
        }
        match self.lookup_operator_impl(id, op, lt) {
            Some(ret) => Some(ret),
            None => {
                let key = self.table.ty_key(lt);
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

    /// The *pure* half of [`resolve_operator_trait`]: does `lt` carry an `impl`
    /// for `op`'s trait? Records the dispatch on a hit (that is what makes the
    /// backend lower the operator to the impl call) and emits **no diagnostic** on
    /// a miss, so a caller that has its own fallback — the `distinct`
    /// inheritance rule — can consult the impl first without committing to an
    /// error it is about to handle.
    ///
    /// [`resolve_operator_trait`]: Self::resolve_operator_trait
    fn lookup_operator_impl(&mut self, id: ExprId, op: BinOp, lt: &Ty) -> Option<Ty> {
        let (trait_name, method) = op_trait_method(op)?;
        if !matches!(lt, Ty::Named(_) | Ty::GenStruct { .. }) {
            return None;
        }
        let key = self.table.ty_key(lt);
        let ret: Ty = self
            .table
            .impls
            .iter()
            .find(|im| {
                im.trait_name == trait_name && im.type_key == key && im.method_rets.contains_key(method)
            })
            .map(|im| im.method_rets.get(method).cloned().unwrap_or(Ty::Unknown))?;
        self.record_impl_call(
            id,
            ImplCall {
                trait_name: trait_name.to_string(),
                type_key: key,
                method: method.to_string(),
            },
        );
        Some(ret)
    }

    fn register_type(&mut self, name: &Ident, is_enum: bool) -> usize {
        // Key by the *canonical* type name: two modules each defining `Slot` get
        // distinct keys (`Slot__m<a>` / `Slot__m<b>`), so a clash here is now a
        // genuine same-module redefinition.
        let key = self.canon_type_cur(&name.name);
        if let Some(&i) = self.table.type_index.get(&key) {
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
            // The canonical name *is* the type's identity (its C symbol); for a
            // non-colliding type this is just the bare name, so output is unchanged.
            name: key.clone(),
            kind,
            is_copy: false,
            is_move: false,
            is_record: false,
            type_params: Vec::new(),
        });
        self.table.type_index.insert(key, idx);
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

    /// `t` itself when it is a `distinct` type, else `None`.
    ///
    /// This is the type an inherited operation is reinstated *at* (design §2.2):
    /// `distinct P = str` inherits `str: [Range] -> str` as `P: [Range] -> P`,
    /// so a sub-view of a `P` is a `P` and needs no cast. It deliberately returns
    /// the OUTERMOST distinct of a chain (`Key = Id = i64` yields `Key`), because
    /// that is the type the expression was written at.
    fn distinct_root(&self, t: &Ty) -> Option<Ty> {
        self.is_distinct(t).then(|| t.clone())
    }

    /// Follow `distinct` bases to the representation type underneath.
    ///
    /// Transitive (`distinct Key = Id`, `distinct Id = i64` peels to `i64`) and
    /// **capped**, because `check` accepts a cyclic declaration today (`distinct
    /// A = B` / `distinct B = A`) and an uncapped walk would hang the compiler on
    /// one. Exhausting the cap yields `Ty::Error`, which every consumer already
    /// treats as already-diagnosed.
    fn peel_distinct(&self, t: &Ty) -> Ty {
        let mut cur = t.clone();
        for _ in 0..16 {
            let Ty::Named(i) = &cur else { return cur };
            let TypeKindG::Distinct { base } = &self.table.types[*i].kind else { return cur };
            cur = base.clone();
        }
        Ty::Error
    }

    /// The result of `d OP d` where `d` is a `distinct` type whose representation
    /// is `base` — the base's own operator signature with every occurrence of the
    /// base replaced by `d` (design §2.2). `None` means the base has no such
    /// operator, so `d` has none either.
    ///
    /// A deliberate **positive list**, not "whatever the base does": `str == str`
    /// and `str + str` are HEAD holes that pass `check` and die in gcc, so
    /// inheriting them would move a distinct's `==`/`+` from a Jestyr refusal to a
    /// gcc one — losing the rejection rather than keeping it.
    fn distinct_op_result(&self, op: BinOp, d: &Ty, base: &Ty) -> Option<Ty> {
        use BinOp::*;
        let int = matches!(base, Ty::Prim(p) if integer_prim(p));
        let float = matches!(base, Ty::Prim("f32") | Ty::Prim("f64"));
        let boolean = matches!(base, Ty::Prim("bool"));
        let character = matches!(base, Ty::Prim("char"));
        // A pointer compares for identity but has no arithmetic (`cptr` is opaque
        // by design, and typed-pointer arithmetic is not a Jestyr operator).
        let pointer = matches!(base, Ty::Prim("cptr") | Ty::Ptr { .. });
        let bool_ty = Ty::Prim("bool");
        match op {
            Add | Sub | Mul | Div if int || float => Some(d.clone()),
            Rem | BitAnd | BitOr | BitXor | Shl | Shr if int => Some(d.clone()),
            Eq | Ne if int || float || boolean || character || pointer => Some(bool_ty),
            Lt | Le | Gt | Ge if int || float || character => Some(bool_ty),
            And | Or if boolean => Some(bool_ty),
            _ => None,
        }
    }

    /// The `distinct` half of binary-operator resolution (design §2.3). Returns
    /// `None` when **no** operand is a distinct type — that hands the node to the
    /// unchanged HEAD path, so a program with no `distinct` in it infers exactly
    /// as before.
    ///
    /// The rule is a predicate over the two **types**, never over the expression
    /// tree. That is the whole reason it is safe: the previous attempt at this
    /// exempted "untyped literal" operands through `literal_defaulted`, whose
    /// Binary arm is a *recursive disjunction*, so one literal anywhere in a
    /// subtree exempted the whole operand and `a + (b + 1)` mixed two id spaces
    /// silently. Here `1` types as `i32` and `(b + 1)` types as `Error`; neither
    /// is a distinct type, so both are refused by the same clause that refuses
    /// `a + b` — with no literal predicate to get wrong.
    fn binary_distinct_rule(&mut self, id: ExprId, op: BinOp, lt: &Ty, rt: &Ty, span: Span) -> Option<Ty> {
        let (dl, dr) = (self.distinct_root(lt), self.distinct_root(rt));
        if dl.is_none() && dr.is_none() {
            return None;
        }
        // A hand-written `impl Add for Id` still wins: inheritance supplies the
        // operation the base has, it does not override one the author declared.
        if let Some(ret) = self.lookup_operator_impl(id, op, lt) {
            return Some(ret);
        }
        if dl == dr {
            // Both operands are the SAME distinct type: inherit the base's
            // operation, at this type.
            let base = self.peel_distinct(lt);
            if let Some(t) = self.distinct_op_result(op, lt, &base) {
                return Some(t);
            }
            let (d, b) = (lt.display(&self.table), base.display(&self.table));
            self.error(
                span,
                format!(
                    "type `{d}` has no `{}` operator — its base `{b}` has none either",
                    op_symbol(op)
                ),
            );
            return Some(Ty::Error);
        }
        // A mixed pair. `Error` on one side means the inner node already reported
        // — cascading a second diagnostic onto the same mistake helps nobody.
        if matches!(lt, Ty::Error) || matches!(rt, Ty::Error) {
            return Some(Ty::Error);
        }
        let (l, r) = (lt.display(&self.table), rt.display(&self.table));
        let why = if dl.is_some() && dr.is_some() {
            "unrelated `distinct` types over the same base"
        } else {
            "a `distinct` type shares its base's operations only with itself"
        };
        self.error(
            span,
            format!(
                "operator `{}` mixes `{l}` with `{r}` — {why}; cast one side",
                op_symbol(op)
            ),
        );
        Some(Ty::Error)
    }

    /// The BinOp a compound assignment performs (`a += b` is `a = a + b`).
    fn assign_op_binop(op: AssignOp) -> Option<BinOp> {
        Some(match op {
            AssignOp::Assign => return None,
            AssignOp::Add => BinOp::Add,
            AssignOp::Sub => BinOp::Sub,
            AssignOp::Mul => BinOp::Mul,
            AssignOp::Div => BinOp::Div,
            AssignOp::Rem => BinOp::Rem,
            AssignOp::BitAnd => BinOp::BitAnd,
            AssignOp::BitOr => BinOp::BitOr,
            AssignOp::BitXor => BinOp::BitXor,
        })
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

    /// Is a value of type `got` assignable to a location of type `want`?
    ///
    /// Deliberately **conservative**: it answers `true` whenever it is not
    /// *certain* the pair is wrong. This pass is lenient by design — `Unknown`
    /// and `Opaque` stand in for everything it declines to resolve — so a false
    /// positive here would reject a valid program, which is much worse than the
    /// false negative it replaces. It therefore judges only *fully-known
    /// primitives*; every other pair is accepted and left to the backend.
    ///
    /// `rhs` is the source expression when the caller has one. Jestyr has no
    /// integer inference variables (`ExprKind::Int` types as `i32` flat, see
    /// `infer`), so a *literal* must be allowed to adopt the expected numeric
    /// type or `let n: i64 = 5` would be an error.
    fn assignable(&self, want: &Ty, got: &Ty, rhs: Option<ExprId>) -> bool {
        if want == got {
            return true;
        }
        // A SLICE is a `{ptr, len}` pair. A raw pointer is not (it has no length)
        // and a fixed array is not (it is a value, `struct { T a[N]; }`). There is
        // no implicit conversion in any of those directions, and cgen emits none —
        // `slice(T, p, n)` is the explicit spelling, and it exists precisely because
        // the length has to come from somewhere.
        //
        // This is judged HERE rather than left to the "not modelled yet" default
        // below, because the default was not neutral: it accepted the mismatch and
        // let it reach gcc as `incompatible type for argument 1`. That is the
        // degrades-to-gcc failure the port work spent real effort eliminating, and
        // it makes `check` a false negative for an easy mistake — one made twice
        // while writing `std/test_report` and again while writing `std/smallvec`,
        // where `let s: []T = a` on a fixed array passed `check` and failed in gcc.
        //
        // Deliberately narrow: only the pairings that have no conversion at all in
        // either direction. Everything else the default still declines to judge.
        // **Unit converts to nothing and nothing converts to Unit.** It reads as a rule so
        // obvious it needs no writing down, and it did need writing down: the moment
        // `fn f(…) !{ E }` (a fallible function with no return type) became lowerable, its
        // `catch` acquired the ok type Unit — and `let b: bool = f(x) catch true` passed
        // `check` and failed in gcc with *void value not ignored as it ought to be*.
        // Exactly the degrades-to-gcc failure the slice/pointer row above exists to stop,
        // reached through a shape that did not exist before.
        //
        // `want == got` already accepted Unit-to-Unit at the top, so this is only the
        // mismatch, and it is symmetric: a Unit value cannot be stored anywhere, and
        // nothing can be stored where Unit is wanted.
        let no_conversion = |a: &Ty, b: &Ty| {
            matches!(
                (a, b),
                (Ty::Slice(_), Ty::Ptr { .. }) | (Ty::Slice(_), Ty::Array { .. })
            ) || (matches!(a, Ty::Unit) && !matches!(b, Ty::Unknown | Ty::Error))
        };
        if no_conversion(want, got) || no_conversion(got, want) {
            return false;
        }
        // A `distinct` type is NOMINAL: it borrows its base's representation and
        // nothing else. Judged here rather than only at `let` initializers, because
        // a rule that holds in one position and not the others is not a rule.
        // Measured before this change: `takes_uid(n)` with a bare `i64` was accepted,
        // and so was `takes_uid(a)` with an unrelated `AccountId` — so `distinct`
        // bought a *name* with no check, which reads as safety and is not. The
        // `AccountId`-for-`UserId` row is the one that makes this a correctness fix
        // rather than a strictness preference.
        if self.distinct_mismatch(want, got) {
            return false;
        }
        // A typed pointer may be WIDENED to `cptr` — that is how a buffer reaches
        // `fread`, and C performs exactly that conversion implicitly. The reverse is
        // refused: recovering a typed pointer from an opaque handle contradicts the
        // one claim the type makes, so it needs an explicit `as`.
        if matches!(got, Ty::Prim("cptr")) && matches!(want, Ty::Ptr { .. } | Ty::Slice(_)) {
            return false;
        }
        // Only primitive-vs-primitive is judged beyond that. Anything else — named
        // types, generics, references, fn-pointers, `dyn` coercions — has a
        // coercion story this pass does not model yet, so it is left alone.
        let (Ty::Prim(w), Ty::Prim(g)) = (want, got) else {
            return true;
        };
        // Within the text family (`str`/`String`/`cstr`/`os_str`/`Builder`/`Cow`)
        // there are borrow/own conversions this pass does not model, so it
        // declines to judge. *Across* families there is no implicit conversion
        // at all, which is what makes `-> i32 { return "hello" }` reportable.
        if prim_family(w) != prim_family(g) {
            return false;
        }
        if prim_family(w) == PrimFamily::Text {
            return true;
        }
        // Within the numeric family, the integer/floating boundary is judged, and
        // so — as of the int-conversion decision — is any integer conversion that
        // can LOSE OR REINTERPRET a value.
        //
        // ## The decision, and the measurement behind it
        //
        // This was an open language-design question, left alone because "the
        // self-hosted sources spell it both ways" and reporting would decide it.
        // Measured rather than argued: with the literal-defaulting guard below
        // already absorbing `var n: usize = 0`, a strict rule costs **six sites in
        // the entire corpus**, every one of them `i32 → usize` (four in
        // `cgen.jtr`, two in `typeck.jtr`), and every one passing an arena field
        // that uses `-1` as a sentinel into a length parameter. A negative
        // sentinel silently becoming a huge `usize` is precisely the class of bug
        // a determinism-first language should not compile.
        //
        // So: **lossless widening within one signedness is fine; narrowing and any
        // change of signedness need an explicit `as`.** Widening is allowed rather
        // than refused because it cannot lose information, and a rule that flagged
        // `i32 → i64` would be pure noise — the corpus contains no such site, so
        // permitting it costs nothing and keeps the rule about real hazards.
        if prim_family(w) == PrimFamily::Numeric && integer_prim(w) == integer_prim(g) {
            if integer_prim(w) && w != g && !lossless_widening(g, w) {
                // Subject to the literal-defaulting guard below: an untyped literal
                // may still be written at any integer type.
                return rhs.is_none_or(|e| self.literal_defaulted(e, w));
            }
            return true;
        }
        // Only judge when the value's type is trustworthy — see `literal_defaulted`.
        rhs.is_none_or(|e| self.literal_defaulted(e, w))
    }

    /// Might `e`'s inferred numeric type be an artifact of *literal defaulting*
    /// rather than a real type the programmer chose?
    ///
    /// Jestyr has no integer inference variables: `ExprKind::Int` types as `i32`
    /// flat, and binary arithmetic adopts its **left** operand's type. So in
    /// `let lo: i64 = (0 - hi) - 1` with `hi: i64`, the right-hand side infers
    /// as `i32` purely because the leftmost leaf is an untyped literal — the
    /// program is perfectly well-typed. Any untyped literal in an arithmetic
    /// position can poison the result this way.
    ///
    /// The principled fix is real literal inference, but `expr_types` is read by
    /// `cgen`, whose output the goldens pin byte-for-byte. So instead of
    /// changing what is inferred, [`assignable`] simply declines to judge an
    /// expression whose type may have been defaulted. An explicit `as` cast
    /// pins the type and is therefore *not* defaulted — `y as i64` is trusted.
    ///
    /// [`assignable`]: Self::assignable
    /// `want` is the primitive the value is headed for, because defaulting is
    /// directional: an integer literal may be written at any numeric type, but a
    /// *float* literal at an integer type (`let n: i32 = 1.5`) is a genuine
    /// mistake, not a defaulting artifact.
    fn literal_defaulted(&self, e: ExprId, want: &str) -> bool {
        match &self.ast.expr_at(e).kind {
            ExprKind::Int(_) => true,
            ExprKind::Float(_) => !integer_prim(want),
            // A `comptime` block folds to a literal and types as `i32`/`bool`/`str`
            // regardless of the width its body computed at (see `fold_comptime`).
            ExprKind::Comptime(_) => true,
            ExprKind::Unary { op: UnOp::Neg, rhs } => self.literal_defaulted(*rhs, want),
            ExprKind::Binary { op, lhs, rhs } if !matches!(op, BinOp::And | BinOp::Or) => {
                self.literal_defaulted(*lhs, want) || self.literal_defaulted(*rhs, want)
            }
            _ => false,
        }
    }

    /// Report `got` supplied where `want` is required, unless [`assignable`]
    /// says the pair is fine. `what` names the position for the message.
    ///
    /// [`assignable`]: Self::assignable
    fn check_assignable(&mut self, want: &Ty, got: &Ty, rhs: Option<ExprId>, span: Span, what: &str) {
        if self.assignable(want, got, rhs) {
            return;
        }
        let (w, g) = (want.display(&self.table), got.display(&self.table));
        // The `distinct` hint takes precedence: it names the *reason* the pair is
        // refused, and it is the same wording the `let`-initializer arm has always
        // used, so the suggestion does not change with the position.
        let hint = if self.distinct_mismatch(want, got) {
            " — `distinct` types need an explicit `as`".to_string()
        } else {
            match (want, got) {
                (Ty::Prim(w), Ty::Prim(g)) if numeric_prim(w) && numeric_prim(g) => {
                    format!(" — an explicit `as {w}` converts")
                }
                _ => String::new(),
            }
        };
        self.error(span, format!("{what}: expected `{w}`, found `{g}`{hint}"));
    }

    /// Does the bare type name `name` (resolved from the current module) denote a
    /// generic enum (an `enum Name(T) { … }` template)?
    fn is_generic_enum(&self, name: &str) -> bool {
        self.is_generic_enum_key(&self.canon_type_cur(name))
    }

    /// As [`is_generic_enum`] but for an already-canonical `type_index` key (so a
    /// `mod.Box(T)` path can be checked in its *target* module).
    fn is_generic_enum_key(&self, key: &str) -> bool {
        self.table.type_index.get(key).is_some_and(|&i| {
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

    /// Evaluate a `[N]T` array length to a constant `usize`. Supports an integer
    /// literal (decimal/`0x`/`0b`, `_` separators ignored) — the common case; a
    /// `const`-name fallback can follow. Non-constant lengths resolve to 0 (and are
    /// caught downstream), rather than panicking the type-checker.
    /// Evaluate a `[N]T` length. A literal is the common case and is parsed
    /// directly; anything else — a `const`, an arithmetic expression, a call of a
    /// pure function — goes through the comptime interpreter. An unevaluable
    /// length still yields 0 here so lowering stays total; `audit_type_id` is what
    /// turns that into a diagnostic.
    fn eval_array_len(&self, id: ExprId) -> usize {
        match &self.ast.expr_at(id).kind {
            ExprKind::Int(text) => parse_int_literal_usize(text).unwrap_or(0),
            _ => crate::comptime::Interp::new(self.ast).eval_usize(id).unwrap_or(0),
        }
    }

    /// Check every array length written inside a type annotation. `audit_type_id`
    /// covers item signatures; local `let`/`var` annotations are lowered during
    /// inference and never reach it, so they come through here. Deliberately narrow
    /// — it validates lengths only, and does not repeat the module-path audit.
    fn check_type_array_lens(&mut self, id: TypeId) {
        match self.ast.type_at(id).kind.clone() {
            TypeKind::Array { elem, len } => {
                self.check_array_len(len);
                self.check_type_array_lens(elem);
            }
            TypeKind::Ptr { inner, .. }
            | TypeKind::Slice(inner)
            | TypeKind::GenRef(inner)
            | TypeKind::RegionRef { inner, .. } => self.check_type_array_lens(inner),
            TypeKind::App { args, .. } | TypeKind::Path { args, .. } => {
                for a in args {
                    self.check_type_array_lens(a);
                }
            }
            TypeKind::Fn { params, ret, .. } => {
                for p in params {
                    self.check_type_array_lens(p.ty);
                }
                if let Some(r) = ret {
                    self.check_type_array_lens(r);
                }
            }
            TypeKind::Name(_) | TypeKind::TypeKw | TypeKind::Dyn(_) | TypeKind::Error => {}
        }
    }

    /// Report a length that isn't a compile-time constant. Kept separate from
    /// `eval_array_len` so lowering stays `&self` and total: it always produces a
    /// number, and this is the one place that turns "no number" into a diagnostic.
    fn check_array_len(&mut self, id: ExprId) {
        if matches!(&self.ast.expr_at(id).kind, ExprKind::Int(_)) {
            return; // the literal path never fails
        }
        let outcome = crate::comptime::Interp::new(self.ast).eval_usize(id);
        if let Err(e) = outcome {
            // A suggested rewrite (Diagnostics tier 3). Both remedies are real and
            // land in different places, so both are named: a `const` when the length
            // is a fixed number the program can share, a `comptime { … }` block when
            // it is *computed* — the CTFE ladder exists precisely so a length can be
            // derived rather than spelled out.
            self.diags.push(
                crate::diag::Diagnostic::new(
                    format!("array length must be a compile-time constant: {}", e.message),
                    e.span,
                )
                .with_help(
                    "give it a `const N: usize = …`, or compute it in a `comptime { … }` block — \
                     an array's length is part of its type, so it must be known while checking",
                ),
            );
        }
    }

    /// Evaluate a `comptime { … }` block and give it the type of the value it
    /// produced (roadmap G tier 2). Every failure path is a diagnostic — a comptime
    /// block that cannot be evaluated is never quietly treated as runtime code,
    /// because there is no runtime code for it to become.
    ///
    /// The body is deliberately *not* inferred. The interpreter is the only checker
    /// comptime code has, which is what keeps "it typechecks" and "it evaluates" from
    /// ever disagreeing; running inference over the body as well would mean two
    /// checkers with two opinions and no rule for which wins.
    fn fold_comptime(&mut self, id: ExprId, span: Span) -> Ty {
        match crate::comptime::Interp::new(self.ast).eval(id) {
            Ok(comptime::Value::Int(_)) => Ty::Prim("i32"),
            Ok(comptime::Value::Bool(_)) => Ty::Prim("bool"),
            Ok(comptime::Value::Str(_)) => Ty::Prim("str"),
            // An aggregate becomes a fixed-size array — the same `Ty` a written
            // `[a, b, c]` produces, so a comptime table is indistinguishable from a
            // hand-written one to every later pass.
            Ok(comptime::Value::List(items)) => {
                // An annotation wins for the ELEMENT type, exactly as it does for a
                // written array literal (`var t: [N]u64 = [0; N]` makes the `0` a u64
                // rather than the default i32). Without one, the first element decides.
                let elem = match &self.cur_expected {
                    Some(Ty::Array { elem, .. }) => (**elem).clone(),
                    _ => match items.first() {
                        Some(comptime::Value::Int(_)) => Ty::Prim("i32"),
                        Some(comptime::Value::Bool(_)) => Ty::Prim("bool"),
                        Some(comptime::Value::Str(_)) => Ty::Prim("str"),
                        // A nested list, or an empty one with nothing to annotate it:
                        // there is no rule that would not be a guess.
                        _ => {
                            self.error(
                                span,
                                "a `comptime` block producing this aggregate needs a type \
                                 annotation to say what it is"
                                    .to_string(),
                            );
                            return Ty::Error;
                        }
                    },
                };
                Ty::Array { elem: Box::new(elem), len: items.len() }
            }
            // A block ending in a binding, or an empty one. Refused rather than typed
            // as unit: a `comptime` block exists to produce a value, and a pure one
            // that produces none is dead code the author did not mean to write.
            Ok(comptime::Value::Unit) => {
                self.error(span, "a `comptime` block must produce a value".to_string());
                Ty::Error
            }
            Err(e) => {
                self.error(e.span, format!("`comptime` block: {}", e.message));
                Ty::Error
            }
        }
    }

    /// Report a reflection query the compiler cannot answer (roadmap G tier 3).
    ///
    /// The same shape as [`Self::check_array_len`], and for the same reason: the value
    /// is *required* — a reflection call becomes a literal in the emitted C, so there
    /// is no runtime fallback to degrade to. Unanswerable means diagnosed, never
    /// guessed.
    fn check_reflect_call(&mut self, id: ExprId) {
        if let Err(e) = comptime::Interp::new(self.ast).eval(id) {
            self.error(e.span, format!("compile-time reflection: {}", e.message));
        }
    }

    fn lower_type(&self, ty_params: &HashSet<String>, id: TypeId) -> Ty {
        match &self.ast.type_at(id).kind {
            TypeKind::Name(n) => {
                if ty_params.contains(&n.name) {
                    Ty::Opaque(n.name.clone())
                } else if let Some(p) = prim_ty(&n.name) {
                    Ty::Prim(p)
                } else if let Some(&i) = self.table.type_index.get(&self.canon_type_cur(&n.name)) {
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
            TypeKind::Array { len, elem } => Ty::Array {
                elem: Box::new(self.lower_type(ty_params, *elem)),
                len: self.eval_array_len(*len),
            },
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
                // A generic enum's ctor is canonicalized so two modules' `Box(T)`
                // monomorphize to distinct `Jestyr_Box__m<…>__<args>` symbols.
                if self.is_generic_enum(&ctor.name) {
                    Ty::GenEnum { ctor: self.canon_type_cur(&ctor.name), args: aty }
                } else {
                    // Generic struct (comptime-fn form): the ctor is a FUNCTION
                    // name, so it canonicalizes through the fn namespace (`canon_in`
                    // over `dup`), not the type namespace — two modules may each
                    // define `fn Box(comptime T: type) -> type` and their instances
                    // stay distinct (`Jestyr_Box__m<a>__i32` vs `__m<b>__i32`),
                    // exactly like the generic-enum rule above. Bare unless the
                    // name actually collides, so non-colliding programs key and
                    // mangle exactly as before.
                    Ty::GenStruct { ctor: self.canon_cur(&ctor.name), args: aty }
                }
            }
            // `mod.Type` / `mod.Type(args)`: a module-qualified type, resolved in
            // the *target* module (so it picks that module's type even when the
            // name collides across modules). Visibility is checked separately by
            // `audit_type_paths`.
            TypeKind::Path { module, name, args } => {
                let target = self.binding_module(&module.name);
                if args.is_empty() {
                    if let Some(p) = prim_ty(&name.name) {
                        Ty::Prim(p)
                    } else {
                        let key = match target {
                            Some(t) => self.canon_type_in(t, &name.name),
                            None => name.name.clone(),
                        };
                        match self.table.type_index.get(&key) {
                            Some(&i) => Ty::Named(i),
                            None => Ty::Opaque(name.name.clone()),
                        }
                    }
                } else {
                    let aty: Vec<Ty> = args.iter().map(|a| self.lower_type(ty_params, *a)).collect();
                    // Resolve the generic enum in the *target* module so `mod.Box(T)`
                    // picks that module's (possibly colliding) template.
                    let key = match target {
                        Some(t) => self.canon_type_in(t, &name.name),
                        None => self.canon_type_cur(&name.name),
                    };
                    if self.is_generic_enum_key(&key) {
                        Ty::GenEnum { ctor: key, args: aty }
                    } else {
                        // The generic-struct ctor is a fn: canon in the TARGET
                        // module's fn namespace, so `mod.Box(i32)` picks that
                        // module's (possibly colliding) comptime type-fn.
                        let fkey = match target {
                            Some(t) => self.canon_in(t, &name.name),
                            None => self.canon_cur(&name.name),
                        };
                        Ty::GenStruct { ctor: fkey, args: aty }
                    }
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
                } else if let Some(&i) = self.table.type_index.get(&self.canon_type_cur(&n.name)) {
                    Ty::Named(i)
                } else {
                    Ty::Opaque(n.name.clone())
                }
            }
            _ => Ty::Opaque("?".to_string()),
        }
    }

    /// Find a top-level function by its *canonical* name (`canon`). For a name
    /// that doesn't collide across modules this is just the bare name, so callers
    /// passing a bare name resolve exactly as before; for a colliding name the
    /// caller must pass the disambiguated `name__m<mod>` so the right module's
    /// definition is selected.
    fn find_fn_decl(&self, name: &str) -> Option<&'a FnDecl> {
        self.ast.items.iter().enumerate().find_map(|(i, it)| match it {
            Item::Fn(f) if self.canon_in(*self.modules.item_mod.get(i).unwrap_or(&0), &f.name.name) == name => {
                Some(f)
            }
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
    /// Collect the whole-program payload map and check the two declaration rules
    /// (error-payloads E3, `docs/error-payloads.md` §3):
    ///
    /// * **D1 agreement** — a payload is a property of the error NAME. `Parse(i64)`
    ///   anywhere means `Parse` carries `i64` everywhere; a bare `Parse` elsewhere,
    ///   or a `Parse(str)`, is a conflict reported at BOTH sites (two located
    ///   diagnostics — a single diagnostic cannot carry two spans).
    /// * **The v1 domain** — a payload is a scalar or `str`. Nothing owning (a
    ///   `String` would owe a `drop` on every path an error can die on), nothing
    ///   aggregate (keeps the union small and the future binder scalar), no
    ///   references. Each refusal names the rule.
    ///
    /// Declaration sites live in THREE places — free fns, struct-item methods,
    /// and methods inside `struct { … }` EXPRESSIONS (the comptime-generic
    /// factory idiom) — the same three homes the E1 census learned to scan.
    fn collect_err_payloads(&mut self) {
        let ast = self.ast;
        let mut first: HashMap<String, (Option<Ty>, crate::span::Span)> = HashMap::new();
        let mut sets: Vec<&crate::ast::ErrorSet> = Vec::new();
        for item in &ast.items {
            match item {
                Item::Fn(f) => sets.extend(f.errors.iter()),
                Item::Struct { body, .. } => {
                    for m in &body.members {
                        if let StructMember::Method(f) = m {
                            sets.extend(f.errors.iter());
                        }
                    }
                }
                Item::Impl(im) => {
                    for f in &im.methods {
                        sets.extend(f.errors.iter());
                    }
                }
                // A TRAIT method's declared set is a declaration site too
                // (trait-errors T1) — `trait Load { fn get(…) -> T !{ Missing(i64) } }`
                // is where the payload is most naturally stated once.
                Item::Trait(t) => {
                    for m in &t.methods {
                        sets.extend(m.errors.iter());
                    }
                }
                _ => {}
            }
        }
        for e in &ast.exprs {
            if let ExprKind::StructType(body) = &e.kind {
                for m in &body.members {
                    if let StructMember::Method(f) = m {
                        sets.extend(f.errors.iter());
                    }
                }
            }
        }
        let empty = HashSet::new();
        for es in sets {
            for n in &es.names {
                let pay = n.payload.map(|t| self.lower_type(&empty, t));
                if let (Some(ty), Some(tid)) = (&pay, n.payload) {
                    if !err_payload_ty_allowed(ty) {
                        self.error(
                            ast.type_at(tid).span,
                            format!(
                                "a v1 error payload must be a scalar or `str`, not `{}` — \
                                 owning and aggregate payloads are deliberately deferred \
                                 (docs/error-payloads.md §3)",
                                ty.display(&self.table)
                            ),
                        );
                        continue;
                    }
                }
                match first.get(&n.name.name) {
                    None => {
                        first.insert(n.name.name.clone(), (pay.clone(), n.name.span));
                        if let Some(ty) = pay {
                            self.err_payloads.insert(n.name.name.clone(), ty);
                        }
                    }
                    Some((prev, prev_span)) if *prev != pay => {
                        let render = |p: &Option<Ty>| match p {
                            Some(t) => format!("with payload `{}`", t.display(&self.table)),
                            None => "with no payload".to_string(),
                        };
                        let (prev_span, prev_r, here_r) =
                            (*prev_span, render(prev), render(&pay));
                        self.error(
                            n.name.span,
                            format!(
                                "error `{}` is declared {} here, but {} elsewhere — \
                                 a payload is a property of the error name, program-wide",
                                n.name.name, here_r, prev_r
                            ),
                        );
                        self.error(
                            prev_span,
                            format!(
                                "error `{}` is declared {} here, conflicting with a \
                                 later declaration {}",
                                n.name.name, prev_r, here_r
                            ),
                        );
                    }
                    Some(_) => {}
                }
            }
        }
    }

    /// Check the payload extractor `catch |e| match e { … }` (error-payloads E4,
    /// `docs/error-payloads.md` §5). Arms are error NAMES from the base's static
    /// set — `Empty` bare, `TooBig(n)` binding the declared payload, `_` as the
    /// catch-all — and the match must be exhaustive over that set. What is
    /// deliberately refused, each with its reason in the diagnostic: an arm
    /// naming an error outside the set (it cannot occur), a bare arm for a
    /// payload carrier is FINE (the payload is simply ignored) but binding a
    /// payload on a bare name is not (there is nothing to bind), guards (they
    /// would break exhaustiveness accounting), non-name patterns (an error is
    /// not a scalar), duplicate arms, and arms after the wildcard (dead).
    /// Every arm body is typed against the ok type — each is a fallback value.
    fn check_err_match(
        &mut self,
        scope: &mut Scope,
        typ: &HashSet<String>,
        self_ty: &Ty,
        errs: &[String],
        ok: &Ty,
        scrut: ExprId,
        arms: &[crate::ast::MatchArm],
    ) {
        use crate::ast::PatKind;
        // The scrutinee is the binder name: infer it so its type row records the
        // opaque `error`, exactly as any other read of the binder would.
        self.infer(scope, typ, self_ty, scrut);
        let mut covered: Vec<String> = Vec::new();
        let mut saw_wild = false;
        for arm in arms {
            let pat = self.ast.pat_at(arm.pat);
            let pspan = pat.span;
            if saw_wild {
                self.error(pspan, "unreachable arm: it follows the `_` catch-all");
            }
            if let Some(g) = arm.guard {
                self.error(
                    self.ast.expr_at(g).span,
                    "a guard is not supported on an error arm — it would break exhaustiveness accounting",
                );
            }
            // Which name (if any) this arm covers, and the payload binder to push.
            let mut bind: Option<(String, Ty)> = None;
            match &pat.kind {
                PatKind::Wildcard => saw_wild = true,
                PatKind::Ident(n) => {
                    if !errs.iter().any(|e| e == &n.name) {
                        self.error(
                            pspan,
                            format!(
                                "`{}` is not in this expression's error set {{ {} }}",
                                n.name,
                                errs.join(", ")
                            ),
                        );
                    } else if covered.contains(&n.name) {
                        self.error(pspan, format!("duplicate arm for error `{}`", n.name));
                    } else {
                        covered.push(n.name.clone());
                    }
                }
                PatKind::Variant { name, subpats } => {
                    let declared = self.err_payloads.get(&name.name).cloned();
                    if !errs.iter().any(|e| e == &name.name) {
                        self.error(
                            pspan,
                            format!(
                                "`{}` is not in this expression's error set {{ {} }}",
                                name.name,
                                errs.join(", ")
                            ),
                        );
                    } else if declared.is_none() {
                        self.error(
                            pspan,
                            format!(
                                "error `{}` carries no payload — match it bare (`{}`)",
                                name.name, name.name
                            ),
                        );
                    } else if subpats.len() != 1 {
                        self.error(
                            pspan,
                            format!(
                                "error `{}` carries exactly one payload value, found {} pattern(s)",
                                name.name,
                                subpats.len()
                            ),
                        );
                    } else {
                        if covered.contains(&name.name) {
                            self.error(pspan, format!("duplicate arm for error `{}`", name.name));
                        } else {
                            covered.push(name.name.clone());
                        }
                        match &self.ast.pat_at(subpats[0]).kind {
                            PatKind::Ident(b) => {
                                bind = Some((b.name.clone(), declared.unwrap()));
                            }
                            PatKind::Wildcard => {}
                            _ => self.error(
                                self.ast.pat_at(subpats[0]).span,
                                "a payload pattern is a binding name or `_`",
                            ),
                        }
                    }
                }
                _ => self.error(
                    pspan,
                    "an error arm is an error name (`Empty`, `TooBig(n)`) or `_` — \
                     an error is not a scalar to match structurally",
                ),
            }
            // The arm body is a fallback value: typed against the ok type, with
            // the payload binder (if any) in a scope of its own.
            scope.push(HashMap::new());
            if let Some((n, t)) = bind {
                scope.last_mut().unwrap().insert(n, t);
            }
            let prev = self.cur_expected.take();
            self.cur_expected = Some(ok.clone());
            self.infer(scope, typ, self_ty, arm.body);
            self.cur_expected = prev;
            scope.pop();
        }
        if !saw_wild {
            let missing: Vec<&str> = errs
                .iter()
                .filter(|e| !covered.contains(e))
                .map(String::as_str)
                .collect();
            if !missing.is_empty() {
                self.error(
                    self.ast.expr_at(scrut).span,
                    format!(
                        "this `match` does not cover {{ {} }} — add the arm(s) or a `_` catch-all",
                        missing.join(", ")
                    ),
                );
            }
        }
    }

    /// `?`/rethrow inclusion (error-payloads E2): the propagated set must be
    /// included in the enclosing function's declared set. Reported at the
    /// propagation site, naming exactly the names that are missing. When the
    /// enclosing function declares NO set, nothing is reported here — "`?` used
    /// outside a fallible function" is already that construct's own diagnostic.
    fn check_propagation(&mut self, errs: &[String], span: crate::span::Span) {
        let Some(own) = self.cur_errs.clone() else { return };
        let missing: Vec<&String> = errs.iter().filter(|e| !own.contains(e)).collect();
        if !missing.is_empty() {
            let m: Vec<&str> = missing.iter().map(|s| s.as_str()).collect();
            self.error(
                span,
                format!(
                    "propagates {{ {} }}, which the enclosing error set {{ {} }} does not declare",
                    m.join(", "),
                    own.join(", ")
                ),
            );
        }
    }

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
        let (recv_conv, ret, errs, param_tys, runtime_idx, must_use) = {
            let sig = self.table.fns.get(mname)?;
            let param_tys: Vec<Ty> = sig.params.iter().map(|p| p.ty.clone()).collect();
            let runtime_idx: Vec<usize> =
                f.params.iter().enumerate().filter(|(_, p)| !p.comptime).map(|(i, _)| i).collect();
            (
                sig.params[recv_idx].conv,
                sig.ret.clone(),
                sig.errs.clone(),
                param_tys,
                runtime_idx,
                sig.must_use,
            )
        };

        // The receiver type must match for this to be a method call at all.
        // Recorded only AFTER this gate: a receiver mismatch means this was never a
        // call to `mname` at all, and marking the expr before knowing that would
        // attribute the attribute to whichever function the name happened to hit.
        if !head_matches(&param_tys[recv_idx], recv_ty) {
            return None;
        }
        self.record_must_use(call_id, must_use);

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

        self.record_method(
            call_id,
            MethodRes { fn_name: mname.to_string(), recv_ctor: None, type_args, recv_conv },
        );
        let ret = subst_ty(&ret, &subst);
        Some(match errs {
            Some(e) => Ty::Result(Box::new(ret), e),
            None => ret,
        })
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
        // Resolution path 3 of 3 for `@must_use`. A struct-body method's own
        // declaration IS its contract — there is no trait above it that a caller
        // might have been typed by — so the attribute is read straight off the
        // decl rather than out of a `FnSig` (these methods never get one).
        //
        // TRAIT methods are deliberately NOT handled here or in
        // `resolve_impl_method`/`resolve_bound_method`/`resolve_dyn_method`.
        // `wrap_trait_ret` records the principle: a call through a trait is typed
        // by the TRAIT's signature, whichever impl answers. An `@must_use` written
        // on one impl would make the same trait call must-use for some receivers
        // and not others, so the attribute belongs on the trait method — and
        // `TraitMethod` has no `attrs` field to put it on. That is an AST + parser
        // increment (and a port mirror), not a line here.
        self.record_must_use(call_id, method.attr("must_use").is_some());

        let recv_conv =
            method.params.iter().find(|p| p.is_self).map(|p| p.conv).unwrap_or(Conv::Default);
        let errs = errs_of(&method.errors);

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

        self.record_method(
            call_id,
            MethodRes {
                fn_name: mname.to_string(),
                recv_ctor: Some(ctor),
                type_args: recv_args,
                recv_conv,
            },
        );
        Some(match errs {
            Some(e) => Ty::Result(Box::new(ret), e),
            None => ret,
        })
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
        match self.owner.get(&(target_mod, fname.to_string())).copied() {
            Some(true) => {
                // Canonical key: selects this module's definition even when the
                // name is shared with another module, and is the symbol the
                // backend emits for the direct call.
                let key = self.canon_in(target_mod, fname);
                self.record_qualified(id, key.clone());
                // Clone the signature out before any `&mut self` call below, exactly
                // as the unqualified path does.
                let resolved = self.table.fns.get(&key).map(|sig| {
                    let ptys: Vec<(String, Ty)> =
                        sig.params.iter().map(|p| (p.name.clone(), p.ty.clone())).collect();
                    (sig.ret.clone(), sig.errs.clone(), ptys, sig.must_use)
                });
                if let Some((ret, errs, ptys, must_use)) = resolved {
                    self.record_must_use(id, must_use);
                    let want = ptys.len();
                    if want != args.len() {
                        self.error(
                            span,
                            format!("`{binding}.{fname}` expects {want} argument(s), found {}", args.len()),
                        );
                    }
                    // Argument-vs-parameter types. This path used to check ARITY ONLY,
                    // which is why the int→int sweep read 6 sites per-file and 55 on the
                    // flattened program: `list.get(i32, p.roots, r)` was never argument-
                    // checked, and only the flatten — where it becomes a bare
                    // `get__list` — exposed it. The measurement had the same blind spot
                    // as the checker it was measuring. Arity-gated for the same reason
                    // as the unqualified path.
                    if want == args.len() {
                        for ((pname, pty), a) in ptys.iter().zip(args.iter()) {
                            let got = self.expr_types[a.0 as usize].clone();
                            let sp = self.ast.expr_at(*a).span;
                            self.check_assignable(
                                pty,
                                &got,
                                Some(*a),
                                sp,
                                &format!("argument `{pname}` of `{binding}.{fname}`"),
                            );
                        }
                    }
                    let ret = self.monomorphize_ret(&key, args, typ, ret);
                    let t = match errs {
                        Some(e) => Ty::Result(Box::new(ret), e),
                        None => ret,
                    };
                    self.set(id, t)
                } else {
                    self.set(id, Ty::Unknown)
                }
            }
            Some(false) => {
                self.error(span, format!("`{fname}` is private to module `{binding}`"));
                self.set(id, Ty::Unknown)
            }
            None => {
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
        match self.owner.get(&(target_mod, name.to_string())).copied() {
            Some(true) => {
                let key = self.canon_in(target_mod, name);
                self.record_qualified(id, key.clone());
                let t = self.table.consts.get(&key).cloned().unwrap_or(Ty::Unknown);
                self.set(id, t)
            }
            Some(false) => {
                self.error(span, format!("`{name}` is private to module `{binding}`"));
                self.set(id, Ty::Unknown)
            }
            None => {
                self.error(span, format!("module `{binding}` has no public item `{name}`"));
                self.set(id, Ty::Unknown)
            }
        }
    }

    // --- qualified type paths (`mod.Type`) — visibility audit ---

    /// Validate every `mod.Type` path in the program: the head must be an import
    /// binding of the referencing module, and the type must be `pub` in (and owned
    /// by) the target module — the type-position twin of `resolve_qualified_call`.
    /// Resolution of the type itself happens in `lower_type`; this pass only
    /// reports the visibility/ownership errors that `lower_type` (a `&self` method)
    /// cannot. Run after the global table is built.
    fn audit_type_paths(&mut self) {
        let ast = self.ast;
        for (i, item) in ast.items.iter().enumerate() {
            self.cur_mod = *self.modules.item_mod.get(i).unwrap_or(&0);
            match item {
                Item::Fn(f) => self.audit_fn_sig(f),
                Item::Struct { body, .. } => {
                    for m in &body.members {
                        match m {
                            StructMember::Field { ty, .. } => self.audit_type_id(*ty),
                            StructMember::Method(f) => self.audit_fn_sig(f),
                        }
                    }
                }
                Item::Const(c) => {
                    if let Some(t) = c.ty {
                        self.audit_type_id(t);
                    }
                }
                Item::Distinct(d) => self.audit_type_id(d.base),
                Item::Extern(e) => {
                    for p in &e.params {
                        if let Some(t) = p.ty {
                            self.audit_type_id(t);
                        }
                    }
                    if let Some(t) = e.ret_ty {
                        self.audit_type_id(t);
                    }
                }
                Item::Enum(en) => {
                    for v in &en.variants {
                        for (_, t) in &v.fields {
                            self.audit_type_id(*t);
                        }
                    }
                }
                Item::Trait(tr) => {
                    for m in &tr.methods {
                        for p in &m.params {
                            if let Some(t) = p.ty {
                                self.audit_type_id(t);
                            }
                        }
                        if let Some(t) = m.ret_ty {
                            self.audit_type_id(t);
                        }
                    }
                }
                Item::Impl(im) => {
                    self.audit_type_id(im.ty);
                    for m in &im.methods {
                        self.audit_fn_sig(m);
                    }
                }
                Item::Import(_) => {}
            }
        }
    }

    fn audit_fn_sig(&mut self, f: &FnDecl) {
        for p in &f.params {
            if let Some(t) = p.ty {
                self.audit_type_id(t);
            }
        }
        if let Some(t) = f.ret_ty {
            self.audit_type_id(t);
        }
    }

    /// Recurse through a type, validating any `mod.Type` paths within. The node's
    /// kind is cloned first so the recursive `&mut self` calls don't alias the
    /// `&self.ast` borrow (the same pattern `lower_type` avoids by being `&self`).
    fn audit_type_id(&mut self, id: TypeId) {
        let kind = self.ast.type_at(id).kind.clone();
        let span = self.ast.type_at(id).span;
        match kind {
            TypeKind::Ptr { inner, .. }
            | TypeKind::Slice(inner)
            | TypeKind::GenRef(inner)
            | TypeKind::RegionRef { inner, .. } => self.audit_type_id(inner),
            TypeKind::Array { elem, len } => {
                // The length must be a compile-time constant: it becomes part of the
                // type (`[4]i32`) and of the emitted C type name, so a value the
                // compiler cannot compute is an error rather than a silent zero.
                self.check_array_len(len);
                self.audit_type_id(elem)
            }
            TypeKind::App { args, .. } => {
                for a in args {
                    self.audit_type_id(a);
                }
            }
            TypeKind::Fn { params, ret, .. } => {
                for p in params {
                    self.audit_type_id(p.ty);
                }
                if let Some(r) = ret {
                    self.audit_type_id(r);
                }
            }
            TypeKind::Path { module, name, args } => {
                for a in &args {
                    self.audit_type_id(*a);
                }
                self.audit_one_path(&module, &name, span);
            }
            TypeKind::Name(_) | TypeKind::TypeKw | TypeKind::Dyn(_) | TypeKind::Error => {}
        }
    }

    /// Check one `module.name` type path: the head is an import binding, and the
    /// target module exposes `name` as a `pub` item.
    fn audit_one_path(&mut self, module: &Ident, name: &Ident, span: Span) {
        let Some(target) = self.binding_module(&module.name) else {
            self.error(span, format!("`{}` is not an imported module", module.name));
            return;
        };
        match self.owner.get(&(target, name.name.clone())).copied() {
            Some(true) => {} // a public item of the target module — fine
            Some(false) => {
                self.error(span, format!("type `{}` is private to module `{}`", name.name, module.name))
            }
            None => self.error(
                span,
                format!("module `{}` has no public type `{}`", module.name, name.name),
            ),
        }
    }

    // --- checking bodies ---

    /// The module an import `binding` refers to, in the current module's scope.
    fn binding_module(&self, binding: &str) -> Option<ModId> {
        self.modules.imports.get(self.cur_mod).and_then(|m| m.get(binding)).copied()
    }

    /// An unqualified `name` that the current module does not define, but which
    /// *another* module does, is an unresolved-name error under per-module
    /// namespacing (design §9): cross-module access must be qualified. A name no
    /// module defines (a builtin/intrinsic/opaque symbol) stays quiet.
    fn report_cross_module_name(&mut self, name: &str, span: Span) {
        if let Some(mods) = self.name_mods.get(name) {
            // (It can't be owned by the current module here — that path resolved
            // it locally — so every listed module is a different one.)
            let m = mods[0];
            let owner = self.modules.names.get(m).map(String::as_str).unwrap_or("?");
            self.error(
                span,
                format!(
                    "cannot find `{name}` in this module; it is defined in module `{owner}` — call it qualified as `{owner}.{name}`"
                ),
            );
        }
    }

    fn check_items(&mut self) {
        let ast = self.ast;
        let empty = HashSet::new();
        for (i, item) in ast.items.iter().enumerate() {
            self.cur_mod = *self.modules.item_mod.get(i).unwrap_or(&0);
            match item {
                Item::Fn(f) => {
                    // Entering a comptime type-fn: record its canonical name and
                    // type params so the `return struct { … }` arm can type its
                    // methods' `self` as the real generic-struct type.
                    self.cur_type_fn = self.ctor_struct_body(f).is_some().then(|| {
                        (self.canon_cur(&f.name.name), self.comptime_tp_names(f))
                    });
                    self.check_fn(f, &empty, &Ty::Unit);
                    self.cur_type_fn = None;
                }
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
                // An `impl`'s method BODIES. Stage B (`register_impls`) checks only the
                // SIGNATURES — coherence, membership, fallibility conformance, the
                // recorded return types — and never looks at `m.body`. Without this
                // arm nothing inside an impl body was ever inferred: no arity, no
                // assignability, no exhaustiveness, and — the reason this arm exists —
                // no *resolution*. `record_qualified` and `record_call_sym` are only
                // ever written by `infer`, so a `mod.f(…)` call or a collision-renamed
                // bare call inside an impl body reached cgen with no resolution at all
                // and degraded: `helper.note(x)` emitted as the field access
                // `j_helper.j_note(x)`, and a colliding `release(x)` as the bare
                // `jestyr_release` instead of the canonical `jestyr_release__m0` —
                // both undeclared C identifiers, i.e. a whole-front-end pass followed
                // by a gcc failure.
                //
                // `self_ty` is the impl target lowered with the impl's bracket
                // parameters in scope, so a blanket `impl[T] Drop for Deque(T)` types
                // `self` as `Deque(T)` exactly as `register_impls` keys it.
                //
                // Two deliberate deferrals, both with zero occurrences in the corpus:
                //  * the impl's own bracket BOUNDS are not merged into
                //    `cur_type_param_bounds` (`check_fn` rebuilds it from `f.generics`),
                //    so a bound-method call on the impl's `T` reports at the definition
                //    site rather than dispatching. That is the honest answer today:
                //    the blanket-impl emission path has never seen a recorded bound
                //    call from an impl body, so resolving one would trade a diagnostic
                //    for a mis-emission.
                //  * `Self` in a body's TYPE position lowers to `Opaque("Self")` (no
                //    `self_subst` here, unlike the recorded return types). `assignable`
                //    is lenient on `Opaque`, so this costs nothing today; `Self { … }`
                //    in *expression* position already resolves through `self_ty`.
                Item::Impl(im) => {
                    let gen: HashSet<String> =
                        im.generics.iter().map(|g| g.name.name.clone()).collect();
                    let self_ty = self.lower_type(&gen, im.ty);
                    for m in &im.methods {
                        self.check_fn(m, &gen, &self_ty);
                    }
                }
                Item::Enum(_) | Item::Distinct(_) | Item::Extern(_) | Item::Import(_) => {}
                // A trait's DEFAULT method bodies keep the identical hole (`self` would
                // be `Opaque("Self")`). Nothing in the corpus has one, and a fallible
                // default body is already refused outright (`register_traits`), so this
                // is left for the increment that needs it — but it is a hole, not a
                // "checked in Stage B" as the comment here used to claim.
                Item::Trait(_) => {}
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
        // The (ok) return type is the expected type for `return <expr>` and for the
        // block's **tail expression** (an implicit return). Seeding `cur_expected`
        // here lets a tail `match`/`if` propagate the return type into its arms, so a
        // nullary generic variant in tail position resolves: `-> Option(U) { match …
        // { none => none } }`. Non-tail `let`/`return` statements save/restore
        // `cur_expected`, so by the tail it is back to this seeded value.
        let prev_ret = self.cur_ret.take();
        self.cur_ret = f.ret_ty.map(|t| self.lower_type(&typ, t));
        let prev_errs = self.cur_errs.take();
        self.cur_errs = errs_of(&f.errors);
        let prev_exp = self.cur_expected.take();
        self.cur_expected = self.cur_ret.clone();
        self.infer_block(&mut scope, &typ, self_ty, &f.body);
        self.cur_expected = prev_exp;
        self.cur_errs = prev_errs;
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
                    if let Some(t) = ty {
                        self.check_type_array_lens(*t);
                    }
                    let expected = ty.map(|t| self.lower_type(typ, t));
                    let prev = self.cur_expected.take();
                    self.cur_expected = expected.clone();
                    let inferred = init.map(|e| self.infer(scope, typ, self_ty, e));
                    self.cur_expected = prev;
                    // `let d: dyn Trait = concrete` coerces the initializer (Stage F).
                    if let (Some(ann), Some(e)) = (&expected, init) {
                        self.check_dyn_coercion(*e, ann);
                    }
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
                        } else {
                            // General assignability. Disjoint from the `distinct`
                            // arm above (that one fires only on `Ty::Named`, this
                            // one only on `Ty::Prim`), so a mismatch is reported
                            // once with the more specific message.
                            let (ann, got) = (ann.clone(), got.clone());
                            self.check_assignable(
                                &ann,
                                &got,
                                *init,
                                name.span,
                                &format!("`{}`", name.name),
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
                        let got = self.infer(scope, typ, self_ty, *v);
                        self.cur_expected = prev;
                        // `fn f() -> dyn Trait { return concrete }` coerces (Stage F).
                        if let Some(ret) = self.cur_ret.clone() {
                            self.check_dyn_coercion(*v, &ret);
                            // `cur_ret` is the *ok* type of a fallible fn, so a
                            // `return <T>` in a `-> T !E` compares against `T`.
                            let span = self.ast.expr_at(*v).span;
                            self.check_assignable(&ret, &got, Some(*v), span, "return");
                        }
                        // **A `return` in a fallible function must be Result-typed.**
                        //
                        // The ok-type comparison above is deliberate and is NOT the whole
                        // rule: cgen emits `return <value>` verbatim, so returning a bare
                        // ok value from a `-> T !E` produced C that assigns an `int64_t`
                        // to a `JestyrResult_i64`. `check` passed and gcc refused — the
                        // degrades-to-gcc class this session set out to burn down.
                        //
                        // Probed rather than reasoned about, because the boundary is not
                        // where it looks. Legal (all Result-typed): `return ok(v)`,
                        // `return err(E)`, and `return other_fallible(x)` — forwarding a
                        // whole result is fine and worth keeping. Broken: `return f(x)?`
                        // and `return f(x) catch v`, both of which UNWRAP to the ok type
                        // and then get emitted as bare values. So the rule is exactly
                        // "the returned expression is a Result", which covers all three
                        // shapes with one condition.
                        //
                        // Reference-only: this adds a diagnostic and changes no emitted
                        // byte, so there is no port mirror and no reseed. The port stays
                        // permissive here exactly as it does for assignability.
                        if self.cur_errs.is_some()
                            && !matches!(got, Ty::Result(..) | Ty::Error | Ty::Unknown)
                        {
                            let span = self.ast.expr_at(*v).span;
                            self.error(
                                span,
                                "a fallible function must return a result, not a bare value",
                            );
                            self.diags.last_mut().unwrap().help = Some(
                                "wrap it: `return ok(<expr>)` for success, `return err(<Name>)` \
                                 for failure. `?` and `catch` unwrap a result, so they cannot \
                                 appear directly after `return` here"
                                    .to_string(),
                            );
                        }
                    }
                    result = Ty::Unit;
                }
                Stmt::Expr(e) => {
                    let t = self.infer(scope, typ, self_ty, *e);
                    if i + 1 == n {
                        result = t;
                    } else if let Ty::Result(_, errs) = &t {
                        // **A discarded fallible result.** `file.finish(w)` written as a
                        // statement throws away the only verdict the whole `std/file`
                        // write half produces — whether the bytes actually landed. Until
                        // now that compiled and ran with no diagnostic at all.
                        //
                        // Only NON-trailing statements are judged. A block's last
                        // expression is its value, so in a `-> T !E` body it is the
                        // implicit return and discards nothing; flagging it would refuse
                        // the single most ordinary way to write a fallible function.
                        //
                        // `e?` and `e catch v` both unwrap to the ok type before they get
                        // here, so the two spellings that DO handle the error are not
                        // reachable by this rule — which is what makes it a rule about
                        // discarding rather than a rule about calling.
                        //
                        // **An ERROR, not a warning, and the corpus is why.** Measured
                        // over all 208 files before choosing: FOUR sites, every one of
                        // them `file.finish(…)` in `file_test.jtr` — the exact call
                        // `std/file`'s header names as the one that reports whether the
                        // bytes landed. Zero false positives anywhere else. A rule that
                        // narrow, with a deliberate-discard spelling already in the
                        // language, does not need a grace period; a warning here would
                        // just be an error nobody reads.
                        //
                        // REFERENCE-ONLY, deliberately. The port has no assignability
                        // check either (the int→int rule set that precedent), and this
                        // creates no Error *type*, so `jc` stays permissive where
                        // `jestyrc` refuses. That asymmetry is the checker being ahead of
                        // the bootstrap, not a divergence in what the two backends emit —
                        // no C changes, so no mirror and no reseed.
                        let set = if errs.is_empty() {
                            String::new()
                        } else {
                            format!(" `!{{ {} }}`", errs.join(", "))
                        };
                        self.error(
                            self.ast.expr_at(*e).span,
                            format!(
                                "the fallible result of this call is discarded; its error set{set} is thrown away"
                            ),
                        );
                        self.diags.last_mut().unwrap().help = Some(
                            "handle it: `expr?` propagates, `expr catch <fallback>` recovers, \
                             and `let _v = expr catch <fallback>` records the verdict"
                                .to_string(),
                        );
                    } else if self.must_use_call[e.0 as usize] {
                        // **A discarded `@must_use` result.** The infallible sibling of
                        // the rule above, and until now the attribute's ONLY enforcement
                        // was `__attribute__((warn_unused_result))` in the emitted C —
                        // so whether ignoring `checked_add(a, b)` was diagnosed depended
                        // on which C compiler built the output and at what warning level,
                        // and `jestyrc check` said nothing at all. That is the shape the
                        // notes call degrades-to-gcc: a rule the language advertises and
                        // then subcontracts.
                        //
                        // Ordered AFTER the fallible arm, not beside it. A `@must_use
                        // fn f() -> T !E` is discarded in both senses at once, and the
                        // error set is the more specific complaint — one diagnostic per
                        // discard, and the one that names what was actually thrown away.
                        //
                        // Non-trailing only, for the same reason as above: a block's last
                        // expression is its value, not a discard. That leaves one real
                        // gap — the last statement of a `-> ()` body, where the value has
                        // nowhere to go and still is not judged — which the fallible rule
                        // has had since v3. Closing it needs the block checker to be told
                        // whether its own value is wanted, which neither rule needs badly
                        // enough to justify threading it through every caller.
                        //
                        // REFERENCE-ONLY, like the fallible rule and for the same reason:
                        // no Error *type* is produced and no C changes, so `jc` stays
                        // permissive and the two backends still emit the same bytes. The
                        // `warn_unused_result` lowering is untouched and still there —
                        // this is a second, earlier line of defence, not a replacement.
                        self.error(
                            self.ast.expr_at(*e).span,
                            "the `@must_use` result of this call is discarded".to_string(),
                        );
                        self.diags.last_mut().unwrap().help = Some(
                            "consume it: use the value, or bind it (`let _v = <expr>`) to \
                             record that ignoring it is deliberate"
                                .to_string(),
                        );
                    }
                }
            }
        }
        scope.pop();
        result
    }

    /// The constructor name of a `par for … reduce(r)` argument, for the
    /// deterministic-reduction check: `core.sum_reduction()` → `Some("sum_reduction")`,
    /// reading the call's callee (a qualified `Field` or a bare `Name`). A reduction
    /// that isn't a direct constructor call yields `None` (and is rejected).
    fn reduction_ctor_name(&self, e: ExprId) -> Option<String> {
        if let ExprKind::Call { callee, .. } = &self.ast.expr_at(e).kind {
            match &self.ast.expr_at(*callee).kind {
                ExprKind::Name(n) => return Some(n.name.clone()),
                ExprKind::Field { name, .. } => return Some(name.name.clone()),
                _ => {}
            }
        }
        None
    }

    fn infer(&mut self, scope: &mut Scope, typ: &HashSet<String>, self_ty: &Ty, id: ExprId) -> Ty {
        let ast = self.ast;
        let data = ast.expr_at(id);
        let span = data.span;
        let ty = match &data.kind {
            ExprKind::Int(_) => Ty::Prim("i32"),
            // `comptime { … }` types as *the literal it folds to* — not as whatever
            // its body would infer to. That equivalence is the whole contract: cgen
            // emits the folded literal, so anything else here could disagree with what
            // is actually compiled. The body is not inferred; see `fold_comptime`.
            ExprKind::Comptime(_) => self.fold_comptime(id, span),
            ExprKind::Float(_) => Ty::Prim("f64"),
            ExprKind::Str(_) => Ty::Prim("str"),
            ExprKind::Char(_) => Ty::Prim("char"),
            ExprKind::Bool(_) => Ty::Prim("bool"),
            ExprKind::Null => Ty::Ptr { mutbl: PtrMut::Default, inner: Box::new(Ty::Unknown) },

            ExprKind::Name(n) => {
                let local_const =
                    if self.owns_local(&n.name) { self.table.consts.get(&self.canon_cur(&n.name)) } else { None };
                if let Some(t) = scope_lookup(scope, &n.name) {
                    t
                } else if let Some(t) = local_const {
                    let t = t.clone();
                    let key = self.canon_cur(&n.name);
                    if key != n.name {
                        self.record_call_sym(id, key);
                    }
                    t
                } else if let Some(&i) = self.table.variants.get(&self.canon_variant_in(self.cur_mod, &n.name)) {
                    // A bare nullary variant, e.g. `none` — for a generic enum its
                    // instantiation comes from the expected type (`variant_ctor_type`).
                    self.variant_ctor_type(i, &n.name, &[])
                } else {
                    // A bare function name used as a value (e.g. `&make` to take
                    // its address). Its type stays opaque here, but record the
                    // canonical symbol so the backend names the right one when the
                    // name collides across modules.
                    let key = self.canon_cur(&n.name);
                    if key != n.name && self.owns_local(&n.name) && self.table.fns.contains_key(&key) {
                        self.record_call_sym(id, key);
                    }
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
                // A `distinct` type inherits its base's operators AT ITS OWN TYPE,
                // and only with itself: `Id + Id` is an `Id`, while `Id + i64`,
                // `Id + Acct` and `i64 + Id` are all refused. Consulted first
                // because it is the arm that must see BOTH operands — HEAD's
                // operator-trait rule looks only at the left one, which is how
                // `0 + a + b` and `n + a` mixed id spaces silently. Returns `None`
                // when no operand is a distinct, leaving the path below untouched.
                if let Some(t) = self.binary_distinct_rule(id, *op, &lt, &rt, span) {
                    t
                }
                // Operator traits (Stage E): `a OP b` on a user type dispatches
                // through `impl <OpTrait> for <lhs>`; primitives fall through to
                // native semantics below.
                else if let Some(t) = self.resolve_operator_trait(id, *op, &lt, span) {
                    t
                } else {
                    use BinOp::*;
                    match op {
                        Eq | Ne | Lt | Le | Gt | Ge | And | Or => Ty::Prim("bool"),
                        _ => {
                            // `cptr` is an OPAQUE handle: `f + 1` has no meaning,
                            // and C's own `void*` arithmetic is a GNU extension
                            // rather than standard. Without this guard the
                            // expression took the *other* operand's numeric type,
                            // so `(f + 1).*` type-checked as an `i32` deref and
                            // sailed through to gcc — degrades-to-gcc on a
                            // brand-new feature. Comparisons are unaffected: they
                            // are handled by the arm above, so `f == null` works.
                            if matches!(lt, Ty::Prim("cptr")) || matches!(rt, Ty::Prim("cptr")) {
                                self.error(
                                    span,
                                    "`cptr` is an opaque handle — arithmetic on it has no meaning; cast it to a typed pointer first".to_string(),
                                );
                                Ty::Error
                            } else if is_numeric(&lt) {
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
            ExprKind::Assign { op, target, value } => {
                let tt = self.infer(scope, typ, self_ty, *target);
                let vt = self.infer(scope, typ, self_ty, *value);
                // An assignment STATEMENT is an assignment position like any other.
                // It was the one HEAD left unchecked, so `a = b` with two unrelated
                // id spaces ran end to end while the byte-identical `let a: Id = b`
                // was refused — a rule that holds at three positions and not the
                // fourth is not a rule. Diagnostic only: the recorded type is still
                // `Unit`, so nothing downstream moves.
                if self.distinct_mismatch(&tt, &vt) {
                    let (w, g) = (tt.display(&self.table), vt.display(&self.table));
                    self.error(
                        span,
                        format!(
                            "assignment: expected `{w}`, found `{g}` — `distinct` types need an explicit `as`"
                        ),
                    );
                } else if let Some(bin) = Self::assign_op_binop(*op) {
                    // `a += b` is `a = a + b`, so it owes the operator half of the
                    // rule too: a compound assignment on a distinct whose base has
                    // no such operator is the same refusal as writing it out.
                    if self.is_distinct(&tt) {
                        let base = self.peel_distinct(&tt);
                        if self.distinct_op_result(bin, &tt, &base).is_none() {
                            let (d, b) = (tt.display(&self.table), base.display(&self.table));
                            self.error(
                                span,
                                format!(
                                    "type `{d}` has no `{}` operator — its base `{b}` has none either",
                                    op_symbol(bin)
                                ),
                            );
                        }
                    }
                }
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
                // `@name(…)` — a compile-time reflection query (roadmap G tier 3).
                // Handled first, and without inferring the arguments: the first one is a
                // *type*, not a value, so running inference over it would report a
                // non-existent binding. Typed as the literal it folds to, and checked
                // here because a reflection call becomes that literal in the emitted C —
                // there is no runtime fallback to degrade to.
                if let ExprKind::Attr(a) = &ast.expr_at(*callee).kind {
                    if let Some(t) = reflect_intrinsic_ret(&a.name) {
                        self.check_reflect_call(id);
                        self.expr_types[id.0 as usize] = t.clone();
                        return t;
                    }
                    // `@size_of(T)` / `@align_of(T)` / `@offset_of(T, f)` — the same
                    // shape and for the same reason: the first argument is a type, so
                    // the arguments must not be inferred, and the query becomes a
                    // literal in the emitted C with no runtime fallback to degrade to.
                    if comptime::is_layout_intrinsic(&a.name) {
                        self.check_reflect_call(id);
                        let t = Ty::Prim("i64");
                        self.expr_types[id.0 as usize] = t.clone();
                        return t;
                    }
                }
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
                    // `d.m(args)` where `d: dyn Trait` — a *dynamic* dispatch through
                    // the vtable (traits, Stage F).
                    if let Some(ret) = self.resolve_dyn_method(id, &name, &recv_ty) {
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
                    .filter(|n| self.owns_local(n))
                    .and_then(|n| self.table.fns.get(&self.canon_cur(n)))
                    .map(|sig| sig.params.iter().map(|p| p.ty.clone()).collect())
                    .unwrap_or_default();
                for (i, a) in args.iter().enumerate() {
                    let prev = self.cur_expected.take();
                    self.cur_expected = param_tys.get(i).cloned();
                    self.infer(scope, typ, self_ty, *a);
                    self.cur_expected = prev;
                    // A concrete argument passed where a `dyn Trait` is expected
                    // coerces into a fat pointer (Stage F).
                    if let Some(pt) = param_tys.get(i) {
                        self.check_dyn_coercion(*a, pt);
                    }
                }
                if let Some(name) = callee_name {
                    // Definition-site bounds (Stage D): a bracket-generic call
                    // `f[T: Tr](…)` must instantiate `T` at a type that `impl`s
                    // `Tr` — checked here, where `T` is concrete (the args carry it).
                    let arg_tys: Vec<Ty> =
                        args.iter().map(|a| self.expr_types[a.0 as usize].clone()).collect();
                    self.check_call_bounds(&name, &arg_tys, span);
                    // Namespace isolation: an unqualified name resolves *only*
                    // against the current module's own items. The canonical key
                    // picks this module's definition of a possibly-shared name,
                    // and (when that differs from the bare name) is handed to the
                    // backend so the call targets the right C symbol.
                    let key = self.canon_cur(&name);
                    let resolved = if self.owns_local(&name) {
                        self.table.fns.get(&key).map(|sig| {
                            let ptys: Vec<(String, Ty)> =
                                sig.params.iter().map(|p| (p.name.clone(), p.ty.clone())).collect();
                            (sig.ret.clone(), sig.errs.clone(), ptys, sig.must_use)
                        })
                    } else {
                        None
                    };
                    if let Some((ret, errs, ptys, must_use)) = resolved {
                        self.record_must_use(id, must_use);
                        let want = ptys.len();
                        if key != name {
                            self.record_call_sym(id, key.clone());
                        }
                        if want != args.len() {
                            self.error(
                                span,
                                format!("`{name}` expects {want} argument(s), found {}", args.len()),
                            );
                        }
                        // Argument-vs-parameter types. Only when the arity is
                        // right — otherwise the positions do not correspond and
                        // every pair would be spurious noise on top of the real
                        // (arity) error.
                        if want == args.len() {
                            for ((pname, pty), a) in ptys.iter().zip(args.iter()) {
                                let got = self.expr_types[a.0 as usize].clone();
                                let sp = self.ast.expr_at(*a).span;
                                self.check_assignable(
                                    pty,
                                    &got,
                                    Some(*a),
                                    sp,
                                    &format!("argument `{pname}` of `{name}`"),
                                );
                            }
                        }
                        // For a generic call, resolve type parameters in the return.
                        let ret = self.monomorphize_ret(&key, args, typ, ret);
                        // A fallible call yields `T !E`; `?` later unwraps it. The
                        // declared set rides the type, so `?` on a stored result
                        // (`let r = f() … r?`) still knows what it propagates.
                        match errs {
                            Some(e) => Ty::Result(Box::new(ret), e),
                            None => ret,
                        }
                    } else if let Some(&ei) = self.table.variants.get(&self.canon_variant_in(self.cur_mod, &name)) {
                        // An enum-variant constructor, e.g. `circle(2.0)`. For a
                        // generic enum, recover its type arguments from the args.
                        let arg_tys: Vec<Ty> =
                            args.iter().map(|a| self.expr_types[a.0 as usize].clone()).collect();
                        self.variant_ctor_type(ei, &name, &arg_tys)
                    } else if name == "unwrap" {
                        // `unwrap(r: T !E) -> T` — the ok type of the result argument.
                        match args.first().map(|a| self.expr_types[a.0 as usize].clone()) {
                            Some(Ty::Result(ok, _)) => *ok,
                            _ => Ty::Unknown,
                        }
                    } else if name == "err" {
                        // The error constructor — reached only when no user fn or
                        // enum variant named `err` shadowed it above (the corpus's
                        // own `Result(T, E) { ok, err }` does). Two spellings:
                        // `err(Name)` for a bare error, `err(Name(v))` for a
                        // payload carrier (error-payloads E3). Checked here:
                        // membership in the ENCLOSING declared set (E2), and that
                        // the spelling matches the name's declaration — a payload
                        // name must be applied, a bare name must not be, and the
                        // payload value must fit the declared type. The recorded
                        // type stays `Unknown`, exactly as before: this arm adds
                        // diagnostics, never a type, so the P3 golden cannot move.
                        let head = match args.first().map(|a| &ast.expr_at(*a).kind) {
                            Some(ExprKind::Name(n)) => Some((n.name.clone(), None)),
                            Some(ExprKind::Call { callee, args: iargs }) => {
                                match &ast.expr_at(*callee).kind {
                                    ExprKind::Name(n) => {
                                        Some((n.name.clone(), Some(iargs.clone())))
                                    }
                                    _ => None,
                                }
                            }
                            _ => None,
                        };
                        if let Some((ename, payload_args)) = head {
                            if let Some(errs) = self.cur_errs.clone() {
                                if !errs.contains(&ename) {
                                    self.error(
                                        span,
                                        format!(
                                            "`err({ename})` — `{ename}` is not in the enclosing declared error set {{ {} }}",
                                            errs.join(", ")
                                        ),
                                    );
                                }
                            }
                            let declared = self.err_payloads.get(&ename).cloned();
                            match (&declared, &payload_args) {
                                (Some(want), None) => self.error(
                                    span,
                                    format!(
                                        "error `{ename}` carries a payload of type `{}` — construct it with `err({ename}(…))`",
                                        want.display(&self.table)
                                    ),
                                ),
                                (None, Some(_)) => self.error(
                                    span,
                                    format!(
                                        "error `{ename}` carries no payload — write `err({ename})`"
                                    ),
                                ),
                                (Some(want), Some(ia)) => {
                                    if ia.len() != 1 {
                                        self.error(
                                            span,
                                            format!(
                                                "error `{ename}` carries exactly one payload value, found {}",
                                                ia.len()
                                            ),
                                        );
                                    } else {
                                        let got = self.expr_types[ia[0].0 as usize].clone();
                                        let sp = ast.expr_at(ia[0]).span;
                                        self.check_assignable(
                                            want,
                                            &got,
                                            Some(ia[0]),
                                            sp,
                                            &format!("the payload of error `{ename}`"),
                                        );
                                    }
                                }
                                (None, None) => {}
                            }
                        }
                        Ty::Unknown
                    } else if name == "is_err" {
                        Ty::Prim("bool")
                    } else if let Some(t) = string_intrinsic_ret(&name) {
                        // String intrinsics aren't declared functions; type their
                        // results so a `let` (without an annotation) gets the right C type.
                        t
                    } else if let Some(t) = io_intrinsic_ret(&name) {
                        // File-I/O intrinsics, same deal: `read_file -> String`,
                        // `write_file`/`file_exists -> bool`.
                        t
                    } else if let Some(t) = atomic_intrinsic_ret(&name) {
                        // Atomics yield `i64` (a seq-cst op on an `int64` cell), so a
                        // `let`/comparison like `atomic_xchg(lock,1) != 0` in the
                        // spinlock types without an explicit annotation or cast.
                        t
                    } else if name == "slice" && !args.is_empty() {
                        // `slice(T, buf, n) -> []T` (B5): the element type is the first
                        // (type) argument. Typing it here means the builder works
                        // *unannotated* in argument position — e.g. straight into
                        // `from_utf8(slice(u8, buf, n))` — instead of mis-inferring its
                        // temp as `int` and only working bound to an annotated `let`.
                        Ty::Slice(Box::new(self.eval_type_expr(typ, args[0])))
                    } else {
                        // Not local, not a variant/intrinsic: if some *other*
                        // module defines it, this is the v1 namespace leak that is
                        // now an error — the name must be reached qualified
                        // (`mod.name`). Otherwise it stays an opaque/builtin name.
                        self.report_cross_module_name(&name, span);
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
                let bt0 = self.infer(scope, typ, self_ty, *base);
                self.infer(scope, typ, self_ty, *index);
                // Indexing a slice yields its element type; a string yields a byte,
                // *except* `s[i..j]` (a range index) which slices a sub-view — of the
                // same slice type for `[]T`, and of `str` for a string.
                //
                // A `distinct` inherits the whole of that from its base, under the
                // substitution rule: the base's signature with the base replaced by
                // the distinct. So `p[i]` on a `distinct P = str` is a `u8` (no
                // occurrence of `str` in the result, nothing to substitute) while
                // `p[a..b]` is a **`P`** — which is exactly what makes the internal
                // helpers of a `distinct Path = str` need no casts at all. Peeling
                // is a no-op on every non-distinct type, so nothing else moves.
                let sub = self.distinct_root(&bt0);
                let bt = self.peel_distinct(&bt0);
                let ranged = matches!(ast.expr_at(*index).kind, ExprKind::Range { .. });
                match bt {
                    // `xs[i..j]` on a `[]T` re-slices: same element type, narrower view.
                    // Deliberately NOT extended to a fixed-size array, whose sub-view
                    // would have to borrow the array's storage — that needs the
                    // borrowed-projection story (safety-mosaic item 2), not just a type.
                    Ty::Slice(elem) if ranged => sub.unwrap_or(Ty::Slice(elem)),
                    Ty::Slice(elem) => *elem,
                    Ty::Array { elem, .. } => *elem, // a fixed-size array indexes to its element
                    Ty::Prim("str") => {
                        if ranged {
                            sub.unwrap_or(Ty::Prim("str"))
                        } else {
                            Ty::Prim("u8")
                        }
                    }
                    _ => Ty::Unknown,
                }
            }
            ExprKind::ArrayRepeat { value, count } => {
                // If an array type is expected (`var xs: [N]i64 = [0; N]`), adopt its
                // element type so an `i32`-by-default literal element (`0`) takes the
                // annotated element type — otherwise the literal would be `[N]i32`,
                // a *different* value-struct than the `[N]i64` binding.
                let exp_elem = match &self.cur_expected {
                    Some(Ty::Array { elem, .. }) => Some((**elem).clone()),
                    _ => None,
                };
                let prev = self.cur_expected.take();
                self.cur_expected = exp_elem.clone();
                let inferred = self.infer(scope, typ, self_ty, *value);
                self.cur_expected = prev;
                let elem = exp_elem.unwrap_or(inferred);
                self.check_array_len(*count);
                Ty::Array { elem: Box::new(elem), len: self.eval_array_len(*count) }
            }
            ExprKind::ArrayLit { elems } => {
                // Adopt an expected element type (`var t: [N]u64 = [a, b, …]`) so an
                // `i32`-by-default literal element takes the annotated element type.
                let exp_elem = match &self.cur_expected {
                    Some(Ty::Array { elem, .. }) => Some((**elem).clone()),
                    _ => None,
                };
                let mut elem_ty = exp_elem.clone();
                for e in elems {
                    let prev = self.cur_expected.take();
                    self.cur_expected = exp_elem.clone();
                    let t = self.infer(scope, typ, self_ty, *e);
                    self.cur_expected = prev;
                    // Each element is an assignment position against the annotated
                    // element type: `let ids: [3]Id = [1, 2, 3]` is three `let x: Id
                    // = 1`s, and was accepted only because nothing looked.
                    if let Some(exp) = &exp_elem {
                        if self.distinct_mismatch(exp, &t) {
                            let (w, g) = (exp.display(&self.table), t.display(&self.table));
                            self.error(
                                self.ast.expr_at(*e).span,
                                format!(
                                    "array element: expected `{w}`, found `{g}` — `distinct` types need an explicit `as`"
                                ),
                            );
                        }
                    }
                    if elem_ty.is_none() {
                        elem_ty = Some(t);
                    }
                }
                let elem = elem_ty.unwrap_or(Ty::Unknown);
                Ty::Array { elem: Box::new(elem), len: elems.len() }
            }
            ExprKind::Deref { base } => {
                let bt = self.infer(scope, typ, self_ty, *base);
                match bt {
                    Ty::Ptr { inner, .. } => *inner,
                    Ty::GenRef(elem) => *elem,    // `r.*` on a generational reference
                    Ty::RegionRef(elem) => *elem, // `r.*` on a region reference
                    // `cptr` is opaque: there is nothing on the other end Jestyr has
                    // a type for, and `*(void*)` is not valid C either. A silent
                    // `Unknown` here would have let it reach gcc.
                    Ty::Prim("cptr") => {
                        self.error(
                            span,
                            "`cptr` is an opaque handle and cannot be dereferenced — cast it to a typed pointer (`as *mut u8`) if you really own the memory".to_string(),
                        );
                        Ty::Error
                    }
                    _ => Ty::Unknown,
                }
            }
            ExprKind::Cast { expr, ty } => {
                self.infer(scope, typ, self_ty, *expr);
                self.lower_type(typ, *ty) // the cast's type is its target type
            }
            ExprKind::Try { base } => {
                // `e?` unwraps a `T !E` to its ok type `T` — and propagates `E`,
                // so the callee's set must be included in the enclosing declared
                // set (error-payloads E2; the set rides the Result type, so a
                // stored result propagates its ORIGIN's set). Diagnostic only:
                // the recorded type is the ok type, exactly as before.
                let bt = self.infer(scope, typ, self_ty, *base);
                match bt {
                    Ty::Result(ok, errs) => {
                        self.check_propagation(&errs, span);
                        *ok
                    }
                    _ => Ty::Unknown,
                }
            }
            ExprKind::Catch { base, binder, fallback, rethrow } => {
                // `e catch v` recovers: it unwraps `T !E` to `T`, using `v` when the
                // error path is taken. Unlike `?` it does **not** need a fallible
                // enclosing function — recovering is precisely how a fallible call is
                // made infallible.
                // No early `return` anywhere in this arm: `infer` records the computed
                // type via `set` at its tail, and a `return` skips that — leaving the
                // Catch node's recorded type `Unknown`, which the P3 typeck golden
                // caught as a divergence from the port (which records faithfully).
                let bt = self.infer(scope, typ, self_ty, *base);
                // The base's static set, kept for the `match e` extractor below
                // (empty when the base is not a Result — that path errors anyway).
                let base_errs = match &bt {
                    Ty::Result(_, errs) => errs.clone(),
                    _ => Vec::new(),
                };
                // The rethrow form (`catch |e| return e`) is `?` spelled out, so
                // it owes exactly `?`'s inclusion obligation; a recovering
                // `catch` CONSUMES the error and owes nothing.
                if *rethrow {
                    if let Ty::Result(_, errs) = &bt {
                        self.check_propagation(errs, span);
                    }
                }
                let ok = match bt {
                    Ty::Result(ok, _) => Some(*ok),
                    // Not fallible: nothing to recover from. Reported rather than
                    // silently accepted, because `catch` on an infallible expression
                    // reads as a guarantee that an error was handled.
                    other => {
                        if !matches!(other, Ty::Unknown | Ty::Error) {
                            self.error(
                                self.ast.expr_at(*base).span,
                                format!(
                                    "`catch` needs a fallible expression, but this has type `{}`",
                                    other.display(&self.table)
                                ),
                            );
                        }
                        None
                    }
                };
                let Some(ok) = ok else {
                    // **The degraded path still binds `e`.** `catch |e| …` binds the
                    // binder — that is what the syntax says — and whether the base's type
                    // could be RECOVERED has nothing to do with whether the name exists.
                    // Inferring the fallback without it left `e` an unknown name typed
                    // `?`, which cascades: a `match e { … }` extractor under an
                    // unresolvable base reports a second, invented problem on top of the
                    // real one.
                    //
                    // It is also where the reference and the port disagreed. `jestyr_
                    // typeck_dump_matches_reference` runs over the WHOLE corpus with no
                    // allowlist, and `examples/std/sysfs_test.jtr` is the first file to
                    // put a `catch |e| match e { … }` on a fallible call into another
                    // module: with imports unresolved the base degrades, the reference
                    // typed `e` as `?` and the port typed it `error`. The port's answer is
                    // the better one and this adopts it, so the two agree by fixing the
                    // behaviour rather than by excluding the file.
                    //
                    // Scoped and popped exactly as the recovered path does, so the binder
                    // cannot leak past the fallback.
                    scope.push(HashMap::new());
                    if let Some(b) = binder {
                        scope.last_mut().unwrap().insert(b.name.clone(), Ty::Prim("error"));
                    }
                    self.infer(scope, typ, self_ty, *fallback);
                    scope.pop();
                    return self.set(id, Ty::Error);
                };
                // `catch |e| …`: the binder carries the opaque `error` type, in scope
                // for the FALLBACK alone — a pushed-and-popped scope, exactly a `let`
                // inside a block. Opaque on purpose: the runtime value is an integer
                // tag, but typing it `i32` would let `return e` in an `i32`-returning
                // fallible fn silently return the tag as a SUCCESS value.
                scope.push(HashMap::new());
                if let Some(b) = binder {
                    scope.last_mut().unwrap().insert(b.name.clone(), Ty::Prim("error"));
                }
                // The rethrow form (`catch |e| return e`) yields the ok value on the
                // success path and *returns* on the error path, so the fallback (the
                // binder name) is inferred only to record its type.
                if *rethrow {
                    self.infer(scope, typ, self_ty, *fallback);
                    scope.pop();
                    return self.set(id, ok);
                }
                // `catch |e| match e { … }` — THE payload extractor (error-payloads
                // E4, `docs/error-payloads.md` §5). The match must be the immediate
                // fallback over the binder itself: that is what puts the base's
                // STATIC set (E2 carries it on `Ty::Result`) in hand for
                // exhaustiveness, with no set-through-the-binder-type plumbing —
                // the binder stays the opaque `error` everywhere else.
                if let Some(b) = binder {
                    if let ExprKind::Match { scrut, arms } = &self.ast.expr_at(*fallback).kind {
                        if matches!(&self.ast.expr_at(*scrut).kind,
                                    ExprKind::Name(n) if n.name == b.name)
                        {
                            let errs = base_errs.clone();
                            self.check_err_match(scope, typ, self_ty, &errs, &ok, *scrut, arms);
                            self.set(*fallback, ok.clone());
                            scope.pop();
                            return self.set(id, ok);
                        }
                    }
                }
                // The fallback is inferred **against the ok type** (the `cur_expected`
                // idiom every other expected-type site uses), so a literal fallback
                // picks up the right width and a struct/closure literal gets its
                // expected type — the same courtesy a `let` annotation gives.
                let prev = self.cur_expected.take();
                self.cur_expected = Some(ok.clone());
                let ft = self.infer(scope, typ, self_ty, *fallback);
                self.cur_expected = prev;
                scope.pop();
                // The binder is OPAQUE: recovering with the raw tag (`catch |e| e`)
                // would silently turn an error code into a success value — the exact
                // confusion the `error` type exists to prevent. An explicit cast
                // remains the escape hatch, as it is for `distinct`.
                if matches!(ft, Ty::Prim("error")) && !matches!(ok, Ty::Prim("error")) {
                    self.error(
                        self.ast.expr_at(*fallback).span,
                        format!(
                            "the error binder cannot recover as a value of type `{}` — \
                             it is an error, not a result; cast explicitly (`e as i64`) if you mean the tag",
                            ok.display(&self.table)
                        ),
                    );
                }
                // The one mismatch class this checker reports, applied here too: a
                // `distinct` type is not interchangeable with its base, so recovering
                // a `UserId` with a bare `u64` needs an explicit `as`.
                if self.distinct_mismatch(&ok, &ft) {
                    self.error(
                        self.ast.expr_at(*fallback).span,
                        format!(
                            "expected `{}`, found `{}` — `distinct` types need an explicit `as`",
                            ok.display(&self.table),
                            ft.display(&self.table)
                        ),
                    );
                }
                ok
            }
            ExprKind::StructLit { path, fields, spread } => {
                // For a plain named struct, resolve its index *first*, so each field
                // value is inferred against its **declared field type** as the
                // expected type. That is what lets a fn-pointer-typed field accept a
                // coercing closure literal — `Allocator{ alloc_fn: |n| … }`.
                let named_idx = if path.name != "Self"
                    && !self.table.variants.contains_key(&self.canon_variant_in(self.cur_mod, &path.name))
                {
                    self.table.type_index.get(&self.canon_type_cur(&path.name)).copied()
                } else {
                    None
                };
                for fi in fields {
                    let expected =
                        named_idx.and_then(|i| self.struct_field_decl_ty(i, &fi.name.name));
                    let prev = self.cur_expected.take();
                    self.cur_expected = expected.clone();
                    let vt = self.infer(scope, typ, self_ty, fi.value);
                    self.cur_expected = prev;
                    // A struct-literal field is an assignment position: `Rec { id: 5 }`
                    // on a `distinct`-typed field is the same mistake as `let x: Id = 5`,
                    // which was already refused. `ExprKind::Int` types as `i32` flat
                    // regardless of the expected type, so the literal really is caught.
                    if let Some(exp) = &expected {
                        if self.distinct_mismatch(exp, &vt) {
                            let (w, g) = (exp.display(&self.table), vt.display(&self.table));
                            let fname = fi.name.name.clone();
                            self.error(
                                self.ast.expr_at(fi.value).span,
                                format!(
                                    "field `{fname}`: expected `{w}`, found `{g}` — `distinct` types need an explicit `as`"
                                ),
                            );
                        }
                    }
                }
                if let Some(s) = spread {
                    self.infer(scope, typ, self_ty, *s);
                }
                if path.name == "Self" {
                    self_ty.clone()
                } else if let Some(&ei) = self.table.variants.get(&self.canon_variant_in(self.cur_mod, &path.name)) {
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
                // The ctor canons in the fn namespace, so a colliding `Box(i32){..}`
                // infers ITS module's instance type and its field expectations
                // resolve through the right template (bare when nothing collides).
                let ckey = self.canon_cur(&ctor.name);
                for fi in fields {
                    let expected = self.gen_struct_field_decl_ty(&ckey, &args, &fi.name.name);
                    let prev = self.cur_expected.take();
                    self.cur_expected = expected;
                    self.infer(scope, typ, self_ty, fi.value);
                    self.cur_expected = prev;
                }
                Ty::GenStruct { ctor: ckey, args }
            }
            ExprKind::StructType(body) => {
                // Inside a comptime type-fn, a ctor-body method's `self` is the
                // REAL generic-struct type (`Box(T)`, `T` opaque) — so
                // `self.field` resolves through the template via
                // `gen_struct_field_decl_ty`, and the escape checker judges it by
                // its actual type instead of refusing an `Unknown`. An anonymous
                // `struct { … }` outside a type-fn keeps the `Self` placeholder.
                let struct_self = match &self.cur_type_fn {
                    Some((ctor, tps)) => Ty::GenStruct {
                        ctor: ctor.clone(),
                        args: tps.iter().map(|t| Ty::Opaque(t.clone())).collect(),
                    },
                    None => Ty::Opaque("Self".to_string()),
                };
                for m in &body.members {
                    if let StructMember::Method(f) = m {
                        self.check_fn(f, typ, &struct_self);
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
                    let et = self.infer(scope, typ, self_ty, *e);
                    // An `if` EXPRESSION has one type, and it is the then-branch's.
                    // Without this the else-branch could smuggle a foreign distinct
                    // through the only assignment position that never re-checked it:
                    // `let c: Id = if p { a } else { b }` type-checked against the
                    // then-branch alone, so the `Acct` in the else arm was invisible.
                    if self.distinct_mismatch(&t, &et) {
                        let (w, g) = (t.display(&self.table), et.display(&self.table));
                        self.error(
                            self.ast.expr_at(*e).span,
                            format!(
                                "`if` branches disagree: expected `{w}`, found `{g}` — `distinct` types need an explicit `as`"
                            ),
                        );
                    }
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
                // `spawn f(args)` yields a `Task(T)` where `T` is the target's return
                // type — the type of the inner call. As a bare statement the handle is
                // discarded (fire-and-forget); bound with `let h = …` it is awaitable.
                let t = self.infer(scope, typ, self_ty, *call);
                Ty::Task(Box::new(t))
            }
            ExprKind::Await(task) => {
                // `await h` joins the task handle and yields its result `T`.
                let t = self.infer(scope, typ, self_ty, *task);
                match t {
                    Ty::Task(inner) => *inner,
                    Ty::Unknown | Ty::Error => Ty::Unknown,
                    other => {
                        self.error(
                            span,
                            format!("`await` expects a task handle (`Task(T)` from `spawn`), found `{}`", other.display(&self.table)),
                        );
                        Ty::Unknown
                    }
                }
            }
            ExprKind::ParFor { var, iter, reduction, body } => {
                // The deterministic parallel reduction loop. The *reduction* is `i64`
                // (the declared deterministic operators are exactly associative on
                // machine integers, which is what makes any schedule give the same
                // bits), but the loop does not have to iterate `i64`.
                //
                // The engine never sees the source slice: `emit_par_for` materializes
                // an `i64` map buffer by running the body per element and reduces
                // *that*. So the element type is free, and widening it costs the
                // determinism argument nothing — the reduction domain is unchanged.
                // It buys real width for a later SIMD lowering, where a body over
                // `i32` fills twice the lanes of one over `i64` (and `u8`, eight
                // times), so this is workstream Q's prerequisite as much as an
                // ergonomic one.
                let elem = self.iter_elem_type(scope, typ, self_ty, *iter);
                let elem_ok = match &elem {
                    Ty::Prim(p) => is_integer_prim(p),
                    Ty::Unknown | Ty::Error => true,
                    _ => false,
                };
                if !elem_ok {
                    self.error(
                        span,
                        format!(
                            "`par for` reduces over a slice of any integer type; found element type `{}`",
                            elem.display(&self.table)
                        ),
                    );
                }
                // The loop variable has the element's OWN type, so the body computes in
                // that width rather than in a silently-widened one.
                let bind_ty = if elem_ok { elem.clone() } else { Ty::Prim("i64") };
                scope.push(HashMap::new());
                scope.last_mut().unwrap().insert(var.name.clone(), bind_ty);
                let body_ty = self.infer(scope, typ, self_ty, *body);
                scope.pop();
                // Any integer contribution is accepted and widened to `i64` once per
                // element. A non-integer is refused rather than coerced: the reduction
                // is defined on integers, and inventing a conversion is what this
                // compiler does not do.
                let body_ok = match &body_ty {
                    Ty::Prim(p) => is_integer_prim(p),
                    Ty::Unknown | Ty::Error => true,
                    _ => false,
                };
                if !body_ok {
                    self.error(
                        span,
                        format!(
                            "a `par for` body must produce an integer (the per-element contribution, widened to `i64` for the reduction); found `{}`",
                            body_ty.display(&self.table)
                        ),
                    );
                }
                // Infer the reduction (records its resolution), then enforce THE checked
                // guarantee: it must be a declared deterministic reduction. A
                // non-deterministic one would reassociate under parallelism — a compile
                // error, the property `par for` exists to make impossible.
                let _ = self.infer(scope, typ, self_ty, *reduction);
                match self.reduction_ctor_name(*reduction) {
                    Some(n) if DETERMINISTIC_REDUCTIONS.contains(&n.as_str()) => {}
                    other => {
                        let shown = other.unwrap_or_else(|| "this reduction".to_string());
                        self.error(
                            span,
                            format!(
                                "`par for` requires a declared deterministic reduction \
                                 (one of: {}); `{}` is not one. A non-deterministic reduction \
                                 (e.g. a naive float `+`, which reassociates under parallelism) \
                                 would make the result depend on the thread schedule — exactly \
                                 what `par for` exists to prevent.",
                                DETERMINISTIC_REDUCTIONS.join(", "),
                                shown
                            ),
                        );
                    }
                }
                Ty::Prim("i64")
            }
            ExprKind::Select(arms) => {
                // Each arm waits on a `Channel(i64)` and binds the received `i64`.
                for arm in arms {
                    let cht = self.infer(scope, typ, self_ty, arm.chan);
                    let ok = matches!(&cht,
                        Ty::GenStruct { ctor, args } if ctor == "Channel"
                            && matches!(args.as_slice(), [Ty::Prim("i64")]));
                    if !ok && !matches!(cht, Ty::Unknown | Ty::Error) {
                        self.error(
                            self.ast.expr_at(arm.chan).span,
                            format!(
                                "a `select` arm waits on a `Channel(i64)`; found `{}`",
                                cht.display(&self.table)
                            ),
                        );
                    }
                    scope.push(HashMap::new());
                    scope.last_mut().unwrap().insert(arm.bind.name.clone(), Ty::Prim("i64"));
                    self.infer_block(scope, typ, self_ty, &arm.body);
                    scope.pop();
                }
                Ty::Unit
            }
            ExprKind::Region { body, .. } => {
                self.infer_block(scope, typ, self_ty, body);
                Ty::Unit
            }
            ExprKind::WithAlive { genref, name, body, els } => {
                // The scrutinee must be a generational reference `&T`; the block
                // binds `name : T` (a second-class `read` borrow of the referent,
                // enforced by the escape checker) for the body's extent.
                let gt = self.infer(scope, typ, self_ty, *genref);
                let inner = match &gt {
                    Ty::GenRef(inner) => (**inner).clone(),
                    Ty::Unknown | Ty::Error => Ty::Unknown,
                    other => {
                        let shown = other.display(&self.table);
                        self.error(
                            self.ast.expr_at(*genref).span,
                            format!(
                                "`with alive` takes a generational reference `&T`, found `{shown}` —                                  only a genref carries the generation this block checks"
                            ),
                        );
                        Ty::Unknown
                    }
                };
                scope.push(HashMap::new());
                scope.last_mut().unwrap().insert(name.name.clone(), inner);
                self.infer_block(scope, typ, self_ty, body);
                scope.pop();
                if let Some(e) = els {
                    self.infer_block(scope, typ, self_ty, e);
                }
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
                Ty::Array { elem, .. } => *elem, // iterating a fixed-size array yields its element
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
        if let Ty::Array { len, .. } = base {
            // A fixed-size array's `.len` is its constant length (an O(1) `usize`).
            if fname == "len" {
                let _ = len;
                return Ty::Prim("usize");
            }
            return Ty::Unknown;
        }
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
        // The `Unknown` finalization's follow-up (safety mosaic item 1): the two
        // shapes the escape-side gate caught — a field on a primitive, a field on a
        // bracket type parameter — are rejected HERE, at the access, with a
        // field-shaped message. The expression types `Error` (already-diagnosed),
        // not `Unknown` (never-resolved), so the finalization gate stays silent for
        // them and remains the backstop for shapes typeck cannot yet name.
        if let Ty::Prim(_) = base {
            // `str`/`String`/slices/arrays expose their documented fields above;
            // every other field on a primitive is ill-formed.
            let shown = base.display(&self.table);
            self.error(span, format!("no field `{fname}` on `{shown}` — a primitive has no fields"));
            return Ty::Error;
        }
        if let Ty::Opaque(tp) = base {
            // Only for the *enclosing fn's own* bracket parameters: a bound provides
            // methods, never fields (the same map `resolve_bound_method` consults).
            // A comptime-`T` template is not gated here — its instances re-infer
            // with the concrete type and take the real field check above.
            if self.cur_type_param_bounds.contains_key(tp) {
                self.error(
                    span,
                    format!(
                        "no field `{fname}` on type parameter `{tp}` — a bound provides methods, not fields; field access needs a concrete type"
                    ),
                );
                return Ty::Error;
            }
        }
        if matches!(base, Ty::Error) {
            return Ty::Error; // already diagnosed — a chained access must not cascade
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
            // A `distinct` inherits its base's MEMBERS: `p.len` on a `distinct P =
            // str` is the string's `usize` length, `w.x` on a `distinct W = Pt` is
            // `Pt`'s declared field. Recursing on the peeled base rather than
            // duplicating the arms above is what keeps the struct case honest —
            // the per-field visibility check below still runs, at the struct's own
            // module. (An enum keeps `Unknown`: its payloads project through
            // `match`, not through a field.)
            if matches!(self.table.types[*i].kind, TypeKindG::Distinct { .. }) {
                let peeled = self.peel_distinct(base);
                return self.field_type(span, &peeled, fname);
            }
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
                        if let Some(owner) = self.defining_module(&sname) {
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
                if !self.table.variants.contains_key(&self.canon_variant_in(self.cur_mod, &n.name)) {
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
            PatKind::StructVariant { name, fields, .. } => {
                // Bind each named field to its *declared* type, by looking its
                // position up in `variant_field_names` and indexing the positional
                // types `Variant` already uses. `variant_field_types_in` projects a
                // generic instance's arguments, so `some { v }` on an `Option(i32)`
                // binds `v: i32` exactly as the positional `some(v)` does.
                //
                // These used to bind `Ty::Unknown`, on the grounds that the table
                // carries no field names and cgen resolves the field itself. That
                // was unsound, not merely lenient: `Unknown` is `Copy`, so
                // `escapes_as` treated a *borrowed* field bound this way as a copy
                // and let it escape the frame. The positional form rejected the
                // identical program. See
                // `a_named_variant_binding_cannot_escape_its_borrow`.
                let ftys = self.variant_field_types_in(scrut, &name.name);
                let fnames = self
                    .table
                    .variants
                    .get(&self.canon_variant_in(self.cur_mod, &name.name))
                    .and_then(|&ei| self.variant_field_names.get(&(ei, name.name.clone())))
                    .cloned()
                    .unwrap_or_default();
                for (fname, sp) in fields {
                    // An unknown field name stays lenient: the name error is
                    // reported elsewhere, and guessing a type here would cascade.
                    let fty = fnames
                        .iter()
                        .position(|n| *n == fname.name)
                        .and_then(|i| ftys.get(i).cloned())
                        .unwrap_or(Ty::Unknown);
                    self.bind_pattern_types(scope, &fty, *sp);
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
        if let Some(&ei) = self.table.variants.get(&self.canon_variant_in(self.cur_mod, vname)) {
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
        let &ei = self.table.variants.get(&self.canon_variant_in(self.cur_mod, vname))?;
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
                if self.table.variants.contains_key(&self.canon_variant_in(self.cur_mod, &n.name)) {
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
                if !self.table.variants.contains_key(&self.canon_variant_in(self.cur_mod, &n.name)) {
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

/// The coarse family a primitive belongs to. Conversions *within* a family may
/// exist (integer widening, `str` → `String`); conversions *across* one never do
/// implicitly, so a cross-family pair is the only thing [`TypeChecker::assignable`]
/// is willing to call an error.
#[derive(PartialEq, Eq, Clone, Copy)]
enum PrimFamily {
    Numeric,
    Bool,
    Char,
    Text,
    /// `cptr` alone — the opaque FFI handle. It gets its OWN family rather than
    /// falling into `Text`, and that is the whole safety story: `Text` is the
    /// family whose members convert freely into one another (`prim_family(w) ==
    /// Text` returns `true` unconditionally in `assignable`, because the
    /// borrow/own conversions there are not modelled). A `cptr` landing in `Text`
    /// would have been silently interchangeable with `str`, `String` and `cstr` —
    /// the exact opposite of an opaque handle.
    Opaque,
}

fn prim_family(p: &str) -> PrimFamily {
    match p {
        _ if numeric_prim(p) => PrimFamily::Numeric,
        "bool" => PrimFamily::Bool,
        "char" => PrimFamily::Char,
        "cptr" => PrimFamily::Opaque,
        _ => PrimFamily::Text,
    }
}

/// The bit width of an integer primitive. `isize`/`usize` are pointer-width,
/// which this backend emits as 64 — the same assumption `size_of` already makes.
fn int_width(p: &str) -> u32 {
    match p {
        "i8" | "u8" => 8,
        "i16" | "u16" => 16,
        "i32" | "u32" => 32,
        _ => 64,
    }
}

/// Does `from` fit in `to` with neither truncation nor reinterpretation — that
/// is, same signedness and no narrowing?
///
/// Sign changes are excluded at every width, including equal ones: `i32 → u32`
/// loses nothing in bits but turns a negative value into a large positive one,
/// which is the reinterpretation the rule exists to catch.
fn lossless_widening(from: &str, to: &str) -> bool {
    from.starts_with('i') == to.starts_with('i') && int_width(to) >= int_width(from)
}

/// Is `p` one of the fixed-width integer primitives?
fn integer_prim(p: &str) -> bool {
    matches!(p, "i8" | "i16" | "i32" | "i64" | "isize" | "u8" | "u16" | "u32" | "u64" | "usize")
}

/// Is `p` an integer or floating-point primitive? (`bool`/`char` are scalars but
/// not numeric — they take no implicit numeric literal.)
fn numeric_prim(p: &str) -> bool {
    integer_prim(p) || matches!(p, "f32" | "f64")
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
        // The owned-String family, absent from this table until now — and its absence
        // was a MISCOMPILE, not a missing convenience.
        //
        // `string_view(s)` typed as `Unknown`, so cgen's field-access arm fell past its
        // `Ty::Prim("str")` case to the generic one and emitted `.j_len` — the Jestyr
        // field mangling — against `JestyrStr`, whose C field is `len`. gcc then failed
        // with "no member named 'j_len'". `let v: str = string_view(s)` followed by
        // `v.len` worked, because the annotation supplied the type the intrinsic did not,
        // which is exactly why this survived so long: every call site in the tree had
        // been written around it, and `docs/session-notes` records "never chain
        // `string_view(x).len`" as a .jtr subset TRAP rather than as the bug it is.
        "string_new" | "string_from" => Ty::Prim("String"),
        "string_view" => Ty::Prim("str"),
        "os_from_bytes" => Ty::Prim("os_str"),
        "to_str_lossy" => Ty::Prim("String"),
        "cow_borrow" | "cow_to_mut" => Ty::Prim("Cow"),
        "cow_view" => Ty::Prim("str"),
        "cow_is_owned" => Ty::Prim("bool"),
        // Recoverable: yields a Result so `is_err`/`unwrap`/`?` compose.
        // Its error is `Utf8Error` (the design's name), not `IoError` — E2 had
        // them conflated; corrected with the E4 wart fix, and safe because no
        // corpus code propagates it (`try_utf8.jtr` recovers via `is_err`).
        "try_from_utf8" => Ty::Result(Box::new(Ty::Prim("str")), vec!["Utf8Error".to_string()]),
        "count_codepoints" | "count_graphemes" => Ty::Prim("usize"),
        "find" => Ty::Prim("isize"),
        "is_utf8" | "str_eq" | "eq_fold" | "starts_with" | "ends_with" | "contains" => {
            Ty::Prim("bool")
        }
        _ => return None,
    })
}

/// The return type of a file-I/O intrinsic (not a declared function), so a `let`
/// bound to one gets the right C type without an annotation. `read_file` yields an
/// owned `String` (empty if the file can't be opened); `try_read_file` is the
/// recoverable form — `String !IoError` (a `Ty::Result`), so `?`/`unwrap`/`is_err`
/// compose; `write_file`/`file_exists` report success as a `bool`.
fn io_intrinsic_ret(name: &str) -> Option<Ty> {
    Some(match name {
        "read_file" => Ty::Prim("String"),
        // The tag-1 wart (docs/error-payloads.md §6) is RESOLVED at emission: an
        // intrinsic error construction emits the USER tag of its name when the
        // program declares it (`error_tags.get("IoError")`), falling back to the
        // historical literal 1 otherwise — so `match e { IoError => … }`
        // discriminates correctly, and every existing program (where the name is
        // undeclared, or declared first and therefore tag 1) is byte-identical.
        "try_read_file" => Ty::Result(Box::new(Ty::Prim("String")), vec!["IoError".to_string()]),
        "write_file" | "file_exists" | "remove_file" => Ty::Prim("bool"),
        // The self-hosted driver's plumbing: drive gcc, print diagnostics to stderr.
        "run_command" => Ty::Prim("i32"),
        "eprint_str" => Ty::Unit,
        "arg_count" => Ty::Prim("i32"),
        "arg" => Ty::Prim("str"),
        // `env_var(name) -> str` — a view into the C runtime's environment
        // block, empty when unset. A view (not a `String`) for the same reason
        // `arg` is one: the storage is OS-owned and outlives the call.
        "env_var" => Ty::Prim("str"),
        // `mono_nanos() -> i64` — a monotonic counter in nanoseconds. Only
        // DIFFERENCES are meaningful; the origin is unspecified.
        "mono_nanos" => Ty::Prim("i64"),
        _ => return None,
    })
}

/// The return type of an atomic intrinsic (a seq-cst op on an `int64` cell): all
/// yield `i64` — the prior/loaded value (`atomic_load`/`add`/`sub`/`xchg`).
/// `atomic_store` is statement-position only, so it needs no type. Registering
/// these lets the spinlock's `atomic_xchg(lock,1) != 0` test (and any `let`-bound
/// atomic result) type without an explicit annotation or `as` cast.
fn atomic_intrinsic_ret(name: &str) -> Option<Ty> {
    Some(match name {
        "atomic_load" | "atomic_add" | "atomic_sub" | "atomic_xchg" => Ty::Prim("i64"),
        _ => return None,
    })
}

/// The return type of a compile-time **reflection** intrinsic (roadmap G tier 3).
/// These always fold to a literal before C sees them, so their type is simply the
/// type of the literal they become.
fn reflect_intrinsic_ret(name: &str) -> Option<Ty> {
    Some(match name {
        "field_count" => Ty::Prim("i64"),
        "type_name" | "field_name" | "field_type" => Ty::Prim("str"),
        _ => return None,
    })
}

/// The declared *deterministic* reductions a `par for … reduce(r)` may use — the
/// `core` built-ins whose `combine` is associative *and* commutative at the
/// machine-integer level, so the parallel result is bit-identical to serial for any
/// schedule. Anything else (a custom or non-deterministic reduction) is rejected at
/// compile time — the checked guarantee. (A `@deterministic` attribute admitting
/// user-declared reductions is future work; today the trusted set is these.)
const DETERMINISTIC_REDUCTIONS: [&str; 4] =
    ["sum_reduction", "min_reduction", "max_reduction", "xor_reduction"];

/// Is `p` one of the integer primitives? The element and contribution types a
/// `par for` accepts — the reduction itself stays `i64`, so this is the set that
/// widens to it losslessly.
fn is_integer_prim(p: &str) -> bool {
    matches!(
        p,
        "i8" | "i16" | "i32" | "i64" | "isize" | "u8" | "u16" | "u32" | "u64" | "usize"
    )
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

/// If `ty` is a `dyn Trait` (lowered to `Ty::Opaque("dyn <Trait>")`), the trait
/// name. The single place that decodes the `dyn` representation, shared by the
/// coercion and dispatch paths (traits, Stage F).
fn dyn_trait_of(ty: &Ty) -> Option<&str> {
    match ty {
        Ty::Opaque(s) => s.strip_prefix("dyn "),
        _ => None,
    }
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
        // Completed for the `distinct` inheritance rule, which covers EVERY binary
        // operator rather than only the six that map to a trait: `a % b` and
        // `a << b` across two id spaces used to run, and their diagnostic would
        // otherwise have named the operator `?`.
        Rem => "%",
        And => "and",
        Or => "or",
        BitAnd => "&",
        BitOr => "|",
        BitXor => "^",
        Shl => "<<",
        Shr => ">>",
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
        (Ty::Result(o1, _), Ty::Result(o2, _)) => unify_tp(o1, o2, tps, subst),
        (Ty::Slice(e1), Ty::Slice(e2)) => unify_tp(e1, e2, tps, subst),
        (Ty::GenRef(e1), Ty::GenRef(e2)) => unify_tp(e1, e2, tps, subst),
        (Ty::RegionRef(e1), Ty::RegionRef(e2)) => unify_tp(e1, e2, tps, subst),
        _ => {}
    }
}

/// Parse an integer literal's source text to a `usize` (the `[N]T` array length):
/// decimal, `0x`/`0X` hex, or `0b`/`0B` binary, with `_` digit separators ignored.
fn parse_int_literal_usize(text: &str) -> Option<usize> {
    let t: String = text.chars().filter(|c| *c != '_').collect();
    if let Some(hex) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        usize::from_str_radix(hex, 16).ok()
    } else if let Some(bin) = t.strip_prefix("0b").or_else(|| t.strip_prefix("0B")) {
        usize::from_str_radix(bin, 2).ok()
    } else {
        t.parse::<usize>().ok()
    }
}

/// The v1 error-payload type domain: scalars and `str`. Owning types (`String`,
/// `Builder`, `Cow`) are out (they would owe a `drop` on every path an error can
/// die on); aggregates, pointers and references are out (union width, binder
/// shape, and provenance questions each deferred with the reason in the note).
fn err_payload_ty_allowed(t: &Ty) -> bool {
    matches!(
        t,
        Ty::Prim(
            "i8" | "i16" | "i32" | "i64" | "isize" | "u8" | "u16" | "u32" | "u64" | "usize"
                | "f32" | "f64" | "bool" | "char" | "str"
        )
    )
}

/// A declared error set's names — sorted and deduped, so equality between two
/// mentions of one callee's set is order-independent and the rendered
/// diagnostics are deterministic.
fn errs_of(es: &Option<crate::ast::ErrorSet>) -> Option<Vec<String>> {
    es.as_ref().map(|e| {
        let mut v: Vec<String> = e.names.iter().map(|n| n.name.name.clone()).collect();
        v.sort();
        v.dedup();
        v
    })
}

/// Substitute type parameters (`Ty::Opaque(name)`) throughout a type.
fn subst_ty(ty: &Ty, subst: &HashMap<String, Ty>) -> Ty {
    match ty {
        Ty::Opaque(n) => subst.get(n).cloned().unwrap_or_else(|| ty.clone()),
        Ty::Ptr { mutbl, inner } => Ty::Ptr { mutbl: *mutbl, inner: Box::new(subst_ty(inner, subst)) },
        Ty::Result(ok, errs) => Ty::Result(Box::new(subst_ty(ok, subst)), errs.clone()),
        Ty::GenStruct { ctor, args } => Ty::GenStruct {
            ctor: ctor.clone(),
            args: args.iter().map(|a| subst_ty(a, subst)).collect(),
        },
        Ty::GenEnum { ctor, args } => Ty::GenEnum {
            ctor: ctor.clone(),
            args: args.iter().map(|a| subst_ty(a, subst)).collect(),
        },
        Ty::Slice(elem) => Ty::Slice(Box::new(subst_ty(elem, subst))),
        Ty::Array { elem, len } => Ty::Array { elem: Box::new(subst_ty(elem, subst)), len: *len },
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

    // --- trait error sets (T1; the fallible-impl unlock) ---

    /// The conformance rules, each with its own message: trait-bare + impl-set
    /// (the original refusal, kept verbatim), trait-set + impl-bare, and an impl
    /// set exceeding the trait's. Same-set and subset impls are clean.
    #[test]
    fn impl_fallibility_must_conform_to_the_trait() {
        let base = "struct A { n: i32 } ";
        // Trait declares none, impl does — the original rule.
        let (_, d) = analyze(&format!(
            "{base}trait T {{ fn get(read self) -> i32 }} \
             impl T for A {{ fn get(read self) -> i32 !{{ Io }} {{ return err(Io) }} }}"
        ));
        assert!(
            d.iter().any(|x| x.message.contains("cannot be fallible")
                && x.message.contains("declares no error set")),
            "{d:?}"
        );
        // Trait declares a set, impl declares none.
        let (_, d) = analyze(&format!(
            "{base}trait T {{ fn get(read self) -> i32 !{{ Io }} }} \
             impl T for A {{ fn get(read self) -> i32 {{ return 1 }} }}"
        ));
        assert!(
            d.iter().any(|x| x.message.contains("must declare an error set")
                && x.message.contains("!{ Io }")),
            "{d:?}"
        );
        // The impl's set exceeds the trait's.
        let (_, d) = analyze(&format!(
            "{base}trait T {{ fn get(read self) -> i32 !{{ Io }} }} \
             impl T for A {{ fn get(read self) -> i32 !{{ Io, Parse }} {{ return err(Io) }} }}"
        ));
        assert!(
            d.iter().any(|x| x.message.contains("beyond trait `T`'s set")
                && x.message.contains("{ Parse }")),
            "{d:?}"
        );
        // A subset (and the same set) conform.
        let (_, d) = analyze(&format!(
            "{base}trait T {{ fn get(read self) -> i32 !{{ Io, Parse }} }} \
             impl T for A {{ fn get(read self) -> i32 !{{ Io }} {{ return err(Io) }} }}"
        ));
        assert!(d.is_empty(), "a subset impl conforms: {d:?}");
    }

    /// A call through the trait is typed by the TRAIT's set — so `?` inclusion
    /// (E2) applies: a caller declaring the set propagates, a narrower one is
    /// refused naming what the call can raise.
    #[test]
    fn a_trait_call_is_typed_by_the_traits_set() {
        let src = "struct A { n: i32 } \
                   trait Load { fn get(read self) -> i32 !{ Missing } } \
                   impl Load for A { fn get(read self) -> i32 !{ Missing } { \
                     if self.n < 0 { return err(Missing) } return ok(self.n) } } \
                   fn wide(read a: A) -> i32 !{ Missing } { let v = a.get()? return ok(v) } \
                   fn narrow(read a: A) -> i32 !{ Io } { let v = a.get()? return ok(v) }";
        let (_, d) = analyze(src);
        assert_eq!(d.len(), 1, "{d:?}");
        assert!(
            d[0].message.contains("propagates { Missing }"),
            "the narrow caller is refused with the set named: {:?}",
            d[0].message
        );
    }

    /// The two deferred forms are refused with their reasons: a default body on
    /// a fallible trait method, and a fallible method in a blanket impl.
    #[test]
    fn fallible_default_bodies_and_blanket_impls_are_deferred() {
        let (_, d) = analyze(
            "trait T { fn get(read self) -> i32 !{ Io } { return ok(1) } }",
        );
        assert!(
            d.iter().any(|x| x.message.contains("cannot have a default body")),
            "{d:?}"
        );
        let (_, d) = analyze(
            "fn Box(comptime T: type) -> type { return struct { v: T } } \
             trait T2 { fn get(read self) -> i32 !{ Io } } \
             impl[U] T2 for Box(U) { fn get(read self) -> i32 !{ Io } { return err(Io) } }",
        );
        assert!(
            d.iter().any(|x| x.message.contains("blanket `impl[…]` is not yet supported")),
            "{d:?}"
        );
    }

    /// Dyn dispatch of a fallible method is refused at the COERCION — the vtable
    /// machinery has not learned the result-struct ABI, and a refusal with the
    /// reason beats a wrong lowering.
    #[test]
    fn dyn_coercion_of_a_fallible_trait_is_refused() {
        let (_, d) = analyze(
            "struct A { n: i32 } \
             trait T { fn get(read self) -> i32 !{ Io } } \
             impl T for A { fn get(read self) -> i32 !{ Io } { return ok(self.n) } } \
             fn f(read a: A) -> i32 { let dt: dyn T = a return 0 }",
        );
        assert!(
            d.iter().any(|x| x.message.contains("fallible dynamic dispatch is not yet supported")),
            "{d:?}"
        );
    }

    // --- error payloads (E3; docs/error-payloads.md §3–§4) ---

    /// D1: a payload is a property of the NAME, whole-program. A conflicting
    /// declaration is reported at BOTH sites — two located diagnostics, since a
    /// single diagnostic cannot carry two spans.
    #[test]
    fn a_payload_conflict_is_reported_at_both_sites() {
        let (_, d) = analyze(
            "fn f(n: i64) -> i32 !{ Parse(i64) } { \
               if n > 9 { return err(Parse(n)) } return ok(1) } \
             fn g(n: i32) -> i32 !{ Parse } { return ok(1) }",
        );
        assert_eq!(d.len(), 2, "{d:?}");
        assert!(
            d.iter().any(|x| x.message.contains("declared with no payload here")
                && x.message.contains("with payload `i64`")),
            "the later site names the conflict: {d:?}"
        );
        assert!(
            d.iter().any(|x| x.message.contains("declared with payload `i64` here")),
            "the first site is named too: {d:?}"
        );
    }

    /// Agreement is about the TYPE, not the spelling — the same payload type
    /// restated at two sites is exactly what D1 asks for.
    #[test]
    fn restating_the_same_payload_type_is_agreement_not_conflict() {
        let (info, d) = analyze(
            "fn f(n: i64) -> i32 !{ TooBig(i64) } { \
               if n > 9 { return err(TooBig(n)) } return ok(1) } \
             fn g(n: i64) -> i32 !{ TooBig(i64) } { let v = f(n)? return ok(v) }",
        );
        assert!(d.is_empty(), "{d:?}");
        assert_eq!(info.err_payloads.len(), 1);
        assert_eq!(info.err_payloads.get("TooBig"), Some(&Ty::Prim("i64")));
    }

    /// The v1 domain: scalars and `str`. An owning `String` payload is refused
    /// with the reason (it would owe a `drop` on every path an error can die on).
    #[test]
    fn an_owning_payload_type_is_refused() {
        let (_, d) = analyze(
            "fn f(n: i32) -> i32 !{ Msg(String) } { \
               if n > 9 { return err(Msg(int_to_string(1))) } return ok(1) }",
        );
        assert!(
            d.iter().any(|x| x.message.contains("a v1 error payload must be a scalar or `str`")),
            "{d:?}"
        );
    }

    /// The four spelling/arity mismatches, each with its own diagnostic: a payload
    /// name used bare, a bare name applied, two payload values, a wrong type.
    #[test]
    fn payload_spelling_must_match_the_declaration() {
        // A payload name constructed bare.
        let (_, d) = analyze(
            "fn f(n: i64) -> i32 !{ TooBig(i64) } { \
               if n > 9 { return err(TooBig) } return ok(1) }",
        );
        assert!(
            d.iter().any(|x| x.message.contains("carries a payload of type `i64`")
                && x.message.contains("err(TooBig(…))")),
            "{d:?}"
        );
        // A bare name applied.
        let (_, d) = analyze(
            "fn f(n: i64) -> i32 !{ Empty } { \
               if n > 9 { return err(Empty(n)) } return ok(1) }",
        );
        assert!(
            d.iter().any(|x| x.message.contains("carries no payload — write `err(Empty)`")),
            "{d:?}"
        );
        // Two payload values.
        let (_, d) = analyze(
            "fn f(n: i64) -> i32 !{ TooBig(i64) } { \
               if n > 9 { return err(TooBig(n, n)) } return ok(1) }",
        );
        assert!(
            d.iter().any(|x| x.message.contains("exactly one payload value, found 2")),
            "{d:?}"
        );
        // A wrong payload type (str declared, integer given).
        let (_, d) = analyze(
            "fn f(n: i64) -> i32 !{ BadKey(str) } { \
               if n > 9 { return err(BadKey(n)) } return ok(1) }",
        );
        assert!(
            d.iter().any(|x| x.message.contains("the payload of error `BadKey`")),
            "a str payload given an i64 must be refused: {d:?}"
        );
    }

    // --- the payload extractor (error-payloads E4; docs/error-payloads.md §5) ---

    /// The happy path: bare arms, a payload-binding arm, exhaustive over the set.
    #[test]
    fn an_exhaustive_error_match_is_clean() {
        let (_, d) = analyze(
            "fn f(n: i64) -> i64 !{ Empty, TooBig(i64) } { \
               if n == 0 { return err(Empty) } \
               if n > 9 { return err(TooBig(n)) } \
               return ok(n) } \
             fn g(n: i64) -> i64 { \
               return f(n) catch |e| match e { Empty => 0 - 1, TooBig(v) => v } }",
        );
        assert!(d.is_empty(), "{d:?}");
    }

    /// Exhaustiveness is over the base's STATIC set (E2 carries it): a missing
    /// name is refused with the names listed; `_` covers whatever remains.
    #[test]
    fn an_inexhaustive_error_match_names_what_is_missing() {
        let (_, d) = analyze(
            "fn f(n: i64) -> i64 !{ Empty, TooBig(i64) } { \
               if n == 0 { return err(Empty) } return ok(n) } \
             fn g(n: i64) -> i64 { \
               return f(n) catch |e| match e { Empty => 0 - 1 } }",
        );
        assert_eq!(d.len(), 1, "{d:?}");
        assert!(d[0].message.contains("does not cover { TooBig }"), "{:?}", d[0].message);
        let (_, d) = analyze(
            "fn f(n: i64) -> i64 !{ Empty, TooBig(i64) } { \
               if n == 0 { return err(Empty) } return ok(n) } \
             fn g(n: i64) -> i64 { \
               return f(n) catch |e| match e { Empty => 0 - 1, _ => 0 } }",
        );
        assert!(d.is_empty(), "a wildcard covers the rest: {d:?}");
    }

    /// The refusals, each with its own message: an arm outside the set, a payload
    /// pattern on a bare name, a duplicate arm, a guard, an arm after `_`.
    #[test]
    fn error_match_refusals_each_name_their_rule() {
        let base = "fn f(n: i64) -> i64 !{ Empty, TooBig(i64) } { \
                      if n == 0 { return err(Empty) } return ok(n) } ";
        let (_, d) = analyze(&format!(
            "{base}fn g(n: i64) -> i64 {{ \
               return f(n) catch |e| match e {{ Empty => 0, Missing => 1, _ => 2 }} }}"
        ));
        assert!(
            d.iter().any(|x| x.message.contains("`Missing` is not in this expression's error set")),
            "{d:?}"
        );
        let (_, d) = analyze(&format!(
            "{base}fn g(n: i64) -> i64 {{ \
               return f(n) catch |e| match e {{ Empty(v) => v, _ => 0 }} }}"
        ));
        assert!(
            d.iter().any(|x| x.message.contains("carries no payload — match it bare")),
            "{d:?}"
        );
        let (_, d) = analyze(&format!(
            "{base}fn g(n: i64) -> i64 {{ \
               return f(n) catch |e| match e {{ Empty => 0, Empty => 1, _ => 2 }} }}"
        ));
        assert!(d.iter().any(|x| x.message.contains("duplicate arm for error `Empty`")), "{d:?}");
        let (_, d) = analyze(&format!(
            "{base}fn g(n: i64) -> i64 {{ \
               return f(n) catch |e| match e {{ Empty if n > 0 => 0, _ => 2 }} }}"
        ));
        assert!(
            d.iter().any(|x| x.message.contains("guard is not supported on an error arm")),
            "{d:?}"
        );
        let (_, d) = analyze(&format!(
            "{base}fn g(n: i64) -> i64 {{ \
               return f(n) catch |e| match e {{ _ => 2, Empty => 0 }} }}"
        ));
        assert!(
            d.iter().any(|x| x.message.contains("unreachable arm: it follows the `_` catch-all")),
            "{d:?}"
        );
    }

    /// A bare arm on a payload CARRIER is fine — the payload is simply ignored —
    /// and the payload binder carries the DECLARED type into the arm body.
    #[test]
    fn payload_binders_are_typed_and_bare_carrier_arms_are_legal() {
        let (_, d) = analyze(
            "fn f(n: i64) -> i64 !{ TooBig(i64) } { \
               if n > 9 { return err(TooBig(n)) } return ok(n) } \
             fn g(n: i64) -> i64 { \
               return f(n) catch |e| match e { TooBig => 0 - 1 } }",
        );
        assert!(d.is_empty(), "a bare carrier arm ignores the payload: {d:?}");
        // The binder is the declared `str`, so an i64-context use is refused
        // through the ordinary mismatch machinery.
        let (_, d) = analyze(
            "fn f(n: i64) -> i64 !{ Bad(str) } { \
               if n > 9 { return err(Bad(\"x\")) } return ok(n) } \
             fn g(n: i64) -> str { \
               return f(n) catch |e| match e { Bad(m) => m } }",
        );
        assert!(!d.is_empty(), "an i64 ok-type cannot recover as str — the arm body is typed: {d:?}");
    }

    // --- error-set soundness (error-payloads E2; docs/error-payloads.md §6) ---

    /// `err(E)` must name an error in the enclosing declared set. Strict from day
    /// one because the E1 census measured ZERO corpus violations.
    #[test]
    fn err_outside_the_declared_set_is_refused() {
        let (_, d) = analyze(
            "fn f(b: i32) -> i32 !{ Io, Parse } { \
               if b == 0 { return err(Missing) } \
               return ok(b) }",
        );
        assert_eq!(d.len(), 1, "{d:?}");
        assert!(
            d[0].message.contains("`Missing` is not in the enclosing declared error set { Io, Parse }"),
            "{:?}",
            d[0].message
        );
        // The in-set spelling is clean.
        let (_, d) = analyze(
            "fn f(b: i32) -> i32 !{ Io, Parse } { \
               if b == 0 { return err(Parse) } \
               return ok(b) }",
        );
        assert!(d.is_empty(), "{d:?}");
    }

    /// `?` propagates the callee's set, so the enclosing set must include it —
    /// and the set rides `Ty::Result`, so a STORED result (`let r = f() … r?`)
    /// still knows its origin's set. That flow is what E1's syntactic census
    /// could not check (it counted a stored base as unresolved); this is the
    /// typed version doing strictly more.
    #[test]
    fn try_propagation_needs_set_inclusion_even_through_a_binding() {
        let (_, d) = analyze(
            "fn inner(a: i32) -> i32 !{ Io } { return ok(a) } \
             fn narrow(a: i32) -> i32 !{ Parse } { let r = inner(a) let v = r? return ok(v) }",
        );
        assert_eq!(d.len(), 1, "{d:?}");
        assert!(
            d[0].message
                .contains("propagates { Io }, which the enclosing error set { Parse } does not declare"),
            "{:?}",
            d[0].message
        );
        // A subset propagates clean, also through the binding.
        let (_, d) = analyze(
            "fn inner(a: i32) -> i32 !{ Io } { return ok(a) } \
             fn wide(a: i32) -> i32 !{ Io, Parse } { let r = inner(a) let v = r? return ok(v) }",
        );
        assert!(d.is_empty(), "{d:?}");
    }

    /// `catch |e| return e` is `?` spelled out — the rethrow form owes exactly
    /// the same inclusion obligation; a recovering `catch` consumes the error
    /// and owes nothing.
    #[test]
    fn the_rethrow_form_owes_inclusion_and_a_recovering_catch_does_not() {
        let (_, d) = analyze(
            "fn inner(a: i32) -> i32 !{ Io } { return ok(a) } \
             fn outer(a: i32) -> i32 !{ Parse } { \
               let v: i32 = inner(a) catch |e| return e return ok(v) }",
        );
        assert_eq!(d.len(), 1, "{d:?}");
        assert!(d[0].message.contains("propagates { Io }"), "{:?}", d[0].message);
        // Recovery consumes: no set obligation, even in an INFALLIBLE fn.
        let (_, d) = analyze(
            "fn inner(a: i32) -> i32 !{ Io } { return ok(a) } \
             fn f(a: i32) -> i32 { let v: i32 = inner(a) catch 0 return v }",
        );
        assert!(d.is_empty(), "{d:?}");
    }

    /// A struct method's declared set participates exactly as a free function's
    /// — on both sides of the obligation (the method as callee, the method body
    /// as encloser).
    #[test]
    fn method_sets_participate_on_both_sides() {
        let (_, d) = analyze(
            "struct A { n: i32 \
               fn get(read self) -> i32 !{ Empty } { \
                 if self.n == 0 { return err(Empty) } \
                 return ok(self.n) } } \
             fn f(read a: A) -> i32 !{ Io } { let v = a.get()? return ok(v) }",
        );
        assert_eq!(d.len(), 1, "{d:?}");
        assert!(
            d[0].message.contains("propagates { Empty }"),
            "{:?}",
            d[0].message
        );
    }

    /// A user enum variant named `err` (the corpus's `Result(T, E)`) shadows the
    /// error constructor — its constructions carry no membership obligation. The
    /// E1 census learned this the hard way (16 false violations in core.jtr).
    #[test]
    fn a_user_err_variant_shadows_the_error_constructor_here_too() {
        let (_, d) = analyze(
            "enum R(T, E) { okv(v: T), err(e: E) } \
             fn f(a: i32) -> i32 !{ Io } { \
               let r = err(a) \
               return ok(a) }",
        );
        assert!(d.is_empty(), "{d:?}");
    }

    /// The fallible intrinsics carry the `IoError` set, so propagating one out
    /// demands `IoError` in the enclosing declaration like any other name.
    #[test]
    fn intrinsic_propagation_demands_ioerror() {
        let (_, d) = analyze(
            "fn f(p: str) -> i32 !{ Parse } { let t = try_read_file(p)? return ok(1) }",
        );
        assert_eq!(d.len(), 1, "{d:?}");
        assert!(d[0].message.contains("propagates { IoError }"), "{:?}", d[0].message);
        let (_, d) = analyze(
            "fn f(p: str) -> i32 !{ IoError } { let t = try_read_file(p)? return ok(1) }",
        );
        assert!(d.is_empty(), "{d:?}");
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

    // --- task results + await (concurrency N) ---

    #[test]
    fn spawn_yields_a_task_handle_and_await_unwraps_it() {
        // `spawn sq(3)` types as `Task(i64)` (sq returns i64); `await h` unwraps to i64.
        let (ast, info) = analyze_full(
            "fn sq(n: i64) -> i64 { return n * n } \
             fn main() -> i32 { concurrent { let h = spawn sq(3) print_int(await h as i32) } return 0 }",
        );
        let spawn = ast
            .exprs
            .iter()
            .enumerate()
            .find_map(|(i, e)| matches!(e.kind, ExprKind::Spawn(_)).then_some(ExprId(i as u32)))
            .expect("the spawn expr");
        assert_eq!(
            info.type_of(spawn),
            &Ty::Task(Box::new(Ty::Prim("i64"))),
            "spawn yields Task(T) where T is the target's return"
        );
        let await_e = ast
            .exprs
            .iter()
            .enumerate()
            .find_map(|(i, e)| matches!(e.kind, ExprKind::Await(_)).then_some(ExprId(i as u32)))
            .expect("the await expr");
        assert_eq!(info.type_of(await_e), &Ty::Prim("i64"), "await unwraps Task(i64) to i64");
    }

    #[test]
    fn select_rejects_a_non_channel_arm() {
        // A `select` arm must wait on a `Channel(i64)`; an `i64` is rejected.
        let (_info, d) = analyze("fn f(c: i64) { select { recv(c) => x { } } }");
        assert!(
            d.iter().any(|m| m.message.contains("select") && m.message.contains("Channel(i64)")),
            "a non-channel select arm must error: {d:?}"
        );
    }

    #[test]
    fn await_of_a_non_task_is_a_type_error() {
        // `await` requires a `Task(T)` from `spawn`; awaiting a plain value is rejected.
        let (_info, d) = analyze(
            "fn main() -> i32 { let x: i64 = 1 concurrent { print_int(await x as i32) } return 0 }",
        );
        assert!(
            d.iter().any(|m| m.message.contains("await") && m.message.contains("task handle")),
            "awaiting a non-task must error: {d:?}"
        );
    }

    // --- par for … reduce(r) (the checked deterministic-reduction guarantee) ---

    #[test]
    fn par_for_accepts_a_deterministic_reduction_and_types_as_i64() {
        // A reduction whose constructor is on the declared-deterministic list is
        // accepted; the loop types as `i64`. (The name check is what matters, so a
        // local `sum_reduction` stands in for `core.sum_reduction` here.)
        let (ast, info) = analyze_full(
            "fn sum_reduction() -> i64 { return 0 } \
             fn main() -> i32 { var a: *mut i64 = alloc(i64, 4) let s: []i64 = slice(i64, a, 4) \
                 let r: i64 = par for x in s reduce(sum_reduction()) { x * x } return 0 }",
        );
        let pf = ast
            .exprs
            .iter()
            .enumerate()
            .find_map(|(i, e)| matches!(e.kind, ExprKind::ParFor { .. }).then_some(ExprId(i as u32)))
            .expect("the par for expr");
        assert_eq!(info.type_of(pf), &Ty::Prim("i64"), "par for types as i64");
    }

    #[test]
    fn par_for_rejects_a_non_deterministic_reduction() {
        // THE headline check: a reduction not on the declared-deterministic list is a
        // compile error — the parallel result could otherwise depend on the schedule.
        let (_info, d) = analyze(
            "fn my_reduction() -> i64 { return 0 } \
             fn main() -> i32 { var a: *mut i64 = alloc(i64, 4) let s: []i64 = slice(i64, a, 4) \
                 let r: i64 = par for x in s reduce(my_reduction()) { x } return 0 }",
        );
        assert!(
            d.iter()
                .any(|m| m.message.contains("deterministic reduction") && m.message.contains("my_reduction")),
            "a non-deterministic reduction must be rejected: {d:?}"
        );
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
        assert!(info.impl_call(call).is_some(), "the resolution is recorded for the backend");
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
    fn a_variant_name_shared_by_two_enums_in_one_module_is_an_error() {
        // Variant names resolve by bare name within a module; before this check the
        // second enum's `insert` silently won and an `err(overflow)` of the FIRST
        // enum resolved against the second — a silent miscompile, not a diagnostic.
        let (_i, d) = analyze(
            "enum ParseErr { overflow, bad_digit }\nenum MathErr { overflow }\nfn main() -> i32 { return 0 }",
        );
        assert!(
            d.iter().any(|m| m.message.contains("duplicate variant name `overflow`")
                && m.message.contains("ParseErr")),
            "expected a duplicate-variant error naming the earlier enum: {:?}",
            d
        );
    }

    #[test]
    fn a_variant_name_repeated_inside_one_enum_is_an_error() {
        let (_i, d) = analyze("enum E { a, b, a }\nfn main() -> i32 { return 0 }");
        assert!(
            d.iter().any(|m| m.message.contains("duplicate variant name `a`")),
            "expected a duplicate-variant error: {:?}",
            d
        );
    }

    /// The `@copy` enum contract is CHECKED, not trusted (unlike the struct form):
    /// a copy of a droppable payload would drop twice, so a non-Copy payload under
    /// `@copy` is refused at the payload's type. All-Copy payloads pass.
    #[test]
    fn a_copy_enum_requires_copy_payloads() {
        let (_i, d) = analyze(
            "@copy enum Bad { none, own(s: String) }\nfn main() -> i32 { return 0 }",
        );
        assert!(
            d.iter().any(|m| m.message.contains("non-Copy payload `String`")),
            "a droppable payload under @copy must be refused: {d:?}"
        );
        let (_i2, d2) = analyze(
            "@copy enum Link { nil, at(n: &i64), idx(i: usize) }\nfn main() -> i32 { return 0 }",
        );
        assert!(
            d2.iter().all(|m| !m.is_error()),
            "all-Copy payloads (genref, usize) pass: {d2:?}"
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
                    && info.impl_call(id).is_some())
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
                    && info.impl_call(id).is_some())
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
        assert!(!info.impl_call(plus).is_some(), "primitives don't dispatch operators");
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
                    && info.impl_call(id).is_some())
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
            assert!(info.impl_call(e).is_some(), "{want:?} resolved through its base trait");
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
                    && info.bound_method_call(id).is_some())
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

    // --- traits: `dyn Trait` dynamic dispatch (Stage F) ---

    #[test]
    fn a_dyn_method_call_types_by_the_trait_method_return() {
        // `s.show()` on a `dyn Show` types as `Show::show`'s return and is recorded
        // for vtable dispatch.
        let (ast, info) = analyze_full(
            "trait Show { fn show(read self) -> i64 } \
             impl Show for i32 { fn show(read self) -> i64 { return 1 } } \
             fn describe(read s: dyn Show) -> i64 { return s.show() }",
        );
        let call = ast
            .exprs
            .iter()
            .enumerate()
            .find_map(|(i, e)| {
                let id = ExprId(i as u32);
                (matches!(e.kind, ExprKind::Call { .. }) && info.dyn_call(id).is_some())
                    .then_some(id)
            })
            .expect("the s.show() dyn call");
        assert_eq!(info.type_of(call), &Ty::Prim("i64"), "typed by the trait method's return");
    }

    #[test]
    fn a_concrete_value_coerces_to_dyn_at_a_call() {
        // Passing an `i32` (which `impl`s `Show`) where `dyn Show` is expected
        // records a coercion the backend turns into a fat pointer.
        let (ast, info) = analyze_full(
            "trait Show { fn show(read self) -> i32 } \
             impl Show for i32 { fn show(read self) -> i32 { return self } } \
             fn describe(read s: dyn Show) -> i32 { return s.show() } \
             fn use_it(read n: i32) -> i32 { return describe(n) }",
        );
        let coerced = ast
            .exprs
            .iter()
            .enumerate()
            .any(|(i, _)| info.dyn_coercion(ExprId(i as u32)).is_some());
        assert!(coerced, "the i32 argument is recorded as a `dyn Show` coercion");
    }

    #[test]
    fn a_call_coerced_to_dyn_keeps_both_resolutions() {
        // HIR Stage 1 regression pin. `dyn_coercion` is keyed on the coerced
        // *value*, and here that value is itself a method call — so one `ExprId`
        // carries two resolutions at once. While these were seven independent
        // maps that composed for free; now they share a `Resolved` row, so the
        // checker must fill in a field of the existing row rather than insert a
        // fresh one. A writer that replaced the row would drop whichever
        // resolution came first and emit the call without its fat-pointer wrap —
        // a miscompile no goldens necessarily cover.
        // `p.get()` is method sugar for the free call `get(p)`, so the argument
        // expression carries a `MethodRes`; its `i32` result then coerces to
        // `dyn Show` at `describe`'s parameter. Both land on the same `ExprId`.
        let (ast, info) = analyze_full(
            "trait Show { fn show(read self) -> i32 } \
             impl Show for i32 { fn show(read self) -> i32 { return self + 1 } } \
             struct P { v: i32 } \
             fn get(read p: P) -> i32 { return p.v } \
             fn describe(read s: dyn Show) -> i32 { return s.show() } \
             fn use_it(read p: P) -> i32 { return describe(p.get()) }",
        );
        let both = ast.exprs.iter().enumerate().any(|(i, e)| {
            let id = ExprId(i as u32);
            matches!(e.kind, ExprKind::Call { .. })
                && info.method_call(id).is_some()
                && info.dyn_coercion(id).is_some()
        });
        assert!(
            both,
            "`p.get()` passed as `dyn Show` keeps its method resolution *and* its coercion"
        );
    }

    #[test]
    fn coercing_a_type_without_the_impl_to_dyn_is_an_error() {
        // A type that does not implement the trait cannot become `dyn Trait`.
        let (_i, d) = analyze(
            "trait Show { fn show(read self) -> i32 } \
             fn describe(read s: dyn Show) -> i32 { return 0 } \
             fn use_it(read n: i32) -> i32 { return describe(n) }",
        );
        assert!(
            d.iter().any(|m| m.message.contains("does not implement `Show`")),
            "{:?}",
            d
        );
    }

    #[test]
    fn calling_a_non_trait_method_on_dyn_is_an_error() {
        let (_i, d) = analyze(
            "trait Show { fn show(read self) -> i32 } \
             fn describe(read s: dyn Show) -> i32 { return s.nope() }",
        );
        assert!(d.iter().any(|m| m.message.contains("no method `nope` on `dyn Show`")), "{:?}", d);
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

    /// `with alive` requires a genref scrutinee and binds the referent's type;
    /// the escape checker contains the binding by the ordinary frame rule.
    #[test]
    fn with_alive_types_the_binding_and_rejects_non_genrefs() {
        let src = "struct N { s: String }                    fn ok(read r: &N) { with alive r as read n { print_str(n.s as str) } }                    fn bad(x: i32) { with alive x as read v { print_int(v as i64) } }";
        let (tokens, _) = crate::lexer::Lexer::new(src).tokenize();
        let (ast, _pd) = crate::parser::Parser::new(src, tokens).parse();
        let (info, diags) = check(&ast);
        assert!(
            diags.iter().any(|d| d.message.contains("takes a generational reference")),
            "the i32 scrutinee is rejected: {diags:?}"
        );
        // In `ok`, the binding's uses type as the referent (String field reachable).
        let renders: Vec<String> = (0..ast.exprs.len())
            .map(|i| info.type_of(ExprId(i as u32)).display(&info.table))
            .collect();
        assert!(renders.iter().any(|r| r == "N"), "the binding types as the referent: {renders:?}");
    }

    /// The block's borrow cannot leave it — no new machinery, the frame rule.
    #[test]
    fn with_alive_binding_is_contained_by_the_frame_rule() {
        let src = "struct N { s: String }                    fn steal(read r: &N) -> String {                        with alive r as read n { return n.s }                        return string_new()                    }";
        let (tokens, _) = crate::lexer::Lexer::new(src).tokenize();
        let (ast, _pd) = crate::parser::Parser::new(src, tokens).parse();
        let (info, _d) = check(&ast);
        let esc = crate::escape::check(&ast, &info);
        assert!(
            esc.iter().any(|d| d.message.contains("cannot return borrow `n`")),
            "the binding cannot escape the block: {esc:?}"
        );
    }

    /// A generic-struct ctor-body method's `self` is the REAL instance type
    /// (`Box(T)`, `T` opaque), not an opaque `Self` — so `self.field` resolves
    /// through the template. This superseded the `Unknown` finalization's refusal
    /// of these methods: with `self` typed, the escape checker judges a by-value
    /// field return by the ordinary conservative rule (`T` may be non-`Copy`)
    /// with its actionable message, and the corpus-wide borrow-return idiom
    /// (`-> read T`) checks cleanly on its merits.
    #[test]
    fn a_ctor_body_method_types_self_as_the_generic_struct() {
        let src = "fn Box(comptime T: type) -> type { \
                       return struct { v: T  fn get(read self) -> read T { self.v } } \
                   } \
                   fn main() -> i32 { let a: Box(i32) = Box(i32){ v: 7 } return 0 }";
        let (tokens, _) = crate::lexer::Lexer::new(src).tokenize();
        let (ast, _diags) = crate::parser::Parser::new(src, tokens).parse();
        let (info, diags) = check(&ast);
        assert!(diags.iter().all(|d| !d.is_error()), "clean: {diags:?}");
        let renders: Vec<String> = (0..ast.exprs.len())
            .map(|i| info.type_of(ExprId(i as u32)).display(&info.table))
            .collect();
        assert!(renders.iter().any(|r| r == "Box(T)"), "self typed as the instance: {renders:?}");
        assert!(
            !renders.iter().any(|r| r == "Self"),
            "no Self placeholder survives in a type-fn: {renders:?}"
        );
        assert!(
            crate::escape::check(&ast, &info).iter().all(|d| !d.is_error()),
            "borrow-return ctor-body method is clean"
        );
    }

    /// The by-value form is still refused — but now by the ordinary conservative
    /// escape rule with its actionable help, not by the `Unknown` finalization.
    #[test]
    fn a_ctor_body_method_by_value_field_return_gets_the_ordinary_message() {
        let src = "fn Box(comptime T: type) -> type { \
                       return struct { v: T  fn get(read self) -> T { self.v } } \
                   } \
                   fn main() -> i32 { return 0 }";
        let (tokens, _) = crate::lexer::Lexer::new(src).tokenize();
        let (ast, _diags) = crate::parser::Parser::new(src, tokens).parse();
        let (info, _d) = check(&ast);
        let esc = crate::escape::check(&ast, &info);
        assert!(
            esc.iter().any(|d| d.message.contains("cannot return borrow")),
            "the ordinary conservative rule: {esc:?}"
        );
        assert!(
            !esc.iter().any(|d| d.message.contains("was never resolved")),
            "not the Unknown finalization: {esc:?}"
        );
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

    /// `e catch v` has the **ok type**, not the result type — recovering is what
    /// removes the fallibility, and the whole point is that the value flows on
    /// normally afterwards.
    /// A non-constant array length has **two** real remedies that land in different
    /// places, so the suggestion names both: a `const` when the length is a fixed
    /// number, a `comptime { … }` block when it is *computed*. The CTFE ladder exists
    /// precisely so a length can be derived rather than spelled out, and a diagnostic
    /// that mentioned only `const` would hide that.
    #[test]
    fn a_non_constant_array_length_suggests_const_or_comptime() {
        let (_i, d) = analyze("fn f(n: i32) -> i32 { var a: [n]i32 = [0; 3] return a[0] }");
        assert_eq!(d.len(), 1, "{d:?}");
        let h = d[0].help.as_deref().expect("must suggest a rewrite");
        assert!(h.contains("const"), "{h}");
        assert!(h.contains("comptime"), "{h}");
        // The message is unchanged — only `help` was added.
        assert!(
            d[0].message.starts_with("array length must be a compile-time constant"),
            "{:?}",
            d[0].message
        );
    }

    /// `catch |e| e` would silently turn an error tag into a success value — the exact
    /// confusion the opaque `error` type exists to prevent. Refused with a cast hint;
    /// the explicit cast is the escape hatch, exactly as it is for `distinct`.
    #[test]
    fn the_error_binder_cannot_leak_as_a_success_value() {
        let (_i, d) = analyze(
            "fn f() -> i32 !{ Bad } { return err(Bad) } \
             fn g() -> i32 { return f() catch |e| e }",
        );
        assert_eq!(d.len(), 1, "{d:?}");
        assert!(d[0].message.contains("it is an error, not a result"), "{:?}", d[0].message);
        // The cast form is the sanctioned way to read the tag.
        let (_i, d) = analyze(
            "fn f() -> i32 !{ Bad } { return err(Bad) } \
             fn g() -> i64 { return f() catch |e| (e as i64) }",
        );
        assert!(d.is_empty(), "an explicit cast must pass: {d:?}");
    }

    /// The rethrow form yields the ok type — on the success path it IS the ok value;
    /// on the error path control leaves, so there is no second type to reconcile.
    #[test]
    fn catch_rethrow_yields_the_ok_type() {
        let (_i, d) = analyze(
            "fn f() -> i32 !{ Bad } { return ok(1) } \
             fn g() -> i32 !{ Bad } { let v: i32 = f() catch |e| return e return ok(v + 1) }",
        );
        assert!(d.is_empty(), "{d:?}");
    }

    #[test]
    fn catch_unwraps_to_the_ok_type() {
        let (_i, d) = analyze(
            "fn f() -> i32 !{ Bad } { return ok(1) } \
             fn g() -> i32 { let a: i32 = f() catch 0 return a }",
        );
        assert!(d.is_empty(), "catch in an infallible fn is the point: {:?}", d);
    }

    /// `catch` on something that cannot fail is refused rather than accepted as a
    /// no-op: it reads as a claim that an error was handled, and a claim about
    /// nothing is worse than a diagnostic.
    #[test]
    fn catch_on_an_infallible_expression_is_refused() {
        let (_i, d) = analyze("fn p(n: i32) -> i32 { return n } fn g() -> i32 { return p(1) catch 0 }");
        assert_eq!(d.len(), 1, "{d:?}");
        assert!(d[0].message.contains("`catch` needs a fallible expression"), "{:?}", d[0].message);
        assert!(d[0].message.contains("`i32`"), "the actual type must be named: {:?}", d[0].message);
    }

    /// **A unit value is assignable to nothing, and nothing is assignable to it.**
    ///
    /// The rule reads as too obvious to write down, and it needed writing down: the moment
    /// `fn f(…) !{ E }` became lowerable its `catch` acquired the ok type `()`, and
    /// `let b: bool = f(x) catch true` passed `check` and failed in gcc with *void value
    /// not ignored as it ought to be* — the degrades-to-gcc class, reached through a shape
    /// that did not exist until the backend learned `JestyrResult_unit`.
    #[test]
    fn a_unit_value_is_not_assignable_to_a_typed_binding() {
        let unit_fn = "fn f(x: i32) !{ Bad } { if x < 0 { return err(Bad) } } ";

        let (_i, d) = analyze(&format!("{unit_fn}fn g() -> bool {{ let b: bool = f(1) catch true return b }}"));
        assert_eq!(d.len(), 1, "{d:?}");
        assert!(d[0].message.contains("expected `bool`"), "{:?}", d[0].message);
        assert!(d[0].message.contains("()"), "the unit type must be named: {:?}", d[0].message);

        // **The positive control**: the same call, used as the STATEMENT it is, is clean.
        // Without it the assertion above would also pass for a checker that had started
        // refusing every `catch` on a unit-fallible callee.
        let (_i, d) = analyze(&format!("{unit_fn}fn g() -> i32 {{ f(1) catch {{}} return 0 }}"));
        assert!(d.is_empty(), "a unit catch in statement position is fine: {d:?}");

        // And the rule is symmetric: a unit-typed parameter takes nothing either.
        let (_i, d) = analyze(&format!("{unit_fn}fn g() -> i32 {{ let n: i32 = f(1) catch 0 return n }}"));
        assert_eq!(d.len(), 1, "{d:?}");
    }

    /// **`catch |e|` binds `e` even when the base's type could not be recovered.**
    ///
    /// The binder exists because the syntax says so; whether the base resolved has nothing
    /// to do with it. Inferring the fallback without it left `e` an unknown name typed
    /// `?`, so a program with one real problem (the unresolvable callee) reported a second
    /// invented one underneath it.
    ///
    /// It was also a reference/port divergence. `jestyr_typeck_dump_matches_reference` runs
    /// the WHOLE corpus with no allowlist, and `examples/std/sysfs_test.jtr` — the first
    /// file to put a `catch |e| match e { … }` on a fallible call into another module — hit
    /// it: with imports unresolved the reference typed `e` as `?` while the port typed it
    /// `error`. The port had the better answer; this adopts it.
    #[test]
    fn catch_binds_its_error_name_even_when_the_base_does_not_resolve() {
        // The recorded TYPE of the binder use is what diverged, so it is what is asserted
        // — a diagnostic-shaped assertion would not distinguish `?` from `error` here,
        // because the degraded path reports nothing at all.
        let ty_of_e = |src: &str| -> String {
            let (ast, info) = analyze_full(src);
            let mut seen: Option<String> = None;
            for (id, ed) in ast.exprs.iter().enumerate() {
                if let ExprKind::Name(n) = &ed.kind {
                    if n.name == "e" {
                        seen = Some(info.expr_types[id].display(&info.table));
                    }
                }
            }
            seen.expect("the fallback's `e` must be a recorded expression")
        };

        // `nope(1)` is unresolvable, so the catch degrades — and `e` is still the opaque
        // `error` type rather than an unknown name.
        assert_eq!(
            ty_of_e("fn g() -> i32 { return nope(1) catch |e| e }"),
            "error",
            "the binder must be in scope and opaque even on the degraded path"
        );

        // The positive control: the same shape with a RESOLVABLE fallible base. Without it
        // the assertion above would also pass for a checker that typed every `e` as
        // `error` for reasons having nothing to do with the binder.
        assert_eq!(
            ty_of_e(
                "fn f(n: i32) -> i32 !{ Bad } { return err(Bad) } \
                 fn g() -> i32 { return f(1) catch |e| e }"
            ),
            "error",
            "and the recovered path is unchanged"
        );

        // The anti-vacuity half: an `e` that is NOT a catch binder is not `error`, so
        // `ty_of_e` is reading the binder rather than reporting a constant.
        assert_eq!(
            ty_of_e("fn g() -> i32 { let e: i32 = 1 return e }"),
            "i32",
            "`ty_of_e` reads the binding it is given"
        );
    }

    /// The fallback is inferred against the ok type, so the one mismatch class this
    /// checker reports applies here too — recovering a `distinct` with its bare base
    /// needs an explicit `as`.
    #[test]
    fn catch_checks_the_fallback_against_the_ok_type() {
        let (_i, d) = analyze(
            "distinct UserId = i32 \
             fn f() -> UserId !{ Bad } { return err(Bad) } \
             fn g() -> UserId { return f() catch 0 }",
        );
        assert_eq!(d.len(), 1, "{d:?}");
        assert!(d[0].message.contains("distinct"), "{:?}", d[0].message);
        // …and the same program with the cast is clean.
        let (_i, d) = analyze(
            "distinct UserId = i32 \
             fn f() -> UserId !{ Bad } { return err(Bad) } \
             fn g() -> UserId { return f() catch 0 as UserId }",
        );
        assert!(d.is_empty(), "an explicit cast must satisfy it: {:?}", d);
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

    /// **`distinct` is enforced at ARGUMENT and RETURN positions too, not just at
    /// initializers.**
    ///
    /// Measured before this existed: the initializer arm was the *only* consumer of
    /// `distinct_mismatch`, so a bare `i64` passed where a `UserId` was wanted, and —
    /// the row that matters — **an unrelated `AccountId` passed where a `UserId` was
    /// wanted**. A rule that holds in one position and not the others is not a weak
    /// check, it is no check, and a typed `Path` built on that would have been a name
    /// dressed as safety.
    ///
    /// Each refusal is paired with the `as` spelling that must still be accepted; a
    /// refusal test alone would pass just as well against a checker that rejects
    /// everything.
    #[test]
    fn distinct_is_enforced_at_argument_and_return_positions() {
        let prelude = "distinct UserId = i32\n\
                       distinct AccountId = i32\n\
                       fn takes_uid(u: UserId) -> i32 { return 0 }\n";

        // 1. The base type where a distinct type is wanted.
        let (_a, da) = analyze(&format!(
            "{prelude}fn main() -> i32 {{ let n: i32 = 5 return takes_uid(n) }}"
        ));
        assert!(
            da.iter().any(|m| m.message.contains("distinct")),
            "a bare `i32` must not pass as a `UserId`: {da:?}"
        );

        // 2. A DIFFERENT distinct type over the same base — the row that makes this a
        //    correctness fix rather than a strictness preference.
        let (_b, db) = analyze(&format!(
            "{prelude}fn main() -> i32 {{ let a: AccountId = 5 as AccountId return takes_uid(a) }}"
        ));
        assert!(
            db.iter().any(|m| m.message.contains("distinct")),
            "an `AccountId` must not pass as a `UserId`: {db:?}"
        );

        // 3. The return position.
        let (_c, dc) = analyze(&format!(
            "{prelude}fn ret_uid(x: i32) -> UserId {{ return x }}\nfn main() -> i32 {{ return 0 }}"
        ));
        assert!(
            dc.iter().any(|m| m.message.contains("distinct")),
            "a bare `i32` must not be returned as a `UserId`: {dc:?}"
        );

        // 4. POSITIVE CONTROLS: every one of the above with an explicit `as` is fine.
        let (_d, dd) = analyze(&format!(
            "{prelude}fn ret_uid(x: i32) -> UserId {{ return x as UserId }}\n\
             fn main() -> i32 {{\n\
             \x20   let n: i32 = 5\n\
             \x20   let a: AccountId = 5 as AccountId\n\
             \x20   let p: i32 = takes_uid(n as UserId)\n\
             \x20   let q: i32 = takes_uid(a as i32 as UserId)\n\
             \x20   return p + q\n\
             }}"
        ));
        assert!(dd.is_empty(), "explicit `as` must still be accepted everywhere: {dd:?}");

        // 5. And a distinct value passed where its OWN type is wanted is untouched —
        //    otherwise the rule would make the type unusable rather than safe.
        let (_e, de) = analyze(&format!(
            "{prelude}fn main() -> i32 {{ let u: UserId = 5 as UserId return takes_uid(u) }}"
        ));
        assert!(de.is_empty(), "a `UserId` must pass as a `UserId`: {de:?}");
    }

    /// **A `distinct` inherits its base's operators — with ITSELF, and nothing else.**
    ///
    /// The refusal half of the rule, and specifically the eight *laundering* shapes
    /// that killed the previous attempt at this feature. That attempt exempted an
    /// operand it judged an "untyped literal", delegating to [`literal_defaulted`],
    /// whose Binary arm is a **recursive disjunction over the expression tree** — so
    /// one integer literal anywhere in a subtree exempted the whole operand, and only
    /// the bare spelling `a + b` was still caught. `a + (b + 1)`, `(a + 1) + b`,
    /// `a * (b * 2)`, `a - (0 - b)`, `a == (b + 0)` and `a + (b + (0 * 0))` all mixed
    /// two unrelated id spaces and ran end to end, printing an answer.
    ///
    /// They are refused here without any literal predicate at all: the rule reads the
    /// two operand **types**, `1` types as `i32` and `(b + 1)` types as `Error`, and
    /// neither is a distinct type. That is a structural property, not a predicate to
    /// get right — which is the whole reason this shape was chosen.
    ///
    /// Every refusal is paired with the spelling that must still compile, because a
    /// refusal test alone passes just as well against a checker that rejects
    /// everything.
    ///
    /// [`literal_defaulted`]: Self::literal_defaulted
    #[test]
    fn a_distinct_inherits_its_base_operators_only_with_itself() {
        let prelude = "distinct Id = i64\ndistinct Acct = i64\n";
        let refused = |body: &str| {
            let (_i, d) = analyze(&format!(
                "{prelude}fn main() -> i32 {{\n\
                 \x20   var a: Id = 1 as Id\n\
                 \x20   let b: Acct = 2 as Acct\n\
                 \x20   let n: i64 = 3\n\
                 {body}\n\
                 \x20   return 0\n\
                 }}"
            ));
            assert!(!d.is_empty(), "must be refused at `check` time: `{body}`");
            d
        };

        // The eight laundering shapes. Each is refused at the INNER node, before the
        // outer node exists — `(b + 1)` is `Acct + i32`, which is already a mix.
        for body in [
            "    let z: Id = a + b",             // the bare spelling
            "    let z: Id = a + (b + 1)",       // the row `literal_defaulted` lost
            "    let z: Id = (a + 1) + b",
            "    let z: Id = a * (b * 2)",
            "    let z: Id = a - (0 - b)",
            "    print_bool(a == (b + 0))",
            "    let z: Id = a + (b + (0 * 0))",
            "    let z: Id = a + (n + 1)",
            "    a = a + (b + 1)",
        ] {
            refused(body);
        }

        // The left-operand-only hole: HEAD's rule read `lhs` alone, so every one of
        // these mixed two id spaces and RAN.
        for body in [
            "    let z: Id = 1 + a + b",
            "    let z: Id = 0 + a + b",
            "    print_bool(0 + a == 0 + b)",
            "    let z: Id = n + a",
            "    let z: Id = 1 + a",
        ] {
            refused(body);
        }

        // The operators with no trait mapping at all (`%`, `&`, `<<`): the old rule
        // covered six operators, this one covers every binary operator.
        for body in
            ["    let z: Id = a % b", "    let z: Id = a & b", "    let z: Id = a << b"]
        {
            refused(body);
        }

        // A compound assignment is `a = a OP a`, and it was checked at no position.
        for body in ["    a += b", "    a += n", "    a = b", "    a = 7"] {
            refused(body);
        }

        // POSITIVE CONTROLS. Same-type operators are exactly what this feature adds,
        // so they must compile — including the ones with no trait (`%`, `&`, `<<`),
        // the comparisons, and the compound forms.
        let (_ok, dok) = analyze(&format!(
            "{prelude}fn main() -> i32 {{\n\
             \x20   var a: Id = 12 as Id\n\
             \x20   let c: Id = 5 as Id\n\
             \x20   let s: Id = a + c - c * c / c % c & c | c ^ c\n\
             \x20   let t: Id = a << (1 as Id)\n\
             \x20   a += c\n\
             \x20   a = s + t\n\
             \x20   print_bool(a == s)\n\
             \x20   print_bool(a < s)\n\
             \x20   print_bool(a != s)\n\
             \x20   return 0\n\
             }}"
        ));
        assert!(dok.is_empty(), "a distinct with ITSELF must compile: {dok:?}");

        // ...and the cast spelling of every refusal above still works, so the escape
        // hatch is intact.
        let (_cast, dcast) = analyze(&format!(
            "{prelude}fn main() -> i32 {{\n\
             \x20   var a: Id = 1 as Id\n\
             \x20   let b: Acct = 2 as Acct\n\
             \x20   let n: i64 = 3\n\
             \x20   let z: Id = a + (b as i64 as Id)\n\
             \x20   a += n as Id\n\
             \x20   a = (n + 1) as Id\n\
             \x20   print_int(z as i32)\n\
             \x20   return 0\n\
             }}"
        ));
        assert!(dcast.is_empty(), "the `as` escape hatch must stay open: {dcast:?}");
    }

    /// **Inheritance is a POSITIVE list, not "whatever the base does".**
    ///
    /// `str == str` and `str + str` pass `check` at HEAD and die in gcc. Inheriting
    /// them would move a `distinct P = str`'s `==` from a Jestyr refusal to a gcc
    /// one — which is losing the rejection, not keeping it: `check` is the gate
    /// people run, and gcc knows nothing about id spaces. So a base with no such
    /// operator gives its distinct no such operator, and the message says why.
    ///
    /// The bool/char rows are the paired controls: they prove the list is a *list*
    /// and not a blanket refusal of non-integer bases.
    #[test]
    fn a_distinct_inherits_no_operator_its_base_lacks() {
        let (_i, d) = analyze(
            "distinct P = str\n\
             fn main() -> i32 {\n\
             \x20   let p: P = \"hi\" as P\n\
             \x20   let q: P = \"hi\" as P\n\
             \x20   print_bool(p == q)\n\
             \x20   return 0\n\
             }",
        );
        assert!(
            d.iter().any(|m| m.message.contains("has no `==` operator")),
            "`str` has no `==`, so `P` must not inherit one: {d:?}"
        );

        // Arithmetic on a distinct-over-`str` likewise: `+` is not concatenation.
        let (_j, dj) = analyze(
            "distinct P = str\ndistinct Q = str\n\
             fn main() -> i32 {\n\
             \x20   let p: P = \"a\" as P\n\
             \x20   let q: Q = \"b\" as Q\n\
             \x20   let r: P = p + q\n\
             \x20   return 0\n\
             }",
        );
        assert!(!dj.is_empty(), "two distincts over `str` must not add: {dj:?}");

        // POSITIVE CONTROLS: bases that DO have the operator.
        let (_k, dk) = analyze(
            "distinct Flag = bool\ndistinct Ch = char\ndistinct M = f64\n\
             fn main() -> i32 {\n\
             \x20   let f: Flag = true as Flag\n\
             \x20   let c: Ch = 'a' as Ch\n\
             \x20   let m: M = 1.5 as M\n\
             \x20   print_bool(f and f)\n\
             \x20   print_bool(c < c)\n\
             \x20   print_bool((m + m) > m)\n\
             \x20   return 0\n\
             }",
        );
        assert!(dk.is_empty(), "bool/char/f64 bases keep their operators: {dk:?}");

        // ...and a base without ARITHMETIC still keeps its comparisons: `char` has
        // `<` but no `+`, so the list is per-operator, not per-base.
        let (_l, dl) = analyze(
            "distinct Ch = char\n\
             fn main() -> i32 {\n\
             \x20   let c: Ch = 'a' as Ch\n\
             \x20   let z: Ch = c + c\n\
             \x20   return 0\n\
             }",
        );
        assert!(
            dl.iter().any(|m| m.message.contains("has no `+` operator")),
            "`char` has no `+`, so `Ch` must not inherit one: {dl:?}"
        );
    }

    /// **A hand-written `impl` still wins over inheritance.**
    ///
    /// Inheritance supplies the operation the base has; it does not override one the
    /// author declared. Resolution is impl-first, so the emitted call is unchanged
    /// for every type that already had an operator impl — including a `distinct`,
    /// which the operator-trait path has always accepted (only the *derivation* was
    /// missing).
    #[test]
    fn a_hand_written_operator_impl_beats_the_inherited_one() {
        let (ast, info) = analyze_full(
            "distinct Tag = i64\n\
             impl Eq for Tag { fn eq(self, other: Tag) -> bool { return true } }\n\
             fn main() -> i32 {\n\
             \x20   let s: Tag = 1 as Tag\n\
             \x20   let t: Tag = 2 as Tag\n\
             \x20   print_bool(s == t)\n\
             \x20   return 0\n\
             }",
        );
        let dispatched = |ast: &Ast, info: &TypeInfo| {
            (0..ast.exprs.len())
                .filter(|id| matches!(ast.exprs[*id].kind, ExprKind::Binary { .. }))
                .filter_map(|id| info.impl_call(ExprId(id as u32)))
                .any(|c| c.trait_name == "Eq" && c.method == "eq")
        };
        assert!(
            dispatched(&ast, &info),
            "the `==` must dispatch to `impl Eq for Tag`, not to the inherited integer compare"
        );

        // POSITIVE CONTROL: the same distinct with NO impl records no dispatch — the
        // inherited operator lowers to the native C one, which is what makes the
        // operator half of this feature cost zero emission change.
        let (_ast2, info2) = analyze_full(
            "distinct Tag = i64\n\
             fn main() -> i32 {\n\
             \x20   let s: Tag = 1 as Tag\n\
             \x20   let t: Tag = 2 as Tag\n\
             \x20   print_bool(s == t)\n\
             \x20   return 0\n\
             }",
        );
        assert!(
            !dispatched(&_ast2, &info2),
            "an inherited operator must record no impl dispatch"
        );
    }

    /// **A `distinct` inherits its base's MEMBERS, and a sub-view comes back at the
    /// distinct's own type.**
    ///
    /// `.len` on a `distinct P = str` used to type as `Unknown` and reach gcc as
    /// `'Jestyr_P' has no member named 'j_len'` — `check` said ok. And `p[a..b]`
    /// typing as a bare `str` is what would have forced 21 casts *inside*
    /// `std/path` itself, before a single caller was counted. The substitution rule
    /// (`str: [Range] -> str` inherits as `P: [Range] -> P`) is what takes that 21
    /// to zero.
    #[test]
    fn a_distinct_inherits_its_base_members_and_sub_views() {
        let (ast, info) = analyze_full(
            "distinct P = str\n\
             fn main() -> i32 {\n\
             \x20   let p: P = \"hello\" as P\n\
             \x20   let n: usize = p.len\n\
             \x20   let b: u8 = p[0]\n\
             \x20   let sub: P = p[0..2]\n\
             \x20   return 0\n\
             }",
        );
        // The sub-view is the LAST Index expression in the file; its recorded type
        // must be `P`, not `str` — that is the substitution, and the whole payoff.
        let mut ranged: Option<Ty> = None;
        for (id, ed) in ast.exprs.iter().enumerate() {
            if let ExprKind::Index { index, .. } = &ed.kind {
                if matches!(ast.expr_at(*index).kind, ExprKind::Range { .. }) {
                    ranged = Some(info.expr_types[id].clone());
                }
            }
        }
        assert_eq!(
            ranged.map(|t| format!("{t:?}")),
            Some(format!("{:?}", Ty::Named(0))),
            "`p[0..2]` on a `distinct P = str` must be a `P`"
        );

        // A distinct over a STRUCT projects the struct's declared fields, at their
        // own declared types — reads and writes both.
        let (_a2, d2) = analyze(
            "struct Pt { x: i32, y: i32 }\ndistinct W = Pt\n\
             fn main() -> i32 {\n\
             \x20   var w: W = Pt { x: 3, y: 4 } as W\n\
             \x20   w.x = 9\n\
             \x20   print_int(w.x + w.y)\n\
             \x20   return 0\n\
             }",
        );
        assert!(d2.is_empty(), "a distinct over a struct reads its fields: {d2:?}");

        // NEGATIVE CONTROL: inheritance is of the base's members, not of arbitrary
        // ones — a field the base does not have is still an error.
        let (_a3, d3) = analyze(
            "struct Pt { x: i32 }\ndistinct W = Pt\n\
             fn main() -> i32 {\n\
             \x20   let w: W = Pt { x: 3 } as W\n\
             \x20   print_int(w.nope)\n\
             \x20   return 0\n\
             }",
        );
        assert!(!d3.is_empty(), "a field the BASE lacks must still be refused");
    }

    /// **A cyclic `distinct` declaration must not hang the compiler.**
    ///
    /// `distinct A = B` / `distinct B = A` passes `check` at HEAD (measured), so
    /// every peel of a distinct's base is walking a graph that can contain a loop.
    /// The cap is the safety property; this test is what proves the cap is wired in
    /// rather than merely written — without it, `peel_distinct` spins forever the
    /// first time anyone writes `a + a` on a cyclic type.
    #[test]
    fn a_cyclic_distinct_declaration_terminates() {
        let (_i, _d) = analyze(
            "distinct A = B\ndistinct B = A\n\
             fn main() -> i32 {\n\
             \x20   let x: A = 1 as A\n\
             \x20   let y: A = x + x\n\
             \x20   print_int(0)\n\
             \x20   return 0\n\
             }",
        );
        // Reaching here at all is the assertion: the peel terminated.
    }

    /// **`cptr` is opaque, and every way of looking through it is refused.**
    ///
    /// The module header of `std/file` claims a `cptr` "cannot be dereferenced and
    /// cannot have arithmetic done to it". When that claim was first written it was
    /// **false** — which is §5's "a header comment claiming a property is evidence
    /// the property is false", caught by probing instead of trusting. All three
    /// holes reached gcc rather than the checker:
    ///
    /// * `f.*` fell to the `_ => Ty::Unknown` arm, and `*(void*)` is not valid C;
    /// * `f + 1` took the OTHER operand's numeric type, so `(f + 1).*` type-checked
    ///   as an `i32` deref (and `void*` arithmetic is a GNU extension, not C);
    /// * `let p: *mut u8 = f` was accepted by the "not modelled yet" default.
    ///
    /// The widening direction must stay open in the same breath, because that is how
    /// a buffer reaches `fread` — so the positive controls are not decoration here,
    /// they are the half that keeps the type usable.
    #[test]
    fn a_cptr_is_opaque_in_every_direction_that_matters() {
        let prelude = "extern \"stdio.h\" fn fopen(path: cstr, mode: cstr) -> cptr\n";
        let probe = |body: &str| {
            let (_i, d) = analyze(&format!(
                "{prelude}fn main() -> i32 {{ var f: cptr = fopen(\"x\".cstr, \"rb\".cstr) {body} return 0 }}"
            ));
            d.iter().map(|m| m.message.clone()).collect::<Vec<_>>()
        };

        // REFUSALS.
        assert!(
            probe("unsafe { let x: u8 = f.* }").iter().any(|m| m.contains("cannot be dereferenced")),
            "a bare deref must be refused"
        );
        assert!(
            probe("unsafe { let x: u8 = (f + 1).* }")
                .iter()
                .any(|m| m.contains("arithmetic on it has no meaning")),
            "arithmetic must be refused before the deref can hide it"
        );
        assert!(
            probe("let p: *mut u8 = f").iter().any(|m| m.contains("found `cptr`")),
            "narrowing an opaque handle back to a typed pointer must be refused"
        );
        assert!(
            probe("let s: str = f").iter().any(|m| m.contains("found `cptr`")),
            "`cptr` must not be in the text family — that family converts freely"
        );

        // POSITIVE CONTROLS. Each is something the type would be useless without.
        assert!(probe("let ok: bool = f == null").is_empty(), "comparing to `null` must work");
        assert!(
            probe("var g: cptr = f").is_empty(),
            "a `cptr` must be assignable to a `cptr`"
        );
        assert!(
            probe(
                "var b: *mut u8 = alloc(u8, 4) var h: cptr = b free_ptr(b)"
            )
            .is_empty(),
            "WIDENING a typed pointer to `cptr` must stay open — it is how a buffer reaches `fread`"
        );
    }

    /// **A raw pointer is not a slice, and neither is a fixed array.**
    ///
    /// Both used to pass `check` — the assignability pass judged only
    /// primitive-vs-primitive and accepted everything else — and fail in gcc as
    /// `incompatible type for argument 1`. That is the degrades-to-gcc mode, and it
    /// caught real code twice: `std/test_report` against a changed signature, and
    /// `std/smallvec`'s `let s: []T = a` on its inline array.
    #[test]
    fn a_pointer_or_array_is_not_a_slice() {
        let (_i, d) = analyze(
            "fn takes(read s: []u8) -> usize { return s.len }\n\
             fn main() -> i32 { var raw: *mut u8 = alloc(u8, 4) let n: usize = takes(raw) free_ptr(raw) return 0 }",
        );
        assert!(
            d.iter().any(|m| m.message.contains("found `*mut u8`")),
            "a raw pointer has no length: {d:?}"
        );

        let (_i, d) = analyze(
            "fn takes(read s: []i64) -> usize { return s.len }\n\
             fn main() -> i32 { let a: [4]i64 = [1, 2, 3, 4] let n: usize = takes(a) return 0 }",
        );
        assert!(
            d.iter().any(|m| m.message.contains("found `[4]i64`")),
            "a fixed array is a value, not a fat pointer: {d:?}"
        );

        // …and the explicit construction is of course still fine.
        let (_i, d) = analyze(
            "fn takes(read s: []u8) -> usize { return s.len }\n\
             fn main() -> i32 { var raw: *mut u8 = alloc(u8, 4) let s: []u8 = slice(u8, raw, 4)\n \
             let n: usize = takes(s) free_ptr(raw) return 0 }",
        );
        assert!(d.is_empty(), "`slice(T, p, n)` is the spelling that carries a length: {d:?}");
    }

    /// **The int-conversion rule** (the decision recorded on `assignable`): lossless
    /// widening within one signedness is implicit; narrowing and any change of
    /// signedness need an explicit `as`.
    ///
    /// The corpus measurement behind it: exactly six sites, all `i32 → usize`, all
    /// passing a `-1`-sentinel arena field into a length parameter.
    #[test]
    fn integer_conversions_allow_widening_and_refuse_loss() {
        // Widening within a signedness loses nothing, so it stays implicit.
        let (_i, d) = analyze(
            "fn wide(x: i64) -> i64 { return x }\n\
             fn main() -> i32 { var a: i32 = 5 let r: i64 = wide(a) return 0 }",
        );
        assert!(d.is_empty(), "i32 → i64 is lossless: {d:?}");

        // Narrowing can truncate.
        let (_i, d) = analyze(
            "fn narrow(x: i32) -> i32 { return x }\n\
             fn main() -> i32 { var a: i64 = 5 let r: i32 = narrow(a) return 0 }",
        );
        assert!(d.iter().any(|m| m.message.contains("found `i64`")), "i64 → i32 truncates: {d:?}");

        // A sign change reinterprets even at equal width — the case the six corpus
        // sites were, and the reason equal width is not a free pass.
        let (_i, d) = analyze(
            "fn takes(x: usize) -> usize { return x }\n\
             fn main() -> i32 { var a: i32 = 5 let r: usize = takes(a) return 0 }",
        );
        assert!(
            d.iter().any(|m| m.message.contains("found `i32`")),
            "i32 → usize turns a negative sentinel into a huge length: {d:?}"
        );

        // An explicit `as` is the escape hatch, and it is trusted.
        let (_i, d) = analyze(
            "fn takes(x: usize) -> usize { return x }\n\
             fn main() -> i32 { var a: i32 = 5 let r: usize = takes(a as usize) return 0 }",
        );
        assert!(d.is_empty(), "an explicit cast says the conversion was meant: {d:?}");

        // …and an untyped literal is still writable at any integer type, which is
        // what keeps `var n: usize = 0` from becoming five thousand diagnostics.
        let (_i, d) = analyze("fn main() -> i32 { var n: usize = 0 var m: u8 = 7 return 0 }");
        assert!(d.is_empty(), "literal defaulting is untouched: {d:?}");
    }

    #[test]
    fn assignability_is_checked_at_all_three_positions() {
        // The initializer, the argument and the return expression each get the
        // same treatment. (This is the program from the report that used to exit
        // 0 with no diagnostics at all.)
        let (_i, d) = analyze(
            "fn takes_int(x: i32) -> i32 { return x }\n\
             fn bad_return() -> i32 { return \"hello\" }\n\
             fn main() -> i32 { let d: f64 = 1.5 let y: i32 = d let z: i32 = takes_int(d) return 0 }",
        );
        let msgs: Vec<&str> = d.iter().map(|m| m.message.as_str()).collect();
        assert!(msgs.iter().any(|m| m.starts_with("`y`:")), "initializer: {msgs:?}");
        assert!(msgs.iter().any(|m| m.starts_with("argument `x`")), "argument: {msgs:?}");
        assert!(msgs.iter().any(|m| m.starts_with("return:")), "return: {msgs:?}");
    }

    #[test]
    fn a_wrong_family_is_reported_but_an_explicit_cast_is_accepted() {
        let (_i, d) = analyze("fn f() -> i32 { let n: i32 = 1.5 return n }");
        assert!(d.iter().any(|m| m.message.contains("found `f64`")), "{:?}", d);
        let (_i2, d2) = analyze("fn f() -> i32 { let n: i32 = 1.5 as i32 return n }");
        assert!(d2.is_empty(), "an explicit `as` pins the type: {:?}", d2);
    }

    #[test]
    fn a_literal_adopts_the_expected_numeric_type() {
        // Jestyr has no integer inference variables — `5` types as `i32` flat — so
        // the check must not fire on a literal written at another numeric width.
        for src in [
            "fn f() -> i64 { let n: i64 = 5 return n }",
            "fn f() -> f64 { let n: f64 = 1 return n }",
            "fn f() -> u8 { let n: u8 = -3 + 4 return n }",
        ] {
            let (_i, d) = analyze(src);
            assert!(d.is_empty(), "literal defaulting must be accepted in `{src}`: {:?}", d);
        }
    }

    #[test]
    fn a_literal_operand_makes_the_whole_expression_unjudgeable() {
        // Binary arithmetic adopts its LEFT operand's type, so `0 - hi` infers as
        // `i32` even when `hi: i64`. The program is well-typed; the check must
        // stay out of it rather than report a phantom mismatch.
        let (_i, d) =
            analyze("fn f(hi: i64) -> f64 { let lo: f64 = (0 - hi) - 1 return lo }");
        assert!(d.is_empty(), "a literal-defaulted operand is not judged: {:?}", d);
    }

    /// **The open question is now settled — this test is the inversion its own
    /// previous version asked for.**
    ///
    /// It used to assert that `i32 → usize` was *deliberately not reported*,
    /// because whether it should need an explicit `as` was an open language
    /// question and "the self-hosted sources spell it both ways". Deciding it by
    /// measurement rather than by argument turned out to be cheap: with literal
    /// defaulting already absorbing `var n: usize = 0`, the strict rule costs six
    /// sites in the whole corpus, every one passing a `-1`-sentinel arena field
    /// into a length parameter. So a sign change is now reported, and the sibling
    /// `integer_conversions_allow_widening_and_refuse_loss` states the full rule.
    #[test]
    fn integer_sign_changes_now_need_an_explicit_cast() {
        let (_i, d) = analyze("fn g(i: usize) -> i32 { return 0 }\n\
                               fn f(r: i32) -> i32 { return g(r) }");
        assert!(
            d.iter().any(|m| m.message.contains("found `i32`")),
            "i32 → usize reinterprets a negative value: {d:?}"
        );
        // The same call with the conversion spelled out is accepted.
        let (_i, d) = analyze("fn g(i: usize) -> i32 { return 0 }\n\
                               fn f(r: i32) -> i32 { return g(r as usize) }");
        assert!(d.is_empty(), "{d:?}");
    }

    #[test]
    fn leniency_is_preserved_where_a_type_is_unknown() {
        // An unresolved/external type stays `Opaque`, and a generic parameter is
        // opaque by construction — neither may be judged, or the lenient checker
        // would start rejecting valid programs.
        let (_i, d) = analyze("fn f(x: Unknown1) -> i32 { let y: i32 = x return 0 }");
        assert!(d.is_empty(), "an opaque type is not judged: {:?}", d);
    }

    #[test]
    fn a_mismatched_argument_is_not_reported_on_top_of_an_arity_error() {
        // Wrong arity means the positions do not correspond, so per-argument
        // types would be noise stacked on the real error.
        let (_i, d) = analyze("fn g(a: i32, b: i32) -> i32 { return a }\n\
                               fn f() -> i32 { return g(1.5) }");
        assert_eq!(d.len(), 1, "only the arity error: {:?}", d);
        assert!(d[0].message.contains("expects 2 argument(s)"), "{:?}", d);
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
        let mr = info.method_resolutions().next().expect("a method call was recorded");
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
        let mr = info.method_resolutions().next().expect("a method call was recorded");
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
        let mr = info.method_resolutions().next().expect("a method call was recorded");
        assert_eq!(mr.fn_name, "get");
        assert_eq!(mr.recv_ctor.as_deref(), Some("List"), "resolved to a struct method");
        assert_eq!(mr.type_args, vec![Ty::Prim("i32")]);
    }

    // --- CTFE tier 2: `comptime { … }` ---

    /// A comptime block is typed as *the literal it folds to*, so it is
    /// indistinguishable from having written that literal — which is exactly what
    /// reaches C.
    #[test]
    fn a_comptime_block_types_as_the_value_it_folds_to() {
        for (src, want) in [
            ("fn main() -> i32 { let a = comptime { 2 + 2 } return a }", Ty::Prim("i32")),
            ("fn main() -> i32 { let a = comptime { 3 > 2 } return 0 }", Ty::Prim("bool")),
            ("fn main() -> i32 { let a = comptime { \"x\" + \"y\" } return 0 }", Ty::Prim("str")),
        ] {
            let (ast, info) = analyze_full(src);
            let id = ast
                .exprs
                .iter()
                .position(|e| matches!(e.kind, ExprKind::Comptime(_)))
                .map(|i| ExprId(i as u32))
                .expect("fixture needs a comptime block");
            assert_eq!(*info.type_of(id), want, "{src}");
        }
    }

    /// A comptime block folds anywhere a constant is needed — including an array
    /// length, where the number becomes part of the type itself.
    #[test]
    fn a_comptime_block_is_accepted_as_an_array_length() {
        let (_info, d) =
            analyze("fn main() -> i32 {\n    var xs: [comptime { 2 + 2 }]i32 = [0; 4]\n    return 0\n}\n");
        assert!(!d.iter().any(|x| x.is_error()), "{d:?}");
    }

    /// Refusal quality: every way a comptime block can fail names the reason and
    /// points at the culprit. None of these may be silently treated as runtime code.
    #[test]
    fn an_unevaluable_comptime_block_is_a_diagnostic_never_a_guess() {
        let cases: [(&str, &str); 6] = [
            ("runtime value", "fn main() -> i32 { var n: i32 = 1\n let a = comptime { n }\n return 0 }"),
            ("division by zero", "fn main() -> i32 { let a = comptime { 1 / 0 }\n return 0 }"),
            ("overflow", "fn main() -> i32 { let a = comptime { 9223372036854775807 + 1 }\n return 0 }"),
            (
                "unbounded recursion",
                "fn f(n: i64) -> i64 { return f(n + 1) }\nfn main() -> i32 { let a = comptime { f(0) }\n return 0 }",
            ),
            ("float", "fn main() -> i32 { let a = comptime { 1.5 as i64 }\n return 0 }"),
            // Produces no value — refused rather than typed as unit, because a pure
            // block that yields nothing is dead code the author did not mean to write.
            ("no value", "fn main() -> i32 { let a = comptime { let x = 1 }\n return 0 }"),
        ];
        for (label, src) in cases {
            let (_info, d) = analyze(src);
            let msgs: Vec<&str> = d.iter().filter(|x| x.is_error()).map(|x| x.message.as_str()).collect();
            assert!(!msgs.is_empty(), "{label}: expected a diagnostic, got none");
            assert!(
                msgs.iter().any(|m| m.contains("comptime")),
                "{label}: diagnostic should name `comptime`: {msgs:?}"
            );
        }
    }

    /// Determinism: the same source yields the same folded type and the same
    /// diagnostics, every time. The interpreter holds no cross-run state, and this
    /// pins that it stays that way.
    #[test]
    fn comptime_folding_is_deterministic() {
        let src = "const N: i64 = 6\nfn tri(n: i64) -> i64 { if n <= 0 { return 0 }\n return n + tri(n - 1) }\n\
                   fn main() -> i32 { let a = comptime { tri(N) * 2 } return 0 }";
        let first = analyze(src);
        for _ in 0..8 {
            let (_i, d) = analyze(src);
            assert_eq!(
                d.iter().map(|x| x.message.clone()).collect::<Vec<_>>(),
                first.1.iter().map(|x| x.message.clone()).collect::<Vec<_>>()
            );
        }
    }

    /// **An `impl` method BODY is type-checked at all.**
    ///
    /// `check_items` used to skip `Item::Impl` outright, on a comment claiming the
    /// bodies were "checked in Stage B (against the trait)". Stage B is
    /// `register_impls`, which reads only the SIGNATURES — coherence, membership,
    /// fallibility conformance, the recorded return types — and never touches
    /// `m.body`. So an impl body accepted literally anything: no arity, no
    /// assignability, no exhaustiveness, no resolution.
    ///
    /// Each case is a PAIR: the ill-typed body must be diagnosed and its well-typed
    /// twin must stay clean, so the refusal cannot pass because the whole file is
    /// rejected for some other reason. And each error is one the identical body in a
    /// FREE fn has always been refused for — the impl body was the only place it was
    /// invisible.
    #[test]
    fn an_impl_method_body_is_type_checked() {
        let base = "struct A { n: i32 } \
                    fn takes_two(a: i32, b: i32) -> i32 { return a + b } \
                    trait T { fn get(read self) -> i32 } ";
        // Wrong arity, inside the impl body.
        let (_, d) = analyze(&format!(
            "{base}impl T for A {{ fn get(read self) -> i32 {{ return takes_two(1) }} }}"
        ));
        assert!(
            d.iter().any(|x| x.is_error() && x.message.contains("takes_two")),
            "a wrong-arity call in an impl body must be refused: {d:?}"
        );
        // The well-typed twin — the positive control for the case above.
        let (_, d) = analyze(&format!(
            "{base}impl T for A {{ fn get(read self) -> i32 {{ return takes_two(1, 2) }} }}"
        ));
        assert!(!d.iter().any(|x| x.is_error()), "the correct call must stay clean: {d:?}");
        // An unknown FIELD on `self`, which also pins that `self` types as the impl
        // target rather than staying unknown (an unknown receiver would say nothing).
        let (_, d) = analyze(&format!(
            "{base}impl T for A {{ fn get(read self) -> i32 {{ return self.nope }} }}"
        ));
        assert!(
            d.iter().any(|x| x.is_error() && x.message.contains("nope")),
            "`self` must be typed as the impl target, so a bad field is refused: {d:?}"
        );
        let (_, d) = analyze(&format!(
            "{base}impl T for A {{ fn get(read self) -> i32 {{ return self.n }} }}"
        ));
        assert!(!d.iter().any(|x| x.is_error()), "the real field must stay clean: {d:?}");
    }

    /// A **blanket** `impl[T] …`'s body is checked too, with `self` typed as the
    /// impl target — `Deque(T)`, not `Unknown` — which is what makes a field access
    /// on `self` resolve inside a generic container's `drop`.
    #[test]
    fn a_blanket_impls_body_types_self_as_the_target() {
        let src = "fn Holder(comptime T: type) -> type { return struct { n: i32 } } \
                   trait Drop { fn drop(mut self) } \
                   impl[T] Drop for Holder(T) { fn drop(mut self) { self.nope = 1 } }";
        let (_, d) = analyze(src);
        assert!(
            d.iter().any(|x| x.is_error() && x.message.contains("nope")),
            "a blanket impl body is checked against the target type: {d:?}"
        );
        let ok = "fn Holder(comptime T: type) -> type { return struct { n: i32 } } \
                  trait Drop { fn drop(mut self) } \
                  impl[T] Drop for Holder(T) { fn drop(mut self) { self.n = 1 } }";
        let (_, d) = analyze(ok);
        assert!(!d.iter().any(|x| x.is_error()), "the real field must stay clean: {d:?}");
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
