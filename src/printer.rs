//! A pretty-printer that renders an [`Ast`] as an indented tree.
//!
//! Two rendering styles are mixed deliberately:
//!  * declarations, blocks, statements, and *block-like* expressions
//!    (`if`/`match`/`unsafe`/`struct {}`) render as an indented tree, so nesting
//!    is visible;
//!  * small expressions render *inline* in a source-like form with explicit
//!    parentheses, so operator grouping (precedence/associativity) is obvious at
//!    a glance — `1 + 2 * 3` prints as `(1 + (2 * 3))`.
//!
//! The printer only reads the arena, so it copies the shared `&Ast` into a local
//! (`let ast = self.ast;`) before iterating — that decouples the node borrows
//! from `self`, letting it append to the output buffer freely.

use crate::ast::*;

pub fn print_ast(ast: &Ast) -> String {
    let mut p = Printer { ast, out: String::new() };
    p.run();
    p.out
}

struct Printer<'a> {
    ast: &'a Ast,
    out: String,
}

impl<'a> Printer<'a> {
    fn line(&mut self, depth: usize, s: &str) {
        for _ in 0..depth {
            self.out.push_str("  ");
        }
        self.out.push_str(s);
        self.out.push('\n');
    }

    fn run(&mut self) {
        let ast = self.ast;
        self.line(0, &format!("Ast — {} item(s)", ast.items.len()));
        for item in &ast.items {
            self.item(1, item);
        }
    }

    fn item(&mut self, d: usize, item: &Item) {
        match item {
            Item::Fn(f) => self.func(d, f, "fn"),
            Item::Enum(e) => {
                self.line(d, &format!("enum {}", e.name.name));
                for v in &e.variants {
                    self.line(d + 1, &format!("variant {}", v.name.name));
                    for (fname, fty) in &v.fields {
                        let ts = self.type_str(*fty);
                        self.line(d + 2, &format!("{}: {}", fname.name, ts));
                    }
                }
            }
            Item::Const(c) => {
                let ts = c.ty.map(|t| self.type_str(t)).unwrap_or_else(|| "_".to_string());
                self.line(d, &format!("const {}: {} =", c.name.name, ts));
                self.expr(d + 1, c.value);
            }
            Item::Struct { name, body, attrs, is_record, .. } => {
                self.attrs(d, attrs);
                let kw = if *is_record { "record" } else { "struct" };
                self.line(d, &format!("{kw} {}", name.name));
                self.struct_body(d + 1, body);
            }
            Item::Extern(e) => {
                self.line(d, &format!("extern \"{}\" fn {}", e.abi, e.name.name));
                for p in &e.params {
                    let ts = p.ty.map(|t| self.type_str(t)).unwrap_or_else(|| "_".to_string());
                    self.line(d + 1, &format!("param {}: {}", p.name.name, ts));
                }
                if let Some(t) = e.ret_ty {
                    self.line(d + 1, &format!("returns: {}", self.type_str(t)));
                }
            }
            Item::Import(im) => {
                let alias = im.alias.as_ref().map(|a| format!(" as {}", a.name)).unwrap_or_default();
                self.line(d, &format!("import \"{}\"{}", im.path, alias));
            }
            Item::Distinct(dd) => {
                self.line(d, &format!("distinct {} = {}", dd.name.name, self.type_str(dd.base)));
            }
        }
    }

    /// Render an item's leading attributes, one `@name(args…)` per line.
    fn attrs(&mut self, d: usize, attrs: &[Attribute]) {
        for a in attrs {
            let args: Vec<String> = a.args.iter().map(|e| self.expr_inline(*e)).collect();
            if args.is_empty() {
                self.line(d, &format!("@{}", a.name));
            } else {
                self.line(d, &format!("@{}({})", a.name, args.join(", ")));
            }
        }
    }

