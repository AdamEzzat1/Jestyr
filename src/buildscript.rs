//! `build.jestyr` — the build described in Jestyr itself (roadmap workstream G,
//! tier 4; design §11 "the build is described in Jestyr, comptime-driven, no
//! separate DSL").
//!
//! A build script is an ordinary Jestyr file that is **evaluated, never run**: the
//! comptime interpreter ([`crate::comptime`]) reads it and produces a *build plan*,
//! a deterministic list of `(source, output)` targets. Nothing in the script is
//! compiled, and nothing in it can perform an effect.
//!
//! ## Why a pure *description* rather than a build DSL
//! The obvious shape is the imperative one every build system reaches for:
//!
//! ```text
//! pub fn build() { exe("app", "src/main.jtr"); test("tests/main.jtr") }
//! ```
//!
//! That needs compile-time **effects** — `exe(…)` has to record something — and an
//! effectful comptime evaluator is exactly what the tier ladder exists to prevent
//! (`docs/ctfe-tiers.md`). Allowing it here would mean a build script could read the
//! clock or the environment, and reproducibility would become a convention rather
//! than a property.
//!
//! So the plan is a *pure function of an index* instead. The script declares how many
//! targets there are and answers two questions about each:
//!
//! ```jestyr
//! const targets: i64 = 2
//!
//! fn source(i: i64) -> str {
//!     if i == 0 { return "examples/hello.jtr" }
//!     return "examples/shapes.jtr"
//! }
//!
//! fn output(i: i64) -> str {
//!     if i == 0 { return "hello" }
//!     return "shapes"
//! }
//! ```
//!
//! This costs the evaluator no new powers — no effects, no aggregate values, no
//! comptime `for` — and it keeps every property the ladder promises: the same script
//! always yields the same plan, and the plan is a value the compiler *derived*, so it
//! can be attested like anything else. (Once tier 3 grows aggregate comptime values,
//! a target list becomes expressible directly; the driver contract need not change.)
//!
//! ## Determinism
//! The script is evaluated by the same total interpreter as every other comptime
//! construct: fuel-bounded, depth-capped, cycle-detecting, and with no arm for a
//! clock, an environment read, or any other effect. A script that tries to be
//! non-deterministic cannot be *written*, rather than being detected and rejected.

use crate::ast::Ast;
use crate::comptime::{Interp, Value};

/// One thing to build: a Jestyr source file and the executable name it produces.
#[derive(Clone, Debug, PartialEq)]
pub struct Target {
    pub source: String,
    pub output: String,
}

/// A whole build, in declaration order.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Plan {
    pub targets: Vec<Target>,
}

impl Plan {
    /// The plan as deterministic, diffable text — one target per line. This is what
    /// `jestyrc plan` prints, so a build description can be reviewed and pinned in CI
    /// the same way `attest`'s manifest is.
    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str("build-plan v1\n");
        out.push_str(&format!("targets {}\n", self.targets.len()));
        for t in &self.targets {
            out.push_str(&format!("target {} -> {}\n", t.source, t.output));
        }
        out
    }
}

/// The names a build script is expected to define. Kept here rather than spelled
/// inline so the error messages and the documentation cannot drift apart.
const COUNT_CONST: &str = "targets";
const SOURCE_FN: &str = "source";
const OUTPUT_FN: &str = "output";

/// The largest number of targets one script may declare. A bound rather than a
/// judgement: it turns a typo'd `const targets: i64 = 100000000` into a diagnostic
/// instead of a build that appears to hang.
const MAX_TARGETS: i64 = 4096;

/// Evaluate a parsed build script into a [`Plan`], or explain why it is not one.
///
/// Every failure is reported against the script's own vocabulary, because the author
/// of a build file should never have to read a comptime-evaluator error to understand
/// what they got wrong.
pub fn evaluate(ast: &Ast) -> Result<Plan, String> {
    let mut interp = Interp::new(ast);

    let n = match interp.eval_const(COUNT_CONST) {
        Ok(Value::Int(n)) => n,
        Ok(other) => {
            return Err(format!("`const {COUNT_CONST}` must be an integer, found {}", other.type_name()))
        }
        Err(e) => return Err(format!("`const {COUNT_CONST}`: {}", e.message)),
    };
    if n < 0 {
        return Err(format!("`const {COUNT_CONST}` is negative ({n})"));
    }
    if n > MAX_TARGETS {
        return Err(format!("`const {COUNT_CONST}` is {n}; the limit is {MAX_TARGETS}"));
    }

    let mut targets = Vec::with_capacity(n as usize);
    for i in 0..n {
        let source = string_at(&mut interp, SOURCE_FN, i)?;
        let output = string_at(&mut interp, OUTPUT_FN, i)?;
        if source.is_empty() {
            return Err(format!("`{SOURCE_FN}({i})` is empty"));
        }
        if output.is_empty() {
            return Err(format!("`{OUTPUT_FN}({i})` is empty"));
        }
        targets.push(Target { source, output });
    }
    Ok(Plan { targets })
}

