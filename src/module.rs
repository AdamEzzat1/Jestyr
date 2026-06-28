//! The module loader (design §9) — multi-file Jestyr programs.
//!
//! ## Model: a file is a module, the program is one "translation unit"
//! Jestyr compiles a whole program as a single C translation unit. Rather than
//! fight that, the loader embraces it: starting from a root file it follows
//! `import` declarations, parsing **every** reachable file into *one shared
//! arena* (so all `ExprId`/`TypeId` handles share an id-space — no remapping),
//! and concatenating their sources into *one buffer* (so every span is a unique
//! global offset). Downstream passes — escape and codegen — never learn that
//! there was more than one file.
//!
//! ## What the loader is responsible for
//!  * **`import` resolution** — a path relative to the importing file, `.jtr`
//!    appended; `import "std/mem"` binds `mem` (or an `as` alias);
//!  * **memoization** — a file reached by two paths (a diamond) is loaded once;
//!  * **cycle detection** — `import` graphs must be a DAG (design §9), which is
//!    what makes parallel + incremental compilation possible later;
//!  * **module bookkeeping** — which module each item belongs to, whether it is
//!    `pub`, and each module's import bindings — handed to the type checker so it
//!    can enforce visibility and resolve qualified access (`mem.allocate`).
//!
//! Visibility and qualified-name resolution themselves live in the type checker
//! (`typeck.rs`), where lexical scope is already tracked — so a local variable
//! can never be mistaken for a module name.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::ast::{Ast, Item};
use crate::diag::{Diagnostic, Severity};
use crate::lexer::Lexer;
use crate::parser::Parser;
use crate::span::Span;

/// A module's index. The root file is always module 0.
pub type ModId = usize;

/// Per-module bookkeeping, threaded into the type checker.
#[derive(Default)]
pub struct Modules {
    /// Display name of each module (its file stem), e.g. `mem`.
    pub names: Vec<String>,
    /// Path shown in diagnostics for each module.
    pub paths: Vec<String>,
    /// Each module's own source text (for local-offset diagnostic rendering).
    pub srcs: Vec<String>,
    /// Each module's base offset within the concatenated global source buffer.
    pub bases: Vec<usize>,
    /// The owning module of each item in `Program::ast.items` (parallel vector).
    pub item_mod: Vec<ModId>,
    /// Whether each item in `Program::ast.items` is `pub` (parallel vector).
    pub item_pub: Vec<bool>,
    /// Per module: import binding name → the module it refers to.
    pub imports: Vec<HashMap<String, ModId>>,
}

impl Modules {
    /// A single-module program (used by the unit-test entry points that bypass
    /// the loader): every item belongs to module 0, nothing is imported.
    #[allow(dead_code)]
    pub fn single(ast: &Ast) -> Modules {
        Modules {
            names: vec!["main".to_string()],
            paths: vec!["<input>".to_string()],
            srcs: vec![String::new()],
            bases: vec![0],
            item_mod: vec![0; ast.items.len()],
            item_pub: vec![true; ast.items.len()],
            imports: vec![HashMap::new()],
        }
    }

    /// The module a global span falls in (by base-offset range). Falls back to
    /// module 0 if nothing matches (e.g. a synthesized span).
    fn module_of(&self, span: Span) -> ModId {
        let at = span.start as usize;
        for m in 0..self.bases.len() {
            let lo = self.bases[m];
            let hi = lo + self.srcs[m].len();
            if at >= lo && at <= hi {
                return m;
            }
        }
        0
    }

    /// Render a diagnostic against its own file, translating the global span back
    /// to that file's local offsets so `path:line:col` and the caret are correct.
    pub fn render(&self, d: &Diagnostic, severity: Severity) -> String {
        let m = self.module_of(d.span);
        let base = self.bases[m];
        let local = Span::new(
            (d.span.start as usize).saturating_sub(base),
            (d.span.end as usize).saturating_sub(base),
        );
        let mut d = d.clone();
        d.span = local;
        d.render(&self.srcs[m], &self.paths[m], severity)
    }
}

/// The result of loading a program: the merged AST, the concatenated source, the
/// per-module bookkeeping, and any load-time diagnostics (missing files, cycles,
/// lex/parse errors across every module).
pub struct Program {
    pub ast: Ast,
    pub modules: Modules,
    pub diags: Vec<Diagnostic>,
}

