# `distinct` anti-regression corpus

The measured behaviour of `distinct D = Base` at **HEAD = `e293e8b`**, program by
program, so that any change to distinct-type semantics (operation inheritance, a
typed `std/path`, an operator rule) can be judged against what the language does
**today** rather than against what someone remembers it doing.

A previous attempt at operation inheritance shipped a **soundness regression**
because no such record existed: it replaced the blunt `does not implement Add`
rejection with an operand check that exempted untyped literals, and eight
laundering shapes that HEAD rejects started compiling and running. This directory
exists so that cannot happen twice.

## Replaying

```sh
bash examples/distinct_corpus/record.sh > /tmp/now.tsv
diff examples/distinct_corpus/baseline-HEAD-e293e8b.tsv /tmp/now.tsv
```

Two baselines are kept, and the older one is never retired:

| file | what it records |
| --- | --- |
| `baseline-HEAD-e293e8b.tsv` | the language **before** operation inheritance. The permanent reference for "was this row ever a rejection?" — the question the previous attempt got wrong. |
| `baseline-typeck-inheritance.tsv` | after the **typeck half** of operation inheritance (the operator rule, member/index inheritance, and the four unchecked assignment positions). The cgen half is not in it, so every `GCC_REJECT` row is still present. |

Diffing the two is the honest summary of the change: **18 holes close**
(`RUN_OK` → `TYPECK_REJECT`), **5 rows flip open on purpose** (R4 — a distinct
with *itself*: `a27`, `e01`, `e02`, `e03`, `e07`), and **no other row leaves a
rejection**. `TYPECK_REJECT` goes 37 → 50.

`record.sh` prints one TSV row per program: `<program>\t<verdict>\t<detail>`.
It takes an optional path to the compiler under test, so a self-hosted `jc` can be
recorded the same way.

Nothing here is wired into `cargo test`. It is deliberately a **separate, replayable
record**, not a golden — the design phase is expected to *change* many of these rows
on purpose, and a golden would only invite `--bless`. What must not change is the
required-rejection set below.

The directory is a subdirectory of `examples/`, and every corpus walk in the tree
(`src/span.rs`, `src/proptests.rs`) is a non-recursive `read_dir` over an explicit
list of directories, so these files do not enter any golden, count, or fixpoint.

## Verdict vocabulary

| verdict | meaning |
| --- | --- |
| `TYPECK_REJECT` | `jestyrc check` exits non-zero. The language refused the program. |
| `CGEN_REJECT` | check passes; the Jestyr backend refuses with its own diagnostic. |
| `GCC_REJECT` | check passes, C is emitted, and **gcc** refuses it. **This is a hole, not a rejection** — the language accepted the program and a foreign tool caught it. |
| `RUN_OK` | built and ran; `detail` is stdout. |
| `RUN_FAIL` | built, ran, exited non-zero. |

`GCC_REJECT` rows are the ones to read first. They are places where `check` says
"ok" and the build then dies in a tool that knows nothing about Jestyr's types.

## What HEAD actually enforces

HEAD's `distinct` protection is **two independent, narrow rules**, and the corpus
shows exactly where each one stops.

**Rule 1 — the operator-trait rejection** (`resolve_operator_trait`, `src/typeck.rs`).
A binary operator whose **LEFT operand** types as `Ty::Named` demands
`impl <OpTrait> for <that type>`. A `distinct` has no such impl, so the operator is
refused. This is what catches all eight laundering shapes: each of them puts a
distinct on the left of *some* operator in the tree.

It is a **left-operand-only, per-operator** rule, and the corpus prices both limits:

* `a10`, `a11`, `a12`, `a17`, `a19` — put a **literal or a bare base value
  leftmost** and the rule never fires. `(1 + a) + b` mixes two id spaces and
  **prints 4 today**. `0 + a == 0 + b` compares two id spaces and **prints `true`
  today**.
* `a22`, `a23`, `a24` — `%`, `&`, `<<` have **no operator trait at all**, so
  cross-space `a % b`, `a & b`, `a << b` all run today.
* `a13`, `a14` — **compound assignment** (`a += b`) is not routed through the rule
  at all; cross-space `+=` runs today.
