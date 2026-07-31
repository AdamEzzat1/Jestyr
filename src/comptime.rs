//! Compile-time expression evaluation (roadmap workstream G).
//!
//! A small, **total** interpreter over the AST: it folds integer/bool/string
//! expressions at check time, including calls of pure functions. It is the engine
//! behind every place the language needs a *value* rather than an emitted
//! expression — today `[N]T` array lengths and `[v; N]` repeat counts, later
//! `comptime` blocks/consts and reflection.
//!
//! ## Why an interpreter rather than "fold what C would fold"
//! Codegen can hand most constant expressions to the C compiler and let it fold
//! them. It cannot do that where the *compiler itself* needs the number — an array
//! length becomes part of a C type name (`JestyrArr_i32_4`) and of Jestyr's own
//! type (`Ty::Array { len }`), so it must be known during checking. Before this
//! module those sites accepted only an integer literal and silently fell back to
//! `0`, which turned `[SIZE]i32` into a zero-length array and emitted C that
//! asserted on every access. Evaluation replaces that silent wrong answer with
//! either the right one or a diagnostic.
//!
//! ## Totality
//! A comptime interpreter is a place a compiler can hang or blow its stack on
//! perfectly ordinary-looking input (`const A = B` / `const B = A`, or a deep
//! recursion). Three bounds keep evaluation total, so a bad program gets a
//! diagnostic rather than a hung build:
//!  * a **step budget** ([`FUEL`]) decremented on every expression,
//!  * a **call-depth cap** ([`MAX_DEPTH`]),
//!  * **cycle detection** across `const` references.
//!
//! Anything this module cannot evaluate — a float, a struct, an intrinsic, an
//! effectful call — is an [`EvalError`], never a guess. Callers decide whether
//! that is a hard error (an array length) or simply "not a constant" (a fold that
//! was only an optimization).

use std::collections::{HashMap, HashSet};

use crate::ast::*;
use crate::span::Span;

/// The total number of expression evaluations one top-level `eval` may perform.
/// Generous for real constant folding, small enough that a pathological input
/// fails in microseconds.
const FUEL: u32 = 100_000;

/// The maximum nesting of comptime function calls.
const MAX_DEPTH: u32 = 64;

/// A comptime value. Deliberately small: the value domain grows one increment at
/// a time, and everything outside it is a clean error rather than a coercion.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Int(i64),
    Bool(bool),
    Str(String),
    /// No value — a statement-position `if` with no `else`, or a block that ends
    /// in a binding. Kept as a real value (rather than an error) so the *consumer*
    /// decides whether a missing value is a problem.
    Unit,
}

impl Value {
    /// A non-negative integer as a `usize` — the array-length shape.
    pub fn as_usize(&self) -> Option<usize> {
        match self {
            Value::Int(i) if *i >= 0 => Some(*i as usize),
            _ => None,
        }
    }

    /// The name used in diagnostics ("expected an integer, found a bool").
    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Int(_) => "an integer",
            Value::Bool(_) => "a bool",
            Value::Str(_) => "a string",
            Value::Unit => "no value",
        }
    }
}

/// Why an expression could not be evaluated at compile time. `span` points at the
/// sub-expression that failed, not the whole constant, so a caller's diagnostic
/// lands on the actual culprit.
#[derive(Clone, Debug)]
pub struct EvalError {
    pub message: String,
    pub span: Span,
}

impl EvalError {
    fn new(message: impl Into<String>, span: Span) -> EvalError {
        EvalError { message: message.into(), span }
    }
}

type EvalResult = Result<Value, EvalError>;

/// One lexical scope's bindings, innermost last.
type Env = Vec<Vec<(String, Value)>>;

/// The comptime interpreter over one program's AST.
pub struct Interp<'a> {
    ast: &'a Ast,
    /// Top-level `const` initializers, by name.
    consts: HashMap<String, ExprId>,
    /// Top-level functions with a body, by name — the callable surface.
    fns: HashMap<String, &'a FnDecl>,
    /// Top-level `struct`/`record`/`union` bodies, by name — the surface the
    /// reflection intrinsics read (tier 3).
    structs: HashMap<String, &'a StructBody>,
    /// Consts currently being evaluated, so a cycle is reported rather than looped.
    in_progress: HashSet<String>,
    /// Set when a `return` fires, and carried up through every enclosing block
    /// until the call that owns it consumes it. Saved and restored across calls,
    /// so a callee's `return` can never escape into its caller.
    returning: Option<Value>,
    fuel: u32,
    depth: u32,
}

