//! `jestyrc` — the Jestyr bootstrap compiler.
//!
//! Pipeline stages implemented so far:
//!   ① lexer    — source text → tokens
//!   ② parser   — tokens → AST   (recursive descent + Pratt)
//!   ③④ typeck  — name resolution + type checking
//!   ⑤ escape   — ownership / escape check  (the core of the language)
//!   ⑥ cgen     — lower the non-generic subset to C, compile, and run
//!
//! Usage:
//!   jestyrc <file.jtr>          parse a file and print its AST   (default)
//!   jestyrc parse  <file.jtr>   same as above, explicitly
//!   jestyrc check  <file.jtr>   resolve, type-check, and run the ownership checker
//!   jestyrc emit-c <file.jtr>   lower to C and print it
//!   jestyrc build  <file.jtr>   lower to C and compile a native binary
//!   jestyrc run    <file.jtr>   build, then execute the binary
//!                               (build/run/emit-c take --error-traces: instrument
//!                               err/`?`/unwrap with a Zig-style debug error trace)
//!   jestyrc test   <file.jtr>   build & run the `@test`/`@bench` harness;
//!                               `test <file> <substr>` runs only matching names,
//!                               `test <file> --list` lists them (no compile)
//!   jestyrc tokens <file.jtr>   stop after lexing and dump the token stream
//!   jestyrc layout <file.jtr>   report every type's size, alignment, field offsets
//!                               and padding waste (analysis only — emits nothing)
//!   jestyrc obligations <file.jtr>  report what a `@verified` build would have to
//!                               prove (contracts, invariants, refinements)
//!   jestyrc unsafe <file.jtr>   report every raw-pointer operation and whether an
//!                               `unsafe` block covers it (analysis only)
//!   jestyrc errsets <file.jtr>  report every error-set obligation site (`err`, `?`,
//!                               the rethrow `catch`) and whether the declared sets
//!                               hold — the census before enforcement (analysis only)
//!   jestyrc simd   <file.jtr>   report which `par for` loops may be evaluated a SIMD
//!                               lane at a time, and why not (analysis only)
//!   jestyrc doc    <file.jtr>   render the file's API docs as Markdown (--html for HTML)
//!   jestyrc attest <file.jtr>   emit the reproducible-build + guarantee manifest
//!                               (sha256 of the emitted C, the locked CC flags, and
//!                               every item's machine-checked contracts)
//!   jestyrc attest --diff <old> <new>   classify breaking vs compatible contract
//!                               changes between two manifests (exit 1 if breaking)
//!   jestyrc selfbench           compile a generated program; report per-stage speed +
//!                               footprint (build `--features bench-alloc` for heap bytes)

mod ast;
mod attest;
mod attrs;
mod buildscript;
mod cgen;
mod comptime;
mod cst;
mod diag;
#[cfg(feature = "dharht-experiment")]
mod dharht;
mod doc;
mod errsets;
mod escape;
mod layout;
mod lexer;
mod module;
mod obligations;
mod parser;
mod printer;
mod provenance;
mod sha256;
mod simd;
mod span;
mod token;
mod typeck;
mod types;

#[cfg(test)]
mod proptests;

use std::path::Path;
use std::process::{Command, ExitCode, Stdio};

use lexer::Lexer;
use parser::Parser;
use printer::print_ast;

/// An opt-in counting global allocator (feature `bench-alloc`) tracking current,
/// peak, and total bytes — for `selfbench`'s memory report. Off by default so the
/// production compiler uses the plain System allocator.
#[cfg(feature = "bench-alloc")]
mod bench_alloc {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::sync::atomic::{AtomicUsize, Ordering};

    static CURRENT: AtomicUsize = AtomicUsize::new(0);
    static PEAK: AtomicUsize = AtomicUsize::new(0);
    static TOTAL: AtomicUsize = AtomicUsize::new(0);

    pub fn reset() {
        CURRENT.store(0, Ordering::Relaxed);
        PEAK.store(0, Ordering::Relaxed);
        TOTAL.store(0, Ordering::Relaxed);
    }
    /// (peak resident bytes, total bytes ever allocated).
    pub fn stats() -> (usize, usize) {
        (PEAK.load(Ordering::Relaxed), TOTAL.load(Ordering::Relaxed))
    }

    struct Counting;
    unsafe impl GlobalAlloc for Counting {
        unsafe fn alloc(&self, l: Layout) -> *mut u8 {
            let p = System.alloc(l);
            if !p.is_null() {
                let cur = CURRENT.fetch_add(l.size(), Ordering::Relaxed) + l.size();
                TOTAL.fetch_add(l.size(), Ordering::Relaxed);
                PEAK.fetch_max(cur, Ordering::Relaxed);
            }
            p
        }
        unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
            CURRENT.fetch_sub(l.size(), Ordering::Relaxed);
            System.dealloc(p, l);
        }
    }
    #[global_allocator]
    static GLOBAL: Counting = Counting;
}

/// A representative, always-valid program: `n` triples of (struct, enum, function
/// with a `match`), plus `main`. Exercises lex/parse/typeck/escape/cgen at scale.
fn gen_bench_program(n: usize) -> String {
    let mut s = String::with_capacity(n * 120);
    for i in 0..n {
        s.push_str(&format!("struct S{i} {{ a: i32, b: i32, c: i32 }}\n"));
        s.push_str(&format!("enum E{i} {{ red, green, blue }}\n"));
        s.push_str(&format!(
            "fn f{i}(read e: E{i}) -> i32 {{ match e {{ red => {}, green => {}, blue => {} }} }}\n",
            i,
            i + 1,
            i + 2,
        ));
    }
    s.push_str("fn main() -> i32 { return 0 }\n");
    s
}

