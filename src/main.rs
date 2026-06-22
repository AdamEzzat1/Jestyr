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

mod ast;
mod cgen;
mod diag;
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

enum Mode {
    Parse,
    Check,
    EmitC,
    Build,
    Run,
    Tokens,
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
        Mode::EmitC | Mode::Build | Mode::Run => {
            // Codegen only ever sees well-formed programs: gate on every check.
            let prog = module::load(&path);
            if !prog.diags.is_empty() {
                return report_program(&prog.modules, &prog.diags);
            }
            let (info, type_diags) = typeck::check_program(&prog.ast, &prog.modules);
            let mut diags = type_diags;
            diags.extend(escape::check(&prog.ast, &info));
            if !diags.is_empty() {
                return report_program(&prog.modules, &diags);
            }

            let (c_src, cgen_diags) = cgen::emit(&prog.ast, &info);
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
                    build_and_maybe_run(&path, &c_src, matches!(mode, Mode::Run))
                }
            }
        }
    }
}

/// Report diagnostics that may originate from any module, rendering each against
/// its own source file (for correct `path:line:col` and carets).
fn report_program(modules: &module::Modules, diags: &[diag::Diagnostic]) -> ExitCode {
    if diags.is_empty() {
        return ExitCode::SUCCESS;
    }
    eprintln!();
    for d in diags {
        eprintln!("{}", modules.render(d, diag::Severity::Error));
    }
    eprintln!("{} error(s)", diags.len());
    ExitCode::FAILURE
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
    eprintln!("    jestyrc tokens <file.jtr>   stop after lexing and dump tokens");
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
