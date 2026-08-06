# Error payloads — design note

> Status: **design, pre-implementation.** This note makes the decisions the handoff
> asked for — surface syntax, result-struct layout, `catch |e|` binding semantics —
> and lays out the increment chain. Nothing here is built yet; when a decision below
> is implemented, this note gains a ✅ and the increment's commit, exactly as the
> unsafe contract (`docs/unsafe-contract.md`) recorded its ladder.

## 1. Where we are (precise, verified against the tree)

Today an error is **a whole-program integer tag and nothing else**:

* A declaration site lists bare names: `-> i32 !{ Io, Parse }`. Every error name in
  the program maps to one integer (`error_tags` in cgen.rs, first-declaration order,
  `or_insert` so the same name is the same tag everywhere — free functions and struct
  methods share the map; trait impls are refused).
* The runtime shape is `typedef struct { bool is_err; T ok; int err; } JestyrResult_<ok>`
  — **keyed by ok type only**, shared by every fallible function returning `T`
  regardless of its error set.
* `Ty::Result(ok)` carries **no error set**. Sets are declaration-side decoration:
  `err(E)` is not checked against the enclosing set, and `?` never checks that the
  caller's set includes the callee's. A `!{ A }` function can `?` a `!{ B }` callee
  and the tag flows through silently. This is benign *today* because every consumer
  (`catch`, `unwrap`, `is_err`, traces) is set-agnostic — no construct discriminates.
* `catch |e|` binds an **opaque `error`** (port prim code 20; runtime: the `int err`
  tag as a `const int`). Its only sanctioned consumers: `e as i64` (the tag) and
  `return e` (the rethrow form, `?` spelled out).
* Error traces (`--error-traces`) record file/line per hop in a fixed buffer; they
  never touch the error's value.

The design doc (§6.2) always planned past this: *"Error sets are integer-tag-sized
**unless they carry payloads**; no heap allocation, no dynamic dispatch."* This note
is the shape of that "unless".

## 2. Goals and non-goals

**Goals.** An error can carry a value (which byte failed to parse, which line, which
path) with: no heap allocation, no dynamic dispatch, deterministic emitted C, and
byte-identical emission for every program that uses no payloads (the standing gating
rule — corpus 146, the concat, the fixpoint and the seed must not move until the
first payload-carrying corpus file lands *with* its port mirror).

**Non-goals (v1).** Owning payloads (`String`, anything with `drop`); aggregate
payloads (struct/enum); payload-polymorphic helpers; named error sets
(`error FsError = { … }` is its own later item); error sets in trait signatures
(unblocked by this design, built separately — see §9).

## 3. The three decisions

### D1. A payload is a property of the error NAME, whole-program

`Parse(i64)` declared anywhere means `Parse` carries an `i64` everywhere. Declaring
`Parse` bare in one set and `Parse(i64)` in another — or `Parse(str)` elsewhere — is
a **compile error** naming both sites.

*Why.* This is the existing tag rule ("an error name means one integer everywhere")
extended to the payload. It is what keeps `?` conversion-free (§D3), it means a
`match` over an error can bind the payload knowing its type from the name alone, and
it makes **trait error sets orthogonal**: a trait signature that later declares
`!{ Io, Parse }` needs no payload syntax of its own, because payload-ness travels
with the name (§9).

*Cost, accepted.* Every declaring site must restate the payload type — `!{ Parse(i64) }`
at each function that can produce or propagate it. That is deliberate: signatures stay
self-documenting (doc/attest render the set from the signature; a reader of one
function sees the full contract), and agreement is checked, not inferred.

### D2. One payload per name, from a restricted type domain (v1)

A name carries **zero or one** payload value. v1 payload types: the integer
primitives, `bool`, `char`, and `str`. Not in v1: floats are *allowed* (no
determinism concern — a payload is stored and re-read, never computed on), but
aggregates, owning types, and references are **refused with the reason**:

* **No owning payloads**: a `String` payload would owe a `drop` on every path an
  error can die on — the `catch` fallback path, the unwrap-abort path, and every `?`
  hop that is *not* taken. Those are exactly the paths where leaks hide. Deferred
  until wanted, with the drop obligation designed first (the B1 recursion machinery
  is the starting point).