    fn func(&mut self, d: usize, f: &FnDecl, kw: &str) {
        self.attrs(d, &f.attrs);
        self.line(d, &format!("{} {}", kw, f.name.name));
        if !f.params.is_empty() {
            self.line(d + 1, "params:");
            for p in &f.params {
                let mut s = String::new();
                if p.comptime {
                    s.push_str("comptime ");
                }
                if p.conv != Conv::Default {
                    s.push_str(p.conv.label());
                    s.push(' ');
                }
                s.push_str(&p.name.name);
                if let Some(t) = p.ty {
                    s.push_str(": ");
                    s.push_str(&self.type_str(t));
                }
                if let Some(r) = p.refine {
                    s.push_str(" in ");
                    s.push_str(&self.expr_inline(r));
                }
                self.line(d + 2, &s);
            }
        }
        if let Some(t) = f.ret_ty {
            let conv = if f.ret_conv != Conv::Default {
                format!("{} ", f.ret_conv.label())
            } else {
                String::new()
            };
            self.line(d + 1, &format!("returns: {}{}", conv, self.type_str(t)));
        }
        if let Some(es) = &f.errors {
            let names: Vec<&str> = es.names.iter().map(|n| n.name.as_str()).collect();
            self.line(d + 1, &format!("errors: !{{ {} }}", names.join(", ")));
        }
        self.line(d + 1, "body:");
        self.block(d + 2, &f.body);
    }

    fn struct_body(&mut self, d: usize, b: &StructBody) {
        for m in &b.members {
            match m {
                StructMember::Field { name, ty, .. } => {
                    let ts = self.type_str(*ty);
                    self.line(d, &format!("field {}: {}", name.name, ts));
                }
                StructMember::Method(f) => self.func(d, f, "method fn"),
            }
        }
    }

    fn block(&mut self, d: usize, b: &Block) {
        if b.stmts.is_empty() {
            self.line(d, "<empty>");
            return;
        }
        for s in &b.stmts {
            self.stmt(d, s);
        }
    }

    fn stmt(&mut self, d: usize, s: &Stmt) {
        match s {
            Stmt::Let { mutbl, name, ty, init, .. } => {
                let kw = if *mutbl { "var" } else { "let" };
                let tys = ty.map(|t| format!(": {}", self.type_str(t))).unwrap_or_default();
                match init {
                    Some(e) => {
                        self.line(d, &format!("{} {}{} =", kw, name.name, tys));
                        self.expr(d + 1, *e);
                    }
                    None => self.line(d, &format!("{} {}{}", kw, name.name, tys)),
                }
            }
            Stmt::Return { value, .. } => match value {
                Some(e) => {
                    self.line(d, "return");
                    self.expr(d + 1, *e);
                }
                None => self.line(d, "return"),
            },
            Stmt::Expr(e) => self.expr(d, *e),
        }
    }

    /// Render an expression: block-like forms as a tree, everything else inline.
    fn expr(&mut self, d: usize, id: ExprId) {
        let ast = self.ast;
        let e = ast.expr_at(id);
        match &e.kind {
            ExprKind::Block(b) => {
                self.line(d, "block");
                self.block(d + 1, b);
            }
            ExprKind::Unsafe(b) => {
                self.line(d, "unsafe");
                self.block(d + 1, b);
            }
            ExprKind::If { cond, then, els } => {
                let head = format!("if {}", self.expr_inline(*cond));
                self.line(d, &head);
                self.line(d + 1, "then:");
                self.block(d + 2, then);
                if let Some(els) = els {
                    self.line(d + 1, "else:");
                    self.expr(d + 2, *els);
                }
            }
            ExprKind::Match { scrut, arms } => {
                let head = format!("match {}", self.expr_inline(*scrut));
                self.line(d, &head);
                for a in arms {
                    let arm_head = match a.guard {
                        Some(g) => format!("arm {} if {} =>", self.pat_str(a.pat), self.expr_inline(g)),
                        None => format!("arm {} =>", self.pat_str(a.pat)),
                    };
                    self.line(d + 1, &arm_head);
                    self.expr(d + 2, a.body);
                }
            }
            ExprKind::StructType(b) => {
                self.line(d, "struct-type");
                self.struct_body(d + 1, b);
            }
            _ => {
                let s = self.expr_inline(id);
                self.line(d, &s);
            }
        }
    }