/// Compile a generated program and report per-stage throughput + footprint.
fn selfbench() {
    use std::time::Instant;
    let src = gen_bench_program(500);
    let lines = src.lines().count();
    let bytes = src.len();

    // Warm up and capture the footprint once.
    let (tok0, _) = Lexer::new(&src).tokenize();
    let ntok = tok0.len();
    let (ast0, _) = Parser::new(&src, tok0).parse();
    let (nexpr, ntype, npat) = (ast0.exprs.len(), ast0.types.len(), ast0.pats.len());
    let (info0, _) = typeck::check(&ast0);
    let (c0, _) = cgen::emit(&ast0, &info0);
    let cbytes = c0.len();

    let runs = 30u32;
    let (mut lex, mut parse, mut tyck, mut esc, mut cg) = (0f64, 0f64, 0f64, 0f64, 0f64);
    for _ in 0..runs {
        let a = Instant::now();
        let (tokens, _) = Lexer::new(&src).tokenize();
        let b = Instant::now();
        let (ast, _) = Parser::new(&src, tokens).parse();
        let c = Instant::now();
        let (info, _) = typeck::check(&ast);
        let d = Instant::now();
        let _ = escape::check(&ast, &info);
        let e = Instant::now();
        let _ = cgen::emit(&ast, &info);
        let f = Instant::now();
        lex += (b - a).as_secs_f64();
        parse += (c - b).as_secs_f64();
        tyck += (d - c).as_secs_f64();
        esc += (e - d).as_secs_f64();
        cg += (f - e).as_secs_f64();
    }
    let r = runs as f64;
    let (lex, parse, tyck, esc, cg) = (lex / r, parse / r, tyck / r, esc / r, cg / r);
    let total = lex + parse + tyck + esc + cg;
    let ms = |x: f64| x * 1000.0;

    println!("jestyrc selfbench — generated program: {lines} lines, {bytes} bytes, {ntok} tokens");
    println!("  AST: {nexpr} exprs, {ntype} types, {npat} pats    emitted C: {cbytes} bytes");
    println!("  stage timings (avg of {runs} runs):");
    println!("    lex     {:8.3} ms", ms(lex));
    println!("    parse   {:8.3} ms", ms(parse));
    println!("    typeck  {:8.3} ms", ms(tyck));
    println!("    escape  {:8.3} ms", ms(esc));
    println!("    cgen    {:8.3} ms", ms(cg));
    println!(
        "    total   {:8.3} ms    ({:.0} lines/s, {:.0} tokens/s)",
        ms(total),
        lines as f64 / total,
        ntok as f64 / total
    );

    #[cfg(feature = "bench-alloc")]
    {
        bench_alloc::reset();
        let (tk, _) = Lexer::new(&src).tokenize();
        let (ast, _) = Parser::new(&src, tk).parse();
        let (info, _) = typeck::check(&ast);
        let _ = escape::check(&ast, &info);
        let _ = cgen::emit(&ast, &info);
        let (peak, total_alloc) = bench_alloc::stats();
        println!(
            "  memory (one full compile): peak {} KiB resident, {} KiB total allocated",
            peak / 1024,
            total_alloc / 1024
        );
    }
    #[cfg(not(feature = "bench-alloc"))]
    println!("  memory: rebuild with `--features bench-alloc` for peak/total heap bytes");
}

/// Experiment: D-HARHT (Memory profile) vs `HashMap` on the **build-once /
/// lookup-many** workload a compiler's symbol tables have (build in typeck, read
/// in cgen). Compares lookup latency + footprint at a compiler-realistic size and
/// a large size. Honest about D-HARHT's fixed 256-shard overhead.
#[cfg(feature = "dharht-experiment")]
fn dharht_bench() {
    use crate::dharht::{deterministic_permutation_scatter, DHarht, LookupProfile};
    use std::collections::HashMap;
    use std::hint::black_box;
    use std::time::Instant;

    println!("D-HARHT (Memory profile) vs HashMap — build-once / lookup-many");
    for &n in &[2_000usize, 100_000] {
        let keys: Vec<u64> = (0..n)
            .map(|i| deterministic_permutation_scatter(i as u64 ^ 0x9e37_79b9_7f4a_7c15))
            .collect();
        // A lookup stream of 4n hits in deterministic pseudo-random order.
        let mut lookups = Vec::with_capacity(n * 4);
        let mut st = 0x243f_6a88_85a3_08d3_u64;
        for _ in 0..n * 4 {
            st = deterministic_permutation_scatter(st);
            lookups.push(keys[(st as usize) % n]);
        }

        let mut hm: HashMap<u64, u64> = HashMap::with_capacity(n);
        let mut dh: DHarht<u64> = DHarht::with_capacity(256, n / 256 + 8);
        dh.set_lookup_profile(LookupProfile::Memory);
        for (i, &k) in keys.iter().enumerate() {
            hm.insert(k, i as u64);
            dh.insert(k, i as u64);
        }
        dh.seal_for_lookup();

        let time = |f: &dyn Fn() -> u64| {
            let _ = f(); // warm up
            let runs = 5u32;
            let t = Instant::now();
            let mut acc = 0u64;
            for _ in 0..runs {
                acc ^= f();
            }
            let _ = black_box(acc);
            t.elapsed().as_secs_f64() / runs as f64
        };
        let hm_t = time(&|| {
            let mut c = 0u64;
            for &k in &lookups {
                c ^= *hm.get(black_box(&k)).unwrap_or(&0);
            }
            c
        });
        let dh_t = time(&|| {
            let mut c = 0u64;
            for &k in &lookups {
                c ^= *dh.get(black_box(k)).unwrap_or(&0);
            }
            c
        });
        let per = |t: f64| t * 1e9 / lookups.len() as f64;

        // Footprint: D-HARHT self-reports; HashMap<u64,u64> ≈ capacity × ((K,V) + 1
        // control byte) — a hashbrown-shaped lower bound.
        let dh_mem = dh.approx_memory_bytes();
        let hm_mem = hm.capacity() * (std::mem::size_of::<u64>() * 2 + 1);

        println!("\nn = {n}  ({} lookups):", lookups.len());
        println!(
            "  lookup   HashMap {:7.2} ns/op    D-HARHT(mem) {:7.2} ns/op    ({:.2}x HashMap)",
            per(hm_t),
            per(dh_t),
            per(dh_t) / per(hm_t)
        );
        println!(
            "  memory   HashMap ~{:>10} B    D-HARHT(mem) {:>10} B    ({:.2}x HashMap)",
            hm_mem,
            dh_mem,
            dh_mem as f64 / hm_mem as f64
        );
    }
    println!(
        "\nnote: D-HARHT carries a fixed 256-shard overhead (each Shard has 256-entry\n\
         second_jump/second_leaf arrays), so small tables pay a large constant. Tune the\n\
         shard count down for compiler-sized tables."
    );
}

enum Mode {
    Parse,
    Check,
    EmitC,
    Build,
    Run,
    /// `jestyrc test <file> [substr] [--list]` — build & run the `@test`/`@bench`
    /// harness. `list`: print runnable test/bench names and exit (no compile).
    /// `filter`: bake only items whose name contains the substring.
    Test { list: bool, filter: Option<String> },
    /// `jestyrc attest <file>` — emit the reproducible-build + guarantee manifest.
    Attest,
    Tokens,
    Doc { html: bool },
}

/// Stack for the compiler's worker thread. Every pass after the parser —
/// `typeck::infer`, `escape`, `cgen::emit_expr`, and the AST printer — walks the
/// expression tree recursively, so a legitimately deep (but bounded, see
/// [`parser::MAX_EXPR_DEPTH`]) expression needs headroom the platform default
/// won't always give: Windows' main thread gets only ~1 MiB, enough to overflow
/// on a few hundred nested nodes. Run the whole driver on a thread we size
/// ourselves so the depth the *parser* accepts is the depth every later pass can
/// walk, on every platform.
const WORKER_STACK: usize = 256 * 1024 * 1024;