* **No aggregate payloads**: keeps the payload union (§D3) small and keeps v1's
  `match` binder a scalar. "Use several scalars? — you want a struct" is real, and it
  is the first thing to revisit; it costs union width and nothing conceptual.
* **`str` is in** because a message/path/slice-name is the single most useful payload
  and it is two words, non-owning. It drags in the escape question, answered in §7.

### D3. Result-struct layout: ONE whole-program payload union, gated on use

When (and only when) at least one error name in the program carries a payload, the
emission changes, program-wide:

```c
/* emitted once, before the result typedefs, iff the program has payload errors */
typedef union { int64_t Parse; JestyrStr BadKey; } JestyrErrPay;

/* every result struct in a payload-using program gains the field;
   the mangle and the keying (ok type only) DO NOT change */
typedef struct { bool is_err; int32_t ok; int err; JestyrErrPay pay; } JestyrResult_i32;
```

* `err(Parse(n))` → `(R){ .is_err = true, .err = TAG, .pay = { .Parse = (n) } }`.
* `err(Io)` (a bare name, in a payload-using program) → `.pay = {0}` — explicit,
  so no indeterminate bytes ever flow through a hop. Deterministic emission is a
  language value; "uninitialized but unobservable" is a class of bug we refuse to
  create.
* `?` and `catch |e| return e` copy the union blind:
  `return (Caller){ .is_err = true, .err = _t.err, .pay = _t.pay };` — a C union
  assignment copies the object representation, so the hop needs **no knowledge of
  which member is live**. This is the decision's whole payoff.

*The alternative, considered and rejected: per-set unions* (result structs keyed by
ok type × error set). Tighter storage, but every `?` hop that crosses from a smaller
set to a larger one crosses two different C union types, and copying between them
member-wise requires a runtime `switch` on the tag at **every hop**. It also multiplies
result typedefs and forces the set into the type *mangle* on both sides. The
whole-program union keeps today's keying, today's hop shape, and today's sharing;
its cost is that every fallible function in a payload-using program pays
`max(payload sizes)` — bounded in v1 at 16 bytes (`str`), because of D2.

*Also rejected: a universal `int64_t` payload slot* (no union, no types). It would
dodge the union entirely but caps payloads at one integer forever, and `str` — the
payload most worth having — doesn't fit. The union is barely more machinery.

*Gating.* A program with no payload-carrying name emits **today's C exactly** —
no union typedef, no `.pay` field, no `.pay` copies in hops. This is the
`uses_try_read` pattern: the gate is "any declared error name carries a payload",
computed where `error_tags` is built. All 146 corpus files are non-users until the
feature's own corpus file lands.

## 4. Surface syntax

Declaration — a name optionally applied to one type, matching enum-variant idiom
(`circle(r: f64)` — the parenthesized-payload shape readers already know):

```jestyr
fn parse_port(s: str) -> i32 !{ Empty, BadDigit(char), TooBig(i64) }
```

Creation — `err` takes the applied form; arity is checked against the declaration:

```jestyr
return err(BadDigit(c))     // payload name: applied to exactly one value
return err(Empty)           // bare name: exactly as today
```

`err(BadDigit)` (missing payload) and `err(Empty(3))` (payload on a bare name) are
compile errors naming the declaration site. Inside `err(…)`, the head identifier
resolves in the **error-name namespace first** — an error name shadows a like-named
function/const in this one position (today's `err(Io)` already reads `Io` as an
error name, not a value; the rule just becomes explicit when the name is applied).

Extraction — through `match` on the bound error, and only there (§5):

```jestyr
let port = parse_port(arg) catch |e| match e {
    Empty        => 80,
    BadDigit(c)  => { log_char(c) 0 - 1 },
    TooBig(n)    => { log_int(n) 65535 },
}
```

## 5. `catch |e|` semantics (unchanged core, one new consumer)

* `e` **stays opaque** (`error`). Payloads add no implicit accessors — no `e.payload`,
  no field syntax. The opacity rule exists so a tag can never masquerade as a success
  value; a payload accessor on an undiscriminated error would be worse (which member?).