/// Load the program rooted at `root`, following `import`s.
pub fn load(root: &str) -> Program {
    let mut l = Loader::default();
    l.load_file(Path::new(root), None);
    Program {
        ast: l.ast,
        diags: l.diags,
        modules: Modules {
            names: l.names,
            paths: l.paths,
            srcs: l.srcs,
            bases: l.bases,
            item_mod: l.item_mod,
            item_pub: l.item_pub,
            imports: l.imports,
        },
    }
}

#[derive(Default)]
struct Loader {
    ast: Ast,
    /// The concatenated source of every module (the global span space).
    src_all: String,
    names: Vec<String>,
    paths: Vec<String>,
    srcs: Vec<String>,
    bases: Vec<usize>,
    imports: Vec<HashMap<String, ModId>>,
    item_mod: Vec<ModId>,
    item_pub: Vec<bool>,
    diags: Vec<Diagnostic>,
    /// Canonical path → already-loaded module (memoization across diamonds).
    loaded: HashMap<PathBuf, ModId>,
    /// The DFS stack of canonical paths currently being loaded (cycle detection).
    visiting: Vec<PathBuf>,
}

impl Loader {
    /// Load one file and (recursively) its imports. `import_span` is the span of
    /// the `import` that pulled this file in, so a missing-file or cycle error
    /// points at the offending `import`. Returns the loaded module's id, or
    /// `None` if the file could not be loaded.
    fn load_file(&mut self, path: &Path, import_span: Option<Span>) -> Option<ModId> {
        // A stable key for memoization + cycle detection. `canonicalize` resolves
        // `.`/`..`/symlinks/case so two spellings of one file share a module; if
        // it fails (e.g. the file is missing) fall back to the raw path so the
        // read below produces the user-facing "cannot read" error.
        let key = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());

        if let Some(&id) = self.loaded.get(&key) {
            return Some(id); // already merged (diamond import)
        }
        if self.visiting.contains(&key) {
            self.cycle_error(&key, import_span);
            return None;
        }

        let src = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                let span = import_span.unwrap_or(Span::new(0, 0));
                self.diags.push(Diagnostic::new(
                    format!("cannot read module `{}`: {e}", path.display()),
                    span,
                ));
                return None;
            }
        };

        // Reserve this module's id and record its global source region.
        let id = self.names.len();
        let base = self.src_all.len();
        self.src_all.push_str(&src);
        let end = self.src_all.len();
        self.src_all.push('\n'); // keep module regions disjoint

        self.names.push(module_stem(path));
        self.paths.push(path.display().to_string());
        self.srcs.push(src);
        self.bases.push(base);
        self.imports.push(HashMap::new());

        self.visiting.push(key.clone());

        // Lex this file's *region* of the shared buffer → global spans.
        let (tokens, lex_diags) = Lexer::new_slice(&self.src_all, base, end).tokenize();
        self.diags.extend(lex_diags);

        // Parse into the shared arena, getting this file's items back separately.
        let taken = std::mem::take(&mut self.ast);
        let (ast, items, parse_diags) = Parser::resume(&self.src_all, tokens, taken).parse_module();
        self.ast = ast;
        self.diags.extend(parse_diags);

        // Process imports first (so dependencies are merged), then this module's
        // own items.
        let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
        for item in items {
            if let Item::Import(im) = item {
                let target = dir.join(format!("{}.jtr", im.path));
                let binding = im
                    .alias
                    .as_ref()
                    .map(|a| a.name.clone())
                    .unwrap_or_else(|| last_segment(&im.path));
                if let Some(mid) = self.load_file(&target, Some(im.span)) {
                    self.imports[id].insert(binding, mid);
                }
                continue;
            }
            let is_pub = item_is_pub(&item);
            self.ast.items.push(item);
            self.item_mod.push(id);
            self.item_pub.push(is_pub);
        }

        self.visiting.pop();
        self.loaded.insert(key, id);
        Some(id)
    }

    fn cycle_error(&mut self, key: &Path, import_span: Option<Span>) {
        // Render the cycle from where `key` re-appears on the visiting stack.
        let start = self.visiting.iter().position(|p| p == key).unwrap_or(0);
        let mut chain: Vec<String> = self.visiting[start..]
            .iter()
            .map(|p| stem_of(p))
            .collect();
        chain.push(stem_of(key)); // close the loop
        let span = import_span.unwrap_or(Span::new(0, 0));
        self.diags.push(Diagnostic::new(
            format!("circular module dependency: {}", chain.join(" → ")),
            span,
        ));
    }
}