fn main() -> ExitCode {
    std::thread::Builder::new()
        .stack_size(WORKER_STACK)
        .spawn(run)
        .expect("spawn compiler worker thread")
        .join()
        .unwrap_or(ExitCode::FAILURE)
}

fn run() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();

    // `emit-c <file> --show-drops` annotates each inserted scope-exit drop call
    // with a `/* drop … */` comment, so the implicit RAII glue is inspectable.
    let show_drops = args.iter().any(|a| a == "--show-drops");

    // `check <file> --json` emits one machine-readable diagnostic report on stdout
    // instead of the human caret rendering — for editors, CI and any tool that would
    // otherwise have to parse `error: …` prose.
    let json = args.iter().any(|a| a == "--json");

    // `build/run/emit-c <file> --error-traces` — instrument the error paths with a
    // Zig-style debug trace: `err` records the origin, each `?` a hop, and an
    // `unwrap` of an error prints the path to stderr (Error-handling tier 4).
    let error_traces = args.iter().any(|a| a == "--error-traces");

    // Subcommands that take a file argument.
    let sub = |name: &str, m: Mode| -> Option<Result<(Mode, String), ()>> {
        if args.get(1).map(String::as_str) == Some(name) {
            Some(match args.get(2) {
                Some(p) => Ok((m, p.clone())),
                None => {
                    eprintln!("error: `{name}` needs a file argument");
                    Err(())
                }
            })
        } else {
            None
        }
    };

    let (mode, path) = match args.get(1).map(String::as_str) {
        None | Some("-h") | Some("--help") => {
            print_usage();
            return if args.len() < 2 { ExitCode::FAILURE } else { ExitCode::SUCCESS };
        }
        Some("selfbench") => {
            // No file argument: compile a generated program and report per-stage
            // throughput + footprint (and, with `--features bench-alloc`, heap use).
            selfbench();
            return ExitCode::SUCCESS;
        }
        #[cfg(feature = "dharht-experiment")]
        Some("dharht-bench") => {
            dharht_bench();
            return ExitCode::SUCCESS;
        }
        Some("doc") => {
            // `doc` takes an optional `--html` flag anywhere after it; the file is
            // the first remaining non-flag argument.
            let html = args[2..].iter().any(|a| a == "--html");
            match args[2..].iter().find(|a| !a.starts_with("--")) {
                Some(p) => (Mode::Doc { html }, p.clone()),
                None => {
                    eprintln!("error: `doc` needs a file argument");
                    return ExitCode::FAILURE;
                }
            }
        }
        Some("test") => {
            // `test` takes an optional `--list` flag and an optional name-filter
            // substring, in any order after the file. The file is the first
            // non-flag argument; a second non-flag argument is the filter.
            let list = args[2..].iter().any(|a| a == "--list");
            let mut nonflags = args[2..].iter().filter(|a| !a.starts_with("--"));
            let Some(path) = nonflags.next() else {
                eprintln!("error: `test` needs a file argument");
                return ExitCode::FAILURE;
            };
            let filter = nonflags.next().cloned();
            (Mode::Test { list, filter }, path.clone())
        }
        Some("layout") => {
            // `layout <file>` — report every declared type's size, alignment, field
            // offsets and padding waste. Pure analysis: it compiles nothing and emits
            // nothing, so it can be run on a file that would not even build.
            match args[2..].iter().find(|a| !a.starts_with("--")) {
                Some(p) => return run_layout(p),
                None => {
                    eprintln!("error: `layout` needs a source file argument");
                    return ExitCode::FAILURE;
                }
            }
        }
        Some("unsafe") => {
            // `unsafe <file>` — every raw-pointer operation and whether an `unsafe`
            // block covers it. Analysis only, like `layout`/`simd`/`obligations`:
            // the measurement that has to precede enforcement, because the corpus
            // (including the self-hosted compiler) derefs raw pointers in ordinary
            // code today and enforcement without a migration would break it.
            match args[2..].iter().find(|a| !a.starts_with("--")) {
                Some(p) => return run_unsafe_report(p),
                None => {
                    eprintln!("error: `unsafe` needs a source file argument");
                    return ExitCode::FAILURE;
                }
            }
        }
        Some("errsets") => {
            // `errsets <file>` — every error-set obligation site (`err(E)`, `?`,
            // the rethrow `catch`) and whether the declared sets hold. Analysis
            // only, like `unsafe`/`obligations`: the measurement that precedes
            // enforcement (error-payloads E1 — docs/error-payloads.md).
            match args[2..].iter().find(|a| !a.starts_with("--")) {
                Some(p) => return run_errsets(p),
                None => {
                    eprintln!("error: `errsets` needs a source file argument");
                    return ExitCode::FAILURE;
                }
            }
        }
        Some("obligations") => {
            // `obligations <file>` — report what a `@verified` build would have to
            // prove: every declared precondition, postcondition, loop invariant,
            // termination measure and parameter refinement. Analysis only, like
            // `layout` and `simd` — it compiles nothing and emits nothing.
            match args[2..].iter().find(|a| !a.starts_with("--")) {
                Some(p) => return run_obligations(p),
                None => {
                    eprintln!("error: `obligations` needs a source file argument");
                    return ExitCode::FAILURE;
                }
            }
        }
        Some("simd") => {
            // `simd <file>` — report which `par for` loops may be evaluated a lane at
            // a time under the determinism contract, and name the cause when one may
            // not. Analysis only, exactly like `layout`: it compiles nothing.
            match args[2..].iter().find(|a| !a.starts_with("--")) {
                Some(p) => return run_simd(p),
                None => {
                    eprintln!("error: `simd` needs a source file argument");
                    return ExitCode::FAILURE;
                }
            }
        }
        Some("plan") => {
            // `plan <build.jestyr> [--build]` — evaluate a build description and print
            // its plan; `--build` additionally compiles each target it names.
            let build = args[2..].iter().any(|a| a == "--build");
            let emit = args[2..].iter().any(|a| a == "--emit");
            match args[2..].iter().find(|a| !a.starts_with("--")) {
                Some(p) => return run_plan(p, build, emit),
                None => {
                    eprintln!("error: `plan` needs a build-script file argument");
                    return ExitCode::FAILURE;
                }
            }
        }
        Some("attest") => {
            // `attest --diff <old> <new>` compares two manifest files; plain
            // `attest <file>` emits one. The file args are the non-flag arguments.
            let mut nonflags = args[2..].iter().filter(|a| !a.starts_with("--"));
            if args[2..].iter().any(|a| a == "--diff") {
                return match (nonflags.next(), nonflags.next()) {
                    (Some(old), Some(new)) => run_attest_diff(old, new),
                    _ => {
                        eprintln!("error: `attest --diff` needs two manifest files: <old> <new>");
                        ExitCode::FAILURE
                    }
                };
            }
            match nonflags.next() {
                Some(p) => (Mode::Attest, p.clone()),
                None => {
                    eprintln!("error: `attest` needs a file argument");
                    return ExitCode::FAILURE;
                }
            }
        }
        Some("tokens") | Some("parse") | Some("check") | Some("emit-c") | Some("build")
        | Some("run") => {
            let candidates = [
                sub("tokens", Mode::Tokens),
                sub("parse", Mode::Parse),
                sub("check", Mode::Check),
                sub("emit-c", Mode::EmitC),
                sub("build", Mode::Build),
                sub("run", Mode::Run),
            ];
            match candidates.into_iter().flatten().next() {
                Some(Ok(pair)) => pair,
                Some(Err(())) | None => return ExitCode::FAILURE,
            }
        }
        Some(p) => (Mode::Parse, p.to_string()),
    };

    let src = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: cannot read `{path}`: {e}");
            return ExitCode::FAILURE;
        }
    };

    let (tokens, lex_diags) = Lexer::new(&src).tokenize();

    match mode {
        Mode::Tokens => {
            dump_tokens(&src, &path, &tokens);
            report(&src, &path, &lex_diags)
        }
        Mode::Parse => {
            let (ast, parse_diags) = Parser::new(&src, tokens).parse();
            print!("{}", print_ast(&ast));

            let mut diags = lex_diags;
            diags.extend(parse_diags);
            report(&src, &path, &diags)
        }
        Mode::Doc { html } => {
            // The doc generator re-lexes (it needs the doc-comment side table),
            // so the pre-lexed `tokens` above go unused here.
            let title = Path::new(&path)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or(&path)
                .to_string();
            let (rendered, notices) = doc::generate(&src, &title, html);
            print!("{rendered}");
            // Dangling-doc + parse notices are advisory: print them to stderr and
            // still succeed, so docs are emitted even for an imperfect file.
            for d in &notices {
                eprintln!("{}", d.render(&src, &path, diag::Severity::Warning));
            }
            ExitCode::SUCCESS
        }
        Mode::Check => {
            // The loader follows `import`s, so the check covers the whole program.
            let prog = module::load(&path);
            let mut diags = prog.diags;
            // Only run the semantic passes if every module lexed and parsed cleanly.
            if diags.is_empty() {
                let (info, type_diags) = typeck::check_program(&prog.ast, &prog.modules);
                let mut sema = type_diags;
                sema.extend(escape::check(&prog.ast, &info));
                diags = sema;
            }
            // `--json`: one machine-readable report on **stdout**, for editors and CI.
            // Always emitted, even when the program is clean (an empty `diagnostics`
            // array), so a consumer never has to distinguish "no output" from "the tool
            // did not run". The exit code still says whether it succeeded.
            if json {
                print!("{}", prog.modules.render_json(&diags));
                return if diags.iter().any(|d| d.is_error()) {
                    ExitCode::FAILURE
                } else {
                    ExitCode::SUCCESS
                };
            }
            if diags.is_empty() {
                // Name the checks that actually ran. The old wording ("no type or
                // ownership errors") claimed a *categorical* guarantee this pass does
                // not make: `typeck` is deliberately lenient (see its module docs), so
                // a clean run means "the checks below found nothing", not "this program
                // is well-typed". Keep this list in sync with what the pass reports.
                println!(
                    "ok: resolution, arity, assignability, visibility, trait-bound, \
                     exhaustiveness and escape checks passed in {path}"
                );
                return ExitCode::SUCCESS;
            }
            report_program(&prog.modules, &diags)
        }
        Mode::Attest => {
            // Attestation must describe a *valid* program: run the same
            // load → typeck → escape gate as codegen, then emit the manifest. The
            // C hash inside is over the very C `build`/`run` would compile.
            let prog = module::load(&path);
            if !prog.diags.is_empty() {
                return report_program(&prog.modules, &prog.diags);
            }
            let (info, type_diags) = typeck::check_program(&prog.ast, &prog.modules);
            let mut diags = type_diags;
            diags.extend(escape::check(&prog.ast, &info));
            if diags.iter().any(|d| d.is_error()) {
                return report_program(&prog.modules, &diags);
            }
            for d in &diags {
                eprintln!("{}", prog.modules.render(d, d.severity));
            }
            let src = attest::global_src(&prog.modules);
            print!("{}", attest::manifest(&path, &src, &prog.ast, &info));
            ExitCode::SUCCESS
        }
        Mode::EmitC | Mode::Build | Mode::Run | Mode::Test { .. } => {
            // Codegen only ever sees well-formed programs: gate on every check.
            let prog = module::load(&path);
            if !prog.diags.is_empty() {
                return report_program(&prog.modules, &prog.diags);
            }
            let (info, type_diags) = typeck::check_program(&prog.ast, &prog.modules);
            let mut diags = type_diags;
            diags.extend(escape::check(&prog.ast, &info));
            // Errors block codegen; warnings (e.g. redundant match arms) are
            // reported but the build proceeds.
            if diags.iter().any(|d| d.is_error()) {
                return report_program(&prog.modules, &diags);
            }
            for d in &diags {
                eprintln!("{}", prog.modules.render(d, d.severity));
            }

            // `jestyrc test --list` prints the runnable test/bench names (optionally
            // narrowed by the same name filter) and exits — no codegen, no compiler.
            if let Mode::Test { list: true, filter } = &mode {
                return list_tests(&prog.ast, filter.as_deref());
            }

            // `test` mode emits a `@test`/`@bench` harness `main` (with optional
            // name filtering); the rest emit the ordinary entry-point wrapper.
            let (c_src, cgen_diags) = if let Mode::Test { filter, .. } = &mode {
                match filter {
                    Some(f) => cgen::emit_tests_filtered(&prog.ast, &info, Some(f)),
                    None => cgen::emit_tests(&prog.ast, &info),
                }
            } else if show_drops && matches!(mode, Mode::EmitC) {
                cgen::emit_show_drops(&prog.ast, &info)
            } else if error_traces {
                // `--error-traces` (build/run/emit-c): instrument err/`?`/unwrap with
                // the debug trace. Per-invocation, so nothing golden-shaped sees it.
                cgen::emit_error_traces(&prog.ast, &info)
            } else {
                cgen::emit(&prog.ast, &info)
            };
            match mode {
                Mode::EmitC => {
                    print!("{c_src}");
                    // Unsupported constructs are notes here, not fatal.
                    for d in &cgen_diags {
                        eprintln!("{}", prog.modules.render(d, diag::Severity::Note));
                    }
                    ExitCode::SUCCESS
                }
                _ => {
                    if !cgen_diags.is_empty() {
                        return report_program(&prog.modules, &cgen_diags);
                    }
                    // `run`/`build` need an entry point. Without a `main` the emitted
                    // C has no `main`, so the C linker fails obscurely (`undefined
                    // reference to WinMain`/`_start`). Catch it here with a clear
                    // message — a library file is type-checked with `check`, not run.
                    // (`test` mode is exempt: it synthesizes its own harness `main`.)
                    if matches!(mode, Mode::Build | Mode::Run) && !program_has_main(&prog.ast) {
                        eprintln!("error: no `main` function — `run`/`build` need an entry point");
                        eprintln!(
                            "note: `{path}` looks like a library; type-check it with `jestyrc check {path}` instead"
                        );
                        return ExitCode::FAILURE;
                    }
                    // `run` and `test` both execute the built binary.
                    build_and_maybe_run(&path, &c_src, matches!(mode, Mode::Run | Mode::Test { .. }))
                }
            }
        }
    }
}

