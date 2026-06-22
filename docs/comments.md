# Jestyr Comments & Doc Comments — What Works Now

> A reference for Jestyr's comment system **as implemented**, with runnable
> examples. Every snippet below is exercised by [`examples/docs.jtr`](../examples/docs.jtr),
> which both **runs** (`jestyrc run`) and **documents itself** (`jestyrc doc`).

Jestyr has **three comment tiers** and a **documentation generator**. The guiding
rule throughout is one line:

> **Comments document; contracts prove.**

Prose in a doc comment is never checked — it can lie. The things that *can't* lie
(`requires`/`ensures`, error sets, `@no_panic`, refinements) live in real syntax,
and the doc generator renders them in a separate, clearly-labelled **Guarantees**
block. There are no "magic comments" that change how code compiles.

---

## 1. The three tiers

```jestyr
// a plain comment — ignored by everything

/// an outer doc comment — documents the item that FOLLOWS it
fn f() {}

//! an inner doc comment — documents the ENCLOSING module
```

| Marker | Tier | Documents | Block form |
|---|---|---|---|
| `//` | plain | nothing (discarded) | `/* … */` (nests) |
| `///` | outer doc | the next item | `/** … */` |
| `//!` | inner doc | the whole module | `/*! … */` |

As in Rust, an **extra marker char demotes a doc back to a plain comment**: `////`
and `/*** … */` are ordinary comments, and the empty `/**/` is not a doc comment.

> **Doc comments are trivia.** The lexer collects them into a side table and the
> parser never sees them — so a comment can never alter parsing. This is what
> makes the rule above *structural*, not just convention.

---

## 2. Outer docs — `///`

An outer doc attaches to the item immediately below it. Consecutive `///` lines
form one doc; a blank line is fine.

```jestyr
/// The absolute value of `x`.
///
/// Longer description here. Markdown is welcome: *emphasis*, `code`, lists.
fn abs(x: i32) -> i32 ensures result >= 0 {
    if x < 0 { return 0 - x }
    return x
}
```

Outer docs also attach to **methods** inside a struct, **enum** declarations,
**consts**, and **`extern`** declarations:

```jestyr
/// A monotonic counter.
struct Counter {
    n: i32

    /// Increase the count by one and return the new value.
    fn bump(mut self) -> i32 {
        self.n = self.n + 1
        return self.n
    }
}

/// The answer to everything.
const ANSWER: i32 = 42
```

### Dangling docs are warnings
A `///` that documents nothing (no item follows it) is reported — so a stray or
misplaced doc comment doesn't silently vanish:

```text
warning: doc comment (`///`) is not attached to an item
  --> file.jtr:3:1
   |
 3 | /// orphaned
   | ^^^^^^^^^^^^
   = help: place it directly above an item, or write `//` for a plain comment
```

---

## 3. Inner docs — `//!`

Inner docs document the **module** they appear in (conventionally at the top of
the file). They never attach to an item.

```jestyr
//! Small math helpers.
//!
//! This whole paragraph describes the module, not the function below.

fn add(a: i32, b: i32) -> i32 { return a + b }
```

---

## 4. Structured sections

A doc comment is split into a **summary** (the lead, before any heading) followed
by `#`-headed **sections**. Use any headings you like; some are conventional:

```jestyr
/// Parse one source file.
///
/// # Example
/// ```jestyr
/// let ast = parse_file(src)
/// ```
///
/// # Safety
/// Retains no references into `scratch`.
///
/// # Errors
/// Fails on malformed input.
fn parse_file(read src: []u8) {}
```

Recognized-by-convention (rendered as-is, not special-cased): `# Summary`,
`# Example`/`# Examples`, `# Safety`, `# Errors`, `# Panics`, `# Parameters`.

Fenced code blocks (```` ```jestyr ````) are extracted as **examples** — the seed
for future doctests (running them is not wired up yet).

---

## 5. Generating documentation — `jestyrc doc`

```sh
jestyrc doc examples/docs.jtr            # Markdown to stdout
jestyrc doc examples/docs.jtr --html     # a self-contained HTML page
```

For each item the generator emits:

1. the **reconstructed signature** (in a `jestyr` code block);
2. the **prose** from its `///` comment (summary + sections + examples);
3. a **Guarantees** block extracted from the AST — the *proven* half.

### The Guarantees block (the Jestyr difference)
This is what sets Jestyr's docs apart: the machine-checked facts are pulled
straight from the source, never from prose. For

```jestyr
/// Read element `i` of `xs`.
@no_panic fn at(xs: []i32, i: usize in 0..xs.len) -> i32 !{ OutOfBounds } {
    return xs[i]
}
```

the generator renders:

> **Guarantees** *(checked by the compiler)*
> - `@no_panic` — proven free of faulting operations
> - parameter `i` is constrained to `0..xs.len`
> - may fail with `!{ OutOfBounds }`

So a reader sees both the author's *intent* (prose) and the compiler's
*promises* (guarantees), and never has to wonder which is which.

---

## 6. Scope & limits (today)

- **What's implemented:** all three tiers (line + block forms), doc collection as
  trivia, attachment to items and struct methods, module docs, summary/section/
  example parsing, the `jestyrc doc` Markdown **and** HTML renderers, the
  AST-derived Guarantees block, and dangling-doc warnings.
- **Single file:** `jestyrc doc` documents the file you point it at; it does not
  yet crawl `import`s into one site. Run it per file for now.
- **Doctests:** examples are *extracted* and structured, but not *executed* yet.
- **No magic:** doc comments can never change compilation — by construction.