fn item_is_pub(item: &Item) -> bool {
    match item {
        Item::Fn(f) => f.is_pub,
        Item::Enum(e) => e.is_pub,
        Item::Const(c) => c.is_pub,
        Item::Struct { is_pub, .. } => *is_pub,
        Item::Distinct(d) => d.is_pub,
        Item::Extern(e) => e.is_pub,
        Item::Trait(t) => t.is_pub,
        Item::Impl(_) | Item::Import(_) => false,
    }
}

/// The file stem of a module path, used as its display name.
fn module_stem(path: &Path) -> String {
    path.file_stem().and_then(|s| s.to_str()).unwrap_or("module").to_string()
}

fn stem_of(path: &Path) -> String {
    module_stem(path)
}

/// The default binding for an import path: its last `/`-separated segment.
fn last_segment(path: &str) -> String {
    path.rsplit(['/', '\\']).next().unwrap_or(path).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::typeck;

    /// Write `files` (name → contents) into a fresh temp dir and return the dir.
    /// The dir name is derived from `tag` so parallel tests don't collide.
    fn fixture(tag: &str, files: &[(&str, &str)]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("jestyr_modtest_{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for (name, contents) in files {
            std::fs::write(dir.join(name), contents).unwrap();
        }
        dir
    }

    #[test]
    fn loads_and_merges_an_imported_module() {
        let dir = fixture(
            "merge",
            &[
                ("main.jtr", "import \"lib\"\nfn main() -> i32 { return lib.f(2) }"),
                ("lib.jtr", "pub fn f(x: i32) -> i32 { return x }"),
            ],
        );
        let prog = load(dir.join("main.jtr").to_str().unwrap());
        assert!(prog.diags.is_empty(), "load diags: {:?}", prog.diags);
        assert_eq!(prog.modules.names.len(), 2, "two modules loaded");
        // The root is module 0; `main` and `f` are both merged into one program.
        let names: Vec<&str> = prog
            .ast
            .items
            .iter()
            .filter_map(|i| match i {
                Item::Fn(f) => Some(f.name.name.as_str()),
                _ => None,
            })
            .collect();
        assert!(names.contains(&"main") && names.contains(&"f"), "merged items: {names:?}");
        // The root module imports `lib`.
        assert!(prog.modules.imports[0].contains_key("lib"), "import binding recorded");
    }

    #[test]
    fn detects_circular_imports() {
        let dir = fixture(
            "cycle",
            &[
                ("a.jtr", "import \"b\"\nfn main() -> i32 { return 0 }"),
                ("b.jtr", "import \"a\"\npub fn g() -> i32 { return 1 }"),
            ],
        );
        let prog = load(dir.join("a.jtr").to_str().unwrap());
        assert!(
            prog.diags.iter().any(|d| d.message.contains("circular module dependency")),
            "expected a cycle error, got: {:?}",
            prog.diags
        );
    }

    #[test]
    fn diamond_imports_load_each_module_once() {
        // main → {left, right} → shared. `shared` must be merged exactly once.
        let dir = fixture(
            "diamond",
            &[
                ("main.jtr", "import \"left\"\nimport \"right\"\nfn main() -> i32 { return left.l(0) + right.r(0) }"),
                ("left.jtr", "import \"shared\"\npub fn l(x: i32) -> i32 { return shared.s(x) }"),
                ("right.jtr", "import \"shared\"\npub fn r(x: i32) -> i32 { return shared.s(x) }"),
                ("shared.jtr", "pub fn s(x: i32) -> i32 { return x }"),
            ],
        );
        let prog = load(dir.join("main.jtr").to_str().unwrap());
        assert!(prog.diags.is_empty(), "load diags: {:?}", prog.diags);
        assert_eq!(prog.modules.names.len(), 4, "main, left, right, shared (once)");
        let s_count = prog
            .ast
            .items
            .iter()
            .filter(|i| matches!(i, Item::Fn(f) if f.name.name == "s"))
            .count();
        assert_eq!(s_count, 1, "the shared module's `s` is emitted exactly once");
    }

    #[test]
    fn enforces_visibility_through_the_checker() {
        let dir = fixture(
            "vis",
            &[
                ("main.jtr", "import \"lib\"\nfn main() -> i32 { return lib.secret(1) }"),
                ("lib.jtr", "fn secret(x: i32) -> i32 { return x }"), // not pub
            ],
        );
        let prog = load(dir.join("main.jtr").to_str().unwrap());
        assert!(prog.diags.is_empty(), "load is clean: {:?}", prog.diags);
        let (_info, diags) = typeck::check_program(&prog.ast, &prog.modules);
        assert!(
            diags.iter().any(|d| d.message.contains("private to module `lib`")),
            "expected a visibility error, got: {:?}",
            diags
        );
    }

    /// Run the whole pipeline (load → typeck → escape → cgen) over a real
    /// program file and assert every stage is diagnostic-free.
    fn pipeline_is_clean(root: &str) {
        let prog = load(root);
        assert!(prog.diags.is_empty(), "{root}: load diags: {:?}", prog.diags);
        let (info, type_diags) = typeck::check_program(&prog.ast, &prog.modules);
        let mut sema = type_diags;
        sema.extend(crate::escape::check(&prog.ast, &info));
        assert!(sema.is_empty(), "{root}: sema diags: {:?}", sema);
        let (_c, cgen_diags) = crate::cgen::emit(&prog.ast, &info);
        assert!(cgen_diags.is_empty(), "{root}: cgen diags: {:?}", cgen_diags);
    }

    #[test]
    fn modules_example_compiles_clean() {
        // The shipped multi-file demo (qualified calls + consts + visibility).
        pipeline_is_clean("examples/modules/main.jtr");
    }

    #[test]
    fn stdlib_demo_compiles_clean() {
        // The shipped stdlib demo: mem + list + core + io, the fn-pointer-vtable
        // allocator, a generic growable collection freed by RAII — the full
        // item-K + item-I + Phase-3 integration. (Also exercises a `for` loop:
        // `list.push`'s grow-copy.)
        pipeline_is_clean("examples/std/demo.jtr");
    }

    #[test]
    fn stdlib_demo_frees_its_list_by_raii() {
        // Wiring: the ported `std` routes allocation through the allocator vtable
        // and frees the `List(i32)` automatically at scope exit (no manual free).
        let prog = load("examples/std/demo.jtr");
        assert!(prog.diags.is_empty(), "load diags: {:?}", prog.diags);
        let (info, _td) = typeck::check_program(&prog.ast, &prog.modules);
        let (c, _cd) = crate::cgen::emit(&prog.ast, &info);
        assert!(
            c.contains("jestyr_impl_Drop__List_i32___drop(&j_xs)"),
            "the list must drop by RAII at scope exit:\n{c}"
        );
        // Allocation goes through the vtable's function pointer, not a bare malloc
        // in user code.
        assert!(c.contains("j_a.j_alloc_fn("), "allocation must route through the vtable:\n{c}");
    }

    #[test]
    fn core_combinators_example_compiles_clean() {
        // The core Option/Result combinators (map/unwrap_or/ok_or/...) monomorphize
        // and lower with no diagnostics across the `import "core"` module boundary —
        // generic enums constructed and matched inside generic functions.
        pipeline_is_clean("examples/std/combinators.jtr");
    }

    #[test]
    fn slice_algos_example_compiles_clean() {
        // The core slice/iterator algorithms (fold/find/any/all/sort/binary_search)
        // monomorphize over a generic `[]T` and lower clean across `import "core"`.
        pipeline_is_clean("examples/std/slice_algos.jtr");
    }

    #[test]
    fn binned_example_compiles_clean() {
        // The binned superaccumulator (2048 integer exponent bins over `[N]T`
        // arrays) — the chunk-count-independent reduction — resolves across
        // `import "core"` and lowers clean.
        pipeline_is_clean("examples/std/binned.jtr");
    }

    #[test]
    fn reductions_example_compiles_clean() {
        // The deterministic f64 reductions (naive/Neumaier-Kahan/pairwise) resolve
        // across `import "core"` and lower clean — the serial tier of CJC-inspired
        // numerics, run/platform-deterministic under the locked FP flags.
        pipeline_is_clean("examples/std/reductions.jtr");
    }

    #[test]
    fn float_bits_example_compiles_clean() {
        // The float-support primitives in `core` — IEEE-754 bit access via an
        // untagged union, the field extractors, and `clz64` — resolve across
        // `import "core"` and lower clean (the foundation for float parse/format).
        pipeline_is_clean("examples/std/float_bits.jtr");
    }

    #[test]
    fn numbers_example_compiles_clean() {
        // The core integer parse/format (parse_i64/parse_u64/format_i64/format_u64)
        // resolve across `import "core"` and lower clean — including the slice-index
        // assignment (`buf[i] = …`) the formatter writes through.
        pipeline_is_clean("examples/std/numbers.jtr");
    }

    #[test]
    fn files_example_compiles_clean() {
        // File I/O (self-hosting plumbing): the std `fs` wrappers over the
        // read_file/write_file/file_exists/remove_file intrinsics resolve and lower.
        pipeline_is_clean("examples/std/files.jtr");
    }

    #[test]
    fn args_example_compiles_clean() {
        // Command-line args: the std `env` wrappers over arg_count()/arg(i) resolve
        // across `import "env"` and lower clean (the loop over argv included).
        pipeline_is_clean("examples/std/args.jtr");
    }

    #[test]
    fn strmap_example_compiles_clean() {
        // The string-keyed symbol table (open-addressing str->i64 map): a non-generic
        // struct with a `Drop` impl, pointer-array slots, and the FNV-1a+SplitMix hash
        // resolve across `import "mem"`/`import "core"` and lower clean.
        pipeline_is_clean("examples/std/strmap_demo.jtr");
    }

    #[test]
    fn intern_example_compiles_clean() {
        // The string interner (str->dense id + id->str): an inline open-addressing
        // table with a parallel id array and a `Drop` impl, resolving across
        // `import "mem"`/`import "core"` and lowering clean.
        pipeline_is_clean("examples/std/intern_demo.jtr");
    }

    #[test]
    fn arrays_example_compiles_clean() {
        // Fixed-size arrays `[N]T`: the value-struct typedef, `[v; N]` literal,
        // bounds-checked index read/write, `.len`, and `for x in arr` iteration.
        pipeline_is_clean("examples/arrays.jtr");
    }

    #[test]
    fn loops_example_compiles_clean() {
        // Every MVP loop form + `invariant` + bounds-check elision.
        pipeline_is_clean("examples/loops.jtr");
    }

    #[test]
    fn docs_example_compiles_clean() {
        // The doc-comment demo is decorated with `//!`/`///` (pure trivia) yet
        // must still compile and run like any other program.
        pipeline_is_clean("examples/docs.jtr");
    }

    #[test]
    fn advanced_loops_example_compiles_clean() {
        // Fast-follows: zip, @no_panic, element+index, region scratch, string
        // iteration, casts.
        pipeline_is_clean("examples/loops_advanced.jtr");
    }

    #[test]
    fn attributes_example_compiles_clean() {
        // Compiler attributes (workstream D): @inline/@hot/@cold optimization
        // hints, @must_use, @deprecated, and a @no_mangle export.
        pipeline_is_clean("examples/attributes.jtr");
    }

    #[test]
    fn records_example_compiles_clean() {
        // Immutable `record` product types (struct/record split).
        pipeline_is_clean("examples/records.jtr");
    }

    #[test]
    fn niche_example_compiles_clean() {
        // Niche-optimized optional pointers (enum/ADT step 1).
        pipeline_is_clean("examples/niche.jtr");
    }

    #[test]
    fn option_example_compiles_clean() {
        // In-language generic Option(T)/Result(T,E) (enum/ADT step 1b-codegen):
        // monomorphization + inference + niche inheritance.
        pipeline_is_clean("examples/option.jtr");
    }

    #[test]
    fn discriminants_example_compiles_clean() {
        // Explicit enum discriminants (enum/ADT step 2) + reading them via `as`.
        pipeline_is_clean("examples/discriminants.jtr");
    }

    #[test]
    fn recursion_example_compiles_clean() {
        // Recursive ADTs via `indirect` (enum/ADT step 2.5).
        pipeline_is_clean("examples/recursion.jtr");
    }

    #[test]
    fn distinct_example_compiles_clean() {
        // Distinct nominal types (enum/ADT step 2.6).
        pipeline_is_clean("examples/distinct.jtr");
    }

    #[test]
    fn gen_vtable_example_compiles_clean() {
        // A *generic* vtable: a fn-pointer field on a generic struct, called
        // method-style on the generic-struct receiver (`b.op(n)`). Exercises the
        // generic-struct arm of `fn_ptr_field` end-to-end — the call is fully
        // typed under substitution and lowers to a clean indirect call.
        pipeline_is_clean("examples/gen_vtable.jtr");
    }

    #[test]
    fn traits_static_example_compiles_clean() {
        // Traits Stage C: `recv.m(args)` resolved through `impl Trait for <recv>`
        // lowers to a direct, statically-dispatched call of the mangled impl
        // method — for both primitive and struct receivers, with arguments.
        pipeline_is_clean("examples/traits_static.jtr");
    }

    #[test]
    fn operators_example_compiles_clean() {
        // Traits Stage E: the four trait-backed operators (`+`/`*`/`==`/`<`) on a
        // user type, each dispatching through its `Add`/`Mul`/`Eq`/`Ord` impl.
        pipeline_is_clean("examples/operators.jtr");
    }

    #[test]
    fn bracket_generic_example_compiles_clean() {
        // Bracket-form generics `[T]` monomorphize: each `T` is inferred from the
        // call's value arguments and a mangled instance is emitted.
        pipeline_is_clean("examples/bracket_generic.jtr");
    }

    #[test]
    fn bound_method_example_compiles_clean() {
        // The "Zig fix": inside `describe[T: Show]`, `x.show()` resolves through
        // the bound and dispatches to the concrete impl per instantiation.
        pipeline_is_clean("examples/bound_method.jtr");
    }

    #[test]
    fn dyn_dispatch_example_compiles_clean() {
        // Traits Stage F: `dyn Trait` — a concrete value coerces to a `{ data,
        // vtable }` fat pointer and `d.show()` dispatches through the vtable slot.
        pipeline_is_clean("examples/dyn_dispatch.jtr");
    }

    #[test]
    fn tests_demo_example_builds_a_clean_test_harness() {
        // The `@test`/`@bench` runner (workstream O): emit in test mode and check
        // the harness is generated without diagnostics.
        let prog = load("examples/tests_demo.jtr");
        assert!(prog.diags.is_empty(), "load diags: {:?}", prog.diags);
        let (info, type_diags) = typeck::check_program(&prog.ast, &prog.modules);
        let mut sema = type_diags;
        sema.extend(crate::escape::check(&prog.ast, &info));
        assert!(sema.is_empty(), "sema diags: {:?}", sema);
        let (c, cgen_diags) = crate::cgen::emit_tests(&prog.ast, &info);
        assert!(cgen_diags.is_empty(), "cgen diags: {:?}", cgen_diags);
        assert!(c.contains("running 2 test(s)"), "harness counts the two tests: {c}");
    }

    #[test]
    fn qualified_access_resolves_public_items() {
        let dir = fixture(
            "qual",
            &[
                ("main.jtr", "import \"lib\" as m\nfn main() -> i32 { return m.f(3) }"),
                ("lib.jtr", "pub fn f(x: i32) -> i32 { return x }"),
            ],
        );
        let prog = load(dir.join("main.jtr").to_str().unwrap());
        assert!(prog.diags.is_empty(), "load diags: {:?}", prog.diags);
        // `as m` alias binds the module.
        assert!(prog.modules.imports[0].contains_key("m"), "alias binding recorded");
        let (info, diags) = typeck::check_program(&prog.ast, &prog.modules);
        assert!(diags.is_empty(), "no errors for a valid qualified call: {:?}", diags);
        assert!(
            info.qualified.values().any(|n| n == "f"),
            "the qualified call resolved to `f`: {:?}",
            info.qualified.values().collect::<Vec<_>>()
        );
    }
}