/// Does the program declare a top-level `fn main`? `run`/`build` need one for an
/// entry point; a library file (no `main`) is a `check`-only artifact.
fn program_has_main(ast: &ast::Ast) -> bool {
    ast.items.iter().any(|it| matches!(it, ast::Item::Fn(f) if f.name.name == "main"))
}

/// `jestyrc plan <build.jestyr> [--build]`: evaluate a build script and print the
/// build plan it describes; with `--build`, compile each target it names.
///
/// The build description is a Jestyr file that is **evaluated, never run** — see
/// `src/buildscript.rs` for why the plan is a pure function of an index rather than
/// the imperative `exe(…)`/`test(…)` shape build systems usually reach for.
///
/// An explicit subcommand rather than magic on the filename: `jestyrc plan foo.jestyr`
/// says what it does, and nothing changes meaning because a file happens to be called
/// `build.jestyr`.
/// `jestyrc layout <file>` — the memory-layout report (workstream L, increment 1).
///
/// Type-checking is enough: layout is a property of *declarations*, so this never
/// reaches escape analysis or the backend and emits nothing at all.
fn run_layout(path: &str) -> ExitCode {
    let src = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: cannot read `{path}`: {e}");
            return ExitCode::FAILURE;
        }
    };
    let (tokens, ld) = Lexer::new(&src).tokenize();
    let (ast, pd) = Parser::new(&src, tokens).parse();
    let failed = report_lex_parse_errors(&src, &path, &ld, &pd);
    if failed {
        return ExitCode::FAILURE;
    }
    // Type errors are NOT fatal here: a half-finished file still has declarations, and
    // "what does this struct cost?" is a question worth answering while editing it.
    let (info, _diags) = typeck::check(&ast);
    print!("{}", layout::render(&layout::compute(&ast, &info)));
    ExitCode::SUCCESS
}

