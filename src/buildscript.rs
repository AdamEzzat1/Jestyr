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

/// One **generated artifact** (roadmap G tier 5): a path, and the bytes the comptime
/// evaluator computed for it.
///
/// This is the bounded-generation foundation. Note where the boundary sits: the
/// *evaluator* gains no new power — it still cannot touch a file, and it computed
/// this content the same way it computes any other comptime string. What writes the
/// file is the **driver**, only under an explicit `--emit`. Generation is therefore a
/// pure function whose result the user chooses to place, not an effect a script can
/// perform.
#[derive(Clone, Debug, PartialEq)]
pub struct Artifact {
    pub path: String,
    pub content: String,
}

impl Artifact {
    /// The SHA-256 of the content — the artifact's provenance record. It goes in the
    /// rendered plan, so a generated file can be pinned and drift-checked in CI
    /// exactly the way `attest`'s manifest pins the emitted C.
    pub fn sha256(&self) -> String {
        crate::sha256::hex(self.content.as_bytes())
    }
}

/// A whole build, in declaration order.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Plan {
    pub targets: Vec<Target>,
    pub artifacts: Vec<Artifact>,
}

impl Plan {
    /// The plan as deterministic, diffable text — one target or artifact per line.
    /// This is what `jestyrc plan` prints, so a build description can be reviewed and
    /// pinned in CI the same way `attest`'s manifest is.
    ///
    /// An artifact is rendered by its **hash**, never its content: the line stays one
    /// line whatever was generated, and a plan diff shows *that* an artifact changed
    /// without drowning the reviewer in the change.
    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str("build-plan v1\n");
        out.push_str(&format!("targets {}\n", self.targets.len()));
        for t in &self.targets {
            out.push_str(&format!("target {} -> {}\n", t.source, t.output));
        }
        if !self.artifacts.is_empty() {
            out.push_str(&format!("artifacts {}\n", self.artifacts.len()));
            for a in &self.artifacts {
                out.push_str(&format!(
                    "artifact {} {} sha256 {}\n",
                    a.path,
                    a.content.len(),
                    a.sha256()
                ));
            }
        }
        out
    }
}

/// The names a build script is expected to define. Kept here rather than spelled
/// inline so the error messages and the documentation cannot drift apart.
const COUNT_CONST: &str = "targets";
const SOURCE_FN: &str = "source";
const OUTPUT_FN: &str = "output";
const ARTIFACT_COUNT_CONST: &str = "artifacts";
const ARTIFACT_PATH_FN: &str = "artifact_path";
const ARTIFACT_TEXT_FN: &str = "artifact_text";

/// The largest number of targets one script may declare. A bound rather than a
/// judgement: it turns a typo'd `const targets: i64 = 100000000` into a diagnostic
/// instead of a build that appears to hang.
const MAX_TARGETS: i64 = 4096;

/// The largest a single generated artifact may be. The fuel budget already bounds how
/// much *work* a script may do, but "bounded generation" should say so in bytes too,
/// so a runaway concatenation is a diagnostic rather than a surprise on disk.
const MAX_ARTIFACT_BYTES: usize = 4 << 20;

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

    // Generated artifacts are OPTIONAL: a script with no `const artifacts` declares
    // none, so every build description written before tier 5 keeps working unchanged.
    let artifacts = match interp.eval_const(ARTIFACT_COUNT_CONST) {
        Err(_) => Vec::new(),
        Ok(Value::Int(n)) => {
            if n < 0 {
                return Err(format!("`const {ARTIFACT_COUNT_CONST}` is negative ({n})"));
            }
            if n > MAX_TARGETS {
                return Err(format!("`const {ARTIFACT_COUNT_CONST}` is {n}; the limit is {MAX_TARGETS}"));
            }
            let mut out = Vec::with_capacity(n as usize);
            for i in 0..n {
                let path = string_at(&mut interp, ARTIFACT_PATH_FN, i)?;
                check_artifact_path(&path, i)?;
                let content = string_at(&mut interp, ARTIFACT_TEXT_FN, i)?;
                if content.len() > MAX_ARTIFACT_BYTES {
                    return Err(format!(
                        "`{ARTIFACT_TEXT_FN}({i})` produced {} bytes; the limit is {MAX_ARTIFACT_BYTES}",
                        content.len()
                    ));
                }
                out.push(Artifact { path, content });
            }
            out
        }
        Ok(other) => {
            return Err(format!(
                "`const {ARTIFACT_COUNT_CONST}` must be an integer, found {}",
                other.type_name()
            ))
        }
    };

    Ok(Plan { targets, artifacts })
}

