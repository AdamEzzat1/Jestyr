//! The hand-written parser: recursive descent for declarations and statements,
//! Pratt (precedence-climbing) parsing for expressions.
//!
//! Like the lexer it recovers rather than aborting: a parse error records a
//! diagnostic and the parser keeps going, so one mistake doesn't hide the rest.
//! Every loop has a progress guard so malformed input can never spin forever.
//!
//! Statement boundaries are structural, not newline-driven (the lexer discards
//! newlines): an expression statement ends when the next token can't continue
//! the expression, and a *block-led* expression (`if`/`match`/`unsafe`/`{}`) in
//! statement position is a complete statement that a trailing operator can't
//! extend — the same rule Rust uses. (One known consequence: `f` on its own
//! line followed by `(x)` parses as a call `f(x)`; a production language would
//! resolve that with newline significance. Fine for the current grammar.)

use crate::ast::*;
use crate::ast::Ident; // explicit: shadows the glob clash with `TokenKind::Ident`
use crate::attrs;
use crate::diag::Diagnostic;
use crate::span::Span;
use crate::token::TokenKind::*;
use crate::token::{Token, TokenKind};

pub struct Parser<'src> {
    src: &'src str,
    tokens: Vec<Token>,
    pos: usize,
    /// While true, `Ident {` is not treated as a struct literal (set inside
    /// control-flow headers so the `{` can open the body block).
    no_struct: bool,
    pub ast: Ast,
    pub diagnostics: Vec<Diagnostic>,
}