fn string_at(interp: &mut Interp, f: &str, i: i64) -> Result<String, String> {
    match interp.call_fn(f, &[Value::Int(i)]) {
        Ok(Value::Str(s)) => Ok(s),
        Ok(other) => Err(format!("`{f}({i})` must return a string, found {}", other.type_name())),
        Err(e) => Err(format!("`{f}({i})`: {}", e.message)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::Lexer;
    use crate::parser::Parser;

    fn plan_of(src: &str) -> Result<Plan, String> {
        let (tokens, _) = Lexer::new(src).tokenize();
        let (ast, diags) = Parser::new(src, tokens).parse();
        assert!(diags.iter().all(|d| !d.is_error()), "fixture must parse: {diags:?}");
        evaluate(&ast)
    }

    const TWO: &str = "\
const targets: i64 = 2
fn source(i: i64) -> str {
    if i == 0 { return \"examples/hello.jtr\" }
    return \"examples/shapes.jtr\"
}
fn output(i: i64) -> str {
    if i == 0 { return \"hello\" }
    return \"shapes\"
}
";

    #[test]
    fn evaluates_a_build_script_into_a_plan() {
        let p = plan_of(TWO).expect("should evaluate");
        assert_eq!(
            p.targets,
            vec![
                Target { source: "examples/hello.jtr".into(), output: "hello".into() },
                Target { source: "examples/shapes.jtr".into(), output: "shapes".into() },
            ]
        );
        assert_eq!(
            p.render(),
            "build-plan v1\ntargets 2\ntarget examples/hello.jtr -> hello\ntarget examples/shapes.jtr -> shapes\n"
        );
    }

    /// The plan is *computed*, not merely transcribed — a script may derive names
    /// with the full comptime language, which is the whole point of describing a
    /// build in Jestyr rather than in a data format.
    #[test]
    fn a_plan_may_be_computed() {
        let src = "\
const base: str = \"examples/\"
const targets: i64 = 3
fn stem(i: i64) -> str {
    if i == 0 { return \"a\" }
    if i == 1 { return \"b\" }
    return \"c\"
}
fn source(i: i64) -> str { return base + stem(i) + \".jtr\" }
fn output(i: i64) -> str { return \"out_\" + stem(i) }
";
        let p = plan_of(src).expect("should evaluate");
        assert_eq!(p.targets.len(), 3);
        assert_eq!(p.targets[2], Target { source: "examples/c.jtr".into(), output: "out_c".into() });
    }

    #[test]
    fn an_empty_build_is_a_plan_with_no_targets() {
        let src = "const targets: i64 = 0\nfn source(i: i64) -> str { return \"\" }\n\
                   fn output(i: i64) -> str { return \"\" }\n";
        assert_eq!(plan_of(src).expect("should evaluate"), Plan::default());
    }

    /// The same script always yields the same plan. A build description whose result
    /// depended on evaluation order would break reproducible builds outright.
    #[test]
    fn a_plan_is_deterministic() {
        let first = plan_of(TWO).expect("should evaluate");
        for _ in 0..16 {
            assert_eq!(plan_of(TWO).expect("should evaluate"), first);
        }
    }

    /// Malformed scripts are refused in the *script's* vocabulary, not the
    /// evaluator's — the author should not have to read a comptime error to learn
    /// that they forgot `const targets`.
    #[test]
    fn a_malformed_script_is_refused_with_a_reason() {
        let cases: [(&str, &str); 5] = [
            ("no `const targets`", "fn source(i: i64) -> str { return \"a\" }\n"),
            (
                "targets is not an integer",
                "const targets: str = \"two\"\nfn source(i: i64) -> str { return \"a\" }\n\
                 fn output(i: i64) -> str { return \"b\" }\n",
            ),
            ("negative", "const targets: i64 = 0 - 1\n"),
            ("over the cap", "const targets: i64 = 99999\n"),
            (
                "missing `output`",
                "const targets: i64 = 1\nfn source(i: i64) -> str { return \"a\" }\n",
            ),
        ];
        for (label, src) in cases {
            let e = plan_of(src).expect_err(label);
            assert!(!e.is_empty(), "{label}: empty message");
        }
    }

    /// A build script cannot perform an effect, because the interpreter has no arm
    /// for one — the same structural rule that governs every other comptime
    /// construct. There is no allowlist here to fall out of sync.
    #[test]
    fn a_build_script_cannot_reach_the_outside_world() {
        let src = "const targets: i64 = 1\n\
                   fn source(i: i64) -> str { return read_file(\"cfg\") }\n\
                   fn output(i: i64) -> str { return \"x\" }\n";
        let e = plan_of(src).expect_err("file I/O must not be reachable");
        assert!(e.contains("source(0)"), "{e}");
    }

    /// Totality reaches the build driver too: a non-terminating script is a
    /// diagnostic, not a hung build.
    #[test]
    fn a_non_terminating_script_is_bounded() {
        let src = "const targets: i64 = 1\n\
                   fn spin(n: i64) -> str { return spin(n + 1) }\n\
                   fn source(i: i64) -> str { return spin(0) }\n\
                   fn output(i: i64) -> str { return \"x\" }\n";
        let e = plan_of(src).expect_err("must be bounded");
        assert!(e.contains("too deep") || e.contains("step budget"), "{e}");
    }
}
