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
//!   jestyrc tokens <file.jtr>   stop after lexing and dump the token stream
//!   jestyrc doc    <file.jtr>   render the file's API docs as Markdown (--html for HTML)
//!   jestyrc selfbench           compile a generated program; report per-stage speed +
//!                               footprint (build `--features bench-alloc` for heap bytes)

mod ast;
mod attrs;
mod cgen;
mod diag;
#[cfg(feature = "dharht-experiment")]
mod dharht;
mod doc;
mod escape;
mod lexer;
mod module;
mod parser;
mod printer;
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
use span::line_col;

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
    Test,
    Tokens,
    Doc { html: bool },
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();

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
        Some("tokens") | Some("parse") | Some("check") | Some("emit-c") | Some("build")
        | Some("run") | Some("test") => {
            let candidates = [
                sub("tokens", Mode::Tokens),
                sub("parse", Mode::Parse),
                sub("check", Mode::Check),
                sub("emit-c", Mode::EmitC),
                sub("build", Mode::Build),
                sub("run", Mode::Run),
                sub("test", Mode::Test),
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
                if sema.is_empty() {
                    println!("ok: no type or ownership errors in {path}");
                    return ExitCode::SUCCESS;
                }
                diags = sema;
            }
            report_program(&prog.modules, &diags)
        }
        Mode::EmitC | Mode::Build | Mode::Run | Mode::Test => {
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

            // `test` mode emits a `@test`/`@bench` harness `main`; the rest emit
            // the ordinary entry-point wrapper.
            let (c_src, cgen_diags) = if matches!(mode, Mode::Test) {
                cgen::emit_tests(&prog.ast, &info)
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
                    build_and_maybe_run(&path, &c_src, matches!(mode, Mode::Run | Mode::Test))
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

/// Report diagnostics that may originate from any module, rendering each against
/// its own source file (for correct `path:line:col` and carets).
fn report_program(modules: &module::Modules, diags: &[diag::Diagnostic]) -> ExitCode {
    if diags.is_empty() {
        return ExitCode::SUCCESS;
    }
    eprintln!();
    for d in diags {
        eprintln!("{}", modules.render(d, d.severity));
    }
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
    cmd.args(["-O2", "-std=c11"]);
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
    eprintln!("    jestyrc emit-c <file.jtr>   lower to C and print it");
    eprintln!("    jestyrc build  <file.jtr>   lower to C and compile a native binary");
    eprintln!("    jestyrc run    <file.jtr>   build, then execute the binary");
    eprintln!("    jestyrc test   <file.jtr>   build & run the `@test`/`@bench` harness");
    eprintln!("    jestyrc tokens <file.jtr>   stop after lexing and dump tokens");
    eprintln!("    jestyrc doc    <file.jtr>   render the file's API docs as Markdown");
    eprintln!("                               (add --html for an HTML page)");
}

fn dump_tokens(src: &str, path: &str, tokens: &[token::Token]) {
    println!("// {} tokens from {}", tokens.len(), path);
    for tok in tokens {
        let lc = line_col(src, tok.span.start);
        let lexeme = src[tok.span.range()].replace('\n', "\\n");
        println!("   {:>4}:{:<3} {:<12} {:?}", lc.line, lc.col, tok.kind.describe(), lexeme);
    }
}

fn report(src: &str, path: &str, diags: &[diag::Diagnostic]) -> ExitCode {
    if diags.is_empty() {
        return ExitCode::SUCCESS;
    }
    eprintln!();
    for d in diags {
        eprintln!("{}", d.render(src, path, diag::Severity::Error));
    }
    eprintln!("{} error(s)", diags.len());
    ExitCode::FAILURE
}
