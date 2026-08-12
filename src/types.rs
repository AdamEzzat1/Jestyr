//! Type representation and the global declaration table (stages ③–④).
//!
//! `Ty` is the checker's internal notion of a type — distinct from the AST's
//! *syntax* of a type (`TypeKind`). The type checker lowers each AST type to a
//! `Ty`, infers a `Ty` for every expression, and records them in [`TypeInfo`],
//! which later passes (notably the escape checker) query.
//!
//! ## Leniency, on purpose
//! There is no standard library yet, so an unknown *named* type (`Allocator`) or
//! a generic parameter (`T`) lowers to [`Ty::Opaque`] rather than producing an
//! "unresolved type" error. That keeps real-but-incomplete programs (like the
//! generic `Vec`) quiet while still letting the checker reason structurally.
//! `Opaque` is treated as **non-`Copy`**, which is the correct conservative
//! choice: for a generic `T` you must assume moves, so a borrow of `T` still
//! can't escape.

use std::collections::{HashMap, HashSet};

use crate::ast::{Conv, ExprId, PtrMut};
use crate::module::ModId;

/// The canonical symbol name of a top-level item — what the global table is keyed
/// on and what codegen mangles into a C symbol.
///
/// It is the item's **bare name** unless that name is defined in more than one
/// module (`dup`), in which case it is disambiguated with the owning module's id
/// (`make` → `make__m3`). The crucial property: for any name that is *not*
/// actually duplicated, `canon == name`, so every single-module program — and
/// every collision-free multi-module program — keys and mangles exactly as
/// before (byte-identical C). Disambiguation only fires for a genuine collision,
/// which is new capability the flat name pool could not express at all.
///
/// Shared by the type checker (table keys + resolution) and the backend (symbol
/// emission) so the two never disagree on a symbol's name.
pub fn canon(modid: ModId, name: &str, dup: &HashSet<String>) -> String {
    if dup.contains(name) {
        format!("{name}__m{modid}")
    } else {
        name.to_string()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ty {
    Unit,
    /// A primitive, identified by its canonical name: `i32`, `usize`, `f64`,
    /// `bool`, `char`, `str`, …
    Prim(&'static str),
    Ptr { mutbl: PtrMut, inner: Box<Ty> },
    /// A user struct or enum: an index into [`GlobalTable::types`].
    Named(usize),
    /// An unresolved-but-named type or a generic type parameter. Non-`Copy`.
    Opaque(String),
    /// A fallible result `T !E` — carries the *ok* type `T` and the callee's
    /// declared error-set names (sorted, so two mentions of one callee compare
    /// equal). The set is CARRIED, NOT DISPLAYED: `display` renders `T!` exactly
    /// as before, because the P3 typeck golden compares renderings against the
    /// port corpus-wide — the set becomes visible only through the soundness
    /// diagnostics (`err` membership, `?` inclusion; error-payloads E2,
    /// `docs/error-payloads.md` §6). Non-`Copy`.
    Result(Box<Ty>, Vec<String>),
    /// An applied generic struct, e.g. `List(i32)`. Non-`Copy`.
    GenStruct { ctor: String, args: Vec<Ty> },
    /// An applied generic enum, e.g. `Option(i32)`. Non-`Copy` (unless a niche
    /// instance, but the escape checker treats it conservatively). The `ctor`
    /// names a generic `enum` declaration; `args` are its concrete type arguments.
    GenEnum { ctor: String, args: Vec<Ty> },
    /// A slice `[]T` — a fat pointer `{ptr, len}`. Non-`Copy` (it borrows data).
    Slice(Box<Ty>),
    /// A fixed-size array `[N]T` — a *value* type of `N` elements (lowered to a C
    /// `struct { T a[N]; }`). `Copy` iff its element type is `Copy`.
    Array { elem: Box<Ty>, len: usize },
    /// A generational reference `&T` — `{ptr, gen}` (§4.4). `Copy`: it may be
    /// freely stored/aliased; a stale deref faults at runtime, not at compile time.
    GenRef(Box<Ty>),
    /// A region reference `&[r]T` (§4.4) — lowers to a plain pointer (zero-cost,
    /// raw deref). Safety is compile-time/lexical: it can't outlive its region.
    RegionRef(Box<Ty>),
    /// A thin function-pointer type `fn(T1, T2) -> R`. **`Copy`** — one machine
    /// pointer, captures nothing, so it may be freely stored/returned/aliased
    /// and *escapes freely* (the escape checker keeps borrow-capturing closures
    /// second-class, but a bare fn-pointer is first-class). Each parameter
    /// carries its passing [`Conv`]; `ret` is the (possibly `Unit`) result type.
    Fn { params: Vec<(Conv, Box<Ty>)>, ret: Box<Ty>, ret_conv: Conv },
    /// A task handle `Task(T)` — the result of `spawn f(…)` whose target returns
    /// `T`, joined by `await` to yield that `T`. Non-`Copy`: a one-shot handle
    /// (each task is joined once). It never materializes as a runtime value — the
    /// backend resolves `spawn`/`await` to thread vars inside the `concurrent` scope.
    Task(Box<Ty>),
    /// The type of types (a `comptime` value), e.g. the result of `type`.
    TypeKw,
    /// Inference gave up here. Treated as `Copy` so we don't raise false escapes.
    Unknown,
    Error,
}

impl Ty {
    /// Is a value of this type duplicated implicitly (so moving it out of a
    /// borrow is a copy, not an escape)? This is the predicate the escape
    /// checker consults to refine its rule to *non-`Copy`* borrows.
    pub fn is_copy(&self, tbl: &GlobalTable) -> bool {
        match self {
            Ty::Unit => true,
            // A `str` view borrows; an owned `String`/`Builder` owns heap — none Copy.
            Ty::Prim(n) => !matches!(*n, "str" | "String" | "Builder"),
            Ty::Ptr { .. } => true, // raw pointers are Copy
            Ty::Named(i) => tbl.types.get(*i).map(|t| t.is_copy).unwrap_or(false),
            Ty::Opaque(_) => false, // generic/external: assume non-Copy (conservative & correct generically)
            Ty::Result(..) => false,
            Ty::GenStruct { .. } => false,
            Ty::GenEnum { .. } => false,
            Ty::Slice(_) => false,
            Ty::Array { elem, .. } => elem.is_copy(tbl), // a value array: Copy iff its element is
            Ty::GenRef(_) => true, // a generational reference is a copyable fat pointer
            Ty::RegionRef(_) => true, // a region reference is a copyable plain pointer
            Ty::Fn { .. } => true, // a thin fn-pointer captures nothing — first-class, escapes freely
            Ty::Task(_) => false,  // a one-shot task handle: joined once, not duplicated
            Ty::TypeKw => true,
            Ty::Unknown => true, // lenient: suppress escapes we couldn't type
            Ty::Error => true,   // suppress cascades
        }
    }

    /// A human-readable form for diagnostics.
    #[allow(dead_code)] // used once type-mismatch diagnostics land
    pub fn display(&self, tbl: &GlobalTable) -> String {
        match self {
            Ty::Unit => "()".to_string(),
            Ty::Prim(n) => n.to_string(),
            Ty::Ptr { mutbl, inner } => {
                let m = match mutbl {
                    PtrMut::Mut => "*mut ",
                    PtrMut::Const => "*const ",
                    PtrMut::Default => "*",
                };
                format!("{m}{}", inner.display(tbl))
            }
            Ty::Named(i) => tbl.types.get(*i).map(|t| t.name.clone()).unwrap_or_else(|| "?".to_string()),
            Ty::Opaque(n) => n.clone(),
            Ty::Result(ok, _) => format!("{}!", ok.display(tbl)),
            Ty::GenStruct { ctor, args } | Ty::GenEnum { ctor, args } => {
                let a: Vec<String> = args.iter().map(|t| t.display(tbl)).collect();
                format!("{ctor}({})", a.join(", "))
            }
            Ty::Slice(t) => format!("[]{}", t.display(tbl)),
            Ty::Array { elem, len } => format!("[{len}]{}", elem.display(tbl)),
            Ty::GenRef(t) => format!("&{}", t.display(tbl)),
            Ty::RegionRef(t) => format!("&[r]{}", t.display(tbl)),
            Ty::Fn { params, ret, ret_conv } => {
                let ps: Vec<String> = params
                    .iter()
                    .map(|(c, t)| {
                        let label = c.label();
                        if label.is_empty() {
                            t.display(tbl)
                        } else {
                            format!("{label} {}", t.display(tbl))
                        }
                    })
                    .collect();
                let rc = ret_conv.label();
                let rcs = if rc.is_empty() { String::new() } else { format!("{rc} ") };
                format!("fn({}) -> {rcs}{}", ps.join(", "), ret.display(tbl))
            }
            Ty::Task(inner) => format!("Task({})", inner.display(tbl)),
            Ty::TypeKw => "type".to_string(),
            Ty::Unknown => "?".to_string(),
            Ty::Error => "<error>".to_string(),
        }
    }
}

/// Map a primitive type name to its canonical `'static` spelling.
pub fn prim_ty(name: &str) -> Option<&'static str> {
    Some(match name {
        "i8" => "i8",
        "i16" => "i16",
        "i32" => "i32",
        "i64" => "i64",
        "isize" => "isize",
        "u8" => "u8",
        "u16" => "u16",
        "u32" => "u32",
        "u64" => "u64",
        "usize" => "usize",
        "f32" => "f32",
        "f64" => "f64",
        "bool" => "bool",
        "char" => "char",
        "str" => "str",
        "cstr" => "cstr",
        "os_str" => "os_str",
        "String" => "String",
        "Builder" => "Builder",
        "Cow" => "Cow",
        _ => return None,
    })
}

pub fn is_numeric(t: &Ty) -> bool {
    matches!(t, Ty::Prim(n) if n.starts_with('i') || n.starts_with('u') || n.starts_with('f'))
}

#[derive(Debug)]
pub enum TypeKindG {
    Struct { fields: Vec<(String, Ty)> },
    Enum { variants: Vec<(String, Vec<Ty>)> },
    /// `distinct UserId = u64` — a zero-cost nominal wrapper over `base`. Same
    /// representation, *not* interchangeable with it (Haskell `newtype` / Odin
    /// `distinct`). Convert with an explicit `as`.
    Distinct { base: Ty },
}

#[derive(Debug)]
pub struct TypeDecl {
    pub name: String,
    pub kind: TypeKindG,
    /// User aggregates are non-`Copy` by default (an explicit opt-in lands later).
    pub is_copy: bool,
    /// Declared with `record` rather than `struct` — its fields are immutable
    /// (assigning one is a compile error). Layout/representation is identical.
    pub is_record: bool,
    /// Generic type-parameter names, for a generic `enum Option(T) { … }`. Empty
    /// for a plain type. Drives instantiation inference + monomorphization.
    pub type_params: Vec<String>,
}

#[derive(Debug)]
pub struct ParamSig {
    pub name: String,
    pub conv: Conv,
    #[allow(dead_code)] // read once argument-vs-parameter type checking lands
    pub ty: Ty,
}

#[derive(Debug)]
pub struct FnSig {
    pub params: Vec<ParamSig>,
    /// The *ok* return type (for a fallible fn this is `T`, not `T !E`).
    pub ret: Ty,
    #[allow(dead_code)] // read once callers check returned-borrow lifetimes
    pub ret_conv: Conv,
    /// The declared error set (`!{ … }`): `Some(names)` for a fallible function
    /// (sorted, deduped), `None` for an infallible one. The names feed the
    /// soundness diagnostics and ride into `Ty::Result` at every call.
    pub errs: Option<Vec<String>>,
}

/// A declared `trait`: the set of its method names, each flagged required (no
/// default body) or defaulted. Coherence uses this to check an `impl` provides
/// every required method.
#[derive(Debug, Default)]
pub struct TraitDef {
    /// (method name, is-required).
    pub methods: Vec<(String, bool)>,
    /// Method name → its declared error set (sorted, deduped), for the methods
    /// that have one (trait-errors T1). The TRAIT's set is what a call through
    /// the trait is typed by; impl conformance is set inclusion (⊆).
    pub method_errs: std::collections::HashMap<String, Vec<String>>,
}

impl TraitDef {
    pub fn has_method(&self, name: &str) -> bool {
        self.methods.iter().any(|(m, _)| m == name)
    }
    pub fn required(&self) -> impl Iterator<Item = &str> {
        self.methods.iter().filter(|(_, req)| *req).map(|(m, _)| m.as_str())
    }
}

/// A registered `impl Trait for Type`: which trait, a canonical key for the
/// target type (see [`GlobalTable::ty_key`]), and each provided method's return
/// type (with `Self` already resolved to the target).
#[derive(Debug)]
pub struct ImplDef {
    pub trait_name: String,
    pub type_key: String,
    pub method_rets: HashMap<String, Ty>,
}

/// How a trait-method call `recv.m(args)` resolved — recorded for the backend.
/// Stage C lowers it to a direct call of the mangled impl-method function.
#[derive(Clone, Debug)]
pub struct ImplCall {
    pub trait_name: String,
    pub type_key: String,
    pub method: String,
}

/// A method call on a **bracket type parameter**, resolved through its bound (the
/// "Zig fix"): inside `f[T: Tr]`, `x.m()` on a `T` value calls `Tr`'s `m`. The
/// concrete impl isn't known at type-check time (`T` is abstract), so the backend
/// selects it per monomorphized instance by looking `type_param` up in the active
/// type substitution and dispatching to `impl <trait_name> for <that type>`.
#[derive(Clone, Debug)]
pub struct BoundMethodCall {
    pub trait_name: String,
    pub method: String,
    pub type_param: String,
}


/// All top-level declarations, indexed by name — the output of name resolution.
#[derive(Default)]
pub struct GlobalTable {
    pub types: Vec<TypeDecl>,
    pub type_index: HashMap<String, usize>,
    pub fns: HashMap<String, FnSig>,
    pub consts: HashMap<String, Ty>,
    /// enum-variant name → its enum's index in `types`.
    pub variants: HashMap<String, usize>,
    /// trait name → its method set (for coherence + method resolution).
    pub traits: HashMap<String, TraitDef>,
    /// every `impl Trait for Type` in the program.
    pub impls: Vec<ImplDef>,
    /// `(trait, type-key)` → index into `impls` — the single-pass coherence map
    /// (at most one impl per pair) and the resolution lookup.
    pub impl_index: HashMap<(String, String), usize>,
}

impl GlobalTable {
    /// A canonical, stable string key for a type, used to index `impl`s and to
    /// check coherence. Primitives and named types are their name; everything
    /// else falls back to its display form.
    pub fn ty_key(&self, t: &Ty) -> String {
        match t {
            Ty::Prim(n) => (*n).to_string(),
            Ty::Named(i) => self.types.get(*i).map(|d| d.name.clone()).unwrap_or_default(),
            other => other.display(self),
        }
    }
}

/// How a `base.name(args)` method call resolved (filled in by the type checker
/// for every `Call` whose callee is a `Field`). Later passes read this instead
/// of re-deriving the receiver-to-function match.
#[derive(Clone, Debug)]
pub struct MethodRes {
    /// The resolved free function (item A) or `<Ctor>::<method>` struct method.
    pub fn_name: String,
    /// For a struct method: the generic struct constructor it belongs to.
    pub recv_ctor: Option<String>,
    /// Comptime type arguments inferred from the receiver (empty if non-generic).
    pub type_args: Vec<Ty>,
    /// Convention of the receiver parameter (decides whether to pass `&recv`).
    pub recv_conv: Conv,
}

/// Source-region tables for emitting C `#line N "file.jtr"` debug directives.
///
/// A `Span` is a byte offset into the *concatenated* multi-file source buffer;
/// to map it back to a `(file, line)` we need each source region's path, its own
/// text, and its base offset in that buffer — exactly the per-region arrays the
/// module loader already keeps for diagnostic rendering ([`crate::module::Modules`]).
/// We copy them here so the backend, which only sees a [`TypeInfo`], can resolve a
/// span without taking a `Modules` argument (`cgen::emit` has a wide call surface).
///
/// **Empty by default.** The single-file unit-test path ([`crate::typeck::check`]
/// via `Modules::single`) leaves `srcs` empty, so [`span_to_file_line`] returns
/// `None` and the backend emits no `#line` — keeping that path's emitted C
/// byte-identical. Only the real loader path (`check_program`) populates it.
///
/// [`span_to_file_line`]: DebugInfo::span_to_file_line
#[derive(Default)]
pub struct DebugInfo {
    /// Display path of each source region (1:1 with `Modules::paths`).
    paths: Vec<String>,
    /// Each region's own source text — needed to count newlines for a line number.
    srcs: Vec<String>,
    /// Each region's base offset within the concatenated global source buffer.
    bases: Vec<usize>,
    /// Per region, a line-start table. Built once with the tables; lets
    /// [`span_to_file_line`] binary-search for a line instead of counting newlines
    /// from byte 0 on every call.
    ///
    /// [`span_to_file_line`]: DebugInfo::span_to_file_line
    line_index: Vec<crate::span::LineIndex>,
}

impl DebugInfo {
    /// Build the region tables from the loaded modules (the loader path). The
    /// arrays are 1:1 with `Modules`'s per-region vectors.
    pub fn new(paths: Vec<String>, srcs: Vec<String>, bases: Vec<usize>) -> DebugInfo {
        let line_index = srcs.iter().map(|s| crate::span::LineIndex::new(s)).collect();
        DebugInfo { paths, srcs, bases, line_index }
    }

    /// Resolve a global span to `(file path, 1-based line)`, or `None` when there
    /// is no region info (empty tables — the single-file unit-test path) or the
    /// span falls outside every region (a synthesized span). Mirrors
    /// `Modules::region_of`'s base-offset range lookup, then a binary search of
    /// *that region's* line table so an imported file gets its own line, not the
    /// root's. Pure and side-effect-free: `#line` never changes program behavior.
    ///
    /// The line lookup is `O(log lines)`. It used to call [`crate::span::line_col`],
    /// which counts newlines from byte 0 and is `O(offset)` — fine for diagnostics
    /// (rare), but the backend calls this once per function *and* once per statement
    /// via `cgen::mark_line`, so the cost grew with each item's position in the file
    /// and made code generation quadratic in program size. The result is identical:
    /// `partition_point` counts the line starts at or before `local`, which equals
    /// `1 + (newlines before local)` — exactly what `line_col` returned. That
    /// equivalence now lives in [`crate::span::LineIndex`], shared with the
    /// diagnostic renderer and the token dump.
    pub fn span_to_file_line(&self, span: crate::span::Span) -> Option<(&str, u32)> {
        let at = span.start as usize;
        for r in 0..self.bases.len() {
            let lo = self.bases[r];
            let hi = lo + self.srcs[r].len();
            if at >= lo && at <= hi {
                let local = (at - lo) as u32;
                let line = self.line_index[r].line_col(&self.srcs[r], local).line;
                return Some((&self.paths[r], line));
            }
        }
        None
    }
}

/// Everything the type checker decided about **one expression**, beyond its type:
/// one row of Jestyr's HIR (see [`TypeInfo`]).
///
/// Every field records a resolution the backend cannot re-derive from the AST.
/// All are `None` for the overwhelming majority of expressions — a literal, a
/// binary operator or a local read resolves to nothing — so only expressions that
/// actually resolved to *something* get an entry in [`TypeInfo::resolved`].
///
/// The six *call* resolutions are mutually exclusive in practice — a `Call`
/// resolves as exactly one of method / impl / bound-method / dyn / qualified /
/// colliding-symbol — but that is a property of the checker's dispatch order,
/// not an invariant this type enforces.
///
/// **`dyn_coercion` is not exclusive with them, and that is load-bearing.** It is
/// keyed on the *coerced value*, which is often an argument or a returned
/// expression, and that expression may itself be a call: passing `make()` where a
/// `dyn Shape` is expected records both the call's resolution and the coercion on
/// the same `ExprId`. So the checker's writers fill in one field of an existing
/// row (`entry(id).or_default()`) rather than inserting a fresh `Resolved` —
/// replacing a row would silently drop whichever resolution was recorded first
/// and emit a call without its fat-pointer wrap. Pinned by
/// `typeck::tests::a_call_coerced_to_dyn_keeps_both_resolutions`.
#[derive(Clone, Debug, Default)]
pub struct Resolved {
    /// An *unqualified* direct call (`make(a)`) → the canonical name of the
    /// function it resolved to, recorded **only** when that differs from the bare
    /// callee name (i.e. the name collides across modules). The backend prefers
    /// this over the AST's bare name so a within-module call to a duplicated name
    /// targets the right C symbol; absent for every non-colliding call, keeping
    /// the emitted C byte-identical there.
    pub call_sym: Option<String>,
    /// For a `base.name(args)` call: its method resolution.
    pub method: Option<MethodRes>,
    /// Module-qualified access, resolved to the target's *canonical* name (see
    /// [`canon`] — the bare name unless it collides across modules): set on a
    /// `Call` expr (`mem.allocate(x)`) or a `Field` expr (`mem.PAGE_SIZE`). The
    /// backend emits a direct reference, not a field access / method call
    /// (design §9, qualified access).
    pub qualified: Option<String>,
    /// For a `recv.m(args)` call that resolved through an `impl Trait for
    /// <recv-type>`: the trait-impl method resolution (traits, Stage B).
    pub impl_call: Option<ImplCall>,
    /// For a method call on a bracket type parameter resolved through its bound
    /// (the "Zig fix"): the backend selects the concrete impl per monomorphized
    /// instance via the active type substitution.
    pub bound_method: Option<BoundMethodCall>,
    /// The trait this expression coerces to as `dyn Trait` (traits, Stage F): the
    /// backend wraps the value into a `{ data, vtable }` fat pointer, picking the
    /// vtable for the value's concrete type (from `type_of(expr)`).
    pub dyn_coercion: Option<String>,
    /// For a `dyn Trait` call: the method name, dispatched through the vtable slot
    /// (the trait is implicit in the receiver's fat-pointer type).
    pub dyn_call: Option<String>,
}

/// The result of type checking: the global table plus a type for every
/// expression (indexed by `ExprId`).
///
/// # This is Jestyr's HIR
///
/// Worth saying out loud, because the shape used to hide it: `expr_types` plus
/// [`resolved`] **are** a high-level intermediate representation. Both are keyed
/// by `ExprId`, both record decisions the type checker made that the backend
/// cannot re-derive, and `cgen` reads both. An AST node plus its `Resolved` row is
/// a resolved node; the collection of rows is a resolved tree.
///
/// So the often-proposed "add a HIR between typeck and cgen" is not a new layer —
/// it is **collecting the one that already exists**. Stage 1 of that collection is
/// done: seven separate `HashMap<ExprId, …>` columns (`call_sym`, `method_calls`,
/// `qualified`, `impl_calls`, `bound_method_calls`, `dyn_coercions`, `dyn_calls`)
/// are now one row-wise map behind the accessors below. Every pass read them as
/// point lookups and none iterated them, so the transpose changed no emitted C —
/// it cost nothing in corpus goldens, attest hashes, or bootstrap-seed churn, and
/// owed no port mirror.
///
/// Later stages — moving desugaring into HIR construction, then pointing `escape`
/// and `cgen` at it — do change output and therefore *do* owe a port mirror.
///
/// [`resolved`]: TypeInfo::resolved
///
/// Staging and rationale: `docs/frontend-roadmap.md` §5.
pub struct TypeInfo {
    pub table: GlobalTable,
    pub expr_types: Vec<Ty>,
    /// Source-region tables for `#line` debug directives (empty on the
    /// single-file unit-test path, so its emitted C is byte-identical).
    pub debug: DebugInfo,
    /// The owning module of each item in `Ast::items` (parallel vector), so the
    /// backend can compute a definition's canonical symbol via [`canon`].
    pub item_mod: Vec<ModId>,
    /// Top-level function/const names defined in more than one module — the set
    /// that drives [`canon`] disambiguation. Empty for any collision-free
    /// program, so the backend's symbol emission is unchanged in that case.
    pub dup_fns: HashSet<String>,
    /// Non-generic **type** names (struct / enum / distinct) defined in more than
    /// one module — drives [`canon`] for the `Jestyr_<type>` C symbol so two
    /// modules can each define `Slot`. Empty for any collision-free program (and
    /// `TypeDecl::name` already holds the canonical form), so type-symbol emission
    /// is byte-identical there.
    pub dup_types: HashSet<String>,
    /// Enum **variant** names defined in more than one module — drives [`canon`]
    /// for the backend's variant→enum lookup so two modules' same-named variants
    /// don't alias. Empty for any collision-free program.
    pub dup_variants: HashSet<String>,
    /// Per-module import bindings (binding name → target module), so the backend can
    /// resolve a `mod.Type` path to the right module's (possibly colliding) type.
    pub imports: Vec<std::collections::HashMap<String, ModId>>,
    /// Expr id → everything the type checker resolved about it (see [`Resolved`]),
    /// indexed by `ExprId` exactly like `expr_types`. `None` for every expression
    /// that resolved to nothing, which is most of them.
    ///
    /// **Why a dense `Vec` and not a `HashMap`.** The fold was measured against
    /// the seven-map version it replaced, and a map lost: sparse columns are cheap
    /// to *miss*, because `HashMap::get` short-circuits on an empty table. A
    /// single-module program left `qualified`, `impl_calls`, `dyn_calls` and the
    /// rest empty, so most of `escape`'s and `cgen`'s per-call probes cost
    /// essentially nothing; folding them into one populated map turned every one
    /// of those free misses into a real hash-and-probe (`selfbench`: escape +20%,
    /// total +2.9%). Indexing a `Vec` is cheaper than any of it, and the `Box`
    /// keeps the row itself — seven `Option`s, a few hundred bytes — out of the
    /// spine, so the per-expression cost is one pointer.
    ///
    /// Read through the accessors below rather than directly, so a later HIR
    /// stage can change the storage again without touching call sites. That
    /// indirection is what made *this* change a twenty-line edit.
    pub resolved: Vec<Option<Box<Resolved>>>,
    /// Error names that carry a payload → the payload's type (error-payloads E3;
    /// D1 makes this whole-program). Empty for every payload-free program, which
    /// is the backend's gate: no entry here ⇒ not one byte of payload machinery
    /// in the emitted C. BTreeMap so the backend's union emission is
    /// deterministic without a second sort.
    pub err_payloads: std::collections::BTreeMap<String, Ty>,
}

impl TypeInfo {
    pub fn type_of(&self, id: ExprId) -> &Ty {
        self.expr_types.get(id.0 as usize).unwrap_or(&Ty::Unknown)
    }

    // --- the resolution accessors: one per `Resolved` field ---
    //
    // Each is a point lookup returning `None` when the expression resolved to
    // nothing of that kind. Passes go through these instead of touching
    // `resolved` so the storage stays a private decision — it was seven separate
    // maps before HIR Stage 1, then one map, now a dense `Vec`, and every one of
    // those moves was invisible here.

    /// This expression's resolution row, if it has one. Out-of-range ids answer
    /// `None` rather than panicking, matching `type_of`'s leniency.
    fn row(&self, id: ExprId) -> Option<&Resolved> {
        self.resolved.get(id.0 as usize)?.as_deref()
    }

    /// The canonical symbol of an unqualified call whose name collides across
    /// modules — `None` for every non-colliding call.
    pub fn call_sym(&self, id: ExprId) -> Option<&str> {
        self.row(id)?.call_sym.as_deref()
    }

    /// How a `base.name(args)` call resolved.
    pub fn method_call(&self, id: ExprId) -> Option<&MethodRes> {
        self.row(id)?.method.as_ref()
    }

    /// The canonical function/const symbol behind a module-qualified access.
    pub fn qualified(&self, id: ExprId) -> Option<&str> {
        self.row(id)?.qualified.as_deref()
    }

    /// How a `recv.m(args)` call resolved through an `impl Trait for <recv-type>`.
    pub fn impl_call(&self, id: ExprId) -> Option<&ImplCall> {
        self.row(id)?.impl_call.as_ref()
    }

    /// How a method call on a bracket type parameter resolved through its bound.
    pub fn bound_method_call(&self, id: ExprId) -> Option<&BoundMethodCall> {
        self.row(id)?.bound_method.as_ref()
    }

    /// The trait this expression coerces to as `dyn Trait`.
    pub fn dyn_coercion(&self, id: ExprId) -> Option<&str> {
        self.row(id)?.dyn_coercion.as_deref()
    }

    /// The vtable-dispatched method name of a `dyn Trait` call.
    pub fn dyn_call(&self, id: ExprId) -> Option<&str> {
        self.row(id)?.dyn_call.as_deref()
    }

    /// The **canonical symbol** a call resolved to, whichever way it was written:
    /// a module-qualified call (`m.f(…)`) records it in `qualified`, a bare call
    /// to a *colliding* name records it in `call_sym` — the two are disjoint by
    /// construction (a `Field` callee vs a `Name` callee). `None` means the call
    /// is a bare name that collides with nothing, i.e. the AST's spelling *is*
    /// the canonical symbol, and the caller supplies it as the fallback.
    ///
    /// This is the one lookup every pass that asks "which function does this
    /// call target" must make **in full** — consulting `qualified` alone misses
    /// within-module calls to colliding names, whose `table.fns` key is the
    /// canonical `name__m<id>`, not the bare spelling. The escape checker's
    /// take/no-alloc/deterministic/frozen checks each hand-rolled that chain
    /// without the `call_sym` half and silently skipped exactly those calls
    /// (the port never had the gap: its loader renames collisions in the source
    /// text, so its bare spelling is already canonical).
    pub fn resolved_call_target(&self, id: ExprId) -> Option<&str> {
        let r = self.row(id)?;
        r.qualified.as_deref().or(r.call_sym.as_deref())
    }

    // --- whole-program iteration: tests only, on purpose ---
    //
    // These serve tests that assert *what* a program resolved to without knowing
    // the expression ids. They are `#[cfg(test)]` so that no emitting pass can
    // grow a dependency on iteration order — which is a property of whichever
    // storage `resolved` currently uses, not a contract. Under the `HashMap` this
    // fold briefly used, iterating in `cgen` would have made the generated C
    // depend on hash order and silently broken byte-identity against the
    // self-hosted toolchain; the dense `Vec` happens to iterate in source order
    // instead. Gating them keeps that difference from ever mattering, and keeps
    // the storage free to change again.

    /// Every module-qualified access in the program.
    #[cfg(test)]
    pub fn qualified_targets(&self) -> impl Iterator<Item = &str> {
        self.resolved.iter().flatten().filter_map(|r| r.qualified.as_deref())
    }

    /// Every `base.name(args)` method resolution in the program.
    #[cfg(test)]
    pub fn method_resolutions(&self) -> impl Iterator<Item = &MethodRes> {
        self.resolved.iter().flatten().filter_map(|r| r.method.as_ref())
    }

    /// Would moving/returning/storing this expression's value be a *move* (an
    /// escape for a borrow) rather than a *copy*?
    pub fn is_non_copy(&self, id: ExprId) -> bool {
        !self.type_of(id).is_copy(&self.table)
    }
}