    /// Render an expression as a single source-like line, fully parenthesized so
    /// that precedence and associativity are unambiguous.
    fn expr_inline(&self, id: ExprId) -> String {
        let e = self.ast.expr_at(id);
        match &e.kind {
            ExprKind::Int(s) | ExprKind::Float(s) | ExprKind::Str(s) | ExprKind::Char(s) => s.clone(),
            ExprKind::Bool(b) => b.to_string(),
            ExprKind::Null => "null".to_string(),
            ExprKind::Name(n) => n.name.clone(),
            ExprKind::SelfValue => "self".to_string(),
            ExprKind::SelfType => "Self".to_string(),
            ExprKind::Attr(n) => format!("@{}", n.name),
            ExprKind::Unary { op, rhs } => format!("{}{}", un_label(*op), self.expr_inline(*rhs)),
            ExprKind::Binary { op, lhs, rhs } => {
                format!("({} {} {})", self.expr_inline(*lhs), bin_label(*op), self.expr_inline(*rhs))
            }
            ExprKind::Assign { op, target, value } => {
                format!("{} {} {}", self.expr_inline(*target), assign_label(*op), self.expr_inline(*value))
            }
            ExprKind::Range { lo, hi, inclusive } => {
                let l = lo.map(|e| self.expr_inline(e)).unwrap_or_default();
                let h = hi.map(|e| self.expr_inline(e)).unwrap_or_default();
                format!("{}{}{}", l, if *inclusive { "..=" } else { ".." }, h)
            }
            ExprKind::Call { callee, args } => {
                let a: Vec<String> = args.iter().map(|x| self.expr_inline(*x)).collect();
                format!("{}({})", self.expr_inline(*callee), a.join(", "))
            }
            ExprKind::Field { base, name } => format!("{}.{}", self.expr_inline(*base), name.name),
            ExprKind::Deref { base } => format!("{}.*", self.expr_inline(*base)),
            ExprKind::Cast { expr, ty } => format!("{} as {}", self.expr_inline(*expr), self.type_str(*ty)),
            ExprKind::Try { base } => format!("{}?", self.expr_inline(*base)),
            ExprKind::Index { base, index } => {
                format!("{}[{}]", self.expr_inline(*base), self.expr_inline(*index))
            }
            ExprKind::StructLit { path, fields } => {
                let fs: Vec<String> = fields
                    .iter()
                    .map(|f| format!("{}: {}", f.name.name, self.expr_inline(f.value)))
                    .collect();
                format!("{}{{ {} }}", path.name, fs.join(", "))
            }
            ExprKind::GenStructLit { ctor, type_args, fields } => {
                let ta: Vec<String> = type_args.iter().map(|t| self.expr_inline(*t)).collect();
                let fs: Vec<String> = fields
                    .iter()
                    .map(|f| format!("{}: {}", f.name.name, self.expr_inline(f.value)))
                    .collect();
                format!("{}({}){{ {} }}", ctor.name, ta.join(", "), fs.join(", "))
            }
            ExprKind::Closure { params, body } => {
                let ps: Vec<&str> = params.iter().map(|p| p.name.name.as_str()).collect();
                format!("|{}| {}", ps.join(", "), self.expr_inline(*body))
            }
            // Block-like forms get a placeholder when they appear inline-nested;
            // at statement position they are rendered as a tree by `expr`.
            ExprKind::Block(_) => "{ ... }".to_string(),
            ExprKind::Unsafe(_) => "unsafe { ... }".to_string(),
            ExprKind::If { .. } => "if ...".to_string(),
            ExprKind::Match { .. } => "match ...".to_string(),
            ExprKind::StructType(_) => "struct { ... }".to_string(),
            ExprKind::Concurrent(_) => "concurrent { ... }".to_string(),
            ExprKind::Spawn(e) => format!("spawn {}", self.expr_inline(*e)),
            ExprKind::Region { name, .. } => format!("region {} {{ ... }}", name.name),
            ExprKind::For { head, els, .. } => {
                let tail = if els.is_some() { " else { ... }" } else { "" };
                match head {
                    ForHead::Infinite => format!("for {{ ... }}{tail}"),
                    ForHead::While(c) => format!("for {} {{ ... }}{tail}", self.expr_inline(*c)),
                    ForHead::Iter { binds, sources, .. } => {
                        let bs: Vec<String> = binds
                            .iter()
                            .map(|b| {
                                let c = if b.conv != Conv::Default && b.conv != Conv::Read {
                                    format!("{} ", b.conv.label())
                                } else {
                                    String::new()
                                };
                                format!("{}{}", c, b.name.name)
                            })
                            .collect();
                        let ss: Vec<String> = sources.iter().map(|s| self.expr_inline(*s)).collect();
                        format!("for {} in {} {{ ... }}{tail}", bs.join(", "), ss.join(", "))
                    }
                }
            }
            ExprKind::Break(l) => match l {
                Some(n) => format!("break {}", n.name),
                None => "break".to_string(),
            },
            ExprKind::Continue(l) => match l {
                Some(n) => format!("continue {}", n.name),
                None => "continue".to_string(),
            },
            ExprKind::Invariant(e) => format!("invariant {}", self.expr_inline(*e)),
            ExprKind::Variant(e) => format!("variant {}", self.expr_inline(*e)),
            ExprKind::Error => "<error>".to_string(),
        }
    }

