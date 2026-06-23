//! The abstract syntax tree.
//!
//! Nodes live in flat arenas on [`Ast`] and reference each other by small `Copy`
//! integer handles (`ExprId`, `TypeId`, `PatId`) rather than `Box`/`&`. This is
//! the representation production compilers (rustc included) converge on, and it
//! is the same handle/index idiom Jestyr itself favours (design doc §4.6):
//!
//!  * later passes annotate nodes with *parallel* vectors keyed by the same id,
//!    instead of mutating the tree — no aliasing, no `RefCell`;
//!  * the whole tree is a handful of contiguous `Vec`s — cache-friendly;
//!  * every node is cheap to copy around by id.
//!
//! Identifiers and literals carry their source text directly so the tree is
//! self-describing for the pretty-printer (a transient memory cost we happily pay
//! at this stage).

#![allow(dead_code)] // the AST defines the full surface ahead of its consumers

use crate::span::Span;

// --- arena handles ---

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub struct ExprId(pub u32);
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub struct TypeId(pub u32);
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub struct PatId(pub u32);

/// An identifier together with where it came from.
#[derive(Clone, Debug)]
pub struct Ident {
    pub name: String,
    pub span: Span,
}

// --- shared vocabulary ---

/// A parameter-passing convention — the core of Jestyr's ownership model (§4.3).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Conv {
    Default,
    Read,
    Mut,
    Take,
    Out,
}