impl<'a> Interp<'a> {
    /// Build an interpreter over `ast`'s top-level items. Cheap: it indexes the
    /// consts and functions once and evaluates nothing.
    pub fn new(ast: &'a Ast) -> Interp<'a> {
        let mut consts = HashMap::new();
        let mut fns = HashMap::new();
        let mut structs = HashMap::new();
        for item in &ast.items {
            match item {
                Item::Const(c) => {
                    consts.insert(c.name.name.clone(), c.value);
                }
                Item::Fn(f) => {
                    fns.insert(f.name.name.clone(), f);
                }
                // `record` and `union` share the `Struct` item and the field grammar,
                // so all three reflect identically.
                Item::Struct { name, body, .. } => {
                    structs.insert(name.name.clone(), body);
                }
                _ => {}
            }
        }
        Interp {
            ast,
            consts,
            fns,
            structs,
            in_progress: HashSet::new(),
            returning: None,
            fuel: FUEL,
            depth: 0,
        }
    }

    /// Evaluate one expression to a comptime value. Each call gets a fresh step
    /// budget, so evaluating many constants can't exhaust a shared one.
    pub fn eval(&mut self, id: ExprId) -> EvalResult {
        self.fuel = FUEL;
        self.depth = 0;
        self.in_progress.clear();
        self.returning = None;
        let mut env: Env = vec![Vec::new()];
        self.eval_expr(id, &mut env)
    }

    /// Evaluate to a non-negative integer — the array-length / repeat-count shape.
    pub fn eval_usize(&mut self, id: ExprId) -> Result<usize, EvalError> {
        let span = self.ast.expr_at(id).span;
        let v = self.eval(id)?;
        match v.as_usize() {
            Some(n) => Ok(n),
            None => Err(EvalError::new(
                format!("expected a non-negative integer, found {}", v.type_name()),
                span,
            )),
        }
    }

    /// Look up a top-level `const` by name and evaluate it.
    pub fn eval_const(&mut self, name: &str) -> EvalResult {
        match self.consts.get(name).copied() {
            Some(init) => self.eval(init),
            None => Err(EvalError::new(format!("no `const {name}` in this file"), Span::new(0, 0))),
        }
    }

    /// Call a top-level pure function with already-computed arguments (roadmap G
    /// tier 4). The build driver needs this: it must evaluate `source(0)`,
    /// `source(1)`, … where the indices come from the *driver*, not from the source
    /// text, and synthesising call expressions into the AST to do that would be a
    /// worse answer than a small entry point.
    ///
    /// Identical in every other respect to a call the interpreter makes itself: a
    /// fresh budget, the same depth cap, and a frame that sees only its parameters.
    pub fn call_fn(&mut self, name: &str, args: &[Value]) -> EvalResult {
        let zero = Span::new(0, 0);
        let Some(f) = self.fns.get(name).copied() else {
            return Err(EvalError::new(format!("no `fn {name}` in this file"), zero));
        };
        if f.params.iter().any(|p| p.is_self || p.comptime) {
            return Err(EvalError::new(format!("`{name}` cannot be called at compile time"), f.name.span));
        }
        if f.params.len() != args.len() {
            return Err(EvalError::new(
                format!("`{name}` takes {} argument(s) but {} were given", f.params.len(), args.len()),
                f.name.span,
            ));
        }
        self.fuel = FUEL;
        self.depth = 0;
        self.in_progress.clear();
        self.returning = None;
        let bound: Vec<(String, Value)> =
            f.params.iter().zip(args).map(|(p, v)| (p.name.name.clone(), v.clone())).collect();
        let mut env: Env = vec![bound];
        let out = self.eval_block(&f.body, &mut env);
        let returned = self.returning.take();
        Ok(returned.unwrap_or(out?))
    }

    fn spend(&mut self, span: Span) -> Result<(), EvalError> {
        match self.fuel.checked_sub(1) {
            Some(f) => {
                self.fuel = f;
                Ok(())
            }
            None => Err(EvalError::new(
                "compile-time evaluation exceeded its step budget (is it non-terminating?)",
                span,
            )),
        }
    }

    fn eval_expr(&mut self, id: ExprId, env: &mut Env) -> EvalResult {
        let e = self.ast.expr_at(id);
        let span = e.span;
        self.spend(span)?;
        match &e.kind {
            ExprKind::Int(text) => match parse_int_literal(text) {
                Some(v) => Ok(Value::Int(v)),
                None => Err(EvalError::new("integer literal is out of range", span)),
            },
            ExprKind::Bool(b) => Ok(Value::Bool(*b)),
            ExprKind::Str(lit) => Ok(Value::Str(unescape_str(lit))),
            ExprKind::Char(lit) => match char_value(lit) {
                Some(c) => Ok(Value::Int(c as i64)),
                None => Err(EvalError::new("malformed character literal", span)),
            },
            ExprKind::Name(n) => self.eval_name(&n.name, span, env),
            ExprKind::Unary { op, rhs } => {
                let v = self.eval_expr(*rhs, env)?;
                self.eval_unary(*op, v, span)
            }
            ExprKind::Binary { op, lhs, rhs } => self.eval_binary(*op, *lhs, *rhs, span, env),
            ExprKind::Cast { expr, ty } => {
                // Integer-to-integer casts pass the value through; the defined
                // narrowing/overflow semantics are workstream J's, not this one's,
                // so anything else is refused rather than guessed.
                let v = self.eval_expr(*expr, env)?;
                match (&v, &self.ast.type_at(*ty).kind) {
                    (Value::Int(_), TypeKind::Name(n)) if is_int_type(&n.name) => Ok(v),
                    _ => Err(EvalError::new(
                        "this cast is not supported at compile time",
                        span,
                    )),
                }
            }
            ExprKind::If { cond, then, els } => {
                let c = self.eval_expr(*cond, env)?;
                match c {
                    // An `if` with no `else` is a statement: it runs its block for
                    // effect (which may `return`) and yields no value of its own.
                    Value::Bool(true) => {
                        let v = self.eval_block(then, env)?;
                        Ok(if els.is_some() { v } else { Value::Unit })
                    }
                    Value::Bool(false) => match els {
                        Some(e) => self.eval_expr(*e, env),
                        None => Ok(Value::Unit),
                    },
                    other => Err(EvalError::new(
                        format!("`if` condition must be a bool, found {}", other.type_name()),
                        span,
                    )),
                }
            }
            // `comptime { … }` is *already* being evaluated at compile time, so it is
            // simply its block. Handling it here rather than only at the call site is
            // what makes a nested one (`comptime { comptime { 1 } + 1 }`) work, and
            // what lets a comptime block appear inside a `const` initializer.
            ExprKind::Comptime(b) | ExprKind::Block(b) => self.eval_block(b, env),
            ExprKind::Call { callee, args } => self.eval_call(*callee, args, span, env),
            _ => Err(EvalError::new("this expression is not a compile-time constant", span)),
        }
    }

    fn eval_name(&mut self, name: &str, span: Span, env: &mut Env) -> EvalResult {
        for scope in env.iter().rev() {
            if let Some((_, v)) = scope.iter().rev().find(|(n, _)| n == name) {
                return Ok(v.clone());
            }
        }
        let Some(&init) = self.consts.get(name) else {
            return Err(EvalError::new(
                format!("`{name}` is not a compile-time constant"),
                span,
            ));
        };
        if !self.in_progress.insert(name.to_string()) {
            return Err(EvalError::new(
                format!("constant `{name}` is defined in terms of itself"),
                span,
            ));
        }
        // A const initializer is evaluated in its own (empty) scope: it cannot see
        // the locals of whatever expression referred to it.
        let mut fresh: Env = vec![Vec::new()];
        let out = self.eval_expr(init, &mut fresh);
        self.in_progress.remove(name);
        out
    }

    fn eval_unary(&mut self, op: UnOp, v: Value, span: Span) -> EvalResult {
        match (op, &v) {
            (UnOp::Neg, Value::Int(i)) => match i.checked_neg() {
                Some(n) => Ok(Value::Int(n)),
                None => Err(EvalError::new("negation overflowed at compile time", span)),
            },
            (UnOp::Not, Value::Bool(b)) => Ok(Value::Bool(!b)),
            (UnOp::BitNot, Value::Int(i)) => Ok(Value::Int(!i)),
            _ => Err(EvalError::new(
                format!("this operator does not apply to {} at compile time", v.type_name()),
                span,
            )),
        }
    }

    fn eval_binary(&mut self, op: BinOp, lhs: ExprId, rhs: ExprId, span: Span, env: &mut Env) -> EvalResult {
        // `and`/`or` short-circuit, so the right side is evaluated only if needed —
        // which also means `false and <not-constant>` is still a constant.
        if matches!(op, BinOp::And | BinOp::Or) {
            let l = self.eval_expr(lhs, env)?;
            let Value::Bool(lb) = l else {
                return Err(EvalError::new(
                    format!("`{}` needs a bool, found {}", op_text(op), l.type_name()),
                    span,
                ));
            };
            if (op == BinOp::And && !lb) || (op == BinOp::Or && lb) {
                return Ok(Value::Bool(lb));
            }
            let r = self.eval_expr(rhs, env)?;
            return match r {
                Value::Bool(rb) => Ok(Value::Bool(rb)),
                other => Err(EvalError::new(
                    format!("`{}` needs a bool, found {}", op_text(op), other.type_name()),
                    span,
                )),
            };
        }

        let l = self.eval_expr(lhs, env)?;
        let r = self.eval_expr(rhs, env)?;
        match (&l, &r) {
            (Value::Int(a), Value::Int(b)) => self.int_binop(op, *a, *b, span),
            (Value::Str(a), Value::Str(b)) => match op {
                BinOp::Add => Ok(Value::Str(format!("{a}{b}"))),
                BinOp::Eq => Ok(Value::Bool(a == b)),
                BinOp::Ne => Ok(Value::Bool(a != b)),
                BinOp::Lt => Ok(Value::Bool(a < b)),
                BinOp::Le => Ok(Value::Bool(a <= b)),
                BinOp::Gt => Ok(Value::Bool(a > b)),
                BinOp::Ge => Ok(Value::Bool(a >= b)),
                _ => Err(EvalError::new(
                    format!("`{}` does not apply to strings", op_text(op)),
                    span,
                )),
            },
            (Value::Bool(a), Value::Bool(b)) => match op {
                BinOp::Eq => Ok(Value::Bool(a == b)),
                BinOp::Ne => Ok(Value::Bool(a != b)),
                _ => Err(EvalError::new(
                    format!("`{}` does not apply to bools", op_text(op)),
                    span,
                )),
            },
            _ => Err(EvalError::new(
                format!(
                    "cannot apply `{}` to {} and {} at compile time",
                    op_text(op),
                    l.type_name(),
                    r.type_name()
                ),
                span,
            )),
        }
    }

    /// Integer arithmetic. Every operation is *checked*: a comptime overflow is a
    /// compile error, never a silent wrap. (Runtime overflow semantics are
    /// workstream J's; a constant the compiler folds must simply be right.)
    fn int_binop(&mut self, op: BinOp, a: i64, b: i64, span: Span) -> EvalResult {
        let arith = |o: Option<i64>| match o {
            Some(v) => Ok(Value::Int(v)),
            None => Err(EvalError::new("this arithmetic overflowed at compile time", span)),
        };
        match op {
            BinOp::Add => arith(a.checked_add(b)),
            BinOp::Sub => arith(a.checked_sub(b)),
            BinOp::Mul => arith(a.checked_mul(b)),
            BinOp::Div => {
                if b == 0 {
                    Err(EvalError::new("division by zero at compile time", span))
                } else {
                    arith(a.checked_div(b))
                }
            }
            BinOp::Rem => {
                if b == 0 {
                    Err(EvalError::new("remainder by zero at compile time", span))
                } else {
                    arith(a.checked_rem(b))
                }
            }
            BinOp::Shl | BinOp::Shr => {
                let Ok(sh) = u32::try_from(b) else {
                    return Err(EvalError::new("shift amount is negative", span));
                };
                let r = if op == BinOp::Shl { a.checked_shl(sh) } else { a.checked_shr(sh) };
                match r {
                    Some(v) => Ok(Value::Int(v)),
                    None => Err(EvalError::new("shift amount is too large", span)),
                }
            }
            BinOp::BitAnd => Ok(Value::Int(a & b)),
            BinOp::BitOr => Ok(Value::Int(a | b)),
            BinOp::BitXor => Ok(Value::Int(a ^ b)),
            BinOp::Eq => Ok(Value::Bool(a == b)),
            BinOp::Ne => Ok(Value::Bool(a != b)),
            BinOp::Lt => Ok(Value::Bool(a < b)),
            BinOp::Le => Ok(Value::Bool(a <= b)),
            BinOp::Gt => Ok(Value::Bool(a > b)),
            BinOp::Ge => Ok(Value::Bool(a >= b)),
            BinOp::And | BinOp::Or => unreachable!("short-circuited above"),
        }
    }

    /// A block's value is its tail expression (otherwise [`Value::Unit`]); `let`
    /// bindings scope to the block. A `return` anywhere inside sets `returning`,
    /// which stops evaluation here and propagates to the enclosing call.
    fn eval_block(&mut self, b: &Block, env: &mut Env) -> EvalResult {
        env.push(Vec::new());
        let out = self.eval_stmts(b, env);
        env.pop();
        out
    }

    fn eval_stmts(&mut self, b: &Block, env: &mut Env) -> EvalResult {
        let mut tail = Value::Unit;
        for (i, st) in b.stmts.iter().enumerate() {
            match st {
                Stmt::Let { name, init, span, .. } => {
                    let Some(init) = init else {
                        return Err(EvalError::new("a compile-time `let` needs an initializer", *span));
                    };
                    let v = self.eval_expr(*init, env)?;
                    env.last_mut().expect("a scope is always open").push((name.name.clone(), v));
                    tail = Value::Unit;
                }
                Stmt::Return { value, span } => {
                    let v = match value {
                        Some(value) => self.eval_expr(*value, env)?,
                        None => Value::Unit,
                    };
                    if self.returning.is_none() {
                        self.returning = Some(v.clone());
                    }
                    let _ = span;
                    return Ok(v);
                }
                Stmt::Expr(e) => {
                    let v = self.eval_expr(*e, env)?;
                    // Only a trailing expression is the block's value.
                    tail = if i + 1 == b.stmts.len() { v } else { Value::Unit };
                }
            }
            if self.returning.is_some() {
                // A nested `return` already fixed this call's result.
                return Ok(self.returning.clone().expect("just checked"));
            }
        }
        Ok(tail)
    }

    fn eval_call(&mut self, callee: ExprId, args: &[ExprId], span: Span, env: &mut Env) -> EvalResult {
        // `@name(…)` — a compiler query, not a call. Reflection lives in this
        // `@`-prefixed space rather than beside `size_of`/`align_of`/`offset_of`
        // precisely because those use ordinary identifiers and can therefore be
        // shadowed by a user function of the same name: the self-hosted compiler
        // itself declares `fn field_type(…)` in `examples/std/typeck.jtr`, which a
        // bare-name `field_type` intrinsic would silently have hijacked.
        if let ExprKind::Attr(n) = &self.ast.expr_at(callee).kind {
            let name = n.name.clone();
            if is_reflect_intrinsic(&name) {
                return self.eval_reflect(&name, args, span, env);
            }
            return Err(EvalError::new(format!("`@{name}` is not a compile-time query"), span));
        }
        let ExprKind::Name(n) = &self.ast.expr_at(callee).kind else {
            return Err(EvalError::new("only a named function can be called at compile time", span));
        };
        let Some(f) = self.fns.get(n.name.as_str()).copied() else {
            return Err(EvalError::new(
                format!("`{}` is not a function that can run at compile time", n.name),
                span,
            ));
        };
        // Parameters that erase (`self`, `comptime`) have no runtime argument, so a
        // method or a type-level generic is out of scope for this increment.
        if f.params.iter().any(|p| p.is_self || p.comptime) {
            return Err(EvalError::new(
                format!("`{}` cannot be called at compile time", n.name),
                span,
            ));
        }
        if f.params.len() != args.len() {
            return Err(EvalError::new(
                format!(
                    "`{}` takes {} argument(s) but {} were given",
                    n.name,
                    f.params.len(),
                    args.len()
                ),
                span,
            ));
        }
        if self.depth >= MAX_DEPTH {
            return Err(EvalError::new(
                "compile-time call nesting is too deep (is the recursion unbounded?)",
                span,
            ));
        }

        let mut bound = Vec::with_capacity(args.len());
        for (p, a) in f.params.iter().zip(args) {
            bound.push((p.name.name.clone(), self.eval_expr(*a, env)?));
        }
        // A call body sees only its parameters — never the caller's locals — and its
        // `return` belongs to this frame alone, so the caller's signal is saved across.
        let mut callee_env: Env = vec![bound];
        let outer_return = self.returning.take();
        self.depth += 1;
        let out = self.eval_block(&f.body, &mut callee_env);
        self.depth -= 1;
        let returned = self.returning.take();
        self.returning = outer_return;
        let v = out?;
        Ok(returned.unwrap_or(v))
    }
}

/// The compile-time reflection intrinsics (roadmap G tier 3).
///
/// Plain-call syntax with a *type* as the first argument, matching the convention
/// the language already uses for `size_of(T)`, `align_of(T)` and
/// `offset_of(T, field)` — reflection is not a new syntactic category, so it needs
/// no new grammar.
///
/// **What is deliberately absent: sizes, alignments and offsets.** Those three
/// intrinsics exist today only as *C-deferred* ones — they lower to `sizeof`,
/// `_Alignof` and `offsetof`, so the Jestyr compiler never learns the numbers; it
/// asks the C compiler. Turning them into comptime *values* needs the compiler's own
/// layout pass (workstream L). What this tier reflects is what the compiler already
/// knows without it: the **declared shape**.
pub fn is_reflect_intrinsic(name: &str) -> bool {
    matches!(name, "type_name" | "field_count" | "field_name" | "field_type")
}

impl<'a> Interp<'a> {
    /// Evaluate a reflection intrinsic. Field order is **declaration order** — the
    /// order written in the source — which is what makes repeated queries and
    /// generated output stable. (A future `@layout(auto)` from workstream L may
    /// reorder *storage*; it will not reorder this.)
    fn eval_reflect(&mut self, name: &str, args: &[ExprId], span: Span, env: &mut Env) -> EvalResult {
        // The first argument is a TYPE, named directly — it is not evaluated as a
        // value, exactly as `offset_of(Point, y)`'s second argument is a bare field
        // name rather than an expression.
        let Some(&first) = args.first() else {
            return Err(EvalError::new(format!("`{name}` needs a type as its first argument"), span));
        };
        let ExprKind::Name(tn) = &self.ast.expr_at(first).kind else {
            return Err(EvalError::new(
                format!("`{name}`'s first argument must be a named type"),
                self.ast.expr_at(first).span,
            ));
        };
        let tname = tn.name.clone();

        if name == "type_name" {
            // Answerable for any named type, including primitives: it reports what the
            // author wrote, and needs no declaration to do so.
            return Ok(Value::Str(tname));
        }

        let Some(body) = self.structs.get(tname.as_str()).copied() else {
            return Err(EvalError::new(
                format!("`{tname}` is not a struct this compiler can reflect over"),
                self.ast.expr_at(first).span,
            ));
        };
        // Methods are not fields; only the declared data shape is reflected.
        let fields: Vec<(&Ident, TypeId)> = body
            .members
            .iter()
            .filter_map(|m| match m {
                StructMember::Field { name, ty, .. } => Some((name, *ty)),
                _ => None,
            })
            .collect();

        if name == "field_count" {
            return Ok(Value::Int(fields.len() as i64));
        }

        // The remaining two are indexed. The index is an ordinary comptime expression,
        // so `@field_name(P, I)` for a `const I` works — but it must *evaluate*, and an
        // out-of-range one is an error rather than a clamp or an empty string.
        let Some(&idx_expr) = args.get(1) else {
            return Err(EvalError::new(format!("`{name}` needs a field index as its second argument"), span));
        };
        let idx_span = self.ast.expr_at(idx_expr).span;
        let idx = match self.eval_expr(idx_expr, env)? {
            Value::Int(i) if i >= 0 => i as usize,
            other => {
                return Err(EvalError::new(
                    format!("a field index must be a non-negative integer, found {}", other.type_name()),
                    idx_span,
                ))
            }
        };
        let Some(&(fname, fty)) = fields.get(idx) else {
            return Err(EvalError::new(
                format!("`{tname}` has {} field(s); there is no field {idx}", fields.len()),
                idx_span,
            ));
        };
        match name {
            "field_name" => Ok(Value::Str(fname.name.clone())),
            // Rendered by the same function the documentation generator uses, so a
            // reflected type name can never disagree with the documented one.
            _ => Ok(Value::Str(crate::doc::ty_str(self.ast, fty))),
        }
    }
}

/// Is `name` an integer primitive? (The set a comptime cast may pass through.)
fn is_int_type(name: &str) -> bool {
    matches!(
        name,
        "i8" | "i16" | "i32" | "i64" | "isize" | "u8" | "u16" | "u32" | "u64" | "usize"
    )
}

fn op_text(op: BinOp) -> &'static str {
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
        BinOp::And => "and",
        BinOp::Or => "or",
        BinOp::BitAnd => "&",
        BinOp::BitOr => "|",
        BinOp::BitXor => "^",
        BinOp::Shl => "<<",
        BinOp::Shr => ">>",
    }
}