* `e as i64` still yields the tag. Unchanged, still the sanctioned escape hatch.
* `catch |e| return e` still rethrows tag **and now payload** — it remains exactly
  `?` spelled out (§D3's blind union copy).
* **`match e { … }` is the one payload extractor.** Arms are error names; a
  payload-carrying name binds its value with the declared type (`BadDigit(c)` gives
  `c: char`); `_` or a binding is the catch-all. Runtime lowering reuses the scalar
  `match` machinery — a switch on the tag — with the binder reading `_e_pay.Name`.
  The binder lowering grows from one `const int` to a pair (`_e_tag`, `_e_pay`);
  no C struct type for `error` is materialized, since `match`/`as`/`return e` are
  its only consumers and each wants one half.
* **Exhaustiveness requires a static set** — which is what forces §6. A `match e`
  must cover the set of `e`, and `e`'s set is the base expression's set, which today
  the type system does not carry.

*Rejected for v1: `match` directly over the result* (`match f() { ok(v) => …,
err(Parse(n)) => … }` — the design doc §6.3 sketch). It is strictly more surface
(ok-patterns, err-patterns, interaction with every existing match feature) and
everything it expresses is reachable as `f() catch |e| match e { … }` composed from
pieces that already exist. Worth revisiting as sugar once payload-match is proven;
building it first would couple two features' risks.

## 6. The prerequisite: error sets must become SOUND

Payload extraction is the first construct that *discriminates* errors, so it is the
first construct that can be **wrong** when the static set lies. Before any payload
lands, sets get teeth, in the census-then-enforce shape the unsafe ladder proved:

1. **Census.** ✅ **DONE** — `src/errsets.rs` + `jestyrc errsets <file>`, pinned by
   `error_set_census_is_clean_over_the_corpus`. **The result: 10 obligation sites
   across 146 files, ZERO violations** — so E3's enforcement is a no-migration
   diagnostic and can land strict from day one (where the unsafe ladder first needed
   a 40-site migration). Two honest unresolveds, both pinned: `vec.jtr` (a
   lexer-only fixture calling an undeclared method) and `combinators.jtr` (an `err`
   variant of the *imported* `core.Result`, which a single-file census refuses to
   guess at). Three census findings a successor should know:
   * The first sweep reported 17 violations and **all 17 were census model errors**:
     `err` is SHADOWED by a user enum variant of that name (the corpus's own
     `Result(T, E) { ok, err }` — cgen resolves variants before intrinsics), and
     methods also live inside `struct { … }` EXPRESSIONS (the comptime-generic
     factory idiom of `vec.jtr`/`method_errors.jtr`), invisible to an item-level
     scan. E2's typeck-side enforcement inherits both rules.
   * The rethrow form (`catch |e| return e`) carries exactly `?`'s obligation and
     must be counted/enforced with it — a `?`-only check under-counts.
   * **The intrinsic tag-1 wart:** `try_read_file`/`try_from_utf8` hard-code error
     tag `1`, and user tags also start at 1 — so in any program that declares an
     error set, the intrinsics' `IoError` aliases the first user-declared name.
     Unobservable today; observable the day `match e` lands. E3/E4 must either
     reserve tag space for intrinsic errors or give `IoError` a real entry in
     `error_tags`.
2. **Typed sets.** ✅ **DONE (with step 3 in one increment)** — `Ty::Result(ok)`
   became `Ty::Result(ok, errs)` (`errs`: the sorted, deduped name list), threaded
   through `FnSig` (`fallible: bool` → `errs: Option<Vec<String>>`, one source of
   truth), every call path (free, generic free-method, struct/factory method,
   module-qualified), the intrinsics (`try_read_file`/`try_from_utf8` carry
   `{ IoError }`), unify (oks only — sets never constrain inference) and both
   subst functions (preserved). **The display did NOT change** — `display()` still
   renders `T!`, because the P3 typeck golden compares type *renderings* against
   the port corpus-wide; carry the set, don't show it. Verified, not argued: the
   P3 golden ran green with the set carried, along with every other
   `matches_reference` golden — zero recorded-type drift, zero emission drift.
3. **Enforcement.** ✅ **DONE** — strict from day one, as the census licensed:
   `err(E)` membership (checked in the Call arm AFTER variant resolution, so the
   corpus's `Result(T, E) { ok, err }` variant still shadows the constructor);
   `?` inclusion via `check_propagation` at the Try arm — **through bindings**,
   which the syntactic census could not do (`let r = f() … r?` knows its origin's
   set because the set rides the type); and the rethrow `catch |e| return e`
   sharing the same helper. When the enclosing fn declares no set, nothing is
   reported (that is `?`-outside-fallible's own diagnostic; no double-reporting).
   Exhaustiveness over the base's set arrives with `match e` (E4). Six unit
   tests; 894 default green. What E2 deliberately did NOT do: type `err(…)`/
   `ok(…)` calls (they stay `Unknown`, exactly as before — an arm that adds a
   diagnostic but never a type is what keeps the P3 golden still).

**On set inference — a recorded tension with the design doc, resolved here.** Design
§6.2 says sets "*infer and compose — you rarely write them out*". As implemented, a
function is fallible **iff** it writes `!{ … }`, and this note's enforcement treats a
written set as a **contract**: `?` requires callee ⊆ caller, explicitly. That is the
right v1 rule for the same reason `@span` and `@no_alloc` are checked declarations —
attest and doc render the set from the signature, so an inferred set would put
*computed* facts into an *attested* contract the author never wrote. Inference stays
compatible as a later convenience (the inclusion checker computes the propagated
union anyway, so an elided-set spelling like `-> T !` could be filled from it and
rendered explicitly by doc/attest), but it is a separate decision with its own
attest-stability question — deferred, not forgotten.

The port note for step 2: the port's type arena is integer-tagged `TyData` rows with
two operand slots; a set does not fit a slot. The plan is a side arena (sorted spans
into a set table, the same shape the parser's error-set storage already has) keyed
by the TyData row — the port carries sets beside its types, not inside them. This is
the port's hardest piece and it is bounded: only `Result` rows have entries.

## 7. Escape analysis: a `str` payload is a return-position borrow

`err(BadKey(s))` in a fallible function **returns `s`** for escape purposes — the
payload rides the result out of the frame. The existing return-borrow rules then do
the work: a `str` derived from a local is refused (it would dangle by the time the
caller matches on it), a parameter's or a static's is fine. Concretely:

* `escape.rs` and the port's `escape.jtr` each owe an arm: `err(Name(p))` with a
  borrow-typed `p` is a return of `p`. The `catch` increment's lesson applies
  verbatim — **grep `ExprKind::Try` for every walker/collector and give each one the
  new arm** (structs/moves/refs/closures/calls collectors; a payload expression that
  allocates a closure or names a generic must not be invisible to them).
* Propagation (`?`) needs no new rule: the payload is already "returned" at its
  origin; a hop copies a value the origin was already checked for.
* Scalars (`i64`, `char`, …) borrow nothing and need nothing.

## 8. What does NOT move

* **Traces** (`--error-traces`): the buffer records file/line, never values. A
  payload does not enter the trace; the surfacing print does not render it (printing
  a payload needs to know its type at the unwrap site — a `match` already does that
  better). Zero interaction.
* **`ok`/`is_err`/`unwrap`**: unchanged. Unwrap-on-error aborts exactly as today,
  payload ignored.
* **Faults vs errors**: untouched. Payloads are for the recoverable half only.
* **The tag domain**: `error_tags` numbering is unchanged, so a payload-free
  program's tags — and its emitted C — are byte-identical.
* **Attest/doc**: `fn_sig`/`at_*` render the set from the AST; they gain the
  `Name(T)` rendering. Payload-free signatures render identically (gated on use);
  the first payload corpus file exercises both renderers via the existing goldens.

## 9. What this unlocks, deliberately not built here

* **Error sets in trait signatures** (handoff item 2). The refusal sites to lift are
  marked in typeck.rs (impl registration) and cgen.rs (`emit_impl_method_decl`'s
  backstop). D1 means a trait's `!{ … }` lists names exactly as a function's does —
  no payload syntax needed in the trait grammar. The impl-conformance rule will be
  set inclusion (impl's set ⊆ trait's declared set), which is §6's machinery reused.
  Build it after E1/E2 below; it shares the typed-set prerequisite and none of the
  union layout.
* **Named sets** (`error FsError = { NotFound, Permission(str) }`, design §6.2): an
  item form that *names* a set; sets stay structural underneath. Orthogonal to
  payloads (D1 makes membership-by-name carry payload-ness); wants its own note
  mostly for the module/namespace questions.
* **Match-over-result sugar** (§5's rejected form): revisit once payload-match has
  corpus mileage.

## 10. Increment chain (each lands alone, full gate each time)

| # | Increment | Emission change? | Port mirror due? |
|---|---|---|---|
| E1 | ✅ Set census (`jestyrc errsets`) + the corpus audit — **zero violations, enforcement needs no migration** | none | no |
| E2 | ✅ Typed sets (`Ty::Result(ok, errs)`, display unchanged) + strict `err`∈set and `?`-inclusion diagnostics — **all goldens green, zero drift** | none | not until a corpus file needs the set (E4) — display-dodge holds the P3 golden still |
| E3 | ✅ Payload declaration/creation/propagation, reference side: parser (`Name(T)` in sets), D1 agreement check, the gated union + `.pay` emission, `err(Name(v))`, blind-copy hops, escape arms — **all landed; corpus/concat/test-mode/fixpoint/seed all byte-identical, so the gate held** | **yes, gated on use** — zero corpus files use it, so all goldens stay green | not yet (the standing trigger: no corpus file until the mirror) |
| E4 | `catch |e| match e { … }` + exhaustiveness, reference side | gated on use | not yet |
| E5 | **The port mirror + the first corpus file** (`examples/error_payload.jtr`): parser.jtr / typeck.jtr (side-arena sets) / cgen.jtr / escape.jtr arms, attest+doc renderers, corpus 147, `REFRESH_SEED=1` | the corpus file's own C | **yes — all of it, one increment**, as `catch`'s mirror landed |
| E6+ | Trait error sets; named sets; owning payloads (drop design first) | later | later |

**What E3's landing taught (recorded for E4/E5):**

* **The escape rule cost zero new diagnostics.** `err(Name(p))` walks `p` in
  RETURN position, and the existing return rules do everything: a region-allocated
  `str` payload is refused with the verbatim region-return message, while a plain
  `str` parameter payload passes — correctly, since a view of caller-owned data
  survives the return. §7's wording held as written. The arm fires only for the
  unshadowed constructor with a declared payload name, so the corpus's own
  `Result(T, E) { ok, err }` variant is untouched (P4 golden unchanged).
* **The P2 reference dump hid behind a feature gate.** `ref_dump`'s `errname`
  records (proptests.rs, `c-oracle` only) needed the `ErrName` field path fix, and
  a plain `cargo test` could not see it — **an AST-shape change is not
  compile-clean until the feature builds compile too.** The dump still records the
  NAME span only, deliberately: no corpus file declares a payload, so the port's
  dump needs no new field until E5, where both dumps grow the payload type
  together.
* **The intrinsics compose with a future `IoError(T)`.** `try_read_file` /
  `try_from_utf8` construct errors inline; under the gate they carry an explicit
  `.pay = {0}`, so no indeterminate bytes can flow through a blind hop copy even
  if a user declares a payload on the intrinsic's tag-1-aliased name.
* **After a D1 conflict, the FIRST declaration wins the payload map**, so
  subsequent uses are checked against it deterministically — a conflicting
  program gets the two located conflict diagnostics plus honest downstream
  checking, not a cascade of arbitrary verdicts.
* **Behavioral inertness is pinned by running**: the c-oracle test proves the
  gated C compiles under the locked flags and that every observable answer equals
  the tag-only world's — payloads ride along; nothing changes until E4 reads them.

Test plan highlights (beyond the standing gate): a run-based two-hop propagation test
that matches at the top and prints the payload (proves the blind union copy end to
end); a bare-`err`-in-payload-program test pinning `.pay = {0}`; the D1 conflict
diagnostic with both sites named; the escape refusal for a local-`str` payload; a
`catch |e| return e` payload-preservation run; and an **absence** test pinning that a
payload-free program emits no `JestyrErrPay` anywhere (the `jestyr_et_` pattern — one
stray token is a corpus-wide diff waiting to happen).

## 11. Decisions in one breath

A payload belongs to the error **name**, program-wide, restated and checked at every
declaring site (D1). One value per name, scalars + `str`, nothing owning (D2). One
whole-program payload union, result keying unchanged, hops copy it blind, all of it
gated on use (D3). `err(Name(v))` creates; `catch |e| match e` is the only extractor,
exhaustive over a static set; `e` stays opaque otherwise. Sets become sound first —
census, then typed-but-not-displayed, then enforced — because extraction is the first
construct a lying set can break.