/// `jestyrc unsafe <file>` — the unsafe-boundary report (ownership v2, slice 1).
///
/// Needs the type checker (unlike `simd`): "is this a *raw* deref" is a type
/// question — `g.*` on a genref is generation-checked, and flagging it would teach
/// people to wrap safe code in `unsafe`. Type errors are not fatal, the same
/// while-editing courtesy `layout` extends.
fn run_unsafe_report(path: &str) -> ExitCode {
    let src = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: cannot read `{path}`: {e}");
            return ExitCode::FAILURE;
        }
    };
    let (tokens, ld) = Lexer::new(&src).tokenize();
    let (ast, pd) = Parser::new(&src, tokens).parse();
    let failed = report_lex_parse_errors(&src, &path, &ld, &pd);
    if failed {
        return ExitCode::FAILURE;
    }
    let (info, _diags) = typeck::check(&ast);
    print!("{}", provenance::render(&provenance::collect(&ast, &info), &src));
    ExitCode::SUCCESS
}

/// `jestyrc obligations <file>` — what a `@verified` build would have to prove.
///
/// Parsing is enough: an obligation is *declared* syntax, so this runs neither the type
/// checker nor the backend. Type errors are therefore not fatal — "what does this
/// function promise?" is a question worth answering while the function is still being
/// written, the same call `layout` makes.
fn run_obligations(path: &str) -> ExitCode {
    let src = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: cannot read `{path}`: {e}");
            return ExitCode::FAILURE;
        }
    };
    let (tokens, ld) = Lexer::new(&src).tokenize();
    let (ast, pd) = Parser::new(&src, tokens).parse();
    let failed = report_lex_parse_errors(&src, &path, &ld, &pd);
    if failed {
        return ExitCode::FAILURE;
    }
    print!("{}", obligations::render(&obligations::collect(&ast, &src)));
    ExitCode::SUCCESS
}

/// `jestyrc errsets <file>` — the error-set soundness census (error-payloads E1).
///
/// Parsing is enough: the obligations are between declared sets and syntactic
/// sites, and resolution is deliberately best-effort by name (see `src/errsets.rs`
/// — a census that guessed would mis-size the migration it exists to size).
fn run_errsets(path: &str) -> ExitCode {
    let src = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: cannot read `{path}`: {e}");
            return ExitCode::FAILURE;
        }
    };
    let (tokens, ld) = Lexer::new(&src).tokenize();
    let (ast, pd) = Parser::new(&src, tokens).parse();
    let failed = report_lex_parse_errors(&src, &path, &ld, &pd);
    if failed {
        return ExitCode::FAILURE;
    }
    print!("{}", errsets::render(&ast, &errsets::collect(&ast)));
    ExitCode::SUCCESS
}

/// `jestyrc simd <file>` — the SIMD legality report (workstream Q, increment 1).
///
/// Parsing is enough. The legality whitelist is syntactic (it must be, since the
/// `@simd` attribute is validated in the parser — see `src/simd.rs`), so this runs
/// neither the type checker nor the backend and emits nothing at all.
fn run_simd(path: &str) -> ExitCode {
    let src = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: cannot read `{path}`: {e}");
            return ExitCode::FAILURE;
        }
    };
    let (tokens, ld) = Lexer::new(&src).tokenize();
    let (ast, pd) = Parser::new(&src, tokens).parse();
    let failed = report_lex_parse_errors(&src, &path, &ld, &pd);
    if failed {
        return ExitCode::FAILURE;
    }
    print!("{}", simd::render(&src, &simd::analyze(&ast)));
    ExitCode::SUCCESS
}