/// Parse an integer literal's source text: decimal, `0x`/`0X` hex, or `0b`/`0B`
/// binary, with `_` separators ignored. `None` on overflow or malformed text.
pub fn parse_int_literal(text: &str) -> Option<i64> {
    let t: String = text.chars().filter(|c| *c != '_').collect();
    if let Some(hex) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        i64::from_str_radix(hex, 16).ok()
    } else if let Some(bin) = t.strip_prefix("0b").or_else(|| t.strip_prefix("0B")) {
        i64::from_str_radix(bin, 2).ok()
    } else {
        t.parse::<i64>().ok()
    }
}

/// Decode a string literal's source text (quotes included) to its value.
fn unescape_str(lit: &str) -> String {
    let inner = lit.strip_prefix('"').and_then(|s| s.strip_suffix('"')).unwrap_or(lit);
    unescape(inner)
}

/// Decode a char literal's source text (quotes included) to its code point.
fn char_value(lit: &str) -> Option<char> {
    let inner = lit.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')).unwrap_or(lit);
    let decoded = unescape(inner);
    let mut cs = decoded.chars();
    let c = cs.next()?;
    cs.next().is_none().then_some(c)
}

/// Resolve the escape sequences Jestyr's lexer accepts. An unknown escape keeps
/// its char, matching the lexer's own tolerance.
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut cs = s.chars();
    while let Some(c) = cs.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match cs.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('0') => out.push('\0'),
            Some('\\') => out.push('\\'),
            Some('\'') => out.push('\''),
            Some('"') => out.push('"'),
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::Lexer;
    use crate::parser::Parser;

    /// Parse `src` and evaluate the initializer of its LAST `const`.
    fn eval_last_const(src: &str) -> Result<Value, EvalError> {
        let (tokens, _) = Lexer::new(src).tokenize();
        let (ast, diags) = Parser::new(src, tokens).parse();
        assert!(diags.iter().all(|d| !d.is_error()), "fixture must parse: {diags:?}");
        let last = ast
            .items
            .iter()
            .rev()
            .find_map(|i| match i {
                Item::Const(c) => Some(c.value),
                _ => None,
            })
            .expect("fixture needs a const");
        Interp::new(&ast).eval(last)
    }

    fn int_of(src: &str) -> i64 {
        match eval_last_const(src).expect("should evaluate") {
            Value::Int(i) => i,
            other => panic!("expected an integer, got {other:?}"),
        }
    }

    fn err_of(src: &str) -> String {
        eval_last_const(src).expect_err("should fail to evaluate").message
    }

    #[test]
    fn folds_arithmetic_with_correct_precedence() {
        assert_eq!(int_of("const A: i64 = 2 + 3 * 4\n"), 14);
        assert_eq!(int_of("const A: i64 = (2 + 3) * 4\n"), 20);
        assert_eq!(int_of("const A: i64 = 17 % 5\n"), 2);
        assert_eq!(int_of("const A: i64 = 0 - 7 / 2\n"), -3);
    }

    #[test]
    fn folds_every_literal_base_and_separators() {
        assert_eq!(int_of("const A: i64 = 0xFF\n"), 255);
        assert_eq!(int_of("const A: i64 = 0b1010\n"), 10);
        assert_eq!(int_of("const A: i64 = 1_000_000\n"), 1_000_000);
    }

    #[test]
    fn folds_bitwise_and_shifts() {
        assert_eq!(int_of("const A: i64 = 1 << 10\n"), 1024);
        assert_eq!(int_of("const A: i64 = 0xF0 | 0x0F\n"), 255);
        assert_eq!(int_of("const A: i64 = 0xFF ^ 0x0F\n"), 240);
        assert_eq!(int_of("const A: i64 = ~0\n"), -1);
    }

    #[test]
    fn resolves_const_references_transitively() {
        let src = "const A: i64 = 4\nconst B: i64 = A * 2\nconst C: i64 = A + B\n";
        assert_eq!(int_of(src), 12);
    }

    #[test]
    fn evaluates_comparisons_and_short_circuits() {
        assert_eq!(eval_last_const("const A: bool = 3 < 4\n").unwrap(), Value::Bool(true));
        // The right operand of a short-circuited `and` is never evaluated, so a
        // non-constant there does not make the whole expression non-constant.
        assert_eq!(
            eval_last_const("fn f() -> i32 { return 0 }\nconst A: bool = false and f() == 0\n")
                .unwrap(),
            Value::Bool(false)
        );
    }

    #[test]
    fn evaluates_if_expressions() {
        assert_eq!(int_of("const A: i64 = if 2 > 1 { 10 } else { 20 }\n"), 10);
        assert_eq!(int_of("const A: i64 = if 2 < 1 { 10 } else { 20 }\n"), 20);
    }

    #[test]
    fn calls_pure_functions_including_recursion() {
        let src = "fn double(x: i64) -> i64 { return x * 2 }\nconst A: i64 = double(21)\n";
        assert_eq!(int_of(src), 42);
        let fact = "fn fact(n: i64) -> i64 {\n    if n <= 1 { return 1 }\n    return n * fact(n - 1)\n}\nconst A: i64 = fact(10)\n";
        assert_eq!(int_of(fact), 3_628_800);
    }

    #[test]
    fn binds_let_inside_a_called_body() {
        let src = "fn area(w: i64, h: i64) -> i64 {\n    let a = w * h\n    return a + 1\n}\nconst A: i64 = area(3, 4)\n";
        assert_eq!(int_of(src), 13);
    }

    #[test]
    fn folds_strings_and_chars() {
        assert_eq!(
            eval_last_const("const A: str = \"ab\" + \"cd\"\n").unwrap(),
            Value::Str("abcd".to_string())
        );
        // `\n` in the source is an escape, so the value is one byte, not two.
        assert_eq!(eval_last_const("const A: str = \"a\\nb\"\n").unwrap(), Value::Str("a\nb".into()));
        assert_eq!(int_of("const A: i64 = 'A'\n"), 65);
    }

    // --- totality: every bound produces a diagnostic, never a hang ---

    #[test]
    fn a_self_referential_constant_is_reported_not_looped() {
        let e = err_of("const A: i64 = B\nconst B: i64 = A\n");
        assert!(e.contains("defined in terms of itself"), "{e}");
    }

    #[test]
    fn unbounded_recursion_hits_the_depth_cap() {
        let e = err_of("fn f(n: i64) -> i64 { return f(n + 1) }\nconst A: i64 = f(0)\n");
        assert!(e.contains("too deep") || e.contains("step budget"), "{e}");
    }

    #[test]
    fn overflow_and_division_by_zero_are_errors_not_wraps() {
        assert!(err_of("const A: i64 = 9223372036854775807 + 1\n").contains("overflowed"));
        assert!(err_of("const A: i64 = 1 / 0\n").contains("division by zero"));
        assert!(err_of("const A: i64 = 1 % 0\n").contains("remainder by zero"));
    }

    #[test]
    fn non_constant_expressions_are_refused_with_a_reason() {
        assert!(err_of("const A: i64 = read_file(\"x\").len as i64\n").len() > 0);
        let e = err_of("const A: i64 = MISSING\n");
        assert!(e.contains("not a compile-time constant"), "{e}");
        let e2 = err_of("const A: i64 = 1.5 as i64\n");
        assert!(!e2.is_empty(), "a float must not silently evaluate");
    }

    #[test]
    fn a_type_mismatch_is_an_error_rather_than_a_coercion() {
        let e = err_of("const A: i64 = 1 + true\n");
        assert!(e.contains("cannot apply"), "{e}");
    }

    // --- tier 2: `comptime { … }` blocks ---

    #[test]
    fn evaluates_a_comptime_block_like_the_block_it_wraps() {
        assert_eq!(int_of("const A: i64 = comptime { 2 + 2 }\n"), 4);
        // `let` scoping inside the block, then a tail expression.
        assert_eq!(int_of("const A: i64 = comptime { let x = 4\n x * 2 }\n"), 8);
    }

    #[test]
    fn a_comptime_block_nests_and_composes_with_the_rest_of_the_language() {
        // Nesting works because `Comptime` is evaluated by the same arm as `Block` —
        // an inner block is not a special case, it is simply already comptime.
        //
        // Note the shape: the inner block is in *value* position. Written the other way
        // round (`comptime { comptime { 3 } + 1 }`) it is a parse error, because Jestyr
        // parses a block-led form at STATEMENT position as the block alone so that a
        // trailing operator cannot extend it — `unsafe` behaves identically. `comptime`
        // inherits that rule rather than inventing one.
        assert_eq!(int_of("const A: i64 = comptime { 1 + comptime { 3 } }\n"), 4);
        // A block may call a pure function and read another `const`.
        let src = "const N: i64 = 5\nfn sq(x: i64) -> i64 { return x * x }\n\
                   const A: i64 = comptime { sq(N) + 1 }\n";
        assert_eq!(int_of(src), 26);
        // And an ordinary expression may contain one.
        assert_eq!(int_of("const A: i64 = 1 + comptime { 2 * 3 }\n"), 7);
    }

    #[test]
    fn a_comptime_block_yields_bools_strings_and_unit_too() {
        assert_eq!(eval_last_const("const A: bool = comptime { 3 > 2 }\n").unwrap(), Value::Bool(true));
        assert_eq!(
            eval_last_const("const A: str = comptime { \"ab\" + \"cd\" }\n").unwrap(),
            Value::Str("abcd".to_string())
        );
        // A block ending in a binding produces nothing. The interpreter reports that
        // faithfully as `Unit`; refusing it is the *consumer's* call (typeck does).
        assert_eq!(eval_last_const("const A: i64 = comptime { let x = 1 }\n").unwrap(), Value::Unit);
    }

    #[test]
    fn the_totality_bounds_still_hold_inside_a_comptime_block() {
        // Wrapping an expression in `comptime { … }` must not buy it an escape from
        // any of the three bounds — the block is not a separate evaluation mode.
        assert!(err_of("const A: i64 = comptime { 1 / 0 }\n").contains("division by zero"));
        assert!(err_of("const A: i64 = comptime { 9223372036854775807 + 1 }\n").contains("overflowed"));
        let e = err_of("fn f(n: i64) -> i64 { return f(n + 1) }\nconst A: i64 = comptime { f(0) }\n");
        assert!(e.contains("too deep") || e.contains("step budget"), "{e}");
        let e2 = err_of("const A: i64 = comptime { B }\nconst B: i64 = comptime { A }\n");
        assert!(e2.contains("defined in terms of itself"), "{e2}");
    }

    #[test]
    fn a_comptime_block_cannot_reach_runtime_state() {
        // The effect policy is *structural*: nothing effectful is in the value domain,
        // so each of these is refused by the same rule that refuses a float — there is
        // no arm for it. No allowlist to keep in sync, and no way to bypass it.
        for src in [
            "fn main() -> i32 { var n: i64 = 1\n let a = comptime { n }\n return 0 }\n",
            "const A: i64 = comptime { read_file(\"x\").len as i64 }\n",
            "const A: i64 = comptime { unsafe { 1 } }\n",
            "const A: i64 = comptime { 1.5 as i64 }\n",
        ] {
            let (tokens, _) = Lexer::new(src).tokenize();
            let (ast, _) = Parser::new(src, tokens).parse();
            let mut found = None;
            for i in 0..ast.exprs.len() {
                if matches!(ast.exprs[i].kind, ExprKind::Comptime(_)) {
                    found = Some(ExprId(i as u32));
                    break;
                }
            }
            let id = found.expect("fixture needs a comptime block");
            assert!(Interp::new(&ast).eval(id).is_err(), "should be refused: {src}");
        }
    }

    // --- tier 3: reflection over the declared shape ---

    #[test]
    fn reflects_a_structs_declared_shape() {
        let s = "struct Point { x: i32, y: f64, tag: str }\n";
        assert_eq!(int_of(&format!("{s}const A: i64 = @field_count(Point)\n")), 3);
        assert_eq!(
            eval_last_const(&format!("{s}const A: str = @type_name(Point)\n")).unwrap(),
            Value::Str("Point".into())
        );
        // Declaration order, not any storage order — index 1 is what was written second.
        assert_eq!(
            eval_last_const(&format!("{s}const A: str = @field_name(Point, 1)\n")).unwrap(),
            Value::Str("y".into())
        );
        assert_eq!(
            eval_last_const(&format!("{s}const A: str = @field_type(Point, 1)\n")).unwrap(),
            Value::Str("f64".into())
        );
        // `@type_name` answers for a primitive too — it reports what was written, and
        // needs no declaration to do so.
        assert_eq!(
            eval_last_const("const A: str = @type_name(i32)\n").unwrap(),
            Value::Str("i32".into())
        );
    }

    #[test]
    fn reflection_composes_with_the_rest_of_comptime() {
        let s = "struct Pair { a: i32, b: i32 }\n";
        // The index is an ordinary comptime expression, so a `const` works there.
        assert_eq!(
            eval_last_const(&format!("{s}const I: i64 = 1\nconst A: str = @field_name(Pair, I)\n"))
                .unwrap(),
            Value::Str("b".into())
        );
        // And a reflection query composes inside a `comptime` block like any other value.
        assert_eq!(
            int_of(&format!("{s}const A: i64 = comptime {{ @field_count(Pair) * 10 }}\n")),
            20
        );
        // Recursion + string concat is enough to walk every field, so field ITERATION
        // needs no comptime `for` loop *in the evaluator*.
        //
        // It is not yet usable end-to-end, though, and the reason is worth recording:
        // `names` is a top-level `fn`, so cgen emits it as ordinary runtime code too,
        // and there `i` is a parameter rather than a constant — so the query cannot
        // fold and typeck reports it. What closes the gap is **comptime-only
        // functions** (a body instantiated at comptime and never emitted), not the
        // layout pass. Until then, reflection is usable with constant arguments.
        let walk = format!(
            "{s}fn names(i: i64) -> str {{\n\
             \x20   if i >= @field_count(Pair) {{ return \"\" }}\n\
             \x20   return @field_name(Pair, i) + \";\" + names(i + 1)\n\
             }}\n\
             const A: str = comptime {{ names(0) }}\n"
        );
        assert_eq!(eval_last_const(&walk).unwrap(), Value::Str("a;b;".into()));
    }

    /// **Why reflection lives in the `@` namespace.** The obvious design was to sit
    /// beside `size_of`/`align_of`/`offset_of` as ordinary identifiers — but those can
    /// be shadowed by a user function of the same name, and the self-hosted compiler
    /// *already declares* `fn field_type(…)` in `examples/std/typeck.jtr`. A bare-name
    /// intrinsic would have silently hijacked it and broken the compiler's own build.
    /// `@field_type` cannot collide, because no user can declare one.
    #[test]
    fn a_user_function_may_share_a_reflection_intrinsics_name() {
        let src = "struct P { x: i32 }\n\
                   fn field_type(a: i64, b: i64) -> i64 { return a + b }\n\
                   const A: i64 = field_type(20, 22)\n";
        assert_eq!(int_of(src), 42, "the user's function must win for the bare name");
        // And the `@` form still reaches the compiler's query, not that function.
        let src2 = "struct P { x: i32 }\n\
                    fn field_type(a: i64, b: i64) -> i64 { return a + b }\n\
                    const A: str = @field_type(P, 0)\n";
        assert_eq!(eval_last_const(src2).unwrap(), Value::Str("i32".into()));
    }

    #[test]
    fn methods_are_not_fields() {
        let s = "struct Counter { n: i32, fn bump(mut self) { self.n = self.n + 1 } }\n";
        assert_eq!(int_of(&format!("{s}const A: i64 = @field_count(Counter)\n")), 1);
    }

    #[test]
    fn an_unanswerable_reflection_query_is_an_error_not_a_default() {
        let s = "struct P { x: i32 }\n";
        // Out of range — not clamped, not an empty string.
        let e = err_of(&format!("{s}const A: str = @field_name(P, 5)\n"));
        assert!(e.contains("no field 5"), "{e}");
        // A type with no declared shape to read.
        let e2 = err_of("const A: i64 = @field_count(i32)\n");
        assert!(e2.contains("not a struct"), "{e2}");
        // A negative index is not a wrap-around.
        let e3 = err_of(&format!("{s}const A: str = @field_name(P, 0 - 1)\n"));
        assert!(e3.contains("non-negative"), "{e3}");
        // A missing type argument.
        let e4 = err_of(&format!("{s}const A: i64 = @field_count()\n"));
        assert!(e4.contains("needs a type"), "{e4}");
    }

    #[test]
    fn eval_usize_rejects_a_negative_length() {
        let src = "const A: i64 = 0 - 1\n";
        let (tokens, _) = Lexer::new(src).tokenize();
        let (ast, _) = Parser::new(src, tokens).parse();
        let Item::Const(c) = &ast.items[0] else { panic!("expected a const") };
        let e = Interp::new(&ast).eval_usize(c.value).expect_err("negative is not a length");
        assert!(e.message.contains("non-negative"), "{}", e.message);
    }
}