/// Keep a generated artifact inside the tree the build was invoked in.
///
/// "Bounded generation" is meant literally: a build script is data the *evaluator*
/// cannot act on, but `--emit` does write what it named, so the path itself is the
/// last place a bad script could reach out of the project. An absolute path or a `..`
/// component is refused rather than normalised — a script that wants to write outside
/// the tree is not a script whose intent should be guessed at.
fn check_artifact_path(path: &str, i: i64) -> Result<(), String> {
    if path.starts_with('/') || path.starts_with('\\') || path.contains(':') {
        return Err(format!("`{ARTIFACT_PATH_FN}({i})` is an absolute path (`{path}`)"));
    }
    if path.split(['/', '\\']).any(|c| c == "..") {
        return Err(format!("`{ARTIFACT_PATH_FN}({i})` escapes the project (`{path}`)"));
    }
    Ok(())
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

    // --- tier 5: bounded artifact generation ---

    /// A script with no `const artifacts` generates none — so every build description
    /// written before tier 5 keeps working, unchanged.
    #[test]
    fn artifacts_are_optional() {
        assert!(plan_of(TWO).expect("should evaluate").artifacts.is_empty());
    }

    /// The generated content is *computed*, and the plan records its hash rather than
    /// its content — one line per artifact whatever was generated, so a plan diff
    /// shows that an artifact changed without drowning the reviewer in the change.
    #[test]
    fn generates_a_deterministic_hashed_artifact() {
        let src = "\
const targets: i64 = 0
fn source(i: i64) -> str { return \"\" }
fn output(i: i64) -> str { return \"\" }

const artifacts: i64 = 1
fn artifact_path(i: i64) -> str { return \"gen/table.txt\" }
fn row(i: i64) -> str {
    if i >= 4 { return \"\" }
    return \"row\" + \"\\n\" + row(i + 1)
}
fn artifact_text(i: i64) -> str { return row(0) }
";
        let p = plan_of(src).expect("should evaluate");
        assert_eq!(p.artifacts.len(), 1);
        assert_eq!(p.artifacts[0].path, "gen/table.txt");
        assert_eq!(p.artifacts[0].content, "row\nrow\nrow\nrow\n");

        // The hash is the artifact's provenance record: same script, same bytes, same
        // digest, every time.
        let h = p.artifacts[0].sha256();
        for _ in 0..8 {
            assert_eq!(plan_of(src).expect("should evaluate").artifacts[0].sha256(), h);
        }
        assert!(p.render().contains(&format!("artifact gen/table.txt 16 sha256 {h}")), "{}", p.render());
    }

    /// "Bounded" is meant literally. A script is data the evaluator cannot act on, but
    /// `--emit` does write what a script named — so the path is the last place a bad
    /// script could reach out of the project, and it is refused rather than normalised.
    #[test]
    fn a_generated_artifact_cannot_escape_the_project() {
        let head = "const targets: i64 = 0\nfn source(i: i64) -> str { return \"\" }\n\
                    fn output(i: i64) -> str { return \"\" }\nconst artifacts: i64 = 1\n\
                    fn artifact_text(i: i64) -> str { return \"x\" }\n";
        for bad in ["/etc/passwd", "../../outside.txt", "C:\\\\windows\\\\x", "a/../../b"] {
            let src = format!("{head}fn artifact_path(i: i64) -> str {{ return \"{bad}\" }}\n");
            let e = plan_of(&src).expect_err(bad);
            assert!(
                e.contains("absolute path") || e.contains("escapes the project"),
                "{bad}: {e}"
            );
        }
        // And an ordinary nested path is fine.
        let ok = format!("{head}fn artifact_path(i: i64) -> str {{ return \"gen/a/b.txt\" }}\n");
        assert_eq!(plan_of(&ok).expect("nested is fine").artifacts[0].path, "gen/a/b.txt");
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