fn run_plan(path: &str, build: bool, emit: bool) -> ExitCode {
    let src = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: cannot read `{path}`: {e}");
            return ExitCode::FAILURE;
        }
    };
    let (tokens, ld) = Lexer::new(&src).tokenize();
    let (ast, pd) = Parser::new(&src, tokens).parse();
    let failed = report_lex_parse_errors(&src, &path, &ld, &pd);
    if failed {
        return ExitCode::FAILURE;
    }

    let plan = match buildscript::evaluate(&ast) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: `{path}` is not a valid build script: {e}");
            eprintln!("note: a build script declares `const targets: i64` plus");
            eprintln!("      `fn source(i: i64) -> str` and `fn output(i: i64) -> str`");
            return ExitCode::FAILURE;
        }
    };
    print!("{}", plan.render());

    // `--emit` writes the generated artifacts (roadmap G tier 5). The evaluator never
    // touched a file to produce these — it computed strings, exactly as it computes
    // any other comptime value — so writing them is an explicit act by the *user*,
    // not an effect a build script was able to perform.
    if emit {
        for a in &plan.artifacts {
            if let Some(parent) = Path::new(&a.path).parent() {
                if !parent.as_os_str().is_empty() {
                    if let Err(e) = std::fs::create_dir_all(parent) {
                        eprintln!("error: cannot create `{}`: {e}", parent.display());
                        return ExitCode::FAILURE;
                    }
                }
            }
            if let Err(e) = std::fs::write(&a.path, &a.content) {
                eprintln!("error: cannot write `{}`: {e}", a.path);
                return ExitCode::FAILURE;
            }
            eprintln!("emitted {} ({} bytes, sha256 {})", a.path, a.content.len(), a.sha256());
        }
    } else if !plan.artifacts.is_empty() {
        eprintln!(
            "note: {} generated artifact(s) not written; pass --emit to write them",
            plan.artifacts.len()
        );
    }

    if !build {
        return ExitCode::SUCCESS;
    }
    for t in &plan.targets {
        eprintln!("building {} -> {}", t.source, t.output);
        if build_one(&t.source, &t.output) != ExitCode::SUCCESS {
            eprintln!("error: build failed for `{}`", t.source);
            return ExitCode::FAILURE;
        }
    }
    ExitCode::SUCCESS
}