* `a15` — **unary minus** is not covered.

**Rule 2 — `distinct_mismatch`** (`src/typeck.rs`), applied at exactly three
positions: `let`/`var` **initializers**, **call arguments**, and **`return`**.
Everywhere else it is absent, and the corpus prices that too:

* `b04`, `b05`, `b06` — a plain **assignment statement** is unchecked. `a = b`
  with `a: Id` and `b: Acct` **runs today**.
* `b12`, `b13`, `b14` — **struct-literal fields** and **field writes** are
  unchecked. `Rec { id: b }` with a foreign distinct **runs today**.
* `b15` — **array-literal elements** are unchecked.
* `b18` — the arms of an `if` **expression** may disagree on id space.

## Standing bugs the corpus pins (independent of any design change)

1. **`(x as NonScalarType).field` emits invalid C.** `g07`, `g09`, `g12`, `f14` all
   die in gcc with `conversion to non-scalar type requested`. `g14` is the control:
   it contains **no `distinct` at all** — `(s as str).len` on a plain `str` fails
   the same way. So this is a general cgen cast bug, but it lands directly on the
   `Path` work, because `(p as str).len` is the natural spelling of the boundary
   cast the 132-site count is made of. `g11` and `g13` show the cast is fine in a
   `let` initializer and in argument position — only field-access-on-a-cast breaks.
2. **`.len` / `.ptr` on a distinct over `str`, `String`, a slice, or an array
   type-checks and then fails in gcc.** `c01`, `c02`, `c07`, `c08`, `c09`, `c10`,
   `f05`, `f13`. `field_type` returns `Ty::Unknown` for a `Distinct`, so typeck
   declines to judge and cgen emits `j_len`/`JestyrSlice_i64` that do not exist.
   **This is the shape the reverted attempt was trying to fix** — and note that
   fixing it in the reference alone is what broke
   `jestyr_typeck_dump_matches_reference` (the port's `typeck.jtr:999` still
   returns `t_unknown()`, and the P3 golden has no allowlist).
3. **A distinct over an enum cannot be used at all.** `f07` casts back and dies in
   gcc (`conversion to non-scalar type requested`); `f08` matches directly and is
   refused by the backend.
4. **A trait method of the base is not reachable through a distinct-over-struct.**
   `c13` type-checks and dies in gcc (`no member named 'j_total'`), while plain
   field access (`c05`, `c06`, `c14`) works.
5. **Same-type equality and ordering are refused.** `e01`–`e04`: `a == a2` with
   both operands the *same* `distinct` is a compile error. `e08` shows a
   hand-written `impl Eq for Id` does work, so the operator-trait path accepts
   impls on distinct types — the gap is that nothing derives them.

## Where HEAD is already permissive on purpose

String intrinsics **already accept a distinct-over-`str`** with no cast: `d01`–`d05`,
`d10`, `d11` all run. So does `print_str` (`d08`) and `print_int` on a
distinct-over-`i32` (`d09`). This is worth knowing before designing an operand rule:
the builtins are a **pre-existing, working precedent for base-operation inheritance**
— and `d06` shows the flip side, `str_eq(p, q)` with `p: P` and `q: Q` two different
distincts over `str`, running today and returning `true`.

---

# REQUIRED-REJECTION SET — a hard requirement for the design phase

The programs below are **rejected by HEAD at `check` time**. Every one of them must
**still be rejected at `check` time** after operation inheritance lands.

**Shrinking this set is a regression, not a trade-off.** It is not a strictness
preference to be balanced against ergonomics, and it is not something to be staged,
allowlisted, or deferred to a follow-up increment. If a design cannot keep a row in
this set, the design is wrong; the row is not.

Two clarifications, because both were what went wrong last time:

* **`GCC_REJECT` does not count as rejection.** Moving a row from `TYPECK_REJECT`
  to `GCC_REJECT` is a regression: `check` is the gate people run, and gcc knows
  nothing about id spaces. Only `TYPECK_REJECT` (or `CGEN_REJECT`, which is still
  a Jestyr diagnostic at a Jestyr source location) preserves the property.
* **The message may change; the refusal may not.** `does not implement Add` is a
  blunt message and a better one is welcome. What is fixed is that `jestyrc check`
  exits non-zero on these inputs.

