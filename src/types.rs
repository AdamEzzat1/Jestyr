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

use std::collections::HashMap;

use crate::ast::{Conv, ExprId, PtrMut};

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
    /// A fallible result `T !E` — carries the *ok* type `T`. Non-`Copy`.
    Result(Box<Ty>),
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
            Ty::Result(_) => false,
            Ty::GenStruct { .. } => false,
            Ty::GenEnum { .. } => false,
            Ty::Slice(_) => false,
            Ty::Array { elem, .. } => elem.is_copy(tbl), // a value array: Copy iff its element is
            Ty::GenRef(_) => true, // a generational reference is a copyable fat pointer
            Ty::RegionRef(_) => true, // a region reference is a copyable plain pointer
            Ty::Fn { .. } => true, // a thin fn-pointer captures nothing — first-class, escapes freely
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
            Ty::Result(ok) => format!("{}!", ok.display(tbl)),
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
    /// True if the function declares an error set (`!{ … }`).
    pub fallible: bool,
}

/// A declared `trait`: the set of its method names, each flagged required (no
/// default body) or defaulted. Coherence uses this to check an `impl` provides
/// every required method.
#[derive(Debug, Default)]
pub struct TraitDef {
    /// (method name, is-required).
    pub methods: Vec<(String, bool)>,
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

/// The result of type checking: the global table plus a type for every
/// expression (indexed by `ExprId`).
pub struct TypeInfo {
    pub table: GlobalTable,
    pub expr_types: Vec<Ty>,
    /// `Call`-expr id → its method resolution, for `base.name(args)` calls.
    pub method_calls: HashMap<ExprId, MethodRes>,
    /// Module-qualified access, resolved to the target's *bare* name: a `Call`
    /// expr id (`mem.allocate(x)`) or a `Field` expr id (`mem.PAGE_SIZE`) →
    /// the underlying function/const name. The backend emits a direct reference,
    /// not a field access / method call (design §9, qualified access).
    pub qualified: HashMap<ExprId, String>,
    /// `Call`-expr id → trait-impl method resolution, for `recv.m(args)` calls
    /// that resolved through an `impl Trait for <recv-type>` (traits, Stage B).
    pub impl_calls: HashMap<ExprId, ImplCall>,
    /// `Call`-expr id → a method call on a bracket type parameter resolved through
    /// its bound (the "Zig fix"); the backend selects the concrete impl per
    /// monomorphized instance via the active type substitution.
    pub bound_method_calls: HashMap<ExprId, BoundMethodCall>,
    /// Expr id → the trait name it coerces to as `dyn Trait` (traits, Stage F):
    /// the backend wraps the value into a `{ data, vtable }` fat pointer, picking
    /// the vtable for the value's concrete type (from `type_of(expr)`).
    pub dyn_coercions: HashMap<ExprId, String>,
    /// `Call`-expr id → the method name of a `dyn Trait` call, dispatched through
    /// the vtable slot (the trait is implicit in the receiver's fat-pointer type).
    pub dyn_calls: HashMap<ExprId, String>,
}

impl TypeInfo {
    pub fn type_of(&self, id: ExprId) -> &Ty {
        self.expr_types.get(id.0 as usize).unwrap_or(&Ty::Unknown)
    }

    /// Would moving/returning/storing this expression's value be a *move* (an
    /// escape for a borrow) rather than a *copy*?
    pub fn is_non_copy(&self, id: ExprId) -> bool {
        !self.type_of(id).is_copy(&self.table)
    }
}