/// Compile one target of a build plan to a named executable.
///
/// Deliberately a small, self-contained path rather than a refactor of the `build`
/// subcommand: the two differ in exactly one way — a plan target names its output,
/// where `jestyrc build` derives it from the source stem — and the whole
/// self-hosting golden family runs through that existing path.
fn build_one(source: &str, output: &str) -> ExitCode {
    let prog = module::load(source);
    if !prog.diags.is_empty() {
        return report_program(&prog.modules, &prog.diags);
    }
    let (info, type_diags) = typeck::check_program(&prog.ast, &prog.modules);
    let mut diags = type_diags;
    diags.extend(escape::check(&prog.ast, &info));
    if diags.iter().any(|d| d.is_error()) {
        return report_program(&prog.modules, &diags);
    }
    if !program_has_main(&prog.ast) {
        eprintln!("error: `{source}` has no `main` function — a build target needs an entry point");
        return ExitCode::FAILURE;
    }
    let (c_src, cgen_diags) = cgen::emit(&prog.ast, &info);
    if !cgen_diags.is_empty() {
        return report_program(&prog.modules, &cgen_diags);
    }

    let mut c_file = std::env::temp_dir();
    c_file.push(format!("jestyr_plan_{output}.c"));
    if let Err(e) = std::fs::write(&c_file, &c_src) {
        eprintln!("error: cannot write `{}`: {e}", c_file.display());
        return ExitCode::FAILURE;
    }
    let Some(cc) = find_c_compiler() else {
        eprintln!("note: no C compiler (cc/gcc/clang) found on PATH; wrote C to {}", c_file.display());
        return ExitCode::FAILURE;
    };
    // The plan names the output, so it lands where the build was invoked — the one
    // thing a build system is expected to do that `jestyrc build` does not.
    let exe = format!("{output}{}", std::env::consts::EXE_SUFFIX);
    let mut cmd = Command::new(&cc);
    cmd.args(cc_base_flags());
    if c_src.contains("pthread") {
        cmd.arg("-pthread");
    }
    match cmd.arg("-o").arg(&exe).arg(&c_file).status() {
        Ok(s) if s.success() => ExitCode::SUCCESS,
        Ok(s) => {
            eprintln!("error: {cc} failed to compile `{source}` (exit {:?})", s.code());
            ExitCode::FAILURE
        }
        Err(e) => {
            eprintln!("error: cannot run {cc}: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `jestyrc attest --diff <old> <new>`: read two manifest files, classify every
/// per-item contract change as breaking or compatible, print the report, and exit
/// non-zero iff any breaking change is found — a drop-in CI ABI gate.
fn run_attest_diff(old_path: &str, new_path: &str) -> ExitCode {
    let read = |p: &str| -> Result<String, ExitCode> {
        std::fs::read_to_string(p).map_err(|e| {
            eprintln!("error: cannot read `{p}`: {e}");
            ExitCode::FAILURE
        })
    };
    let (old_text, new_text) = match (read(old_path), read(new_path)) {
        (Ok(a), Ok(b)) => (a, b),
        _ => return ExitCode::FAILURE,
    };
    let parse = |p: &str, t: &str| -> Result<attest::ParsedManifest, ExitCode> {
        attest::parse_manifest(t).map_err(|e| {
            eprintln!("error: `{p}` is not a valid attest manifest: {e}");
            eprintln!("note: pass files produced by `jestyrc attest <file.jtr>`");
            ExitCode::FAILURE
        })
    };
    let (old, new) = match (parse(old_path, &old_text), parse(new_path, &new_text)) {
        (Ok(a), Ok(b)) => (a, b),
        _ => return ExitCode::FAILURE,
    };
    let report = attest::diff(&old, &new);
    print!("{}", report.render());
    // Breaking changes fail the gate; a compatible-only (or empty) diff passes.
    if report.has_breaking() {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// `jestyrc test --list`: print the runnable `@test`/`@bench` names (each on its
/// own line, tagged `test`/`bench`, in source order), narrowed by the optional
/// `filter` substring. One greppable line per item, so CI can slice the harness.
/// Always succeeds — listing zero items (e.g. an over-narrow filter) is not an
/// error, just an empty list with a stderr note.
fn list_tests(ast: &ast::Ast, filter: Option<&str>) -> ExitCode {
    let items: Vec<(String, cgen::TestKind)> = cgen::list_tests(ast)
        .into_iter()
        .filter(|(name, _)| filter.is_none_or(|f| name.contains(f)))
        .collect();
    for (name, kind) in &items {
        let tag = match kind {
            cgen::TestKind::Test => "test",
            cgen::TestKind::Bench => "bench",
        };
        println!("{tag} {name}");
    }
    if items.is_empty() {
        match filter {
            Some(f) => eprintln!("note: no tests or benches match `{f}`"),
            None => eprintln!("note: no `@test` or `@bench` functions found"),
        }
    }
    ExitCode::SUCCESS
}

/// Report diagnostics that may originate from any module, rendering each against
/// its own source file (for correct `path:line:col` and carets).
fn report_program(modules: &module::Modules, diags: &[diag::Diagnostic]) -> ExitCode {
    if diags.is_empty() {
        return ExitCode::SUCCESS;
    }
    eprintln!();
    // One line table per region for the whole report — rendering each diagnostic
    // independently rescans the file from byte 0, which a large error storm feels.
    eprint!("{}", modules.render_all(diags));
    let errors = diags.iter().filter(|d| d.is_error()).count();
    if errors > 0 {
        eprintln!("{errors} error(s)");
        ExitCode::FAILURE
    } else {
        eprintln!("{} warning(s)", diags.len());
        ExitCode::SUCCESS
    }
}

/// Write the emitted C to a temp file, compile it with a detected C compiler,
/// and (when `run` is set) execute the resulting binary.
/// The C compile flags for every Jestyr translation unit. The floating-point pair
/// is the **determinism seam** (`Jestyr-Remaining-And-Numerics-Research.md` §3.3):
///
/// - `-ffp-contract=off` forbids the compiler from fusing `a*b + c` into a single
///   FMA — a *different* rounding than separate mul+add, which would break
///   bit-identity between a machine with FMA and one without (and scalar vs SIMD).
/// - `-fno-fast-math` guarantees none of the value-changing transforms
///   (reassociation, `-Ofast`-style assumptions) ever creep in, even if a future
///   default or build wrapper would enable them.
///
/// Both must be *locked*, not hoped for: that pair is the entire difference between
/// "deterministic" and "usually deterministic" once floating point enters. Never
/// add `-ffast-math`/`-Ofast`. (FTZ/DAZ is a runtime MXCSR state, not a flag — the
/// emitted program simply never sets it.)
const CC_FLAGS: &[&str] = &["-O2", "-std=c11", "-ffp-contract=off", "-fno-fast-math"];

/// Emit debug info: the C compiler carries the `#line N "file.jtr"` directives the
/// backend emits into DWARF, so gdb/lldb/perf/Valgrind map the binary back to
/// `.jtr` source instead of generated C. Kept **separate** from [`CC_FLAGS`]
/// because it is a usability flag, not part of the FP-determinism seam: it does
/// not change codegen, the emitted C, or its hash — so it stays out of the
/// `jestyr attest` provenance (which pins exactly the determinism flags) and out
/// of the FP-lock invariant. `-g` does not affect the locked rounding behavior.
const DEBUG_FLAG: &str = "-g";

/// The full flag list prepended to every cc invocation: the locked determinism
/// seam ([`CC_FLAGS`]) followed by debug info ([`DEBUG_FLAG`]). A pure function so
/// a test can assert the command carries **both** the FP flags and `-g` without
/// running a compiler (mirrors how `fp_determinism_flags_are_locked` inspects the
/// const directly).
fn cc_base_flags() -> Vec<&'static str> {
    let mut flags: Vec<&'static str> = CC_FLAGS.to_vec();
    flags.push(DEBUG_FLAG);
    flags
}

fn build_and_maybe_run(path: &str, c_src: &str, run: bool) -> ExitCode {
    let stem = Path::new(path).file_stem().and_then(|s| s.to_str()).unwrap_or("out");
    let mut c_file = std::env::temp_dir();
    c_file.push(format!("jestyr_{stem}.c"));
    let mut exe = std::env::temp_dir();
    exe.push(format!("jestyr_{stem}{}", std::env::consts::EXE_SUFFIX));

    if let Err(e) = std::fs::write(&c_file, c_src) {
        eprintln!("error: cannot write `{}`: {e}", c_file.display());
        return ExitCode::FAILURE;
    }

    let Some(cc) = find_c_compiler() else {
        eprintln!("note: no C compiler (cc/gcc/clang) found on PATH.");
        eprintln!("      wrote C to: {}", c_file.display());
        eprintln!("      compile it manually, e.g.:  gcc -O2 -o out {}", c_file.display());
        return ExitCode::FAILURE;
    };

    let mut cmd = Command::new(&cc);
    cmd.args(cc_base_flags());
    // Structured-concurrency output uses pthreads; link it only when present.
    if c_src.contains("pthread") {
        cmd.arg("-pthread");
    }
    let status = cmd.arg("-o").arg(&exe).arg(&c_file).status();
    match status {
        Ok(s) if s.success() => {}
        Ok(s) => {
            eprintln!("error: {cc} failed to compile generated C (exit {:?})", s.code());
            eprintln!("       generated C is at: {}", c_file.display());
            return ExitCode::FAILURE;
        }
        Err(e) => {
            eprintln!("error: could not run `{cc}`: {e}");
            return ExitCode::FAILURE;
        }
    }

    println!("built: {} (via {cc})", exe.display());
    if !run {
        return ExitCode::SUCCESS;
    }

    println!("running {}:\n", exe.display());
    match Command::new(&exe).status() {
        Ok(s) => {
            if let Some(code) = s.code() {
                ExitCode::from(code as u8)
            } else {
                ExitCode::SUCCESS
            }
        }
        Err(e) => {
            eprintln!("error: could not run `{}`: {e}", exe.display());
            ExitCode::FAILURE
        }
    }
}

/// Find a Unix-style C compiler on PATH.
fn find_c_compiler() -> Option<String> {
    for cc in ["cc", "gcc", "clang"] {
        let ok = Command::new(cc)
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ok {
            return Some(cc.to_string());
        }
    }
    None
}

fn print_usage() {
    eprintln!("jestyrc — the Jestyr bootstrap compiler (v0.1)");
    eprintln!();
    eprintln!("usage:");
    eprintln!("    jestyrc <file.jtr>          parse a file and print its AST   (default)");
    eprintln!("    jestyrc parse  <file.jtr>   same as above, explicitly");
    eprintln!("    jestyrc check  <file.jtr>   resolve, type-check, ownership-check");
    eprintln!("                               (add --json for a machine-readable report)");
    eprintln!("    jestyrc emit-c <file.jtr>   lower to C and print it");
    eprintln!("    jestyrc build  <file.jtr>   lower to C and compile a native binary");
    eprintln!("    jestyrc run    <file.jtr>   build, then execute the binary");
    eprintln!("    jestyrc test   <file.jtr>   build & run the `@test`/`@bench` harness");
    eprintln!("                               (test [substr] runs matching names; --list lists them)");
    eprintln!("    jestyrc tokens <file.jtr>   stop after lexing and dump tokens");
    eprintln!("    jestyrc errsets <file.jtr>  report every error-set obligation site and");
    eprintln!("                               whether the declared sets hold (analysis only)");
    eprintln!("    jestyrc doc    <file.jtr>   render the file's API docs as Markdown");
    eprintln!("                               (add --html for an HTML page)");
    eprintln!("    jestyrc attest <file.jtr>   emit the reproducible-build + guarantee manifest");
    eprintln!("                               (sha256 of the emitted C + locked CC flags +");
    eprintln!("                                every item's proven contracts)");
    eprintln!("    jestyrc attest --diff <old> <new>");
    eprintln!("                               classify contract changes between two manifests");
    eprintln!("                               as breaking/compatible (exit 1 if any breaking)");
    eprintln!("    jestyrc plan   <build.jestyr>");
    eprintln!("                               evaluate a build script and print its build plan");
    eprintln!("                               (--build compiles each target it names;");
    eprintln!("                                --emit writes its comptime-generated artifacts)");
}

/// Print every lex/parse error as `path:line:col: error: message` and report whether
/// any were found — the terse locator form the analysis commands (`layout`,
/// `provenance`, `obligations`, `errsets`, `simd`, `doc`) use as their entry gate.
///
/// Shares one [`span::LineIndex`] across the loop: resolving each position
/// independently rescans the source from byte 0, so a file with many syntax errors
/// would cost O(errors x file).
fn report_lex_parse_errors(
    src: &str,
    path: &str,
    lex: &[diag::Diagnostic],
    parse: &[diag::Diagnostic],
) -> bool {
    let mut failed = false;
    let mut index = None;
    for d in lex.iter().chain(parse.iter()).filter(|d| d.is_error()) {
        let idx = index.get_or_insert_with(|| span::LineIndex::new(src));
        let lc = idx.line_col(src, d.span.start);
        eprintln!("{path}:{}:{}: error: {}", lc.line, lc.col, d.message);
        failed = true;
    }
    failed
}

fn dump_tokens(src: &str, path: &str, tokens: &[token::Token]) {
    println!("// {} tokens from {}", tokens.len(), path);
    // A position per token: without a shared line table this dump is quadratic in
    // file size, and it is the one command whose whole job is to emit one line per token.
    let index = span::LineIndex::new(src);
    for tok in tokens {
        let lc = index.line_col(src, tok.span.start);
        let lexeme = src[tok.span.range()].replace('\n', "\\n");
        println!("   {:>4}:{:<3} {:<12} {:?}", lc.line, lc.col, tok.kind.describe(), lexeme);
    }
}

fn report(src: &str, path: &str, diags: &[diag::Diagnostic]) -> ExitCode {
    if diags.is_empty() {
        return ExitCode::SUCCESS;
    }
    eprintln!();
    let index = span::LineIndex::new(src);
    for d in diags {
        eprintln!("{}", d.render_indexed(src, &index, path, diag::Severity::Error));
    }
    eprintln!("{} error(s)", diags.len());
    ExitCode::FAILURE
}

#[cfg(test)]
mod budget_canary {
    //! The `selfbench` baseline as an *enforced* regression gate (TESTING.md §5.11).
    //! Two layers, by robustness:
    //!  * a **deterministic** codegen-budget gate (emitted-C bytes per source line,
    //!    AST density) — machine-independent, so it can assert tight bounds; it fires
    //!    on a real codegen-bloat / output regression, not on a slow CI box;
    //!  * a **generous** wall-clock floor that only catches a *catastrophic*
    //!    (order-of-magnitude) throughput regression — the precise speed figure
    //!    (137K lines/s release) is tracked by `jestyrc selfbench` + a CI job, since a
    //!    tight wall-clock floor in `cargo test` (debug, varied CI hardware) flakes.
    use super::gen_bench_program;
    use crate::lexer::Lexer;
    use crate::parser::Parser;
    use crate::{cgen, escape, typeck};
    use std::time::Instant;

    fn compile(src: &str) -> (usize, usize, usize) {
        let (tokens, _) = Lexer::new(src).tokenize();
        let (ast, _) = Parser::new(src, tokens).parse();
        let nexpr = ast.exprs.len();
        let (info, _) = typeck::check(&ast);
        let _ = escape::check(&ast, &info);
        let (c, _) = cgen::emit(&ast, &info);
        (c.len(), nexpr, src.lines().count())
    }

    #[test]
    fn codegen_budget_stays_within_envelope() {
        // Deterministic: same generator → same emitted C, on every machine. The
        // baseline is ~239 emitted-C bytes/line; the ceiling catches a ~35% bloat.
        let src = gen_bench_program(200);
        let (cbytes, nexpr, lines) = compile(&src);
        let c_per_line = cbytes as f64 / lines as f64;
        assert!(
            c_per_line < 320.0,
            "emitted-C budget exceeded: {c_per_line:.1} bytes/source-line (ceiling 320)"
        );
        // AST density sanity — a parser/generator regression would move this sharply.
        assert!(nexpr > lines, "too few AST exprs: {nexpr} for {lines} lines");
    }

    #[test]
    fn throughput_has_not_catastrophically_regressed() {
        // GENEROUS floor: ~13K lines/s in a debug `cargo test`, ~137K release. A
        // 2K-lines/s floor only trips on a >6x debug regression, so it won't flake on
        // a slow CI box while still catching a pathological slowdown. The real budget
        // tracking is `selfbench` (release) + CI — see the module docs.
        let src = gen_bench_program(200);
        let lines = src.lines().count() as f64;
        let runs = 5;
        let t0 = Instant::now();
        for _ in 0..runs {
            let _ = compile(&src);
        }
        let per = t0.elapsed().as_secs_f64() / runs as f64;
        let lines_per_s = lines / per;
        assert!(
            lines_per_s > 2000.0,
            "catastrophic throughput regression: {lines_per_s:.0} lines/s (floor 2000)"
        );
    }
}

#[cfg(test)]
mod fp_contract_tests {
    use super::{cc_base_flags, CC_FLAGS, DEBUG_FLAG};

    /// The floating-point determinism seam is **locked into the build command**, not
    /// hoped for: every translation unit forbids FMA contraction and value-changing
    /// fast-math transforms. (Teeth: dropping either flag fails this; that is the
    /// difference between deterministic and usually-deterministic FP — NUMERICS §3.3.)
    #[test]
    fn fp_determinism_flags_are_locked() {
        assert!(CC_FLAGS.contains(&"-ffp-contract=off"), "FMA contraction must be off: {CC_FLAGS:?}");
        assert!(CC_FLAGS.contains(&"-fno-fast-math"), "fast-math must be off: {CC_FLAGS:?}");
        // And the value-changing escape hatches must never be present.
        assert!(!CC_FLAGS.iter().any(|f| *f == "-ffast-math" || *f == "-Ofast"),
            "no value-changing FP flags allowed: {CC_FLAGS:?}");
    }

    /// Wiring: every cc invocation carries `-g` so the emitted `#line` directives
    /// reach DWARF — and it rides *alongside*, not *inside*, the determinism seam.
    /// (Teeth: dropping `DEBUG_FLAG` from `cc_base_flags` fails the first assert;
    /// folding `-g` into `CC_FLAGS` — which would corrupt the `attest` provenance —
    /// fails the second.)
    #[test]
    fn debug_flag_is_carried_and_separate_from_the_determinism_seam() {
        assert_eq!(DEBUG_FLAG, "-g");
        let flags = cc_base_flags();
        assert!(flags.contains(&"-g"), "cc command must carry -g for DWARF: {flags:?}");
        // The base flags are the determinism seam plus exactly `-g`, in order.
        assert!(CC_FLAGS.iter().all(|f| flags.contains(f)), "FP flags must survive: {flags:?}");
        assert_eq!(flags.len(), CC_FLAGS.len() + 1, "only -g is added: {flags:?}");
        // `-g` is a usability flag, never part of the locked determinism/provenance set.
        assert!(!CC_FLAGS.contains(&"-g"), "-g must stay out of CC_FLAGS (attest provenance)");
    }
}
