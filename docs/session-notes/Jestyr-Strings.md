# Jestyr Strings & Text — Complete Reference

Everything Jestyr can do with strings, as of the completed **"Real strings / text"**
workstream (roadmap E). The design is a deliberate synthesis:

- **Swift** — explicit *views* at each granularity (bytes / codepoints / graphemes), no random
  codepoint indexing.
- **Zig** — bytes + explicit Unicode + a distinct null-terminated FFI type; **cost always visible,
  no expensive default**.
- **Rust** — UTF-8 validity *by construction*; no slicing on a non-char-boundary; `Cow`.
- **Erlang** — iolists (a tree of fragments, flattened once) + zero-copy sub-views.
- **Jestyr's own** — region-allocated strings whose safety is **statically proven**, and UTF-8
  validity carried as a **type-state**.

> Governing rule: **explicit views, visible cost, no expensive default** — then layer regions +
> provability on top. (D's auto-decoding is deliberately *not* adopted — it hides cost.)

---

## 1. The types

| Type | Role | C representation | Owns? |
|---|---|---|---|
| `str` | proven-UTF-8 **view** (borrowed) | `{ const char* ptr; size_t len }` | no (borrows) |
| `String` | proven-UTF-8 **owned**, growable | `{ char* ptr; size_t len; size_t cap }` | yes (heap) |
| `Builder` | iolist — a tree of `str` fragments | `{ JestyrStr* frags; size_t n; size_t cap }` | builds once |
| `Cow` | borrowed **or** owned, visibly | `{ char* ptr; size_t len; size_t cap }` (`cap==0` ⇒ borrowed) | maybe |
| `bytes` (`[]u8`) | raw bytes, **unvalidated** | `{ uint8_t* ptr; size_t len }` | no |
| `os_str` | platform text, **unvalidated** (WTF-8 / `OsStr`) | `{ const char* ptr; size_t len }` | no |
| `cstr` | null-terminated, **for C interop only** | `const char*` | no |

The owned/view split is universal (C++ `string_view`, Rust `&str`/`String`). `str.len` is **O(1)** —
the length is carried, never `strlen`.

---

## 2. Length & cost-visible views

The cost is in the **name**, so you can never accidentally pay O(n) for a "length":

```jestyr
let s: str = "café"          // 5 bytes (é is 2 bytes in UTF-8)
s.len                         // 5   — O(1), the byte length
count_codepoints(s)          // 4   — O(n), a UTF-8 decode
count_graphemes(s)           // 4   — O(n), grapheme clusters
```

`count_graphemes` counts grapheme *clusters* (a base codepoint plus its combining marks), so a
decomposed `"e\xCC\x81"` (e + combining acute U+0301) is **2 codepoints but 1 grapheme**.

---

## 3. Indexing & slicing

```jestyr
let s: str = "Hello"
s[1]                          // a byte (u8) — byte indexing is explicit/honest
s[1..4]                       // "ell"  — a zero-copy sub-VIEW (str)
s[0..]                        // "Hello" — open end → s.len
substr(s, 3, 5)               // "lo"   — the named form of s[3..5]
```

`s[i..j]` and `substr` are **boundary-checked**: `start ≤ end ≤ len` *and* both ends must sit on a
UTF-8 char boundary (Rust's "no slicing mid-codepoint"). The result borrows the same bytes — no
allocation (Erlang sub-binary sharing). Demo: `examples/substr.jtr` → `ell, Hello, lo, 3`.

---

## 4. Iteration

All zero-copy, all with the cost in the name — never an implicit decode:

```jestyr
for b in s              { … }   // bytes (u8)
for cp in codepoints(s) { … }   // codepoints (u32), O(n) decode
for cp, off in codepoints(s) { … }   // Go-style: codepoint + its byte offset
for g in graphemes(s)   { … }   // grapheme clusters, each a str sub-view
for part in split(s, ",") { … } // split on a separator; each part is a str view
```

Demo: `examples/str_iter.jtr` → `1, 2, 3, 3, 0, 1, 2, 1` (split lengths, then byte offsets, then
codepoints-vs-graphemes).

---

## 5. Operations

Byte-level, view-based (`find`/`trim` are zero-copy):

```jestyr
str_eq(a, b)            // exact byte equality
eq_fold(a, b)           // ASCII case-insensitive (the opt-in normalization-aware compare)
starts_with(s, p)       // bool
ends_with(s, p)         // bool
contains(s, n)          // bool
find(s, n)              // isize — byte offset of the first match, or -1
trim(s)                 // str — a zero-copy view with ASCII whitespace stripped
```

`find` + `substr` compose into split-by-hand; `split(...)` (above) is the iterator form. Demo:
`examples/str_ops.jtr` → `true, true, false, 7, foo, bar`.

---

## 6. UTF-8 validity — a type-state, not a hope

`str` and `String` are **proven valid UTF-8 by construction**. The *only* doors from raw bytes to a
proven `str` are validation:

```jestyr
is_utf8(b)              // bool — check without converting
from_utf8(b)            // str  — validates once, then TRAPS on invalid (asserts)
try_from_utf8(b)        // str !Utf8Error — RECOVERABLE; branch with is_err / unwrap / ?
```

```jestyr
let r = try_from_utf8(b)        // no annotation needed — typed str !Utf8Error
if is_err(r) { return -1 }      // recovered, no trap
return unwrap(r).len as i32     // the validated view
```

Once validated, validity is a *trusted invariant* — every later operation assumes it. Demos:
`examples/strings.jtr`, `examples/try_utf8.jtr` (`5, -1`).

---

## 7. Building strings — zero-copy iolists + interpolation

**`Builder`** (Erlang iodata): collect `str` fragments without intermediate copies, flatten **once**
into an owned `String`:

```jestyr
var b: Builder = builder_new()
builder_push(b, "Hello, ")
builder_push(b, name)
var out: String = builder_build(b)   // the single allocation
```

**f-strings** — typed interpolation, lowered to fragment concatenation:

```jestyr
var msg: String = f"{name} says x = {x} ({ok})"
```

Demos: `examples/builder.jtr`, `examples/fstring.jtr`.

---

## 8. `Cow<str>` — borrowed-or-owned, *visibly*

A copy-on-write string. Borrowing is free; you can **see** when it allocates:

```jestyr
var c: Cow = cow_borrow("hello")   // borrow — no allocation
cow_is_owned(c)                    // false
var owned: Cow = cow_to_mut(c)     // the CoW point — clones iff borrowed
cow_is_owned(owned)                // true  — now it owns its bytes
cow_view(owned)                    // str
cow_free(owned)                    // frees only if owned
```

Demo: `examples/cow.jtr` → `false, 5, true, 5`.

---

## 9. Platform text & C interop

**`os_str`** — unvalidated platform text (the Rust `OsStr` / WTF-8 role; relevant on Windows). A
distinct type, so you can't use it where a proven `str` is expected without going through validation
or a lossy decode:

```jestyr
let os: os_str = os_from_bytes(raw)   // platform bytes, unproven
var clean: String = to_str_lossy(os)  // proven String; ill-formed bytes → U+FFFD
```

**`cstr`** — null-terminated, quarantined to the FFI boundary (Zig's `[*:0]u8`):

```jestyr
extern "c" fn puts(s: cstr) -> i32
puts("hello".cstr)                    // bridge a view to a bare pointer for C
```

**`bytes`** (`[]u8`) — the raw byte view, the input to validation. Demos: `examples/os_str.jtr`,
`examples/extern_c.jtr`.

---

## 10. Region strings — provably zero-allocation text

The Jestyr differentiator. Build text in a `region` arena (bump-allocated, freed once at block end),
and the escape checker **statically proves** no fragment outlives the arena:

```jestyr
region r {
    let g: str = region_concat(r, "Hello, ", "region!")   // in the arena
    print_str(g)                                           // fine — used in scope
}                                                          // arena freed here
```

The proof rejects **both** ways a region value could dangle:

```jestyr
fn bad1() -> str {
    region r { let g = region_concat(r, "a", "b") return g }   // ERROR: return escapes
    return ""
}
fn bad2() -> i32 {
    var saved: str = ""
    region r { saved = region_concat(r, "a", "b") }            // ERROR: assign-to-outer escapes
    return 0
}
```

This is the one thing on the whole survey **no surveyed language can do**: text processing that is
*statically proven* to allocate nothing on the heap. Demos: `examples/region_string.jtr` (runs),
`examples/region_escape.jtr` (2 errors).

---

## 11. Where each idea came from

| Source | Idea | In Jestyr |
|---|---|---|
| **Swift** | multi-view (bytes/codepoints/graphemes), no random codepoint index | ✅ views + iterators; byte `s[i]`, no codepoint `s[i]` |
| **Zig** | bytes + explicit Unicode + sentinel C type | ✅ `str`/`bytes` + `cstr` |
| **Rust** | UTF-8 invariant by construction; no mid-codepoint slice; `Cow` | ✅ `from_utf8` gate; boundary-checked `s[i..j]`; `Cow` |
| **Erlang** | iolists; sub-binary sharing | ✅ `Builder`; zero-copy substrings |
| **Go** | `(offset, codepoint)` iteration; explicit builder | ✅ `for cp, off in codepoints(s)`; `Builder` |
| **C++** | `string_view` | ✅ `str` |
| **Raku** | normalization-aware compare (opt-in) | ⚠️ `eq_fold` (ASCII); full NFC deferred |
| **D** | auto-decoding | ❌ deliberately **not** adopted (hides cost) |
| **Jestyr** | region + provability; validity as type-state | ✅ region-escape proof; `from_utf8` boundary |

---

## 12. Deliberately deferred

Honest scope — these need data tables or larger refactors, and are documented as future work:

- **Full UAX#29 grapheme segmentation** (ZWJ emoji, regional indicators) — current segmentation
  handles base + combining marks. Needs the Unicode tables.
- **Full Unicode case-folding / NFC normalization** (so composed "é" == decomposed "é") — needs the
  decomposition tables. `eq_fold` is ASCII-only.
- **Allocator-value threading** through the heap `String`/`Builder` — region strings already show
  allocator-as-value for text (the arena); a *general* `Allocator` value through every `realloc` is
  a larger refactor.
- **`os_str`/`distinct` enforcement at call args** — enforced at `let` annotations today (the lenient
  checker doesn't yet type-check general arguments).

---

## 13. Demo index

| Demo | Output | Shows |
|---|---|---|
| `strings.jtr` | `13, 72, 3, 5, …` | `str` view + `cstr`, O(1) `.len` |
| `substr.jtr` | `ell, Hello, lo, 3` | `s[i..j]` / `substr` zero-copy sub-views |
| `str_ops.jtr` | `true, true, false, 7, foo, bar` | `str_eq`/`starts_with`/`contains`/`find`/`trim` |
| `str_iter.jtr` | `1, 2, 3, 3, 0, 1, 2, 1` | `split` / `(offset,cp)` / `graphemes` |
| `eq_fold.jtr` | `false, true, true` | case-fold comparison |
| `try_utf8.jtr` | `5, -1` | recoverable `try_from_utf8` |
| `os_str.jtr` | `6` | `os_str` + `to_str_lossy` (U+FFFD) |
| `cow.jtr` | `false, 5, true, 5` | `Cow<str>` borrow → CoW |
| `builder.jtr` | `9, …` | iolist `Builder` |
| `fstring.jtr` | message, `25` | f-strings |
| `region_string.jtr` | `Hello, region!, 14, 5, 4` | region-allocated text |
| `region_escape.jtr` | 2 errors | the region-safety **proof** |

*Part of the Jestyr language. Compiler internals: `HANDOFF.md` §5.52–§5.67.*