## R1 — cross-space arithmetic and comparison (the laundering shapes)

The eight shapes named in the post-mortem, plus the ones adjacent to them.

| program | construct | HEAD message |
| --- | --- | --- |
| `a01_add_two_distincts` | `a + b` | `type ``Id`` does not implement ``Add`` (the ``+`` operator)` |
| `a02_add_paren_right_literal` | `a + (b + 1)` | `... ``Acct`` ... ``Add``` |
| `a03_add_left_paren_literal` | `(a + 1) + b` | `... ``Id`` ... ``Add``` |
| `a04_mul_nested_literal` | `a * (b * 2)` | `... ``Acct`` ... ``Mul``` |
| `a05_sub_zero_minus` | `a - (0 - b)` | `... ``Id`` ... ``Sub``` |
| `a06_eq_plus_zero` | `a == (b + 0)` | `... ``Acct`` ... ``Add``` |
| `a07_add_deep_zero` | `a + (b + (0 * 0))` | `... ``Acct`` ... ``Add``` |
| `a08_add_base_plus_one` | `a + (n + 1)`, `n: i64` | `... ``Id`` ... ``Add``` |
| `a09_assign_add_launder` | `a = a + (b + 1)` | `... ``Acct`` ... ``Add``` |
| `a20_nested_parens_only` | `((a)) + ((b))` | `... ``Id`` ... ``Add``` |
| `a21_div_cross` | `a / b` | `... ``Id`` ... ``Div``` |
| `a25_lt_cross` | `a < b` | `... ``Id`` ... ``Ord``` |
| `a26_ne_cross` | `a != b` | `... ``Id`` ... ``Eq``` |

`a02` and `a07` are the specific rows the reverted patch broke, because
`literal_defaulted`'s `Binary` arm returns true if **either** side is defaulted,
recursively, so any operand subtree containing one integer literal exempted the whole
operand. Any operand-based rule proposed in the design phase must be run against
`a02`–`a09` **before** it is written into the compiler.

## R2 — a distinct mixed with its own bare base

| program | construct | HEAD message |
| --- | --- | --- |
| `a16_add_distinct_and_base` | `a + n`, `n: i64` | `... ``Id`` ... ``Add``` |
| `a18_add_literal_right` | `a + 1` | `... ``Id`` ... ``Add``` |
| `a30_str_concat_cross` | `p + q` over two distinct-`str` | `... ``P`` ... ``Add``` |
| `f02_over_f64_arith` | `m + 1.0`, `m: distinct = f64` | `... ``Metres`` ... ``Add``` |
| `f03_over_u8_arith` | `b + 1`, `b: distinct = u8` | `... ``Byte`` ... ``Add``` |

**These five are the rows a design most wants to relinquish**, because they are the
ergonomics complaint: `a + 1` on a `distinct Id = i64` is the thing users expect to
work. Relinquishing them may be the *right call* — but it is a **deliberate
semantics decision that must be argued explicitly, named in the design, and paired
with a demonstration that R1 still holds**. It may not happen as a silent
side-effect of a broader operand rule, which is exactly the mechanism by which R1
was lost last time.

## R3 — position checks (`distinct_mismatch`)

These are the rule that actually buys `distinct` its safety, and none of them are
negotiable.