impl Conv {
    pub fn label(self) -> &'static str {
        match self {
            Conv::Default => "",
            Conv::Read => "read",
            Conv::Mut => "mut",
            Conv::Take => "take",
            Conv::Out => "out",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PtrMut {
    Default,
    Mut,
    Const,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum UnOp {
    Neg,    // -
    Not,    // not / !
    BitNot, // ~
    Ref,    // &
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BinOp {
    Add, Sub, Mul, Div, Rem,
    Eq, Ne, Lt, Le, Gt, Ge,
    And, Or,
    BitAnd, BitOr, BitXor, Shl, Shr,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AssignOp {
    Assign, // =
    Add, Sub, Mul, Div, Rem,
    BitAnd, BitOr, BitXor,
}

// --- types ---

#[derive(Clone, Debug)]
pub enum TypeKind {
    Name(Ident),                              // usize, f64, T, Shape, Self
    TypeKw,                                    // the `type` keyword (the type of types)
    Ptr { mutbl: PtrMut, inner: TypeId },      // *T, *mut T, *const T
    Slice(TypeId),                             // []T — a fat pointer (ptr + len)
    GenRef(TypeId),                            // &T — a generational reference (§4.4)
    RegionRef { region: Ident, inner: TypeId }, // &[r]T — a zero-cost region reference (§4.4)
    App { ctor: Ident, args: Vec<TypeId> },    // List(i32) — applied generic struct
    Error,
}

#[derive(Clone, Debug)]
pub struct TypeData {
    pub kind: TypeKind,
    pub span: Span,
}

// --- patterns ---

#[derive(Clone, Debug)]
pub enum PatKind {
    Wildcard,                                  // _
    Ident(Ident),                              // a binding, or a nullary variant like `none`
    Variant { name: Ident, subpats: Vec<PatId> }, // circle(r), rect(w, h)
    Error,
}

#[derive(Clone, Debug)]
pub struct PatData {
    pub kind: PatKind,
    pub span: Span,
}

// --- expressions ---

#[derive(Clone, Debug)]
pub struct FieldInit {
    pub name: Ident,
    pub value: ExprId,
}

#[derive(Clone, Debug)]
pub struct MatchArm {
    pub pat: PatId,
    /// An optional boolean guard: `circle(r) if r > 0.0 => …`. A guarded arm only
    /// fires when the pattern matches *and* the guard is true — and crucially does
    /// **not** count toward exhaustiveness (the guard could be false at runtime).
    pub guard: Option<ExprId>,
    pub body: ExprId,
}

#[derive(Clone, Debug)]
pub struct ClosureParam {
    pub name: Ident,
    pub ty: Option<TypeId>,
}

#[derive(Clone, Debug)]
pub enum ExprKind {
    // literals (text kept verbatim; value lowering happens later)
    Int(String),
    Float(String),
    Str(String),
    Char(String),
    Bool(bool),
    Null,

    // leaves
    Name(Ident),
    SelfValue, // self
    SelfType,  // Self
    Attr(Ident), // @name (callable: `@address(0x...)`)

    // operators
    Unary { op: UnOp, rhs: ExprId },
    Binary { op: BinOp, lhs: ExprId, rhs: ExprId },
    Assign { op: AssignOp, target: ExprId, value: ExprId },
    Range { lo: Option<ExprId>, hi: Option<ExprId>, inclusive: bool },

    // postfix
    Call { callee: ExprId, args: Vec<ExprId> },
    Field { base: ExprId, name: Ident },
    Deref { base: ExprId }, // base.*
    Try { base: ExprId },   // base?
    Index { base: ExprId, index: ExprId },
    Cast { expr: ExprId, ty: TypeId }, // `expr as T` — an explicit conversion

    // composite
    StructLit { path: Ident, fields: Vec<FieldInit> }, // Self{...}, Foo{...}
    // List(i32){ ... } — generic struct literal; `type_args` are type-valued exprs.
    GenStructLit { ctor: Ident, type_args: Vec<ExprId>, fields: Vec<FieldInit> },
    StructType(StructBody),                            // `struct { ... }` as a value
    Block(Block),
    If { cond: ExprId, then: Block, els: Option<ExprId> },
    Match { scrut: ExprId, arms: Vec<MatchArm> },
    Unsafe(Block),
    Closure { params: Vec<ClosureParam>, body: ExprId },

    /// A structured-concurrency nursery (design §10.2): each `spawn` inside runs
    /// as a task that *must* join before the block exits.
    Concurrent(Block),
    /// `spawn <call>` — launch a task within the enclosing `concurrent` scope.
    Spawn(ExprId),

    /// `region r { … }` — a named arena scope (design §4.4). `&[r]T` references
    /// into it are zero-cost; the whole arena is freed at the block's end.
    Region { name: Ident, body: Block },

    /// A loop — the one looping keyword (design: unified `for`, see
    /// `docs/loops-spec.md`). The header selects the shape; the binding carries a
    /// `read`/`mut` convention, exactly like a parameter. An optional `region`
    /// gives each iteration a fresh scratch arena (reset in O(1) per iteration,
    /// freed once after the loop). An optional `label` (`for outer: …`) is the
    /// target of a labeled `break`/`continue`. An optional `els` block (`for … {
    /// … } else { … }`) runs exactly once *if the loop completes without a
    /// `break`* — the "search-or-default" idiom (Python's loop-`else`). It is
    /// rejected on an infinite loop, whose only exit is `break` (the `else` would
    /// be dead code).
    For { label: Option<Ident>, head: ForHead, region: Option<Ident>, body: Block, els: Option<Block> },
    /// `break` / `break <label>` — exit the nearest enclosing loop, or the loop
    /// named by the label.
    Break(Option<Ident>),
    /// `continue` / `continue <label>` — next iteration of the nearest (or named) loop.
    Continue(Option<Ident>),
    /// `invariant <expr>` — a loop invariant (Ada/SPARK), lowered to a debug
    /// `assert` checked each iteration.
    Invariant(ExprId),
    /// `variant <expr>` — a loop termination measure (Ada/SPARK): a quantity that
    /// must be `>= 0` and strictly decrease each iteration (checked at runtime).
    Variant(ExprId),

    Error,
}

/// One loop binding: a name (possibly `_`) with an ownership convention.
#[derive(Clone, Debug)]
pub struct LoopBind {
    pub conv: Conv,
    pub name: Ident,
}

/// The header of a `for` loop — its shape (design: unified `for`).
#[derive(Clone, Debug)]
pub enum ForHead {
    /// `for { … }` — infinite; exits via `break`.
    Infinite,
    /// `for <cond> { … }` — the "while" job.
    While(ExprId),
    /// `for <binds> in <sources> [step <e>] { … }` — iterate a range or slice(s).
    /// The shape is decided by the counts: 1 bind / 1 source = simple iteration
    /// (range or slice); 2 binds / 1 source = element + index; 2 binds / 2 sources
    /// = lockstep zip. A bind name may be `_` (wildcard). `step` applies to a range
    /// source (a negative literal step descends).
    Iter { binds: Vec<LoopBind>, sources: Vec<ExprId>, step: Option<ExprId> },
}

#[derive(Clone, Debug)]
pub struct ExprData {
    pub kind: ExprKind,
    pub span: Span,
}

// --- statements & blocks ---

#[derive(Clone, Debug)]
pub enum Stmt {
    Let {
        mutbl: bool, // `var` => true, `let` => false
        name: Ident,
        ty: Option<TypeId>,
        init: Option<ExprId>,
        span: Span,
    },
    Return {
        value: Option<ExprId>,
        span: Span,
    },
    Expr(ExprId),
}

#[derive(Clone, Debug)]
pub struct Block {
    pub stmts: Vec<Stmt>,
    pub span: Span,
}

// --- items ---

#[derive(Clone, Debug)]
pub struct Param {
    pub comptime: bool,
    pub conv: Conv,
    pub name: Ident,
    pub is_self: bool,
    pub ty: Option<TypeId>,
    pub refine: Option<ExprId>, // `in <expr>` refinement, e.g. `i: usize in 0..len`
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct ErrorSet {
    pub names: Vec<Ident>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct FnDecl {
    /// `pub fn …` — visible outside its module (design §9). Always `false` for
    /// methods (a method's visibility follows its enclosing struct).
    pub is_pub: bool,
    /// `@no_panic fn …` (design §13) — the compiler must *prove* every faulting
    /// op (today: slice indexing) is fault-free, else it's a compile error. A
    /// cached projection of [`FnDecl::attrs`]; see also [`FnDecl::has_attr`].
    pub no_panic: bool,
    /// Every leading `@name` / `@name(args)` directive on this function (design
    /// §7/§16; roadmap workstream D). Validated by [`crate::attrs`]; consumed by
    /// the backend for optimization/ABI hints (`@inline`, `@cold`, `@no_mangle`,
    /// `@deprecated`, …). The single source of truth — `no_panic` mirrors it.
    pub attrs: Vec<Attribute>,
    pub name: Ident,
    pub params: Vec<Param>,
    pub ret_conv: Conv,
    pub ret_ty: Option<TypeId>,
    pub errors: Option<ErrorSet>,
    /// `requires <expr>` preconditions (design §6.4) — checked on entry.
    pub requires: Vec<ExprId>,
    /// `ensures <expr>` postconditions — checked before return; `result` names
    /// the returned value.
    pub ensures: Vec<ExprId>,
    pub body: Block,
    pub span: Span,
}

impl FnDecl {
    /// Is the attribute `@<name>` present on this function?
    pub fn has_attr(&self, name: &str) -> bool {
        self.attrs.iter().any(|a| a.name == name)
    }

    /// The attribute `@<name>`, if present (e.g. to read `@deprecated`'s message).
    pub fn attr(&self, name: &str) -> Option<&Attribute> {
        self.attrs.iter().find(|a| a.name == name)
    }
}

#[derive(Clone, Debug)]
pub enum StructMember {
    /// `volatile` is set by an `@volatile` field attribute (MMIO; design §16).
    Field { name: Ident, ty: TypeId, volatile: bool, span: Span },
    Method(FnDecl),
}

#[derive(Clone, Debug)]
pub struct StructBody {
    pub members: Vec<StructMember>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct EnumVariant {
    pub name: Ident,
    pub fields: Vec<(Ident, TypeId)>,
    /// An explicit discriminant, e.g. `red = 1` (design §7; Rust/Swift raw values).
    /// Sets the variant's integer tag value — for C-ABI enums, bit flags, and
    /// stable wire formats. `None` lets C assign it sequentially.
    pub discriminant: Option<ExprId>,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct EnumDecl {
    pub is_pub: bool,
    pub name: Ident,
    /// Generic type parameters, e.g. `enum Option(T) { … }` (empty for a plain
    /// enum). A generic enum is a *template* — monomorphized per instantiation,
    /// like a generic struct. (See `docs/structs-enums-design.md` §2.2b.)
    pub type_params: Vec<Ident>,
    pub variants: Vec<EnumVariant>,
    pub span: Span,
}

impl EnumDecl {
    pub fn is_generic(&self) -> bool {
        !self.type_params.is_empty()
    }
}

#[derive(Clone, Debug)]
pub struct ConstDecl {
    pub is_pub: bool,
    pub name: Ident,
    pub ty: Option<TypeId>,
    pub value: ExprId,
    /// `@no_mangle` / `@section(…)` directives (validated by [`crate::attrs`]).
    pub attrs: Vec<Attribute>,
    pub span: Span,
}

impl ConstDecl {
    pub fn has_attr(&self, name: &str) -> bool {
        self.attrs.iter().any(|a| a.name == name)
    }

    pub fn attr(&self, name: &str) -> Option<&Attribute> {
        self.attrs.iter().find(|a| a.name == name)
    }
}

/// A foreign function declared via `extern "c" fn name(...) -> T` — a bodyless
/// signature bound to the C ABI (design §12). Called by its bare C name.
#[derive(Clone, Debug)]
pub struct ExternFn {
    pub is_pub: bool,
    pub abi: String,
    pub name: Ident,
    pub params: Vec<Param>,
    pub ret_conv: Conv,
    pub ret_ty: Option<TypeId>,
    pub span: Span,
}

/// `import "path"` or `import "path" as alias` (design §9). A module is a file;
/// the path is resolved relative to the importing file. The binding defaults to
/// the path's last segment (`"std/mem"` → `mem`) unless an `as` alias is given.
#[derive(Clone, Debug)]
pub struct ImportDecl {
    pub path: String,
    pub alias: Option<Ident>,
    pub span: Span,
}

/// An item-level attribute, e.g. `@packed`, `@align(8)`, `@layout(c)` (design
/// §7/§16). Controls memory layout among other things.
#[derive(Clone, Debug)]
pub struct Attribute {
    pub name: String,
    pub args: Vec<ExprId>,
    pub span: Span,
}

/// `distinct UserId = u64` — a zero-cost nominal wrapper over a base type
/// (Haskell `newtype` / Odin `distinct`; design §2.6). Same representation, not
/// interchangeable with the base; convert with an explicit `as`.
#[derive(Clone, Debug)]
pub struct DistinctDecl {
    pub is_pub: bool,
    pub name: Ident,
    pub base: TypeId,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub enum Item {
    Fn(FnDecl),
    Enum(EnumDecl),
    Const(ConstDecl),
    Distinct(DistinctDecl),
    /// A product type. `is_record` distinguishes an **immutable** `record` (whose
    /// fields cannot be assigned — a static guarantee) from a mutable `struct`.
    /// Both share one field grammar, layout, and C lowering; only the mutation
    /// rule differs (design: struct/record/class split, CJC-inspired §1.2).
    Struct {
        is_pub: bool,
        is_record: bool,
        name: Ident,
        body: StructBody,
        attrs: Vec<Attribute>,
        span: Span,
    },
    Extern(ExternFn),
    Import(ImportDecl),
}

// --- the arena container ---

#[derive(Default)]
pub struct Ast {
    pub items: Vec<Item>,
    pub exprs: Vec<ExprData>,
    pub types: Vec<TypeData>,
    pub pats: Vec<PatData>,
}

impl Ast {
    pub fn new() -> Ast {
        Ast::default()
    }

    pub fn expr(&mut self, kind: ExprKind, span: Span) -> ExprId {
        let id = ExprId(self.exprs.len() as u32);
        self.exprs.push(ExprData { kind, span });
        id
    }

    pub fn ty(&mut self, kind: TypeKind, span: Span) -> TypeId {
        let id = TypeId(self.types.len() as u32);
        self.types.push(TypeData { kind, span });
        id
    }

    pub fn pat(&mut self, kind: PatKind, span: Span) -> PatId {
        let id = PatId(self.pats.len() as u32);
        self.pats.push(PatData { kind, span });
        id
    }

    pub fn expr_at(&self, id: ExprId) -> &ExprData {
        &self.exprs[id.0 as usize]
    }
    pub fn type_at(&self, id: TypeId) -> &TypeData {
        &self.types[id.0 as usize]
    }
    pub fn pat_at(&self, id: PatId) -> &PatData {
        &self.pats[id.0 as usize]
    }
}