    fn type_str(&self, id: TypeId) -> String {
        let t = self.ast.type_at(id);
        match &t.kind {
            TypeKind::Name(n) => n.name.clone(),
            TypeKind::TypeKw => "type".to_string(),
            TypeKind::Ptr { mutbl, inner } => {
                let m = match mutbl {
                    PtrMut::Mut => "*mut ",
                    PtrMut::Const => "*const ",
                    PtrMut::Default => "*",
                };
                format!("{}{}", m, self.type_str(*inner))
            }
            TypeKind::Slice(inner) => format!("[]{}", self.type_str(*inner)),
            TypeKind::GenRef(inner) => format!("&{}", self.type_str(*inner)),
            TypeKind::RegionRef { region, inner } => {
                format!("&[{}]{}", region.name, self.type_str(*inner))
            }
            TypeKind::App { ctor, args } => {
                let a: Vec<String> = args.iter().map(|t| self.type_str(*t)).collect();
                format!("{}({})", ctor.name, a.join(", "))
            }
            TypeKind::Error => "<error-type>".to_string(),
        }
    }

    fn pat_str(&self, id: PatId) -> String {
        let p = self.ast.pat_at(id);
        match &p.kind {
            PatKind::Wildcard => "_".to_string(),
            PatKind::Ident(n) => n.name.clone(),
            PatKind::Variant { name, subpats } => {
                let s: Vec<String> = subpats.iter().map(|x| self.pat_str(*x)).collect();
                format!("{}({})", name.name, s.join(", "))
            }
            PatKind::Error => "<error-pat>".to_string(),
        }
    }
}

fn un_label(op: UnOp) -> &'static str {
    match op {
        UnOp::Neg => "-",
        UnOp::Not => "not ",
        UnOp::BitNot => "~",
        UnOp::Ref => "&",
    }
}

fn bin_label(op: BinOp) -> &'static str {
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

fn assign_label(op: AssignOp) -> &'static str {
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