| program | position | HEAD message |
| --- | --- | --- |
| `b01_let_init_base_literal` | `var x: Id = 5` | ``expected `Id`, found `i32` — `distinct` types need an explicit `as` `` |
| `b02_let_init_distinct_to_base` | `let y: i64 = a` | ``expected `i64`, found `Id` — ...`` |
| `b03_let_init_other_distinct` | `let x: Id = b` | ``expected `Id`, found `Acct` — ...`` |
| `b07_arg_base_for_distinct` | `takes_id(n)` | ``argument `x` of `takes_id`: expected `Id`, found `i64` — ...`` |
| `b08_arg_other_distinct` | `takes_id(b)` | ``argument `x` of `takes_id`: expected `Id`, found `Acct` — ...`` |
| `b09_arg_distinct_for_base` | `takes_i64(a)` | ``argument `x` of `takes_i64`: expected `i64`, found `Id` — ...`` |
| `b10_return_base_for_distinct` | `fn -> Id { return 5 }` | ``return: expected `Id`, found `i32` — ...`` |
| `b11_return_distinct_for_base` | `fn -> i64 { return a }` | ``return: expected `i64`, found `Id` — ...`` |
| `b17_var_init_no_annotation` | inferred `let b = 2 as Acct`, then `takes_id(b)` | ``argument `x` ...: expected `Id`, found `Acct` — ...`` |
| `f11_distinct_over_str_to_str_fn` | `width(p)`, `p: distinct = str` | ``argument `s` of `width`: expected `str`, found `P` — ...`` |
| `f12_str_fn_returns_base_into_distinct` | `let tail: P = <str>` | ``expected `P`, found `str` — ...`` |
| `f15_distinct_pub_module_no_casts` | `fn path_of(s: str) -> Path { return s }` | ``return: expected `Path`, found `str` — ...`` |
| `g08_two_str_distincts_cross_arg` | `takes_p(q)`, two distinct-`str` | ``argument `p` of `takes_p`: expected `P`, found `Q` — ...`` |

`b08`, `b03`, `g08`, and `b17` are the **load-bearing rows**: two unrelated id
spaces over the same base, kept apart. If a design lets any of these through, it has
deleted the feature and kept the syntax.

`f11`, `f12`, and `f15` are the rows the `Path` work is explicitly trying to make
unnecessary at internal call sites. They may be *relaxed by an inheritance rule that
is argued for* — but only at the base↔distinct boundary, and **never in a way that
also relaxes `b03`/`b08`/`g08`**, which are distinct↔distinct. A rule that cannot
tell those two cases apart is not a rule.

## R4 — same-type operators (refusals that are also bugs)

| program | construct | HEAD message |
| --- | --- | --- |
| `e01_eq_same_distinct` | `a == a2`, both `Id` | `type ``Id`` does not implement ``Eq``` |
| `e02_ne_same_distinct` | `a != a2` | `... ``Eq``` |
| `e03_lt_same_distinct` | `a < a2` | `... ``Ord``` |
| `e04_eq_same_distinct_str` | `p == q`, both `P` | `type ``P`` does not implement ``Eq``` |
| `e07_loop_cond_distinct` | `for a < 3 as Id` | `... ``Ord``` |
| `a27_add_same_distinct` | `a + a2`, both `Id` | `... ``Add``` |

**This block is different from R1–R3 and must be treated differently.** These are
refusals of programs that are *semantically fine* — one id space, one operation. A
design that makes them compile is an **improvement**, and the design phase should say
so out loud and record the flip.

They are listed here anyway for one reason: they are the rows most likely to be
turned on by a broad "inherit the base's operators" rule, and if that rule is written
without a same-type/foreign-type distinction it will turn on R1 at the same time.
**Flipping R4 is only acceptable in a change that demonstrates R1 unchanged**, by
replaying this corpus and diffing against the baseline.

## Positive controls that must keep passing

A refusal set with no positive controls proves only that the compiler can say no.
Each of these is `RUN_OK` at HEAD and must stay `RUN_OK`, with the same output:

| program | output | pairs with |
| --- | --- | --- |
| `a28_cast_then_add_control` | `3` | `a01` |
| `b16_positions_all_cast_control` | `7 5` | `b01`, `b07`, `b10` |
| `d12_builtin_cast_control` | `2 el` | `d01`, `d02` |
| `e05_eq_via_cast_control` | `true` | `e01` |
| `e08_eq_impl_control` | `true` | `e01` (a hand-written `impl Eq` on a distinct) |
| `c04_str_range_slice_control` | `he` | `c03` |
| `g04_cast_str_literal_to_distinct` | `hello` | `g01` |
| `g05_cast_i64_var_to_distinct` | `5` | `g01` |
| `g09`/`g13`/`g11` | see baseline | isolate *which* cast position breaks |

`g14_cast_str_to_str_in_expr_control` is the anti-vacuity control for the
`conversion to non-scalar type requested` family: it has no `distinct` in it, so if a
change "fixes" `g07` while leaving `g14` broken, the fix was in the wrong place.