impl<'src> Parser<'src> {
    pub fn new(src: &'src str, tokens: Vec<Token>) -> Parser<'src> {
        Parser::resume(src, tokens, Ast::new())
    }

    /// Construct a parser that *appends* into an existing arena. The module
    /// loader threads one shared `Ast` through every file so all `ExprId`/
    /// `TypeId`/`PatId` handles live in a single id-space — the same "one
    /// translation unit" model the C backend already uses (no id remapping).
    pub fn resume(src: &'src str, tokens: Vec<Token>, ast: Ast) -> Parser<'src> {
        Parser { src, tokens, pos: 0, no_struct: false, ast, diagnostics: Vec::new() }
    }

    pub fn parse(self) -> (Ast, Vec<Diagnostic>) {
        let (mut ast, items, diags) = self.parse_module();
        ast.items = items; // a fresh single-file parse: these are the only items
        (ast, diags)
    }

    /// Parse one file's items, returning them *separately* from the shared arena
    /// (rather than pushing into `ast.items`) so the loader can tag each item
    /// with its owning module before inserting it into the program.
    pub fn parse_module(mut self) -> (Ast, Vec<Item>, Vec<Diagnostic>) {
        let mut items = Vec::new();
        while self.cur().kind != Eof {
            let before = self.pos;
            if let Some(item) = self.parse_item() {
                items.push(item);
            }
            if self.pos == before {
                self.bump(); // guarantee progress on unrecognized input
            }
        }
        (self.ast, items, self.diagnostics)
    }

    // --- token cursor ---

    fn cur(&self) -> Token {
        self.tokens[self.pos]
    }

    fn bump(&mut self) -> Token {
        let t = self.cur();
        if t.kind != Eof {
            self.pos += 1;
        }
        t
    }

    fn at(&self, kind: TokenKind) -> bool {
        self.cur().kind == kind
    }

    /// The kind of the token `n` positions ahead (for the 2-token lookahead the
    /// `for` header needs to tell `binding in …` from a `cond` expression).
    fn peek_kind(&self, n: usize) -> TokenKind {
        self.tokens.get(self.pos + n).map(|t| t.kind).unwrap_or(Eof)
    }

    fn eat(&mut self, kind: TokenKind) -> bool {
        if self.at(kind) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn expect(&mut self, kind: TokenKind, what: &str) -> Token {
        let t = self.cur();
        if t.kind == kind {
            self.bump()
        } else {
            self.error(t.span, format!("expected {}, found `{}`", what, t.kind.describe()));
            t // do not consume — let the caller's loop bounds recover
        }
    }

    fn prev_span(&self) -> Span {
        if self.pos == 0 {
            self.tokens[0].span
        } else {
            self.tokens[self.pos - 1].span
        }
    }

    fn text(&self, span: Span) -> String {
        self.src[span.range()].to_string()
    }

    fn ident(&self, t: Token) -> Ident {
        Ident { name: self.text(t.span), span: t.span }
    }

    fn eat_ident(&mut self, what: &str) -> Ident {
        let t = self.cur();
        if t.kind == TokenKind::Ident {
            self.bump();
            self.ident(t)
        } else {
            self.error(t.span, format!("expected {}, found `{}`", what, t.kind.describe()));
            Ident { name: "<error>".to_string(), span: t.span }
        }
    }

    fn error(&mut self, span: Span, message: impl Into<String>) {
        self.diagnostics.push(Diagnostic::new(message, span));
    }

    // --- items ---

    fn parse_item(&mut self) -> Option<Item> {
        let attrs = self.parse_attrs(); // `@packed`, `@inline`, `@deprecated(…)`, …
        let is_pub = self.eat(Pub); // module-public vs module-private (design §9)
        match self.cur().kind {
            Fn => {
                let mut f = self.parse_fn(attrs);
                f.is_pub = is_pub;
                // Validation needs the parsed signature (for `@must_use`/`@no_mangle`),
                // so it runs *after* `parse_fn` rather than over the raw attrs.
                self.check_fn_attrs(&f, false);
                Some(Item::Fn(f))
            }
            Enum => {
                self.check_attrs(&attrs, attrs::Target::Enum);
                let mut e = self.parse_enum();
                e.is_pub = is_pub;
                Some(Item::Enum(e))
            }
            Const => {
                self.check_attrs(&attrs, attrs::Target::Const);
                let mut c = self.parse_const();
                c.is_pub = is_pub;
                c.attrs = attrs;
                Some(Item::Const(c))
            }
            Struct => {
                self.check_attrs(&attrs, attrs::Target::Struct);
                Some(self.parse_named_struct(attrs, is_pub, false, false))
            }
            Record => {
                // An immutable product type — same grammar/attrs as `struct`.
                self.check_attrs(&attrs, attrs::Target::Struct);
                Some(self.parse_named_struct(attrs, is_pub, true, false))
            }
            Union => {
                // An untagged union — same grammar as `struct`; fields overlap.
                self.check_attrs(&attrs, attrs::Target::Struct);
                Some(self.parse_named_struct(attrs, is_pub, false, true))
            }
            Distinct => {
                if let Some(a) = attrs.first() {
                    self.error(a.span, "attributes are not allowed on `distinct`");
                }
                let mut d = self.parse_distinct();
                d.is_pub = is_pub;
                Some(Item::Distinct(d))
            }
            Extern => {
                self.check_attrs(&attrs, attrs::Target::Extern);
                let mut e = self.parse_extern();
                e.is_pub = is_pub;
                Some(Item::Extern(e))
            }
            Import => {
                if let Some(a) = attrs.first() {
                    self.error(a.span, "attributes are not allowed on `import`");
                }
                Some(Item::Import(self.parse_import()))
            }
            Trait => {
                let mut t = self.parse_trait();
                t.is_pub = is_pub;
                Some(Item::Trait(t))
            }
            Impl => {
                if let Some(a) = attrs.first() {
                    self.error(a.span, "attributes are not allowed on an `impl` block itself");
                }
                Some(Item::Impl(self.parse_impl()))
            }
            other => {
                self.error(
                    self.cur().span,
                    format!("expected an item (`fn`, `trait`, `impl`, `enum`, `const`, `struct`, `record`, `distinct`, `extern`, `import`), found `{}`", other.describe()),
                );
                self.bump();
                None
            }
        }
    }

    /// `distinct Name = BaseType` — a zero-cost nominal wrapper (design §2.6).
    fn parse_distinct(&mut self) -> DistinctDecl {
        let start = self.cur().span;
        self.expect(Distinct, "`distinct`");
        let name = self.eat_ident("distinct type name");
        self.expect(Eq, "`=`");
        let base = self.parse_type();
        let span = start.to(self.ast.type_at(base).span);
        DistinctDecl { is_pub: false, name, base, span }
    }

    /// `import "rel/path"` or `import "rel/path" as alias`.
    fn parse_import(&mut self) -> ImportDecl {
        let start = self.cur().span;
        self.expect(Import, "`import`");
        let path = if self.at(Str) {
            let sp = self.cur().span;
            self.bump();
            // Strip the surrounding quotes; the lexer kept them in the span.
            self.text(sp).trim_matches('"').to_string()
        } else {
            self.error(self.cur().span, "expected a module path string after `import`");
            String::new()
        };
        let alias = if self.eat(As) { Some(self.eat_ident("import alias")) } else { None };
        ImportDecl { path, alias, span: start.to(self.prev_span()) }
    }

    fn parse_fn(&mut self, attrs: Vec<Attribute>) -> FnDecl {
        let start = self.cur().span;
        self.expect(Fn, "`fn`");
        let name = self.eat_ident("function name");
        let generics = self.parse_generics(); // optional `[T: Add, U]`
        self.expect(LParen, "`(`");
        let params = self.parse_params();
        self.expect(RParen, "`)`");

        let mut ret_conv = Conv::Default;
        let mut ret_ty = None;
        if self.eat(Arrow) {
            ret_conv = self.parse_conv();
            ret_ty = Some(self.parse_type());
        }

        let errors = if self.at(Bang) { Some(self.parse_error_set()) } else { None };

        // Contracts: zero or more `requires <expr>` / `ensures <expr>` clauses,
        // between the signature and the body. `no_struct` keeps a trailing `{`
        // attached to the body, not parsed as a struct literal in the condition.
        let mut requires = Vec::new();
        let mut ensures = Vec::new();
        loop {
            let is_req = self.at(Requires);
            if !is_req && !self.at(Ensures) {
                break;
            }
            self.bump();
            let saved = self.no_struct;
            self.no_struct = true;
            let cond = self.parse_expr();
            self.no_struct = saved;
            if is_req {
                requires.push(cond);
            } else {
                ensures.push(cond);
            }
        }

        let body = self.parse_block();
        let span = start.to(body.span);
        // `no_panic` is a cached projection of `attrs` (the single source of truth)
        // so the many `f.no_panic` read sites need not scan the vector each time.
        let no_panic = attrs.iter().any(|a| a.name == "no_panic");
        FnDecl { is_pub: false, generics, no_panic, attrs, name, params, ret_conv, ret_ty, errors, requires, ensures, body, span }
    }

    /// Optional bracket-form bounded generics after a function name: `[T: Add, U]`.
    /// Empty when absent. Each entry is a type-parameter name with an optional
    /// trait bound (`T: Add`); the bound is checked at the definition site.
    fn parse_generics(&mut self) -> Vec<GenericParam> {
        let mut gs = Vec::new();
        if !self.eat(LBracket) {
            return gs;
        }
        while !self.at(RBracket) && !self.at(Eof) {
            let before = self.pos;
            let name = self.eat_ident("type parameter");
            let bound = if self.eat(Colon) { Some(self.eat_ident("trait bound")) } else { None };
            gs.push(GenericParam { name, bound });
            if self.pos == before {
                self.bump(); // guarantee progress on malformed input
            }
            if !self.eat(Comma) {
                break;
            }
        }
        self.expect(RBracket, "`]`");
        gs
    }

    /// `trait Name { fn m(self) -> T  fn n(self) { <default> } }` — a behavior
    /// description (design §7.3). A method with a `{ … }` body is a *default*
    /// (optional for an `impl`); a bodyless one is required.
    fn parse_trait(&mut self) -> TraitDecl {
        let start = self.cur().span;
        self.expect(Trait, "`trait`");
        let name = self.eat_ident("trait name");
        self.expect(LBrace, "`{`");
        let mut methods = Vec::new();
        while !self.at(RBrace) && !self.at(Eof) {
            let before = self.pos;
            if self.at(Fn) {
                methods.push(self.parse_trait_method());
            } else {
                self.error(self.cur().span, "expected a method signature (`fn …`) in the trait body");
            }
            if self.pos == before {
                self.bump();
            }
        }
        let end = self.cur().span;
        self.expect(RBrace, "`}`");
        TraitDecl { is_pub: false, name, methods, span: start.to(end) }
    }

    fn parse_trait_method(&mut self) -> TraitMethod {
        let start = self.cur().span;
        self.expect(Fn, "`fn`");
        let name = self.eat_ident("method name");
        self.expect(LParen, "`(`");
        let params = self.parse_params();
        self.expect(RParen, "`)`");
        let mut ret_conv = Conv::Default;
        let mut ret_ty = None;
        if self.eat(Arrow) {
            ret_conv = self.parse_conv();
            ret_ty = Some(self.parse_type());
        }
        // A `{ … }` body is a default implementation; otherwise the signature is
        // required, with an optional `;` terminator.
        let default_body = if self.at(LBrace) {
            Some(self.parse_block())
        } else {
            self.eat(Semi);
            None
        };
        TraitMethod { name, params, ret_conv, ret_ty, default_body, span: start.to(self.prev_span()) }
    }

    /// `impl Trait for Type { fn m(self) -> T { … } }`.
    fn parse_impl(&mut self) -> ImplDecl {
        let start = self.cur().span;
        self.expect(Impl, "`impl`");
        // Optional bracket generics: `impl[T] Drop for Vec(T)` — a blanket impl.
        let generics = self.parse_generics();
        let trait_name = self.eat_ident("trait name");
        self.expect(For, "`for`");
        let ty = self.parse_type();
        self.expect(LBrace, "`{`");
        let mut methods = Vec::new();
        while !self.at(RBrace) && !self.at(Eof) {
            let before = self.pos;
            let mattrs = self.parse_attrs(); // `@inline fn …` on an impl method
            if self.at(Fn) {
                let f = self.parse_fn(mattrs);
                self.check_fn_attrs(&f, true);
                methods.push(f);
            } else if let Some(a) = mattrs.first() {
                self.error(a.span, "an attribute here applies to a method (`fn …`)");
            } else {
                self.error(self.cur().span, "expected a method (`fn …`) in the impl body");
            }
            if self.pos == before {
                self.bump();
            }
        }
        let end = self.cur().span;
        self.expect(RBrace, "`}`");
        ImplDecl { generics, trait_name, ty, methods, span: start.to(end) }
    }

    /// `extern "c" fn name(params) -> ret` — a bodyless foreign declaration.
    fn parse_extern(&mut self) -> ExternFn {
        let start = self.cur().span;
        self.expect(Extern, "`extern`");
        let abi = if self.at(Str) {
            let sp = self.cur().span;
            self.bump();
            self.text(sp).trim_matches('"').to_string()
        } else {
            "c".to_string()
        };
        self.expect(Fn, "`fn`");
        let name = self.eat_ident("function name");
        self.expect(LParen, "`(`");
        let params = self.parse_params();
        self.expect(RParen, "`)`");

        let mut ret_conv = Conv::Default;
        let mut ret_ty = None;
        if self.eat(Arrow) {
            ret_conv = self.parse_conv();
            ret_ty = Some(self.parse_type());
        }
        let span = start.to(self.prev_span());
        ExternFn { is_pub: false, abi, name, params, ret_conv, ret_ty, span }
    }

    fn parse_conv(&mut self) -> Conv {
        match self.cur().kind {
            Read => { self.bump(); Conv::Read }
            Mut => { self.bump(); Conv::Mut }
            Take => { self.bump(); Conv::Take }
            Out => { self.bump(); Conv::Out }
            _ => Conv::Default,
        }
    }

    fn parse_params(&mut self) -> Vec<Param> {
        let mut params = Vec::new();
        while !self.at(RParen) && !self.at(Eof) {
            params.push(self.parse_param());
            if !self.eat(Comma) {
                break;
            }
        }
        params
    }

    fn parse_param(&mut self) -> Param {
        let start = self.cur().span;
        let comptime = self.eat(Comptime);
        let conv = self.parse_conv();

        if self.at(SelfValue) {
            let t = self.bump();
            return Param {
                comptime,
                conv,
                name: Ident { name: "self".to_string(), span: t.span },
                is_self: true,
                ty: None,
                refine: None,
                span: start.to(t.span),
            };
        }

        let name = self.eat_ident("parameter name");
        let mut ty = None;
        let mut refine = None;
        if self.eat(Colon) {
            ty = Some(self.parse_type());
            if self.eat(In) {
                refine = Some(self.parse_expr());
            }
        }
        Param { comptime, conv, name, is_self: false, ty, refine, span: start.to(self.prev_span()) }
    }

    fn parse_error_set(&mut self) -> ErrorSet {
        let start = self.cur().span;
        self.expect(Bang, "`!`");
        self.expect(LBrace, "`{`");
        let mut names = Vec::new();
        while !self.at(RBrace) && !self.at(Eof) {
            let before = self.pos;
            names.push(self.eat_ident("error name"));
            if self.pos == before {
                self.bump();
            }
            if !self.eat(Comma) {
                break;
            }
        }
        let end = self.cur().span;
        self.expect(RBrace, "`}`");
        ErrorSet { names, span: start.to(end) }
    }

    fn parse_enum(&mut self) -> EnumDecl {
        let start = self.cur().span;
        self.expect(Enum, "`enum`");
        let name = self.eat_ident("enum name");
        // Optional generic parameters: `enum Option(T, E) { … }`.
        let mut type_params = Vec::new();
        if self.eat(LParen) {
            while !self.at(RParen) && !self.at(Eof) {
                type_params.push(self.eat_ident("type parameter"));
                if !self.eat(Comma) {
                    break;
                }
            }
            self.expect(RParen, "`)`");
        }
        self.expect(LBrace, "`{`");
        let mut variants = Vec::new();
        while !self.at(RBrace) && !self.at(Eof) {
            let before = self.pos;
            let vstart = self.cur().span;
            let vname = self.eat_ident("variant name");
            let mut fields = Vec::new();
            if self.eat(LParen) {
                while !self.at(RParen) && !self.at(Eof) {
                    let fname = self.eat_ident("field name");
                    self.expect(Colon, "`:`");
                    let fty = self.parse_type();
                    fields.push((fname, fty));
                    if !self.eat(Comma) {
                        break;
                    }
                }
                self.expect(RParen, "`)`");
            }
            // Optional explicit discriminant: `red = 1`.
            let discriminant = if self.eat(Eq) {
                let saved = self.no_struct;
                self.no_struct = true; // a trailing `{` is the next item, not a struct literal
                let d = self.parse_expr();
                self.no_struct = saved;
                Some(d)
            } else {
                None
            };
            variants.push(EnumVariant {
                name: vname,
                fields,
                discriminant,
                span: vstart.to(self.prev_span()),
            });
            if self.pos == before {
                self.bump();
            }
            if !self.eat(Comma) {
                break;
            }
        }
        let end = self.cur().span;
        self.expect(RBrace, "`}`");
        EnumDecl { is_pub: false, name, type_params, variants, span: start.to(end) }
    }

    fn parse_const(&mut self) -> ConstDecl {
        let start = self.cur().span;
        self.expect(Const, "`const`");
        let name = self.eat_ident("const name");
        let ty = if self.eat(Colon) { Some(self.parse_type()) } else { None };
        self.expect(Eq, "`=`");
        let value = self.parse_expr();
        let span = start.to(self.ast.expr_at(value).span);
        ConstDecl { is_pub: false, name, ty, value, attrs: Vec::new(), span }
    }

    fn parse_named_struct(
        &mut self,
        attrs: Vec<Attribute>,
        is_pub: bool,
        is_record: bool,
        is_union: bool,
    ) -> Item {
        let start = self.cur().span;
        if is_record {
            self.expect(Record, "`record`");
        } else if is_union {
            self.expect(Union, "`union`");
        } else {
            self.expect(Struct, "`struct`");
        }
        let what = if is_record {
            "record name"
        } else if is_union {
            "union name"
        } else {
            "struct name"
        };
        let name = self.eat_ident(what);
        let body = self.parse_struct_body();
        // A record's fields are immutable, so a `mut self` / `out self` method —
        // which exists to mutate the receiver — is a contradiction. Reject it.
        if is_record {
            for m in &body.members {
                if let StructMember::Method(f) = m {
                    if let Some(p) =
                        f.params.iter().find(|p| p.is_self && matches!(p.conv, Conv::Mut | Conv::Out))
                    {
                        self.error(
                            f.name.span,
                            format!(
                                "a method of immutable record `{}` cannot take `{} self`",
                                name.name,
                                p.conv.label()
                            ),
                        );
                    }
                }
            }
        }
        let span = start.to(body.span);
        Item::Struct { is_pub, is_record, is_union, name, body, attrs, span }
    }

    /// Parse a run of leading `@name` / `@name(args)` item attributes.
    fn parse_attrs(&mut self) -> Vec<Attribute> {
        let mut attrs = Vec::new();
        while self.at(At) {
            let start = self.cur().span;
            self.bump(); // `@`
            let name = self.eat_ident("attribute name");
            let mut args = Vec::new();
            if self.eat(LParen) {
                while !self.at(RParen) && !self.at(Eof) {
                    args.push(self.parse_expr());
                    if !self.eat(Comma) {
                        break;
                    }
                }
                self.expect(RParen, "`)`");
            }
            attrs.push(Attribute { name: name.name, args, span: start.to(self.prev_span()) });
        }
        attrs
    }

    /// Validate item/field attributes against the registry (`crate::attrs`),
    /// folding any problems into the parser's diagnostic stream. The immutable
    /// borrow of `self.ast` and the mutable borrow of `self.diagnostics` are
    /// disjoint struct fields, so the borrow checker accepts both at once.
    fn check_attrs(&mut self, attrs: &[Attribute], target: attrs::Target) {
        attrs::validate(&self.ast, attrs, target, &mut self.diagnostics);
    }

    /// Validate a function's (or method's) attributes — the generic checks plus
    /// the signature-dependent ones (`@must_use`, `@no_mangle`).
    fn check_fn_attrs(&mut self, f: &FnDecl, is_method: bool) {
        attrs::validate_fn(&self.ast, f, is_method, &mut self.diagnostics);
    }

    fn parse_struct_body(&mut self) -> StructBody {
        let start = self.cur().span;
        self.expect(LBrace, "`{`");
        let mut members = Vec::new();
        while !self.at(RBrace) && !self.at(Eof) {
            let before = self.pos;
            // Leading attributes here belong to a *method* (`@inline fn …`); a
            // field's attributes (`@volatile`) are written after its `:` instead.
            let mattrs = self.parse_attrs();
            if self.at(Fn) {
                let f = self.parse_fn(mattrs);
                self.check_fn_attrs(&f, true);
                members.push(StructMember::Method(f));
            } else {
                if let Some(a) = mattrs.first() {
                    self.error(
                        a.span,
                        "an attribute here applies to a method; a field's attributes (e.g. `@volatile`) go after its `:`",
                    );
                }
                let fstart = self.cur().span;
                // `pub x: T` exposes the field across modules (private by default).
                let is_pub = self.eat(Pub);
                let name = self.eat_ident("field name");
                self.expect(Colon, "`:`");
                let fattrs = self.parse_attrs(); // field-level: `@volatile`
                self.check_attrs(&fattrs, attrs::Target::Field);
                let volatile = fattrs.iter().any(|a| a.name == "volatile");
                let ty = self.parse_type();
                // An optional bit-field width: `flags: u8 : 3` → a 3-bit field.
                let bits = if self.eat(Colon) {
                    let t = self.cur();
                    if t.kind == Int {
                        self.bump();
                        match self.text(t.span).replace('_', "").parse::<u32>() {
                            Ok(n) => Some(n),
                            Err(_) => {
                                self.error(t.span, "bit-field width must be a non-negative integer".to_string());
                                None
                            }
                        }
                    } else {
                        let sp = t.span;
                        self.error(sp, "expected a bit-field width (an integer) after `:`".to_string());
                        None
                    }
                } else {
                    None
                };
                // An optional field default: `x: i32 = 0`. Used to fill the field
                // when a struct literal omits it.
                let default = if self.eat(Eq) {
                    Some(self.parse_expr())
                } else {
                    None
                };
                members.push(StructMember::Field {
                    name,
                    ty,
                    volatile,
                    default,
                    is_pub,
                    bits,
                    span: fstart.to(self.prev_span()),
                });
                self.eat(Comma); // optional separator between fields
            }
            if self.pos == before {
                self.bump();
            }
        }
        let end = self.cur().span;
        self.expect(RBrace, "`}`");
        StructBody { members, span: start.to(end) }
    }

    // --- types ---

    fn parse_type(&mut self) -> TypeId {
        let start = self.cur().span;
        match self.cur().kind {
            Star => {
                self.bump();
                let mutbl = if self.eat(Mut) {
                    PtrMut::Mut
                } else if self.eat(Const) {
                    PtrMut::Const
                } else {
                    PtrMut::Default
                };
                let inner = self.parse_type();
                let span = start.to(self.ast.type_at(inner).span);
                self.ast.ty(TypeKind::Ptr { mutbl, inner }, span)
            }
            Type => {
                let t = self.bump();
                self.ast.ty(TypeKind::TypeKw, t.span)
            }
            // `indirect T` — a recursive-indirection field. Currently sugar for a
            // raw pointer `*T` (it breaks the size cycle and is the readable
            // spelling for self-referential ADTs; tier-aware `indirect &[r]T` is
            // future work). See docs/structs-enums-design.md §2.5.
            Indirect => {
                self.bump();
                let inner = self.parse_type();
                let span = start.to(self.ast.type_at(inner).span);
                self.ast.ty(TypeKind::Ptr { mutbl: PtrMut::Default, inner }, span)
            }
            LBracket => {
                self.bump();
                if self.eat(RBracket) {
                    // `[]T` — a slice (fat pointer).
                    let inner = self.parse_type();
                    let span = start.to(self.ast.type_at(inner).span);
                    self.ast.ty(TypeKind::Slice(inner), span)
                } else {
                    // `[N]T` — a fixed-size array. `N` is a constant expression.
                    let len = self.parse_expr();
                    self.expect(RBracket, "`]`");
                    let elem = self.parse_type();
                    let span = start.to(self.ast.type_at(elem).span);
                    self.ast.ty(TypeKind::Array { len, elem }, span)
                }
            }
            Amp => {
                self.bump();
                if self.eat(LBracket) {
                    // `&[r]T` — a zero-cost region reference tagged with region `r`
                    let region = self.eat_ident("region name");
                    self.expect(RBracket, "`]`");
                    let inner = self.parse_type();
                    let span = start.to(self.ast.type_at(inner).span);
                    self.ast.ty(TypeKind::RegionRef { region, inner }, span)
                } else {
                    // `&T` — a generational reference
                    let inner = self.parse_type();
                    let span = start.to(self.ast.type_at(inner).span);
                    self.ast.ty(TypeKind::GenRef(inner), span)
                }
            }
            TokenKind::Ident => {
                let t = self.bump();
                let id = self.ident(t);
                // type application: `List(i32)`
                if self.at(LParen) {
                    self.bump();
                    let mut args = Vec::new();
                    while !self.at(RParen) && !self.at(Eof) {
                        args.push(self.parse_type());
                        if !self.eat(Comma) {
                            break;
                        }
                    }
                    let end = self.cur().span;
                    self.expect(RParen, "`)`");
                    return self.ast.ty(TypeKind::App { ctor: id, args }, t.span.to(end));
                }
                self.ast.ty(TypeKind::Name(id), t.span)
            }
            SelfType => {
                let t = self.bump();
                let id = Ident { name: "Self".to_string(), span: t.span };
                self.ast.ty(TypeKind::Name(id), t.span)
            }
            // `dyn Trait` — a type-erased fat pointer dispatching through a vtable.
            Dyn => {
                self.bump();
                let name = self.eat_ident("trait name");
                self.ast.ty(TypeKind::Dyn(name), start.to(self.prev_span()))
            }
            // `fn(read T1, take T2) -> read R` — a thin function-pointer type.
            // Each parameter may carry a passing convention; the `-> conv R`
            // return is optional (its absence means a unit-returning pointer).
            // This is a *type*, not a declaration: parameters have no names.
            Fn => {
                self.bump(); // `fn`
                self.expect(LParen, "`(`");
                let mut params = Vec::new();
                while !self.at(RParen) && !self.at(Eof) {
                    let before = self.pos;
                    let conv = self.parse_conv();
                    let ty = self.parse_type();
                    params.push(FnTypeParam { conv, ty });
                    if self.pos == before {
                        self.bump(); // guarantee progress on malformed input
                    }
                    if !self.eat(Comma) {
                        break;
                    }
                }
                self.expect(RParen, "`)`");
                let mut ret_conv = Conv::Default;
                let mut ret = None;
                if self.eat(Arrow) {
                    ret_conv = self.parse_conv();
                    ret = Some(self.parse_type());
                }
                let span = start.to(self.prev_span());
                self.ast.ty(TypeKind::Fn { params, ret_conv, ret }, span)
            }
            other => {
                self.error(start, format!("expected a type, found `{}`", other.describe()));
                self.bump(); // ensure progress
                self.ast.ty(TypeKind::Error, start)
            }
        }
    }

    // --- statements & blocks ---

    fn parse_block(&mut self) -> Block {
        let start = self.cur().span;
        self.expect(LBrace, "`{`");
        let saved = self.no_struct;
        self.no_struct = false; // struct literals are fine again inside a block
        let mut stmts = Vec::new();
        while !self.at(RBrace) && !self.at(Eof) {
            let before = self.pos;
            stmts.push(self.parse_stmt());
            self.eat(Semi); // semicolons are optional separators
            if self.pos == before {
                self.bump();
            }
        }
        let end = self.cur().span;
        self.expect(RBrace, "`}`");
        self.no_struct = saved;
        Block { stmts, span: start.to(end) }
    }

    fn parse_stmt(&mut self) -> Stmt {
        match self.cur().kind {
            Return => {
                let start = self.cur().span;
                self.bump();
                let value = if matches!(self.cur().kind, RBrace | Semi | Eof) {
                    None
                } else {
                    Some(self.parse_expr())
                };
                let end = value.map(|e| self.ast.expr_at(e).span).unwrap_or(start);
                Stmt::Return { value, span: start.to(end) }
            }
            Let | Var => {
                let mutbl = self.at(Var);
                let start = self.cur().span;
                self.bump();
                let name = self.eat_ident("variable name");
                let ty = if self.eat(Colon) { Some(self.parse_type()) } else { None };
                let init = if self.eat(Eq) { Some(self.parse_expr()) } else { None };
                Stmt::Let { mutbl, name, ty, init, span: start.to(self.prev_span()) }
            }
            // Block-led expressions in statement position: parse only the block
            // form so a trailing operator cannot extend them.
            If | Match | Unsafe | Concurrent | Region | For | While | Loop | LBrace => {
                Stmt::Expr(self.parse_block_like())
            }
            _ => Stmt::Expr(self.parse_expr()),
        }
    }

    fn parse_block_like(&mut self) -> ExprId {
        match self.cur().kind {
            If => self.parse_if(),
            Match => self.parse_match(),
            Unsafe => self.parse_unsafe(),
            Concurrent => self.parse_concurrent(),
            Region => self.parse_region(),
            For => self.parse_for(),
            While | Loop => self.parse_reserved_loop(),
            LBrace => {
                let b = self.parse_block();
                let sp = b.span;
                self.ast.expr(ExprKind::Block(b), sp)
            }
            _ => self.parse_expr(),
        }
    }

    // --- expressions (Pratt) ---

    fn parse_expr(&mut self) -> ExprId {
        self.parse_assignment()
    }

    fn parse_assignment(&mut self) -> ExprId {
        let lhs = self.parse_binary(0);
        if let Some(op) = assign_op(self.cur().kind) {
            self.bump();
            let value = self.parse_assignment(); // right-associative
            let span = self.ast.expr_at(lhs).span.to(self.ast.expr_at(value).span);
            return self.ast.expr(ExprKind::Assign { op, target: lhs, value }, span);
        }
        lhs
    }

    fn parse_binary(&mut self, min_bp: u8) -> ExprId {
        let mut lhs = self.parse_unary();
        loop {
            let k = self.cur().kind;

            // Ranges are infix but build a distinct node.
            if k == DotDot || k == DotDotEq {
                let (lbp, rbp) = (5, 6);
                if lbp < min_bp {
                    break;
                }
                let inclusive = k == DotDotEq;
                self.bump();
                let hi = if self.starts_expr() { Some(self.parse_binary(rbp)) } else { None };
                let lo_span = self.ast.expr_at(lhs).span;
                let span = hi.map(|h| lo_span.to(self.ast.expr_at(h).span)).unwrap_or(lo_span);
                lhs = self.ast.expr(ExprKind::Range { lo: Some(lhs), hi, inclusive }, span);
                continue;
            }

            let Some((lbp, rbp, op)) = bin_op(k) else { break };
            if lbp < min_bp {
                break;
            }
            self.bump();
            let rhs = self.parse_binary(rbp);
            let span = self.ast.expr_at(lhs).span.to(self.ast.expr_at(rhs).span);
            lhs = self.ast.expr(ExprKind::Binary { op, lhs, rhs }, span);
        }
        lhs
    }

    fn parse_unary(&mut self) -> ExprId {
        let start = self.cur().span;
        let op = match self.cur().kind {
            Minus => Some(UnOp::Neg),
            Not | Bang => Some(UnOp::Not),
            Tilde => Some(UnOp::BitNot),
            Amp => Some(UnOp::Ref),
            _ => None,
        };
        if let Some(op) = op {
            self.bump();
            let rhs = self.parse_unary();
            let span = start.to(self.ast.expr_at(rhs).span);
            return self.ast.expr(ExprKind::Unary { op, rhs }, span);
        }
        self.parse_cast()
    }

    /// `expr as T` — binds tighter than binary operators (`a + b as T` is
    /// `a + (b as T)`) and chains left (`x as A as B`).
    fn parse_cast(&mut self) -> ExprId {
        let mut e = self.parse_postfix();
        while self.at(As) {
            self.bump();
            let ty = self.parse_type();
            let span = self.ast.expr_at(e).span.to(self.ast.type_at(ty).span);
            e = self.ast.expr(ExprKind::Cast { expr: e, ty }, span);
        }
        e
    }

    fn parse_postfix(&mut self) -> ExprId {
        let mut e = self.parse_primary();
        loop {
            match self.cur().kind {
                LParen => {
                    self.bump();
                    let saved = self.no_struct;
                    self.no_struct = false;
                    let mut args = Vec::new();
                    while !self.at(RParen) && !self.at(Eof) {
                        args.push(self.parse_expr());
                        if !self.eat(Comma) {
                            break;
                        }
                    }
                    let end = self.cur().span;
                    self.expect(RParen, "`)`");
                    self.no_struct = saved;
                    // generic struct literal: `Name(typeargs){ fields }`
                    let name_info = match &self.ast.expr_at(e).kind {
                        ExprKind::Name(n) => Some((n.clone(), self.ast.expr_at(e).span)),
                        _ => None,
                    };
                    if self.at(LBrace) && !self.no_struct && name_info.is_some() {
                        let (ctor, start) = name_info.unwrap();
                        e = self.parse_gen_struct_lit(ctor, args, start);
                        continue;
                    }
                    let span = self.ast.expr_at(e).span.to(end);
                    e = self.ast.expr(ExprKind::Call { callee: e, args }, span);
                }
                LBracket => {
                    self.bump();
                    let saved = self.no_struct;
                    self.no_struct = false;
                    let index = self.parse_expr();
                    let end = self.cur().span;
                    self.expect(RBracket, "`]`");
                    self.no_struct = saved;
                    let span = self.ast.expr_at(e).span.to(end);
                    e = self.ast.expr(ExprKind::Index { base: e, index }, span);
                }
                DotStar => {
                    let t = self.bump();
                    let span = self.ast.expr_at(e).span.to(t.span);
                    e = self.ast.expr(ExprKind::Deref { base: e }, span);
                }
                Dot => {
                    self.bump();
                    let name = self.eat_ident("field or method name");
                    let span = self.ast.expr_at(e).span.to(name.span);
                    e = self.ast.expr(ExprKind::Field { base: e, name }, span);
                }
                Question => {
                    let t = self.bump();
                    let span = self.ast.expr_at(e).span.to(t.span);
                    e = self.ast.expr(ExprKind::Try { base: e }, span);
                }
                _ => break,
            }
        }
        e
    }

    /// Split an f-string `f"a {x} b"` into literal `parts` and interpolated `exprs`.
    /// Interpolations are bare identifiers (`{x}`); a `String` is the result.
    fn parse_fstring(&mut self, span: Span) -> ExprId {
        let raw = self.text(span);
        // Drop the `f"` prefix and the closing `"` (both ASCII, so byte-safe).
        let body: String = if raw.len() >= 3 { raw[2..raw.len() - 1].to_string() } else { String::new() };
        let mut parts: Vec<String> = Vec::new();
        let mut exprs: Vec<ExprId> = Vec::new();
        let mut cur = String::new();
        let mut chars = body.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '{' {
                parts.push(std::mem::take(&mut cur));
                let mut name = String::new();
                while let Some(&nc) = chars.peek() {
                    chars.next();
                    if nc == '}' {
                        break;
                    }
                    name.push(nc);
                }
                let name = name.trim().to_string();
                if name.is_empty() {
                    self.error(span, "empty `{}` interpolation in an f-string".to_string());
                }
                let id = self.ast.expr(ExprKind::Name(Ident { name, span }), span);
                exprs.push(id);
            } else {
                cur.push(c);
            }
        }
        parts.push(cur);
        self.ast.expr(ExprKind::FString { parts, exprs }, span)
    }

    fn parse_primary(&mut self) -> ExprId {
        let tok = self.cur();
        let span = tok.span;
        match tok.kind {
            Int => { self.bump(); let t = self.text(span); self.ast.expr(ExprKind::Int(t), span) }
            Float => { self.bump(); let t = self.text(span); self.ast.expr(ExprKind::Float(t), span) }
            Str => { self.bump(); let t = self.text(span); self.ast.expr(ExprKind::Str(t), span) }
            FStr => { self.bump(); self.parse_fstring(span) }
            Char => { self.bump(); let t = self.text(span); self.ast.expr(ExprKind::Char(t), span) }
            True => { self.bump(); self.ast.expr(ExprKind::Bool(true), span) }
            False => { self.bump(); self.ast.expr(ExprKind::Bool(false), span) }
            Null => { self.bump(); self.ast.expr(ExprKind::Null, span) }
            SelfValue => { self.bump(); self.ast.expr(ExprKind::SelfValue, span) }
            SelfType => {
                self.bump();
                if self.at(LBrace) && !self.no_struct {
                    return self.parse_struct_lit(Ident { name: "Self".to_string(), span });
                }
                self.ast.expr(ExprKind::SelfType, span)
            }
            TokenKind::Ident => {
                let t = self.bump();
                let id = self.ident(t);
                if self.at(LBrace) && !self.no_struct {
                    return self.parse_struct_lit(id);
                }
                self.ast.expr(ExprKind::Name(id), span)
            }
            At => {
                self.bump();
                let name = self.eat_ident("attribute name");
                let sp = span.to(name.span);
                self.ast.expr(ExprKind::Attr(name), sp)
            }
            LBracket => {
                // A fixed-size array literal: `[value; count]` (repeat) or
                // `[e0, e1, …]` (list). The token after the first element selects.
                self.bump();
                let saved = self.no_struct;
                self.no_struct = false;
                let first = self.parse_expr();
                let node = if self.at(Semi) {
                    self.bump();
                    let count = self.parse_expr();
                    ExprKind::ArrayRepeat { value: first, count }
                } else {
                    let mut elems = vec![first];
                    while self.at(Comma) {
                        self.bump();
                        if self.at(RBracket) {
                            break; // tolerate a trailing comma
                        }
                        elems.push(self.parse_expr());
                    }
                    ExprKind::ArrayLit { elems }
                };
                self.no_struct = saved;
                let end = self.cur().span;
                self.expect(RBracket, "`]`");
                self.ast.expr(node, span.to(end))
            }
            LParen => {
                self.bump();
                let saved = self.no_struct;
                self.no_struct = false;
                let inner = self.parse_expr();
                self.expect(RParen, "`)`");
                self.no_struct = saved;
                inner // grouping: postfix continues on the inner expression
            }
            Struct => {
                self.bump(); // consume `struct`; parse_struct_body starts at `{`
                let body = self.parse_struct_body();
                let sp = span.to(body.span);
                self.ast.expr(ExprKind::StructType(body), sp)
            }
            If => self.parse_if(),
            Match => self.parse_match(),
            Unsafe => self.parse_unsafe(),
            Concurrent => self.parse_concurrent(),
            Spawn => self.parse_spawn(),
            Region => self.parse_region(),
            For => self.parse_for(),
            While | Loop => self.parse_reserved_loop(),
            Break => {
                self.bump();
                let label = self.opt_loop_label();
                let sp = label.as_ref().map(|l| span.to(l.span)).unwrap_or(span);
                self.ast.expr(ExprKind::Break(label), sp)
            }
            Continue => {
                self.bump();
                let label = self.opt_loop_label();
                let sp = label.as_ref().map(|l| span.to(l.span)).unwrap_or(span);
                self.ast.expr(ExprKind::Continue(label), sp)
            }
            Invariant => {
                self.bump();
                let e = self.parse_expr();
                let sp = span.to(self.ast.expr_at(e).span);
                self.ast.expr(ExprKind::Invariant(e), sp)
            }
            TokenKind::Variant => {
                self.bump();
                let e = self.parse_expr();
                let sp = span.to(self.ast.expr_at(e).span);
                self.ast.expr(ExprKind::Variant(e), sp)
            }
            // `|x| body` and `|| body`: in expression-leading position `|`/`||`
            // can only begin a closure (the bit/logical-or operators are infix).
            Pipe | PipePipe => self.parse_closure(),
            LBrace => {
                let b = self.parse_block();
                let sp = b.span;
                self.ast.expr(ExprKind::Block(b), sp)
            }
            other => {
                self.error(span, format!("expected an expression, found `{}`", other.describe()));
                self.bump();
                self.ast.expr(ExprKind::Error, span)
            }
        }
    }

    fn parse_struct_lit(&mut self, path: Ident) -> ExprId {
        let start = path.span;
        self.expect(LBrace, "`{`");
        let saved = self.no_struct;
        self.no_struct = false;
        let mut fields = Vec::new();
        let mut spread = None;
        while !self.at(RBrace) && !self.at(Eof) {
            // `..base` — a functional-update spread; takes the remaining fields from
            // `base`. It is the final element of the literal.
            if self.at(DotDot) {
                self.bump();
                spread = Some(self.parse_expr());
                break;
            }
            let before = self.pos;
            let name = self.eat_ident("field name");
            self.expect(Colon, "`:`");
            let value = self.parse_expr();
            fields.push(FieldInit { name, value });
            if self.pos == before {
                self.bump();
            }
            if !self.eat(Comma) {
                break;
            }
        }
        let end = self.cur().span;
        self.expect(RBrace, "`}`");
        self.no_struct = saved;
        self.ast.expr(ExprKind::StructLit { path, fields, spread }, start.to(end))
    }

    fn parse_gen_struct_lit(&mut self, ctor: Ident, type_args: Vec<ExprId>, start: Span) -> ExprId {
        self.expect(LBrace, "`{`");
        let saved = self.no_struct;
        self.no_struct = false;
        let mut fields = Vec::new();
        while !self.at(RBrace) && !self.at(Eof) {
            let before = self.pos;
            let name = self.eat_ident("field name");
            self.expect(Colon, "`:`");
            let value = self.parse_expr();
            fields.push(FieldInit { name, value });
            if self.pos == before {
                self.bump();
            }
            if !self.eat(Comma) {
                break;
            }
        }
        let end = self.cur().span;
        self.expect(RBrace, "`}`");
        self.no_struct = saved;
        self.ast.expr(ExprKind::GenStructLit { ctor, type_args, fields }, start.to(end))
    }

    fn parse_if(&mut self) -> ExprId {
        let start = self.cur().span;
        self.expect(If, "`if`");
        let saved = self.no_struct;
        self.no_struct = true;
        let cond = self.parse_expr();
        self.no_struct = saved;
        let then = self.parse_block();
        let mut els = None;
        if self.eat(Else) {
            if self.at(If) {
                els = Some(self.parse_if());
            } else {
                let b = self.parse_block();
                let sp = b.span;
                els = Some(self.ast.expr(ExprKind::Block(b), sp));
            }
        }
        let end = els.map(|e| self.ast.expr_at(e).span).unwrap_or(then.span);
        self.ast.expr(ExprKind::If { cond, then, els }, start.to(end))
    }

    fn parse_unsafe(&mut self) -> ExprId {
        let start = self.cur().span;
        self.expect(Unsafe, "`unsafe`");
        let b = self.parse_block();
        let span = start.to(b.span);
        self.ast.expr(ExprKind::Unsafe(b), span)
    }

    /// `concurrent { spawn f(..); spawn g(..); }` — a structured-concurrency
    /// nursery (design §10.2). The spawned tasks join at the closing brace.
    fn parse_concurrent(&mut self) -> ExprId {
        let start = self.cur().span;
        self.expect(Concurrent, "`concurrent`");
        let b = self.parse_block();
        let span = start.to(b.span);
        self.ast.expr(ExprKind::Concurrent(b), span)
    }

    /// `spawn <call>` — launch a task. The inner expression is normally a call.
    fn parse_spawn(&mut self) -> ExprId {
        let start = self.cur().span;
        self.expect(Spawn, "`spawn`");
        let inner = self.parse_unary();
        let span = start.to(self.ast.expr_at(inner).span);
        self.ast.expr(ExprKind::Spawn(inner), span)
    }

    /// `region r { … }` — an arena scope. `&[r]T` references into it are zero-cost
    /// and the arena is freed at the closing brace (design §4.4).
    fn parse_region(&mut self) -> ExprId {
        let start = self.cur().span;
        self.expect(Region, "`region`");
        let name = self.eat_ident("region name");
        let body = self.parse_block();
        let span = start.to(body.span);
        self.ast.expr(ExprKind::Region { name, body }, span)
    }

    /// `for …` — the one loop keyword (see `docs/loops-spec.md`). Four shapes:
    /// infinite (`for { … }`), conditional (`for cond { … }`), and iteration over
    /// a range or slice (`for [conv] binding in iter { … }`).
    fn parse_for(&mut self) -> ExprId {
        let start = self.cur().span;
        self.expect(For, "`for`");
        // Optional loop label: `for outer: …` — the target of `break`/`continue outer`.
        let label = if self.at(TokenKind::Ident) && self.peek_kind(1) == Colon {
            let l = self.eat_ident("loop label");
            self.expect(Colon, "`:`");
            Some(l)
        } else {
            None
        };
        let head = self.parse_for_head();
        // Optional `region <name>` — a per-iteration scratch arena.
        let region = if self.eat(Region) { Some(self.eat_ident("region name")) } else { None };
        let body = self.parse_block();
        // Optional `else { … }` — runs once if the loop completes without `break`
        // (Python's loop-`else`; the search-or-default idiom). An infinite loop
        // only ever exits via `break`, so its `else` is dead code — reject it.
        let els = if self.eat(Else) {
            let els_start = self.prev_span();
            let b = self.parse_block();
            if matches!(head, ForHead::Infinite) {
                self.error(
                    els_start.to(b.span),
                    "an infinite `for { … }` only exits via `break`, so its `else` can never run — remove the `else`, or give the loop a condition or range",
                );
            }
            Some(b)
        } else {
            None
        };
        let span = els.as_ref().map_or(body.span, |b| b.span);
        let span = start.to(span);
        self.ast.expr(ExprKind::For { label, head, region, body, els }, span)
    }

    /// Parse a loop header (everything between `for` and the body / `region`).
    fn parse_for_head(&mut self) -> ForHead {
        // `for { … }` / `for region r { … }` — infinite.
        if self.at(LBrace) || self.at(Region) {
            return ForHead::Infinite;
        }

        // Iteration binding? A conv keyword (`read`/`mut`/`take`) introduces one,
        // or an ident/`_` followed by `in` or `,`.
        let is_iter = matches!(self.cur().kind, Read | Mut | Take)
            || ((self.at(TokenKind::Ident) || self.at(Underscore))
                && matches!(self.peek_kind(1), In | Comma));

        if is_iter {
            // A comma-list of bindings, each with an optional conv keyword.
            let mut binds = Vec::new();
            loop {
                let before = self.pos;
                let conv = self.parse_loop_conv();
                let name = self.parse_loop_binding();
                binds.push(LoopBind { conv, name });
                if self.pos == before {
                    self.bump();
                }
                if !self.eat(Comma) {
                    break;
                }
            }
            self.expect(In, "`in`");
            // A comma-list of source expressions (one per slice, plus the implicit
            // index for the element+index form).
            let saved = self.no_struct;
            self.no_struct = true;
            let mut sources = vec![self.parse_expr()];
            while self.eat(Comma) {
                sources.push(self.parse_expr());
            }
            // Optional `step <expr>` (a contextual keyword) for a range source.
            let step = if self.at(TokenKind::Ident) && self.text(self.cur().span) == "step" {
                self.bump();
                Some(self.parse_expr())
            } else {
                None
            };
            self.no_struct = saved;
            return ForHead::Iter { binds, sources, step };
        }

        // `for cond { … }` — conditional (the "while" job).
        let saved = self.no_struct;
        self.no_struct = true;
        let cond = self.parse_expr();
        self.no_struct = saved;
        ForHead::While(cond)
    }

    /// An optional `read`/`mut`/`take` convention before a loop binding.
    /// (`take` iteration is deferred — recover as `read` with a diagnostic.)
    fn parse_loop_conv(&mut self) -> Conv {
        match self.cur().kind {
            Read => { self.bump(); Conv::Read }
            Mut => { self.bump(); Conv::Mut }
            Take => {
                let sp = self.cur().span;
                self.bump();
                self.error(sp, "`take` iteration is not supported yet (slices are borrows); use `read` or `mut`");
                Conv::Read
            }
            _ => Conv::Read,
        }
    }

    /// An optional label after `break`/`continue` (`break outer`). A bare
    /// identifier; absent if the next token isn't one.
    fn opt_loop_label(&mut self) -> Option<Ident> {
        if self.at(TokenKind::Ident) {
            let t = self.bump();
            Some(self.ident(t))
        } else {
            None
        }
    }

    /// A loop variable: an identifier or the wildcard `_`.
    fn parse_loop_binding(&mut self) -> Ident {
        let t = self.cur();
        if t.kind == Underscore {
            self.bump();
            Ident { name: "_".to_string(), span: t.span }
        } else {
            self.eat_ident("loop variable")
        }
    }

    /// Reject the reserved `while`/`loop` keywords with a pointer to `for`, then
    /// recover by parsing the same shape as a `for` (no cascade of errors).
    fn parse_reserved_loop(&mut self) -> ExprId {
        let start = self.cur().span;
        if self.at(While) {
            self.error(start, "Jestyr has one loop keyword — write `for <cond> { … }` (not `while`)");
            self.bump();
            let saved = self.no_struct;
            self.no_struct = true;
            let cond = self.parse_expr();
            self.no_struct = saved;
            let body = self.parse_block();
            let span = start.to(body.span);
            self.ast.expr(ExprKind::For { label: None, head: ForHead::While(cond), region: None, body, els: None }, span)
        } else {
            self.error(start, "Jestyr has one loop keyword — write `for { … }` (not `loop`)");
            self.bump();
            let body = self.parse_block();
            let span = start.to(body.span);
            self.ast.expr(ExprKind::For { label: None, head: ForHead::Infinite, region: None, body, els: None }, span)
        }
    }

    fn parse_closure(&mut self) -> ExprId {
        let start = self.cur().span;
        let mut params = Vec::new();
        if self.eat(PipePipe) {
            // `||` — no parameters.
        } else {
            self.expect(Pipe, "`|`");
            while !self.at(Pipe) && !self.at(Eof) {
                let before = self.pos;
                let name = self.eat_ident("closure parameter name");
                let ty = if self.eat(Colon) { Some(self.parse_type()) } else { None };
                params.push(ClosureParam { name, ty });
                if self.pos == before {
                    self.bump();
                }
                if !self.eat(Comma) {
                    break;
                }
            }
            self.expect(Pipe, "`|`");
        }
        let body = self.parse_expr();
        let span = start.to(self.ast.expr_at(body).span);
        self.ast.expr(ExprKind::Closure { params, body }, span)
    }

    fn parse_match(&mut self) -> ExprId {
        let start = self.cur().span;
        self.expect(Match, "`match`");
        let saved = self.no_struct;
        self.no_struct = true;
        let scrut = self.parse_expr();
        self.no_struct = saved;
        self.expect(LBrace, "`{`");
        let mut arms = Vec::new();
        while !self.at(RBrace) && !self.at(Eof) {
            let before = self.pos;
            let pat = self.parse_pattern();
            // Optional guard: `pat if <bool-expr> => body`. The `if` here is a
            // contextual marker, not an if-expression — we parse the boolean that
            // follows it directly. The guard stops before `=>` since `=>` is not an
            // operator the Pratt parser will consume.
            let guard = if self.eat(If) {
                Some(self.parse_expr())
            } else {
                None
            };
            self.expect(FatArrow, "`=>`");
            let body = self.parse_expr();
            arms.push(MatchArm { pat, guard, body });
            if self.pos == before {
                self.bump();
            }
            if !self.eat(Comma) {
                break;
            }
        }
        let end = self.cur().span;
        self.expect(RBrace, "`}`");
        self.ast.expr(ExprKind::Match { scrut, arms }, start.to(end))
    }

    /// Parse a pattern, folding `|`-separated alternatives into an or-pattern.
    fn parse_pattern(&mut self) -> PatId {
        let first = self.parse_pattern_atom();
        if !self.at(Pipe) {
            return first;
        }
        let start = self.ast.pat_at(first).span;
        let mut alts = vec![first];
        while self.eat(Pipe) {
            alts.push(self.parse_pattern_atom());
        }
        let end = self.ast.pat_at(*alts.last().unwrap()).span;
        self.ast.pat(PatKind::Or(alts), start.to(end))
    }

    fn parse_pattern_atom(&mut self) -> PatId {
        let tok = self.cur();
        let span = tok.span;
        match tok.kind {
            Underscore => {
                self.bump();
                self.ast.pat(PatKind::Wildcard, span)
            }
            TokenKind::Ident => {
                let t = self.bump();
                let name = self.ident(t);
                if self.at(LBrace) {
                    // A struct-variant pattern `circle { r }` / `rect { w: 0.0, .. }`.
                    self.bump();
                    let mut fields = Vec::new();
                    let mut has_rest = false;
                    while !self.at(RBrace) && !self.at(Eof) {
                        if self.at(DotDot) {
                            self.bump();
                            has_rest = true;
                            break;
                        }
                        let ft = self.bump();
                        let fname = self.ident(ft);
                        let subpat = if self.eat(Colon) {
                            self.parse_pattern()
                        } else {
                            // shorthand: `{ r }` binds the field to a variable `r`
                            self.ast.pat(PatKind::Ident(fname.clone()), fname.span)
                        };
                        fields.push((fname, subpat));
                        if !self.eat(Comma) {
                            break;
                        }
                    }
                    let end = self.cur().span;
                    self.expect(RBrace, "`}`");
                    return self.ast.pat(
                        PatKind::StructVariant { name, fields, has_rest },
                        span.to(end),
                    );
                }
                if self.at(LParen) {
                    self.bump();
                    let mut subpats = Vec::new();
                    while !self.at(RParen) && !self.at(Eof) {
                        subpats.push(self.parse_pattern());
                        if !self.eat(Comma) {
                            break;
                        }
                    }
                    let end = self.cur().span;
                    self.expect(RParen, "`)`");
                    // `..` rest may only appear as the final field of a variant.
                    let n = subpats.len();
                    for (i, sp) in subpats.iter().enumerate() {
                        let is_rest = matches!(self.ast.pat_at(*sp).kind, PatKind::Rest);
                        let sp_span = self.ast.pat_at(*sp).span;
                        if is_rest && i + 1 != n {
                            self.error(sp_span, "`..` may only appear as the last field pattern".to_string());
                        }
                    }
                    self.ast.pat(PatKind::Variant { name, subpats }, span.to(end))
                } else {
                    self.ast.pat(PatKind::Ident(name), span)
                }
            }
            Int | Char | True | False | Minus => {
                let lo = self.parse_pat_lit().expect("literal-starting token");
                let lo_span = self.ast.expr_at(lo).span;
                if self.at(DotDot) || self.at(DotDotEq) {
                    let inclusive = self.at(DotDotEq);
                    self.bump();
                    let hi = match self.parse_pat_lit() {
                        Some(h) => h,
                        None => {
                            let sp = self.cur().span;
                            self.error(sp, "expected the upper bound of a range pattern".to_string());
                            return self.ast.pat(PatKind::Error, lo_span);
                        }
                    };
                    let s = lo_span.to(self.ast.expr_at(hi).span);
                    self.ast.pat(PatKind::Range { lo, hi, inclusive }, s)
                } else {
                    self.ast.pat(PatKind::Lit(lo), lo_span)
                }
            }
            DotDot => {
                // The `..` rest — only meaningful as a variant's last field, which
                // the variant branch validates.
                self.bump();
                self.ast.pat(PatKind::Rest, span)
            }
            other => {
                self.error(span, format!("expected a pattern, found `{}`", other.describe()));
                self.bump();
                self.ast.pat(PatKind::Error, span)
            }
        }
    }

    /// Parse a scalar literal used inside a pattern (`0`, `-3`, `'a'`, `true`),
    /// returning its expression id — or `None` if the cursor isn't on a literal.
    /// Float literals are intentionally excluded (float equality is a footgun).
    fn parse_pat_lit(&mut self) -> Option<ExprId> {
        let tok = self.cur();
        let span = tok.span;
        match tok.kind {
            Minus => {
                self.bump();
                let rhs = self.parse_pat_lit()?;
                let s = span.to(self.ast.expr_at(rhs).span);
                Some(self.ast.expr(ExprKind::Unary { op: UnOp::Neg, rhs }, s))
            }
            Int => {
                self.bump();
                let t = self.text(span);
                Some(self.ast.expr(ExprKind::Int(t), span))
            }
            Char => {
                self.bump();
                let t = self.text(span);
                Some(self.ast.expr(ExprKind::Char(t), span))
            }
            True => {
                self.bump();
                Some(self.ast.expr(ExprKind::Bool(true), span))
            }
            False => {
                self.bump();
                Some(self.ast.expr(ExprKind::Bool(false), span))
            }
            _ => None,
        }
    }

    fn starts_expr(&self) -> bool {
        matches!(
            self.cur().kind,
            Int | Float | Str | Char | True | False | Null | TokenKind::Ident | Underscore | SelfValue
                | SelfType | LParen | At | If | Match | Unsafe | LBrace | Struct | Minus | Not
                | Bang | Tilde | Amp | Pipe | PipePipe
        )
    }
}

/// Infix binding powers: `(left, right, op)`. Higher binds tighter; left < right
/// makes the operator left-associative.
fn bin_op(k: TokenKind) -> Option<(u8, u8, BinOp)> {
    Some(match k {
        Or => (7, 8, BinOp::Or),
        And => (9, 10, BinOp::And),
        EqEq => (11, 12, BinOp::Eq),
        Ne => (11, 12, BinOp::Ne),
        Lt => (11, 12, BinOp::Lt),
        Le => (11, 12, BinOp::Le),
        Gt => (11, 12, BinOp::Gt),
        Ge => (11, 12, BinOp::Ge),
        Pipe => (13, 14, BinOp::BitOr),
        Caret => (15, 16, BinOp::BitXor),
        Amp => (17, 18, BinOp::BitAnd),
        Shl => (19, 20, BinOp::Shl),
        Shr => (19, 20, BinOp::Shr),
        Plus => (21, 22, BinOp::Add),
        Minus => (21, 22, BinOp::Sub),
        Star => (23, 24, BinOp::Mul),
        Slash => (23, 24, BinOp::Div),
        Percent => (23, 24, BinOp::Rem),
        _ => return None,
    })
}

fn assign_op(k: TokenKind) -> Option<AssignOp> {
    Some(match k {
        Eq => AssignOp::Assign,
        PlusEq => AssignOp::Add,
        MinusEq => AssignOp::Sub,
        StarEq => AssignOp::Mul,
        SlashEq => AssignOp::Div,
        PercentEq => AssignOp::Rem,
        AmpEq => AssignOp::BitAnd,
        PipeEq => AssignOp::BitOr,
        CaretEq => AssignOp::BitXor,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::Lexer;
    use crate::printer::print_ast;

    fn parse(src: &str) -> (Ast, Vec<Diagnostic>) {
        let (tokens, lex_diags) = Lexer::new(src).tokenize();
        assert!(lex_diags.is_empty(), "lex errors: {:?}", lex_diags);
        Parser::new(src, tokens).parse()
    }

    fn parse_ok(src: &str) -> Ast {
        let (ast, diags) = parse(src);
        assert!(diags.is_empty(), "parse errors: {:?}", diags);
        ast
    }

    #[test]
    fn parses_region_block_and_region_ref() {
        let ast = parse_ok("fn f() { region r { var a: &[r]i32 = region_alloc(r, i32, 1) } }");
        let has_region = ast.exprs.iter().any(|e| matches!(e.kind, ExprKind::Region { .. }));
        let has_rref = ast.types.iter().any(|t| matches!(t.kind, TypeKind::RegionRef { .. }));
        assert!(has_region, "a region block was parsed");
        assert!(has_rref, "a &[r]T region-ref type was parsed");
    }

    #[test]
    fn parses_genref_type() {
        let ast = parse_ok("fn f(r: &i32) -> i32 { r.* }");
        match &ast.items[0] {
            Item::Fn(f) => {
                let ty = f.params[0].ty.expect("param has a type");
                assert!(matches!(ast.type_at(ty).kind, TypeKind::GenRef(_)), "param is a &T");
            }
            _ => panic!("expected a function"),
        }
    }

    // --- function-pointer types ---

    #[test]
    fn parses_a_fn_pointer_type_in_a_signature() {
        let ast = parse_ok("fn apply(f: fn(i32) -> i32, x: i32) -> i32 { f(x) }");
        let Item::Fn(func) = &ast.items[0] else { panic!("expected a function") };
        let ty = func.params[0].ty.expect("f has a type");
        let TypeKind::Fn { params, ret, .. } = &ast.type_at(ty).kind else {
            panic!("param `f` should be a fn-pointer type, got {:?}", ast.type_at(ty).kind)
        };
        assert_eq!(params.len(), 1, "one parameter type");
        assert!(ret.is_some(), "an explicit `-> i32` return type");
    }

    #[test]
    fn parses_conventions_inside_a_fn_pointer_type() {
        // The Jestyr-novel bit: passing conventions live *inside* the function
        // type, so `read`/`take`/`mut`/`out` are part of the type, not the name.
        let ast = parse_ok("fn f(g: fn(read i32, take i32) -> read i32) { }");
        let Item::Fn(func) = &ast.items[0] else { panic!() };
        let ty = func.params[0].ty.unwrap();
        let TypeKind::Fn { params, ret_conv, ret } = &ast.type_at(ty).kind else { panic!() };
        assert_eq!(params[0].conv, Conv::Read);
        assert_eq!(params[1].conv, Conv::Take);
        assert_eq!(*ret_conv, Conv::Read);
        assert!(ret.is_some());
    }

    #[test]
    fn parses_a_unit_returning_fn_pointer_without_an_arrow() {
        let ast = parse_ok("fn f(free_fn: fn(*mut u8)) { }");
        let Item::Fn(func) = &ast.items[0] else { panic!() };
        let ty = func.params[0].ty.unwrap();
        let TypeKind::Fn { params, ret, ret_conv } = &ast.type_at(ty).kind else { panic!() };
        assert_eq!(params.len(), 1);
        assert!(ret.is_none(), "no `->` means a unit-returning pointer");
        assert_eq!(*ret_conv, Conv::Default);
    }

    #[test]
    fn parses_a_struct_of_fn_pointers_the_vtable_shape() {
        let ast = parse_ok(
            "struct Allocator { alloc_fn: fn(usize) -> *mut u8, free_fn: fn(*mut u8) }",
        );
        let Item::Struct { body, .. } = &ast.items[0] else { panic!() };
        let fn_fields = body
            .members
            .iter()
            .filter(|m| {
                matches!(m, StructMember::Field { ty, .. }
                    if matches!(ast.type_at(*ty).kind, TypeKind::Fn { .. }))
            })
            .count();
        assert_eq!(fn_fields, 2, "both fields are fn-pointer typed");
    }

    #[test]
    fn prints_a_fn_pointer_type_round_trip() {
        let ast = parse_ok("fn f(g: fn(read i32) -> i32) { }");
        let out = print_ast(&ast);
        assert!(out.contains("fn(read i32) -> i32"), "printer renders the fn-ptr type: {out}");
    }

    #[test]
    fn parser_recovers_from_a_malformed_fn_pointer_type() {
        // Garbage between the parens must not spin the parser (progress guard).
        let (_ast, diags) = parse("fn f(g: fn(,,,) -> i32) { }");
        assert!(!diags.is_empty(), "a malformed parameter list reports an error");
    }

    // --- traits / impl / dyn / bounds (Stage A) ---

    #[test]
    fn parses_a_trait_with_required_and_default_methods() {
        let ast = parse_ok(
            "trait Show { fn show(read self) -> i32  fn label(read self) -> i32 { return 0 } }",
        );
        let Item::Trait(t) = &ast.items[0] else { panic!("expected a trait") };
        assert_eq!(t.name.name, "Show");
        assert_eq!(t.methods.len(), 2);
        assert!(t.methods[0].default_body.is_none(), "show is a required signature");
        assert!(t.methods[1].default_body.is_some(), "label has a default body");
    }

    #[test]
    fn parses_an_impl_block() {
        let ast = parse_ok("impl Show for i32 { fn show(read self) -> i32 { return self } }");
        let Item::Impl(im) = &ast.items[0] else { panic!("expected an impl") };
        assert_eq!(im.trait_name.name, "Show");
        assert!(matches!(ast.type_at(im.ty).kind, TypeKind::Name(_)), "impl target is a type");
        assert_eq!(im.methods.len(), 1);
    }

    #[test]
    fn parses_a_bounded_generic_function() {
        let ast = parse_ok("fn describe[T: Show, U](read x: T) -> i32 { return 0 }");
        let Item::Fn(f) = &ast.items[0] else { panic!() };
        assert_eq!(f.generics.len(), 2);
        assert_eq!(f.generics[0].name.name, "T");
        assert_eq!(f.generics[0].bound.as_ref().map(|b| b.name.as_str()), Some("Show"));
        assert!(f.generics[1].bound.is_none(), "U is an unbounded type parameter");
    }

    #[test]
    fn parses_a_dyn_trait_type() {
        let ast = parse_ok("fn render(read s: dyn Show) -> i32 { return 0 }");
        let Item::Fn(f) = &ast.items[0] else { panic!() };
        let ty = f.params[0].ty.unwrap();
        assert!(matches!(&ast.type_at(ty).kind, TypeKind::Dyn(n) if n.name == "Show"));
    }

    #[test]
    fn prints_trait_impl_and_bounds_round_trip() {
        let ast = parse_ok(
            "trait Show { fn show(read self) -> i32 } \
             impl Show for i32 { fn show(read self) -> i32 { return self } } \
             fn f[T: Show](read x: T) -> i32 { return 0 }",
        );
        let out = print_ast(&ast);
        assert!(out.contains("trait Show"), "{out}");
        assert!(out.contains("impl Show for i32"), "{out}");
        assert!(out.contains("fn f[T: Show]"), "{out}");
    }

    #[test]
    fn recovers_from_a_malformed_trait_body() {
        let (_ast, diags) = parse("trait T { @ @ @ }");
        assert!(!diags.is_empty(), "garbage in a trait body reports an error");
    }

    #[test]
    fn parses_match_arm_guard() {
        let ast = parse_ok(
            "enum S { circle(r: f64), square } \
             fn f(read s: S) -> i32 { match s { circle(r) if r > 0.0 => 1, square => 0 } }",
        );
        let arms = ast
            .exprs
            .iter()
            .find_map(|e| match &e.kind {
                ExprKind::Match { arms, .. } => Some(arms.clone()),
                _ => None,
            })
            .expect("a match expression");
        assert_eq!(arms.len(), 2);
        assert!(arms[0].guard.is_some(), "the guarded arm carries a guard");
        assert!(arms[1].guard.is_none(), "the unguarded arm has no guard");
    }

    #[test]
    fn parses_literal_and_range_patterns() {
        let ast = parse_ok(
            "fn f(read n: i32) -> i32 { match n { 0 => 0, 1..=9 => 1, 10..20 => 2, _ => 9 } }",
        );
        let arms = ast
            .exprs
            .iter()
            .find_map(|e| match &e.kind {
                ExprKind::Match { arms, .. } => Some(arms.clone()),
                _ => None,
            })
            .expect("a match expression");
        assert!(matches!(ast.pat_at(arms[0].pat).kind, PatKind::Lit(_)), "first arm is a literal");
        assert!(
            matches!(ast.pat_at(arms[1].pat).kind, PatKind::Range { inclusive: true, .. }),
            "second arm is an inclusive range"
        );
        assert!(
            matches!(ast.pat_at(arms[2].pat).kind, PatKind::Range { inclusive: false, .. }),
            "third arm is a half-open range"
        );
        assert!(matches!(ast.pat_at(arms[3].pat).kind, PatKind::Wildcard));
    }

    #[test]
    fn parses_or_pattern() {
        let ast = parse_ok(
            "enum C { red, green, blue } fn f(read c: C) -> i32 { match c { red | green | blue => 1 } }",
        );
        let arms = ast
            .exprs
            .iter()
            .find_map(|e| match &e.kind {
                ExprKind::Match { arms, .. } => Some(arms.clone()),
                _ => None,
            })
            .expect("a match expression");
        match &ast.pat_at(arms[0].pat).kind {
            PatKind::Or(alts) => assert_eq!(alts.len(), 3, "three alternatives"),
            other => panic!("expected an or-pattern, got {other:?}"),
        }
    }

    #[test]
    fn parses_rest_in_variant_pattern() {
        let ast = parse_ok(
            "enum E { c(x: i32, y: i32) } fn f(read e: E) -> i32 { match e { c(x, ..) => x } }",
        );
        let arms = ast
            .exprs
            .iter()
            .find_map(|e| match &e.kind {
                ExprKind::Match { arms, .. } => Some(arms.clone()),
                _ => None,
            })
            .expect("a match expression");
        match &ast.pat_at(arms[0].pat).kind {
            PatKind::Variant { subpats, .. } => {
                assert_eq!(subpats.len(), 2);
                assert!(matches!(ast.pat_at(subpats[0]).kind, PatKind::Ident(_)));
                assert!(matches!(ast.pat_at(subpats[1]).kind, PatKind::Rest));
            }
            other => panic!("expected a variant pattern, got {other:?}"),
        }
    }

    #[test]
    fn rest_must_be_the_last_field() {
        let (_ast, d) = parse(
            "enum E { c(x: i32, y: i32) } fn f(read e: E) -> i32 { match e { c(.., y) => y } }",
        );
        assert!(d.iter().any(|x| x.message.contains("last field")), "{:?}", d);
    }

    #[test]
    fn parses_bit_field_width() {
        let ast = parse_ok("struct F { a: u8 : 1, b: u8 : 3 }");
        let body = ast
            .items
            .iter()
            .find_map(|it| match it {
                Item::Struct { body, .. } => Some(body),
                _ => None,
            })
            .expect("a struct");
        let widths: Vec<Option<u32>> = body
            .members
            .iter()
            .filter_map(|m| match m {
                StructMember::Field { bits, .. } => Some(*bits),
                _ => None,
            })
            .collect();
        assert_eq!(widths, vec![Some(1), Some(3)], "bit-field widths parsed");
    }

    #[test]
    fn parses_union() {
        let ast = parse_ok("union U { a: i32, b: f32 }");
        let is_union = ast.items.iter().any(|it| matches!(it, Item::Struct { is_union: true, .. }));
        assert!(is_union, "a `union` parses as an Item::Struct with is_union set");
    }

    #[test]
    fn parses_pub_field() {
        let ast = parse_ok("struct P { pub x: i32, y: i32 }");
        let body = ast
            .items
            .iter()
            .find_map(|it| match it {
                Item::Struct { body, .. } => Some(body),
                _ => None,
            })
            .expect("a struct");
        let pubs: Vec<bool> = body
            .members
            .iter()
            .filter_map(|m| match m {
                StructMember::Field { is_pub, .. } => Some(*is_pub),
                _ => None,
            })
            .collect();
        assert_eq!(pubs, vec![true, false], "x is pub, y is private");
    }

    #[test]
    fn parses_field_default() {
        let ast = parse_ok("struct C { x: i32 = 3, y: i32 }");
        let body = ast
            .items
            .iter()
            .find_map(|it| match it {
                Item::Struct { body, .. } => Some(body),
                _ => None,
            })
            .expect("a struct");
        let has_default =
            body.members.iter().any(|m| matches!(m, StructMember::Field { default: Some(_), .. }));
        assert!(has_default, "the `= 3` field default was parsed");
    }

    #[test]
    fn parses_struct_spread() {
        let ast = parse_ok("struct P { x: i32, y: i32 } fn f(read p: P) -> P { P { x: 9, ..p } }");
        let has_spread = ast
            .exprs
            .iter()
            .any(|e| matches!(&e.kind, ExprKind::StructLit { spread: Some(_), .. }));
        assert!(has_spread, "the `..p` functional-update spread was parsed");
    }

    #[test]
    fn parses_struct_variant_pattern() {
        let ast = parse_ok(
            "enum S { circle(r: f64), dot } \
             fn f(read s: S) -> f64 { match s { circle { r } => r, dot => 0.0 } }",
        );
        let arms = ast
            .exprs
            .iter()
            .find_map(|e| match &e.kind {
                ExprKind::Match { arms, .. } => Some(arms.clone()),
                _ => None,
            })
            .expect("a match expression");
        match &ast.pat_at(arms[0].pat).kind {
            PatKind::StructVariant { name, fields, has_rest } => {
                assert_eq!(name.name, "circle");
                assert_eq!(fields.len(), 1);
                assert!(!has_rest);
            }
            other => panic!("expected a struct-variant pattern, got {other:?}"),
        }
    }

    #[test]
    fn parses_struct_variant_pattern_with_rest() {
        let ast = parse_ok(
            "enum S { rect(w: f64, h: f64) } \
             fn f(read s: S) -> f64 { match s { rect { w, .. } => w } }",
        );
        let arms = ast
            .exprs
            .iter()
            .find_map(|e| match &e.kind {
                ExprKind::Match { arms, .. } => Some(arms.clone()),
                _ => None,
            })
            .expect("a match expression");
        match &ast.pat_at(arms[0].pat).kind {
            PatKind::StructVariant { fields, has_rest, .. } => {
                assert_eq!(fields.len(), 1, "only `w` is named");
                assert!(has_rest, "`..` sets has_rest");
            }
            other => panic!("expected a struct-variant pattern, got {other:?}"),
        }
    }

    #[test]
    fn parses_slice_type() {
        let ast = parse_ok("fn f(s: []i32) -> i32 { s.len }");
        match &ast.items[0] {
            Item::Fn(f) => {
                let ty = f.params[0].ty.expect("param has a type");
                assert!(matches!(ast.type_at(ty).kind, TypeKind::Slice(_)), "param is a slice");
            }
            _ => panic!("expected a function"),
        }
    }

    #[test]
    fn parses_struct_attributes() {
        let ast = parse_ok("@packed @align(8) struct S { x: i32 }");
        match &ast.items[0] {
            Item::Struct { attrs, .. } => {
                assert_eq!(attrs.len(), 2);
                assert_eq!(attrs[0].name, "packed");
                assert_eq!(attrs[1].name, "align");
            }
            _ => panic!("expected a struct"),
        }
    }

    #[test]
    fn parses_a_record_as_an_immutable_struct() {
        let ast = parse_ok("record Point { x: i32, y: i32 }");
        match &ast.items[0] {
            Item::Struct { is_record, name, .. } => {
                assert!(*is_record, "parsed as a record");
                assert_eq!(name.name, "Point");
            }
            _ => panic!("expected a struct item"),
        }
        // …while a plain `struct` is not a record.
        let s = parse_ok("struct S { x: i32 }");
        assert!(matches!(&s.items[0], Item::Struct { is_record: false, .. }));
    }

    #[test]
    fn parses_a_generic_enum_with_type_params() {
        let ast = parse_ok("enum Option(T) { none, some(x: T) }");
        match &ast.items[0] {
            Item::Enum(e) => {
                assert!(e.is_generic());
                assert_eq!(e.type_params.len(), 1);
                assert_eq!(e.type_params[0].name, "T");
                assert_eq!(e.variants.len(), 2);
            }
            _ => panic!("expected an enum item"),
        }
        // A plain enum has no type parameters.
        let p = parse_ok("enum Color { red, green }");
        assert!(matches!(&p.items[0], Item::Enum(e) if !e.is_generic()));
    }

    #[test]
    fn parses_explicit_enum_discriminants() {
        let ast = parse_ok("enum Color { red = 1, green = 2, blue = 4 }");
        match &ast.items[0] {
            Item::Enum(e) => {
                assert!(e.variants.iter().all(|v| v.discriminant.is_some()), "all have `= n`");
            }
            _ => panic!("expected an enum item"),
        }
        // No `= n` → no discriminant.
        let p = parse_ok("enum E { a, b }");
        assert!(matches!(&p.items[0], Item::Enum(e) if e.variants[0].discriminant.is_none()));
    }

    #[test]
    fn indirect_parses_as_a_pointer() {
        // `indirect T` is sugar for a raw pointer `*T` (the recursion spelling).
        let ast = parse_ok("fn f(t: indirect i32) -> i32 { return 0 }");
        match &ast.items[0] {
            Item::Fn(f) => {
                let ty = f.params[0].ty.expect("param has a type");
                assert!(matches!(ast.type_at(ty).kind, TypeKind::Ptr { .. }), "indirect → pointer");
            }
            _ => panic!("expected a function"),
        }
    }

    #[test]
    fn parses_a_distinct_type() {
        let ast = parse_ok("distinct UserId = i32");
        assert!(
            matches!(&ast.items[0], Item::Distinct(d) if d.name.name == "UserId"),
            "expected a distinct item"
        );
    }

    #[test]
    fn record_rejects_a_mut_self_method() {
        let (_ast, d) = parse("record P { x: i32  fn bump(mut self) { self.x = 1 } }");
        assert!(
            d.iter().any(|m| m.message.contains("cannot take `mut self`")),
            "expected a mut-self rejection: {:?}",
            d
        );
    }

    #[test]
    fn parses_concurrent_and_spawn() {
        let ast = parse_ok("fn f() { concurrent { spawn g() spawn h() } }");
        let has_conc = ast.exprs.iter().any(|e| matches!(e.kind, ExprKind::Concurrent(_)));
        let has_spawn = ast.exprs.iter().filter(|e| matches!(e.kind, ExprKind::Spawn(_))).count();
        assert!(has_conc, "a concurrent nursery was parsed");
        assert_eq!(has_spawn, 2, "two spawn tasks");
    }

    #[test]
    fn parses_extern_c_declaration() {
        let ast = parse_ok("extern \"c\" fn malloc(size: usize) -> *mut u8");
        match &ast.items[0] {
            Item::Extern(e) => {
                assert_eq!(e.abi, "c");
                assert_eq!(e.name.name, "malloc");
                assert_eq!(e.params.len(), 1);
                assert!(e.ret_ty.is_some());
            }
            _ => panic!("expected an extern declaration"),
        }
    }

    #[test]
    fn parses_imports_and_pub_visibility() {
        let ast = parse_ok(
            "import \"std/mem\"\nimport \"io\" as iolib\npub fn f() {}\nfn g() {}",
        );
        // Two imports: default binding `mem` (from the path tail) and alias `iolib`.
        let imports: Vec<(&str, Option<&str>)> = ast
            .items
            .iter()
            .filter_map(|i| match i {
                Item::Import(im) => Some((im.path.as_str(), im.alias.as_ref().map(|a| a.name.as_str()))),
                _ => None,
            })
            .collect();
        assert_eq!(imports, vec![("std/mem", None), ("io", Some("iolib"))]);
        // `pub fn f` is public; `fn g` is private.
        let vis: Vec<(&str, bool)> = ast
            .items
            .iter()
            .filter_map(|i| match i {
                Item::Fn(f) => Some((f.name.name.as_str(), f.is_pub)),
                _ => None,
            })
            .collect();
        assert_eq!(vis, vec![("f", true), ("g", false)]);
    }

    #[test]
    fn parses_all_loop_header_shapes() {
        let ast = parse_ok(
            "fn f() { for i in 0..n {} for i in 0..=n {} for x in xs {} for mut y in ys {} \
             for _ in 0..3 {} for c {} for {} }",
        );
        let fors = ast.exprs.iter().filter(|e| matches!(e.kind, ExprKind::For { .. })).count();
        assert_eq!(fors, 7, "all seven loop forms parse");
    }

    #[test]
    fn rejects_while_and_loop_with_a_pointer_to_for() {
        let (_a, d) = parse("fn f() { while c { } }");
        assert_eq!(d.len(), 1, "exactly one error, no cascade: {:?}", d);
        assert!(d[0].message.contains("one loop keyword") && d[0].message.contains("`while`"));
        let (_a2, d2) = parse("fn g() { loop { } }");
        assert_eq!(d2.len(), 1, "{:?}", d2);
        assert!(d2[0].message.contains("not `loop`"), "{:?}", d2);
    }

    #[test]
    fn parses_labels_steps_and_variant() {
        let ast = parse_ok(
            "fn f() { for outer: i in 0..n step 2 { variant n break outer continue outer } \
             for j in n..0 step -1 {} }",
        );
        // The labeled loop carries its label and step.
        let labeled = ast.exprs.iter().find_map(|e| match &e.kind {
            ExprKind::For { label: Some(l), head: ForHead::Iter { step: Some(_), .. }, .. } => Some(l.name.clone()),
            _ => None,
        });
        assert_eq!(labeled.as_deref(), Some("outer"), "labeled + stepped loop parsed");
        let kinds = || ast.exprs.iter().map(|e| &e.kind);
        assert!(kinds().any(|k| matches!(k, ExprKind::Variant(_))), "variant parsed");
        assert!(
            kinds().any(|k| matches!(k, ExprKind::Break(Some(l)) if l.name == "outer")),
            "labeled break parsed",
        );
        assert!(
            kinds().any(|k| matches!(k, ExprKind::Continue(Some(l)) if l.name == "outer")),
            "labeled continue parsed",
        );
    }

    #[test]
    fn parses_loop_else_on_iter_and_conditional() {
        let ast = parse_ok(
            "fn f() { for x in xs { break } else { } for c { } else { } }",
        );
        let with_else = ast
            .exprs
            .iter()
            .filter(|e| matches!(&e.kind, ExprKind::For { els: Some(_), .. }))
            .count();
        assert_eq!(with_else, 2, "both an iter-loop and a conditional loop carry an `else`");
    }

    #[test]
    fn rejects_else_on_an_infinite_loop() {
        let (_a, d) = parse("fn f() { for { break } else { } }");
        assert_eq!(d.len(), 1, "exactly one error: {:?}", d);
        assert!(
            d[0].message.contains("infinite") && d[0].message.contains("`else`"),
            "points at the dead `else`: {:?}",
            d,
        );
    }

    #[test]
    fn parses_break_continue_and_invariant() {
        let ast = parse_ok("fn f() { for { invariant 1 == 1 if 0 == 0 { break } continue } }");
        let kinds = || ast.exprs.iter().map(|e| &e.kind);
        assert!(kinds().any(|k| matches!(k, ExprKind::Break(_))), "break parsed");
        assert!(kinds().any(|k| matches!(k, ExprKind::Continue(_))), "continue parsed");
        assert!(kinds().any(|k| matches!(k, ExprKind::Invariant(_))), "invariant parsed");
    }

    #[test]
    fn parses_function_contracts() {
        let ast = parse_ok("fn f(b: i32) -> i32 requires b != 0 ensures result > 0 { return b }");
        match &ast.items[0] {
            Item::Fn(f) => {
                assert_eq!(f.requires.len(), 1, "one precondition");
                assert_eq!(f.ensures.len(), 1, "one postcondition");
            }
            _ => panic!("expected a function"),
        }
    }

    #[test]
    fn simple_fn() {
        let ast = parse_ok("fn area(read s: Shape) -> f64 { s }");
        assert_eq!(ast.items.len(), 1);
        match &ast.items[0] {
            Item::Fn(f) => {
                assert_eq!(f.name.name, "area");
                assert_eq!(f.params.len(), 1);
                assert_eq!(f.params[0].conv, Conv::Read);
            }
            _ => panic!("expected fn"),
        }
    }

    #[test]
    fn enum_with_payloads() {
        let ast = parse_ok("enum Shape { circle(r: f64), rect(w: f64, h: f64), none }");
        match &ast.items[0] {
            Item::Enum(e) => {
                assert_eq!(e.variants.len(), 3);
                assert_eq!(e.variants[1].fields.len(), 2);
                assert!(e.variants[2].fields.is_empty());
            }
            _ => panic!("expected enum"),
        }
    }

    #[test]
    fn precedence_is_left_and_multiplicative_binds_tighter() {
        // `1 + 2 * 3` must group as `1 + (2 * 3)`, and `a * b * c` as `(a*b)*c`.
        let ast = parse_ok("const C: i32 = 1 + 2 * 3");
        let out = print_ast(&ast);
        assert!(out.contains("(1 + (2 * 3))"), "got:\n{out}");
    }

    #[test]
    fn assignment_is_right_associative_and_low() {
        let ast = parse_ok("fn f() { a = b = c }");
        let out = print_ast(&ast);
        assert!(out.contains("a = b = c"), "got:\n{out}");
    }

    #[test]
    fn postfix_chain() {
        // self.grow(alloc)?  =>  ((self.grow)(alloc))?
        let ast = parse_ok("fn f() { self.grow(alloc)? }");
        let out = print_ast(&ast);
        assert!(out.contains("self.grow(alloc)?"), "got:\n{out}");
    }

    #[test]
    fn deref_assignment() {
        let ast = parse_ok("fn f() { unsafe { (p + i).* = v } }");
        let out = print_ast(&ast);
        assert!(out.contains("(p + i).* = v"), "got:\n{out}");
    }

    #[test]
    fn match_does_not_eat_scrutinee_as_struct_literal() {
        let ast = parse_ok("fn f() { match s { none => 0 } }");
        assert!(parse(/* sanity */ "fn f() { match s { none => 0 } }").1.is_empty());
        let out = print_ast(&ast);
        assert!(out.contains("match s"), "got:\n{out}");
    }

    #[test]
    fn parses_type_application_and_generic_struct_literal() {
        // type application in a signature, plus a generic struct literal in the body
        let ast = parse_ok("fn f(l: List(i32)) -> List(f64) { List(f64){ x: 1, y: 2 } }");
        let out = print_ast(&ast);
        assert!(out.contains("List(i32)"), "type application: {out}");
        assert!(out.contains("List(f64){ x: 1, y: 2 }"), "generic struct literal: {out}");
    }

    /// The headline test: the full Vec/Shape example file must parse cleanly.
    #[test]
    fn parses_the_vec_example_file() {
        let src = include_str!("../examples/vec.jtr");
        let (ast, diags) = parse(src);
        assert!(diags.is_empty(), "parse errors: {:?}", diags);
        // fn Vec, enum Shape, fn area, const UART0, const TX_FULL
        assert_eq!(ast.items.len(), 5, "items: {}", ast.items.len());
    }
}
