# Jestyr — next-frontier handoff (post-self-hosting): G/L/Q

## ▶ START HERE — state of `master` (`fddf02a`), and the next move

**Every ladder this note ever tracked is CLOSED.** Group 3 (G CTFE tiers 0–7, L layout,
Q SIMD — all both sides), the place-lowering defect class (X1/X2), and the entire V1
tier list: transitive `@no_alloc`, diagnostic JSON, suggested rewrites, the `@verified`
planning slice, the **whole error ladder** (sets → `?` → `catch` → `catch |e|` → debug
traces → **fallible methods**, each on both sides, corpus 146), and the **whole unsafe
ladder** (report → contract → migration → warning → **compile error** on both
toolchains).

**Gate status on `master`:** 876 default tests green, warning-clean; P5 corpus (146
byte-identical) + concat + test-mode; P2/P3/P4 goldens; `selfhost_fixpoint_full`;
`jestyr_driver_builds_itself`; seed current.

**What remains is post-v1-shaped — design items, not gaps.** In rough value order:

1. **Richer error payloads.** Today an error is an integer tag; the design wants
   payload-carrying errors. **The design note is WRITTEN — `docs/error-payloads.md`**
   — with the three decisions (payload is a property of the NAME, whole-program;
   one program-wide payload union so `?` hops copy it blind, gated on use;
   `catch |e| match e { … }` as the only extractor) and the E1–E6 increment chain.
   **Start at E1: the set-soundness census** — `err(E)`∈set and `?`-inclusion are
   UNCHECKED today (`Ty::Result` carries no set), and extraction is the first
   construct a lying set can break, so sets get teeth before any payload lands.
   Touches: parser/typeck/cgen on both sides + the seed (E5 is the mirror increment).
2. **Error sets in TRAIT signatures** — unlocks fallible trait-impl methods, which are
   currently refused at check time with the reason (calls are typed by the trait's
   signature, which cannot declare an error set). The refusal sites to lift are marked:
   `typeck.rs` (the impl-registration loop) and `emit_impl_method_decl`'s backstop.
3. **The `#line` port + module-loader unification.** The module-path golden
   (`jestyr_module_cgen_matches_reference_except_line_directives`) pins THREE
   divergences, all from the two loaders producing different merged buffers: `#line`
   directives (reference-only), per-type artifact order, and offset-derived spawn
   symbol names (constant 78-byte skew). Unify the loaders first (that fixes 2 and 3),
   then port `mark_line`, then tighten the golden in its three recorded steps.
4. **Errors in more positions** (error-set syntax beyond `fn` returns) — smallest.

**Standing rules that don't expire:** the two-sided tax (any emitted-C change needs the
port mirror + corpus + REFRESH_SEED in the same increment); check `master` before
building anything; the `.jtr` subset traps listed in §0 — including `fn take(…)` in a
fixture (a conveyance keyword; its 14-diagnostic cascade has now cost TWO sessions).

**What closed most recently** (details in the items below and `docs/error-handling.md`):

1. ~~**The `catch` port mirror**~~ — **DONE** *(this session)*: both sides, corpus
   **145** (`examples/error_catch.jtr`), seed refreshed. See item 1 below for the
   collector-arm lesson it surfaced.
2. ~~**Error traces** (Error tier 4)~~ — **DONE** *(this session)*: `--error-traces` on
   build/run/emit-c. `err` = origin (reset + push), each `?` = hop, unwrap-on-error =
   the stderr print. Opt-in per invocation → zero emission change for non-users (pinned
   as an *absence* test — one stray `jestyr_et_` in plain emission is a corpus-wide
   diff), stdout byte-identical even when a trace fires, no port mirror due. The
   near-miss worth remembering: the first draft added a brace to the **flag-off** `?`
   string ("just one redundant brace") — that alone would have diffed every fallible
   corpus file against the port. See `docs/error-handling.md`.
3. ~~**Unsafe/provenance v2**~~ — **THE WHOLE LADDER IS DONE** *(this session, five
   rungs)*: report → contract → migration → warning → **ERROR**. A raw-pointer deref,
   pointer arithmetic, or an int-to-pointer cast outside `unsafe` is now a **compile
   error** on both toolchains (`jc build` refuses on any escape diagnostic, so error
   is the severity where the drivers agree). `docs/unsafe-contract.md` carries the
   contract and the completed plan. Facts a successor needs:
   * The census moved twice before the migration, both times from *classifier*
     corrections: casts with an untypable operand are NOT int-to-ptr (`alloc(…) as
     *mut T` reuses provenance), and `spawn`/`await`/f-string operands were unwalked.
     Final: **171 sites, all covered**, pinned at zero uncovered by
     `unsafe_census_is_total_over_the_corpus`.
   * An `unsafe` wrap is **not** always C-invariant (statement position emits a scope
     block) — the full golden gate ran for the migration, and must for any re-wrap.
   * The port mirror (`unsafe_boundary` in `escape.jtr`) is a **flat arena scan with
     span containment**, not a walk mirror; both sides sort by span start, and that
     shared sort is the equality contract. Its own two-sided probe
     (`jestyr_escape_unsafe_warnings_match_reference`) exists because the migrated
     corpus emits zero diagnostics and cannot distinguish a working mirror from a
     missing one.
   * Comptime bodies are excluded on both sides (the interpreter has no pointers);
     closure bodies are included (runtime code like any other).
4. ~~**`catch |e|`**~~ — **DONE, BOTH SIDES**: the binder carries an **opaque `error`
   type** (typing it `i32` would let `catch |e| e` silently return the tag as a
   success value — refused, with `e as i64` as the sanctioned escape hatch), and
   `catch |e| return e` is `?` spelled out (same lowering, tag preserved, fallible-fn
   requirement). A `|` after `catch` is always the binder; a closure fallback needs
   parens. **The port mirror** landed with the first `|e|` corpus use
   (`error_catch.jtr`): binder span + rethrow flag on kind 45 in parser.jtr, the
   `error` prim (code 20) + a pushed-and-popped binder scope in typeck.jtr, and all
   three lowerings in cgen.jtr — byte-identical on the first P5 run. Two things worth
   carrying: the P2 dump identifies the binder by its **span**, the `field` idiom
   (a text push would need a String on one side and a source slice on the other); and
   the P3 golden caught a **reference** bug — the typeck arm's early `return`s
   bypassed `set()`, so a `catch` node's recorded type stayed `Unknown` while the port
   recorded faithfully. The rare divergence where the port was right; the rule it
   yields: **no early `return` in an `infer` arm — every exit goes through `set`**.
5. **Fallible METHODS, both sides** (`fddf02a`, corpus **146** —
   `examples/method_errors.jtr`): a struct method's `-> T !{ E }` returns its tagged
   result; a generic struct's method gets one result typedef per instantiation,
   lowered through the instance's substitution; `cur_result`/`res_ok` set during the
   body is all `ok`/`err`/`?` ever consult. Fallible **trait-impl** methods are a
   check-time error with the reason (see open item 2 above). **The mirror's finding,
   worth more than the feature:** the reference's `method_instances` is ONE LIFO
   worklist for plain and generic methods alike — three plain methods called
   `first/second/third` emit `third/second/first` — and the port's flat first-seen
   scan of plain methods was a **latent order divergence** that passed the golden for
   the project's whole life because no corpus file ever had two plain-method
   instances. Plain methods now ride the same worklist as generic ones (argc-0
   records whose trailing slot holds the STRUCT item; `arm_gmi_su`'s argc-0 guard
   matters — a struct item's `(a, b)` are member-array coordinates, and reading them
   as param 7-tuples indexes the arena with garbage). The whole corpus stayed
   byte-identical under the rerouting. Port helpers worth knowing:
   `push_su_result_mangle` (subst-aware ok-type mangle, shared by signature and
   typedef scan so they cannot disagree) and `emit_result_name_plain` (the no-subst
   form for sig emitters without a Checker in scope).

**Do NOT start an SMT backend.** The planning slice measured it: **7 declared
obligations across 144 corpus files**, so a solver would have nothing to discharge. The
prerequisite for `@verified` is writing contracts. (`jestyrc obligations <file>`.)

**Three known gaps to be aware of before you plan anything:**

| Gap | Where |
|---|---|
| The **module path** diverges from the port in **three** ways (`#line`, artifact order, offset-derived spawn symbol names) — all from the two loaders building different merged buffers | §1 + the `jestyr_module_cgen_matches_reference_except_line_directives` golden, which pins them |
| **Transitive `@no_alloc` sees free functions only** — not methods, closures, or `fn(…)` pointers | `docs/attributes.md` |
| `emit_dyn_coercion` still takes `&({…})` of a statement expression — the **last** site of the X1/X2 defect class | X2's ledger row |

**Do this first, whatever you build:** `git log --oneline -15 origin/master`. Two
sessions have now independently built the same fix twice (see the Integration note at
the end of §5), and workstream Q discarded duplicate work three times.

---

> **Cold-start orientation.** Two whole workstreams are finished and on `master`
> (`ebf8397`, **757 tests green** with `--features selfhost-fixpoint`, 700 in the
> toolchain-free default suite):
>
> * **P (self-hosting) + its productization arc** — the fixed point (jc2 ≡ jc1 on the
>   compiler's own C, all modes), the driver, in-language modules (the `ml_*` flatten
>   loader — **jc compiles itself from its real multi-file sources**), exact per-file
>   diagnostics, parse/typeck/escape refusal gates, and the committed **bootstrap seed**
>   (`bootstrap/` — building Jestyr from scratch needs only a C compiler, never Rust).
> * **O (tooling) — now COMPLETE IN-LANGUAGE**: the ported compiler emits the full
>   attestation manifest, the breaking-change diff/verify gate, and the documentation
>   page, each byte-for-byte identical to the Rust reference.
>
> **What this note now scopes is only Group 3** — the three Rust-side workstreams
> (G CTFE, L memory-layout, Q SIMD), of which **G increment 1 is done**. Section 2 is
> kept as a record of how O was closed; §3 is the live worklist.
>
> **Read alongside:** `docs/session-notes/jestyr-selfhost-P5-cgen-R2-handoff.md`
> (increments 1–54, the authoritative history + every recorded trap), `ROADMAP.md`
> workstreams G/L/Q (percentages there are stale for F/H — both are DONE — but the
> open-item descriptions are accurate), `MOTLEY.md` (the long game).

---

## Progress ledger (newest first — what landed since this note was written)

| Increment | Commit | What |
|---|---|---|
| **Q-S2e** the `@simd` PROMOTION miscompile — narrow elements vectorize at `int` width | `3a0be59` | **A confirmed silent wrong answer, and the entry above it is what caused it.** `@simd` lowered the vector head to GNU vector types at the SOURCE element width, so it computed elementwise at that width while the scalar remainder of the *same loop* went through C's integer promotions. `@simd` sum-of-squares over `[]i8` of 33 × 100 returned **10596** instead of 108900 — 96 lanes wrapping `33*33` to 65, plus a 4-element tail promoting it to 1089 — with `jestyrc check` clean. **It is not about width-expanding arithmetic:** `~x` over `[]u8` diverged with no arithmetic at all, because promotion zero-extends to `-34` where a `uint8_t` lane gives `222`. So a "whitelist the non-expanding operations" fix would have missed it; the fix has to be at the TYPE. `simd_compute_elem` promotes `i8`/`u8`/`i16`/`u16` → `int32_t` **before** the vector type is chosen, and the head loads lane by lane (a `memcpy` of `sizeof(JestyrVec_i32)` over `[]i8` would reinterpret four source elements per lane, so the widening load is required, not an optimization). Unpromoted elements keep the raw-`memcpy` path, so the corpus, concat, seed and attested hashes were untouched by the fix itself. **The cost is density and it is not optional: a `[]i8` body now gets 8 lanes, not 32 — the same as `i32` — so narrow elements buy load bandwidth, not lanes.** Three things worth carrying forward: (i) **`src/simd.rs` structurally could not have caught this** — it is syntactic by design (attributes validate in the parser, before types exist), so the element type is outside its view, and its `simd_lanes_match_scalar_bit_for_bit` oracle proves the *whitelist* against a hand-written C harness at `i64`, never cgen's actual lowering; **the pass being sound is not the backend being sound**, and the module doc's soundness claim quietly covered only the former. (ii) The old unit test literally asserted `u8 → 32 lanes`, and **the 11-element corpus demo passed under the broken lowering**, because 11 is below the 32 lanes an `int8_t` vector holds — the head processed nothing and the whole loop ran in the scalar tail. Any lane-width regression test must use `N >` the widest lane count a *plausible regression* would pick, not just the correct one; the demo now carries a 100-element `i8` case for exactly that reason. (iii) **The open language question this exposed:** typeck types `i8 * i8` as `i8`, but `let a: i8 = 33; (a * a) as i64` is **1089**, not 65, while `let b: i8 = a * a` truncates to 65 — Jestyr's scalar arithmetic leaks C promotion and has no settled width for narrow types. Q-S2e chose to match the *remainder* (promotion), because "`@simd` changes no answers" is the guarantee. Truly dense narrow SIMD (32 `i8` lanes) requires the other resolution — making `i8 * i8` genuinely `i8` — which would change emitted C for all narrow-integer arithmetic and rewrite the corpus goldens, the concat build, the bootstrap seed and every attested hash in one commit. **Decide that before promising narrow-element density anywhere.** Port mirrored (`sd_is_promoted`/`sd_push_vec_name` in `cgen.jtr` — it carried the identical bug and is the bootstrap compiler), seed regenerated. 840 default / 915 `c-oracle` / 919 `selfhost-fixpoint` green. |
| **X2** the by-ADDRESS half — `mut`/`out` receivers and arguments | *(this run)* | **Closes the sibling gap X1 left open, and it was the one-line mechanism X1 predicted.** A `mut`/`out` parameter and a `mut`/`out self` receiver are passed by address, so `cs[i].bump()` emitted `&({ …; _a->a[_ix]; })` — "lvalue required as unary `&` operand", the *same* defect as `xs[i].f = v` at a different call site. One helper, `emit_addr_arg` = `&(` + `emit_place(id, true)` + `)`, and every conveyance site routes through it: **11 in the reference** (six argument loops, the monomorphized-call loop, the inherent-method and impl-call receivers, and the operator-trait receiver + rhs) and **14 in the port**, which splits its dispatch more finely. Byte-identity is again by construction — `emit_place` falls through to `emit_expr` for anything not reached through a checked index, and `read`/by-value conveyances never take an address at all, which is what a dedicated test now pins. `examples/nested_place.jtr` grew the by-address half rather than spawning a corpus 142: the three call lowerings (inherent `mut self`, trait impl, `mut` parameter) each mutate the same element in sequence — `15, 115, 116` — and the file re-reads **element 0** afterwards, so a lowering that handed over a temporary would print `10, 10, 10` and leave the neighbour check unmoved. **Two things worth carrying:** the port's shape count is not the reference's (14 vs 11) and that is fine — what the mirror contract fixes is the emitted *text* and the temp *numbering*, not the number of arms; and `fn take(…)` in a test fixture cost a cascade of 14 parse errors, because `take` is a **conveyance keyword** — the diagnostic says "expected function name, found `take`", which reads like a parser bug (finding 4's hazard, now recorded with its actual error text). **Still deliberately out of scope, and now the only remaining site of this class:** `emit_dyn_coercion`'s `&({inner})` (cgen.rs, mirrored at cgen.jtr) — coercing `xs[i]` to a `dyn Trait` takes the address of a statement expression the same way. It is a *different* construct (the fat pointer's data pointer, with its own lifetime question about pointing into an array), so it wants its own increment rather than a reflex fix. |
| **X1** places through a checked index — `xs[i].f = v`, `m[i][j]`, **corpus 141** | *(this run)* | **A real miscompile, and the class it belongs to.** A bounds-checked index lowers to a GNU statement expression, which yields a **value** — fine for a read, and wrong in all three *place* positions: the left of `=`, the operand of `&`, and the base of another index. So `xs[i].b = 9` emitted `({ …; _a->a[_ix]; }).j_b = 9` and gcc said "lvalue required as left operand of assignment". The probe found the defect is **wider than the write path**: `m[i][j]` fails to *read* as well, because an array index takes `&base` and `&({ … })` is "lvalue required as unary `&` operand" — and the spilled pointer was `const` on a path that writes. The fix is a second emitter, `emit_place`, agreeing with `emit_expr` everywhere except a checked index, where it yields the element's ADDRESS and derefs it (`(*({ …; &elem; }))`) so a projection chain can continue through it; `write` picks the qualifier, since a read of a `static const` table must keep `const` and a store must not. **Byte-identity is by construction, not by luck** — `emit_place` falls through to `emit_expr` for every non-index form, and a *directly* indexed target (`xs[i] = v`) deliberately keeps its existing lvalue lowering, so all 137 allowlisted files were unchanged on the first run. `examples/nested_place.jtr` is corpus **141**, and it is written so a write that lands in a *copy* still fails: it reads back a **neighbour** (`xs[0]` right after writing `xs[1]`), which is the only way to tell "wrote the array" from "wrote a copy of one element" — both print `9` for the element itself. **The mirror surfaced a dormant divergence worth remembering:** the reference emits an array index's base and index into temps *before* allocating its own `_a{n}`, while the port allocated `n` first. Those orders agree only while no base or index allocates a temp — true of every corpus file until `m[i][j]` existed. The port's array-read branch had to be restructured to buffer-first; the same latent ordering is still present in the port's *slice*-read branch (unreachable today, recorded in §3 as the next trivial increment). **Known remaining gap, same root cause, different call site:** a `mut`/`out` receiver or argument is emitted as `&({e})`, so `cs[i].method()` with a `mut self` is still "lvalue required as unary `&` operand" — the ready-made fix is to route those through `emit_place(a, true)`; left out to keep this increment to one construct. **— CLOSED by X2 (the row above), which found 11 such sites in the reference, not nine.** **One deliberate non-mirror**, so nobody reads it as a missed one: the reference's `emit_place` Field arm also excludes `info.qualified` (a `mem.PAGE` module const), and the port has no counterpart because it has no `qualified` map at all — the in-language loader rewrites module paths at flatten time. The guard is unreachable on both sides (a module const's base is a module *name*, never an index), so it is structural, not a divergence. |
| **L3** `@abi(ref)` — large `read` aggregates stop being copies | `cea779a` | **Workstream L is complete.** `read` has always said "borrowed, not mutated" while physically being a **copy**; `@abi(ref)` makes a large one cross as `const T*`. Opt-in per function, so corpus 141 / concat / fixpoint / seed are untouched. **The increment was small because the mechanism already existed:** `mut`/`out` have always been pointers via a `ptr_params` set whose members render `(*j_x)`, so adding the qualifying `read` params to that set is the *entire* body change — every field read, pass-on and capture follows for free. Finding the existing indirection was worth more than any code written. **Which params change:** `read`/default-borrow only, aggregate only, **larger than two machine words** — below that a by-value pass is already one or two registers and a pointer would be *slower*, and an ABI attribute that pessimized the small cases would be a bad attribute. The size comes from `layout.rs` (L1's payoff); an unknowable size is left by value rather than guessed. **Two things the tests caught that reasoning had not:** (1) **a Jestyr place is not automatically a C lvalue** — `xs[1]` is a place, but a *bounds-checked* index lowers to a GNU statement expression, which yields a value, so `&` of it does not compile. Lvalue-ness must follow the EMISSION: `Field` is one iff its base is (so `xs[0].a` is not), `Deref` always is, `Index` never is. (2) **the address-taken check had to be whole-program** — first written per-item in `validate_fn`, where it silently passed the exact program it exists to reject, because the function is validated as it is parsed and a `let g = f` in a later item sees an empty arena. Now `attrs::validate_program`, called from `Parser::parse` *and* `module::load`. **The soundness rule:** an indirect call cannot be compiled against the convention (a `fn(T) -> R` type carries none), so taking the address is refused — detected cheaply, since a call names its function in *callee* position, so a `Name` that is nobody's callee is an address-taking mention. Generics refused (param types unknown until instantiation); **methods refused by the target list**, deliberately — a method reaches its callee through method sugar, bound values, vtables and `dyn`, and a signature some call sites do not match is worse than a "not yet". A temporary argument uses a **compound literal of array type** (`(const T[1]){ e }` — block lifetime, plain C99); the GNU statement-expression alternative returns a pointer to a temporary that dies at the closing brace. The lvalue path is the one that avoids the copy, which is the whole point. **On L3's other half — niche packing already exists on master**, automatic rather than gated, and correctly so: the optimization is provably free, so an opt-in would ask users to request something that costs nothing. |
| **L (CTFE)** `@size_of`/`@align_of`/`@offset_of` as comptime VALUES + two real model bugs | `fbef2fb` | Closes the gap `docs/ctfe-tiers.md` recorded against tier 3. **Both spellings remain and mean different things:** bare `size_of(T)` still lowers to C's `sizeof` (untouched, so every existing program is byte-identical), while `@size_of(T)` is a number *this* compiler computed and can therefore appear in a `const`, an array length, or folding arithmetic. The `@` namespace decision from tier 3 paid off a second time — had reflection taken the bare names, there'd have been no room to say this. **The architectural problem:** `Interp::new(ast)` takes the AST *alone* (it answers array lengths, so it runs during type checking, before the table exists), so layout needed an AST-side front end. Only the **traversal** is duplicated; every rule (`prim_layout`, `aggregate`, `align_to`, `auto_order`) is one copy, and `the_two_layout_models_agree` pins them across the corpus. **That test found a real bug on its first run:** unions were laid out **sequentially** — `TypeKindG` has no union arm, so `union Bits { i: i32, f: f32 }` was reported as 8 bytes with `f` at offset 4 (it is 4, both at 0). Fixed by threading the declaration form through a new `Model` context. **Then a second bug it could NOT catch:** niche-optimized enums were reported as tag+payload when the backend has always emitted them as a bare pointer — `@size_of(Maybe)` would have folded to 16 while `size_of(Maybe)` gave 8, *a disagreement inside one program*. **The lesson worth carrying: cross-checking two implementations finds divergence, never shared blindness.** Only an external authority does that — here `a_folded_layout_query_equals_the_c_compilers_answer`, which prints `@size_of(T)` beside `size_of(T)` in one running program over 13 shapes and pins the values as well as the agreement. Every unknowable case is an **error, never a zero** (the G1 `[0]T` failure mode). |
| **L2b** the `@layout(auto)` port mirror — **corpus 141**, byte-identical first run | `bff09d8` | The self-hosted compiler reorders fields exactly as the reference does; the gcc-only seed carries the ordering model. **The mirror is alignment-only, by design:** `la_align` reproduces `layout_of`'s alignment column and nothing else, because the ordering needs nothing else — an aggregate's alignment is the max over its components, which no permutation changes, so it can be computed without first knowing which structs are reordered. `-1` is the port's `Option::None` (both sides must agree on *which* structs are reordered or their C diverges). **The sort is four passes, not a sort:** `la_emit_fields` walks levels 8/4/2/1 emitting matching fields in declaration order — exactly `sort_by_key(Reverse(align))`'s stable semantics with no list, no comparator, no allocation, and equivalent *only* because the model's alignments are drawn from those four values, which `la_reorderable` **checks** rather than assumes. `examples/layout_auto.jtr` is the corpus file, written so the load-bearing assertion is an `Outer` embedding a reordered `Tidy` **by value** — 24 bytes only if the inner really shrank; a model that failed to propagate would print 40 and pass everything else. |
| **L2a** `@layout(auto)` reorders fields — opt-in, and provably minimal | `8d04770` | L's first emission change, landed the way L1 predicted: opt-in per struct, so non-annotated output is byte-identical and no port mirror was due (the Q-S2a ordering). **The enabling fact, verified before a line was written:** cgen constructs structs with C **designated initializers** and reads them by name, so permuting the declaration changes *storage* and nothing else — no initializer rewritten, no access site taught anything, and `size_of`/`@offset_of` follow for free. **The order is minimal, not merely tighter:** every layout in the model satisfies `size % align == 0` and every alignment is a power of two, so descending alignment leaves **zero interior padding** and the total is `align_to(Σ sizes, align)`, which no ordering can beat. `every_layout_size_is_a_multiple_of_its_alignment` checks the invariant rather than trusting it. Ties keep declaration order — the result feeds the attest hash, so a hash-iteration-dependent sort would make builds unreproducible. **Reordering is not local:** a smaller inner struct moves every offset after it, so the auto-set is threaded through the whole size computation; the circularity that implies is broken by the observation that alignment is order-invariant. **The vocabulary is now closed** (`c` \| `auto`) — `Args::Word` checked only that the argument *was* an identifier, so `@layout(packd)` validated clean and did nothing, which reads exactly like a guarantee. `auto` is refused with its own named cause on a union, with `@packed`, and on a bit-field struct. |
| **Q-S2b/c/d** SIMD lowering COMPLETE — both sides, **corpus 140**, lane width in the canary | *(this run)* | **Workstream Q's SIMD arc is closed.** (b) The classify/cgen disagreement is gone, fixed on the **backend** side: cgen now lowers a value-position `if`/`else` whose arms are single tail expressions to C's conditional operator. No temporary, no spilling, no drop question — which is why this was not the deferred "statement-expression with drop-safe spilling" case. It reaches well past SIMD: `let a = if c { x } else { y }` and else-if chains work generally now, and it could not disturb byte-identity because every program it newly accepts used to be a compile error. (d) resolved the **opposite** way, deliberately: a multi-statement block stays refused, and `classify` was NARROWED to match, because there the backend's refusal is principled (spilling a block into value position genuinely needs drop safety) rather than incidental. **The rule that fell out: fix the backend when its refusal is incidental; narrow the pass when it is not** — which is what keeps "conservative, never optimistic" true rather than aspirational. (c) The port mirror: `sd_classify`/`sd_emit`/`sd_emit_par_for`/`sd_vector_defs` in cgen.jtr reproduce the whitelist and the vector lowering, **byte-identical on the first run**; only the accept/reject verdict is mirrored, not the rejection reasons, since what must agree is *which sites vectorize*. `examples/std/par_for_simd.jtr` is corpus **140** — 11 elements over 8 `i32` lanes, so every run crosses the scalar remainder, with both loops checked against serial references in-program. And **lane width is now PINNED, not merely tested**: `numerics_canary.jtr` carries two `@simd` loops whose realized values enter the locked cross-OS digest exactly as the thread-count and chunk-size results do (re-locked to `4389bf83…`). **Two port bugs the gate caught, both worth knowing:** the vector typedef was deduped by **TyId**, but the checker allocates a fresh `TyData` row per occurrence, so two `[]i32` sites carry different ids — it agreed in normal mode and emitted the typedef twice in TEST mode, where the harness's extra code shifts the arena. Dedup by the mangled NAME. And `sd_emit_par_for` took its own temp when the caller had already taken one, numbering the port's temps a step ahead of the reference's. |
| **Q-S2a** `@simd` LOWERS — the first vector instructions | `4339d72` | **Jestyr emits SIMD.** A `par for` inside an `@simd` function whose body `simd::classify` certifies now lowers to a vector head plus a scalar remainder, using GCC vector extensions (`typedef int32_t JestyrVec_i32 __attribute__((vector_size(32)));`) — **not** an OpenMP pragma, no `-march` change, no new `CC_FLAGS`. The lowering is *chosen*, not begged for, or determinism sits at the optimizer's discretion. **Opt-in per function**, so no corpus file writes `@simd` and all 139 stay byte-identical — no port mirror, no seed refresh, the Q-W1 boundary again. **Lane count comes from the element type**, which is what Q-W1 bought: 4 for `i64`, 8 for `i32`, 32 for `u8`, from one fixed recorded `SIMD_VECTOR_BYTES = 32` — ⚠️ **the `32 for u8` half of this was UNSOUND and is fixed in Q-S2e (see below); a narrow element is promoted to `int32_t` first and gets 8 lanes** (never a host probe — a width read from the build machine would make the emitted C depend on where it was compiled, which attestation would rightly flag). Results are stored **lane by lane** rather than converted as a vector, which keeps the widening to `int64_t` exact at every element width without reaching for `__builtin_convertvector`. **The headline test is end-to-end**: `simd_lowering_matches_the_scalar_path_bit_for_bit` compiles the corpus demo twice — as shipped and with `@simd` on `main` — and requires identical tokens from both binaries, over **9** elements, deliberately not a multiple of 8, 4 or 32, so every run exercises the scalar remainder. 12 unit tests + that oracle. |
| **Q-W2** the width port mirror, **corpus 139** | `9a463d8` | Closes Q-W1 on both sides and clears Q-S2's last prerequisite. `typeck.jtr` binds the loop variable with the element's own type (`ty_is_int`, prim codes 0..=9) and `cgen.jtr` emits the element's slice type and C type with the contribution cast omitted when the body is already `i64` — so the `i64` path stays byte-identical while a narrower source keeps its width. `examples/std/par_for_width.jtr` is corpus **139**: an `i32` sum-of-squares over 9 elements (an uneven last worker chunk) checked against the serial fold, an identity max, and a `u8` sum. **The guarantee is verified where it could actually break** — `c_oracle::par_for_width_demo` runs it on real OS threads 8× and requires identical tokens each time, so "the reduction domain is `i64` while the loop iterates `i32`" is tested, not argued. Byte-identical on the first run, both for the mirror and for the new file; seed refreshed. One thing worth knowing for the next mirror: `par_elem_ty` returns `9` for its fallback and tests `ed.x > 9` two lines apart, and those are **different nines** — the first is the well-known TyId of `i64`, the second the largest integer PRIM CODE (i64's own code is 3). |
| **Q-W1** `par for` over any integer width | `86d9e60` | **Q-S2's prerequisite, and it did NOT need the generic `spawn` this note assumed was blocking.** `emit_par_for` is map-then-reduce: it fills an `int64_t` buffer by running the body per element and hands only *that* to `core.par_reduce`, so the engine never sees the source slice. The `[]i64` restriction was therefore a **typeck rule, not an engine limitation** — `par_reduce` stays exactly as it is, i64-only and untouched. `par for` now iterates any integer element type; the loop variable carries the element's **own** type, so a body over `i32` computes in `i32`, and only the per-element contribution is widened, once, on the way into the buffer. The determinism argument does not move at all: the reduction domain is still `i64`, where the declared operators are exactly associative. **Why it is the SIMD prerequisite:** a lowering fills lanes with the *element* type, so `i32` gets twice the lanes of `i64` and `u8` eight times — building Q-S2 first would have shipped the 4-lane version of the feature. **Zero emission change**: the `i64` path reproduces its previous C character for character (no cast is added when the body is already `i64`), so all 138 corpus files, the concat, the fixpoint and the seed are untouched and no port mirror is due yet. 6 tests: every width accepted, the loop variable's C type, the two *different* slice types in one lowering (source keeps its own, the engine still takes `[]i64`), byte-identity of the `i64` path, refusal of a float element or contribution, and — the one that matters — the checked non-deterministic-reduction guarantee surviving the widening. |
| **M9** the lexer dogfood — the compiler's own source uses tier 7 | `5e24ce1` | **The first real USE of the CTFE ladder inside the compiler, and the increment that answers whether it was worth building.** `examples/std/tokens.jtr`'s six classifier functions (`is_space`/`is_alpha`/`is_digit`/`is_alnum`/`is_hex`/`is_digit_us`, ~30 lines of branch chains with `is_hex` calling `is_digit` to avoid restating a rule) become one comptime-computed 256-entry table plus six one-line predicates. One bit per class, so a byte's full classification is a single load; `_` carries both the alpha bit and a separate separator bit, because "is an identifier character" and "is the `1_000` separator" are not the same question and the branch chains had been conflating them by call order. **The crown result: `selfhost_fixpoint_full` still holds — `jc` now lexes its own 25K lines using a table its own compile-time evaluator computed, and still reproduces itself byte-for-byte.** The token stream is unchanged corpus-wide (all six P1 lexer goldens green), so the conversion is behaviour-preserving by test, not by argument. **The honest verdict on the feature, measured rather than asserted:** emitted C shows each classification is now a bounds-checked load (`assert(_ix < 256)`) where it used to be register comparisons — a real per-call cost — but timing both self-hosted compilers over a 12.7K-line file gives medians of ~163 ms (branches) and ~170 ms (table) with a 148–191 ms spread in **both**, i.e. indistinguishable, because lexing is a small fraction of a full compile. So the win is expressiveness and maintainability, and the cost is real but invisible at this granularity; resolving it either way needs a lexer-only benchmark the repo does not have. |
| **M7** the comptime `for` + mutation in the port | `270f947` | **The CTFE ladder (0–7) is now closed on BOTH sides.** Loops and `var` assignment run at compile time in the self-hosted compiler: all three heads, ranges (inclusive / `step` / descending), list iteration, element+index, `break`/`continue` incl. **labelled**, `for … else`, and compound assignment through the same **checked** arithmetic (so `x += 1` overflows into a diagnostic). Fuel per **iteration** — the recorded trap holds, an empty body evaluates nothing so nothing else charges the budget. **The increment is where M6's deferred obligation comes due, and the shape of the debt was exactly as predicted:** a handle copies for free, so the copies the reference gets from ownership have to be written down, at three points now marked in place — reading out of a binding, reading an element, and **each element of `[v; n]`**. The third is the one that is invisible until it isn't: `[[0; 2]; 3]` without it is three cells naming ONE run, so `m[0][1] = 5` writes through all three rows. Verified directly, not just via the golden. **The flat arena paid for itself on the other side of the trade**, also as predicted: an assignment target is a `Place` — an env row or a cell index — so walking an index path is an address computation, where the reference needs `find_binding` to hand out a `&mut Value` and `walk_path` to re-borrow through each index, a chain the `.jtr` subset cannot express at all. Assignment resolution reads through `place_read_raw`, which deliberately does **not** copy: copying there would resolve `t[0]` against a duplicate and drop the write. 16 new golden fixtures (9 accepts + 7 refusals). Two side findings: the escape checker refuses `return v` for a `read` parameter (rebuild the value instead), and a real dump bug surfaced only once there were enough refusal fixtures to see it — `eval` cleared `failed` but not `emsg`, so a second failing `const` reported the concatenation of every message so far. |
| **M8** aggregate EMISSION in the port, **corpus 138** | `f7b0130` | **Tier 6 is now closed end-to-end on both sides**: the port types a comptime block that folds to a list as `[N]T` and emits it, byte-identical to the reference. `examples/comptime_table.jtr` is the corpus file, and it exists to make the tier's point concrete — `const SQUARES: [6]i64 = [sq(0), …]` **does not compile**, because a `static const` initializer must be a constant expression and `jestyr_sq(0)` is a call; wrapping the same literal in `comptime { … }` moves the calls to compile time and what reaches C is `{ { 0, 1, 4, 9, 16, 25 } }`. A recursive `fib` table lands the same way — the C compiler gets `{ { 1, 1, 2, 3, 5 } }`, never the call tree. **Two emission paths, deliberately separate:** a `const` needs the BRACE form (`push_ctfe_brace`), since a static may not be initialized by the GNU statement-expression an expression-position aggregate uses (`({ T _cl0; _cl0.a[0] = …; _cl0; })`) — both are mirrored, and the const path is checked *before* the array-literal path in `emit_consts` rather than inside `emit_expr`, exactly as the reference orders it. Element type follows the reference's rule: an annotation wins, else the first element decides, else it is refused rather than guessed. Worth knowing for the next reader: a `const T: [6]i64 = comptime { … }` emits BOTH `JestyrArr_i32_6` and `JestyrArr_i64_6`, because a const's value is inferred with no expected type (so the block's own type is `[6]i32`) while the annotation gives `[6]i64` — pre-existing documented behaviour that aggregates now exercise. All 138 corpus files byte-identical on the first run; seed refreshed. |
| **M6** aggregate values in the port | `70e06af` | **Tier 6 closed on both sides — and it was the REPRESENTATION change this note predicted, not more of the same.** `ctfe.jtr`'s `Value` stays a tag plus one `i64`; a list's payload is a pair index into `lidx`, whose (start, len) names a contiguous run of CELLS in the parallel `ltag`/`lval` arenas, and a cell tagged 4 names another run — the reference's owned `Vec<Value>` tree, with the ownership expressed as an index. Two things fell out that the design had to answer rather than assume. **(a) Elements must be STAGED before they are stored:** `[[1, 2], 3]` builds the inner list while the outer one is mid-construction, so appending to the arena as each element evaluates would interleave the two runs; elements land on a stack and move into the arena as one run when the literal closes, and nested literals complete first so the stack discipline is free. **(b) Runs are already SHARED, before any binding exists** — `[v; n]` evaluates `v` once and stages one handle `n` times, so `[[0; 2]; 3]` is three cells naming ONE inner run where the reference holds three cloned `Vec`s. Nothing in tier 6 can observe it (no way to write through a handle), which is precisely why the deep-copy obligation belongs to **M7** and is recorded in the module docs at the two places the reference clones. Arms added: ArrayLit (14), ArrayRepeat (15), Index (6), Field `.len` (5), and structural `==`/`!=` (equality only — an ordering on aggregates would have to be invented). Fuel is spent **per element**, so `[0; 10000000000]` and the nested `[[0; 100000]; 100000]` are diagnostics in microseconds on both sides. 19 new golden fixtures (9 accepts + 10 refusals) in `jestyr_ctfe_folding_matches_reference`, which grew a one-line list rendering shared by `render_ctfe_value` (reference) and `render_value` (port). Seed refreshed — the gcc-only bootstrap now carries an aggregate evaluator. Two small traps: the **recorded** reserved-word list bit again (a parameter named `out` — a whole-file parse cascade, ~40 diagnostics, all from one identifier), and decimal rendering has to accumulate the digits as a NEGATIVE number because `0 - i64::MIN` does not exist. |
| **Q-S1** the checked `@simd` legality pass | `c77f6d2` | The SIMD frontier opens where L1 opened the layout one: **analysis first, zero emission change.** `src/simd.rs` decides whether a `par for` body may be evaluated a lane at a time — a total, elementwise integer expression — and `@simd` is the *checked declaration* that it can be, in `@span`'s shape rather than a `#pragma`'s: it emits nothing, so a refactor that slips a call, a division or a float into a hot body is a diagnostic instead of a silent fall off the vector path. Nothing SIMD-shaped existed on master beforehand, so this is increment 1 of the workstream, not a duplicate. The determinism argument is two claims, and only the second was open: the four declared reductions are exactly associative on machine integers (so a lane tree of ANY width equals the serial fold — which is why float is *excluded* rather than discouraged; `-ffp-contract=off` constrains contraction, not order), and the body must be total. **gcc is the authority, as it is for layout**: `simd_lanes_match_scalar_bit_for_bit` evaluates every certified body scalar-wise and through GCC vector extensions at widths 2, 4 and 8, reduced four ways, and requires identical bits. It earned its keep immediately by catching a real miscompile in its own harness — the scalar tail of a vector loop needs the *scalar* lowering of a select, the bug Q-S2 is now forewarned about. |
| **M5** def-order dependency registration | *(this run)* | Closes the divergence M4 recorded: the port emitted `JestyrSlice_<T>` **after** the struct whose field embeds it, where the reference emits it before. The sorter was never the bug — the port's slice and genref typedefs never called `dc_begin`/`dc_end` at all, and the array typedef opened a segment it **never closed**, so all three were absorbed as anonymous *glue*. Glue is pinned in place and invisible to `dc_find_dep`, so the struct's (correctly recorded) dep resolved to nothing. Registering all three as named segments — no deps for slice/genref, which reach `E` through a pointer; one dep for the array, which embeds `E` by value — makes the port's segment graph structurally identical to the reference's. `examples/def_order.jtr` is the new corpus file (**136 → 137**) and pins all three at once; the slice half is also pinned in place by restoring the field `comptime_reflect.jtr` had to omit. **Zero emission change on the other 136 files.** The unclosed `dc_begin` is the failure mode worth remembering: it loses the name without losing any text, so it is a silent divergence rather than a crash or a corrupt buffer. |
| **M4** G3 reflection port mirror | `b292426` | **Tier 3 closed on both sides.** `@type_name`/`@field_count`/`@field_name`/`@field_type` in the self-hosted compiler; `examples/comptime_reflect.jtr` is corpus **136**, byte-identical. The structural decision: `@field_type` needs a Jestyr-syntax type renderer, and one already existed in cgen.jtr for attest + doc — so `at_ty` **MOVED DOWN** into `ctfe.jtr` (below cgen, above parser, depends only on the type arena) and cgen keeps a three-line wrapper, leaving its 16 call sites unchanged. ONE renderer, two consumers: a reflected type name cannot drift from the documented or attested one, the same invariant `at_guarantee_phrases` gives the guarantee text. typeck types a query from a STATIC table (`field_count`→i64, the rest→str) and does **not** infer the arguments — the first is a *type*, so inference would report a binding that does not exist; only emission evaluates. **The `@` sigil justified itself in reverse:** the first cgen arm keyed on the Attr node KIND and broke two passing corpus files, because `@address(0x…)` in mmio.jtr is also `@name(…)` and must still emit normally. `@name` is a family; the guard belongs on the intrinsic NAME. Nine reflection fixtures added to the ctfe golden (incl. a primitive, a `record`, methods-are-not-fields, a const index, composition inside `comptime {}`, and three refusals) — all passed first run. |
| **G8** plan-as-list | *(this run)* | `const targets` now selects the build script's form **by its type**: an integer is a count (the tier-4 index form), a list is the targets themselves — `[[source, output], …]`, buildable by a comptime `for`. A pair is a two-element list because the value domain has no struct; two parallel lists would read better and can disagree in length. Artifacts take the same two forms. Fully backwards compatible, and the two forms render byte-identical plans (pinned by planning one build both ways). `examples/build_list.jestyr` is the worked example. |
| **L1** layout analysis | *(this run)* | `src/layout.rs` + `jestyrc layout <file>` — size, alignment, field offsets and **padding waste** for every declared type. **Zero emission change**, so no port mirror and no seed refresh; L's later increments are opt-in *because* this one is not. Verified rather than asserted: `layout_matches_c_sizeof` generates a probe `main` printing real `sizeof`/`_Alignof`/`offsetof` and diffs 19 values across the corpus shapes against the model — **the C compiler is the authority, and now says so in CI**. Two honest gaps, both stated in the report as `(incomplete)` rather than guessed: generic/opaque components, and **bit-fields** (implementation-defined in C — the model would otherwise report 4 bytes for a struct that occupies 1). Unblocks `@size_of`/`@align_of`/`@offset_of` as comptime *values*, the gap tier 3 records. |
| **M3** typeck fold + cgen emission, **first corpus file** | *(this run)* | `examples/comptime_block.jtr` — **corpus 134 → 135**, and the first `.jtr` in the tree to use comptime syntax. typeck.jtr folds a kind-44 node to its value's type (i32/bool/str, Error on failure); cgen.jtr emits the folded literal (`push_i64`, `push_c_string` with the octal + trigraph rules mirrored from `c_string_literal`); `eval_array_len`/`push_array_len` now go through the interpreter instead of accepting only a literal — the port's own G1. Emitted C byte-identical across all 135. **Two shims the next mirror will hit as well** — see findings 8 and 9. |
| **M2** the CTFE interpreter in `.jtr` | *(this run)* | `examples/std/ctfe.jtr` + `ctfe_cli.jtr` — the self-hosted comptime evaluator, scalars (Int/Bool/Str). Golden `jestyr_ctfe_folding_matches_reference` drives both implementations over 23 fixtures and requires agreement on **what folds** and **what is refused**: arithmetic, every literal base, bitwise/shifts, const chains, comparisons, short-circuit, `if`/`else`, calls, recursion, `let`, strings (concat/escapes/ordering), chars, casts, `comptime` blocks incl. nesting, and 8 refusal cases. **Named `ctfe`, not `comptime`** — see the finding below. Message-TEXT parity is deliberately not claimed (the `.jtr` subset has no `format!`; same concession the P-series made for the refusal gates). |
| **M1** the port parses `comptime` | `63ba3fa` | `parser.jtr` expr kind 44 + the three dispatch sites + dump arm; verified by the P2 expression golden on a temp probe file (6 snippets incl. the tier-7 shape), so it lands without touching a corpus golden. |
| **G7** comptime `for` + mutation | *(this run)* | **Also cleared tier 3's field-iteration blocker** — see below. Loops and `var` assignment run at compile time, so a table's **shape** is computed, not spelled out: a 256-entry CRC-32 table is built by `for i in 0..256 { t[i] = crc_entry(i) }` and emitted as a plain static (values verified against an independent implementation, not just self-consistency). All three loop heads, ranges (inclusive/`step`/descending), list iteration, element+index, `break`/`continue` incl. **labelled**, and `for … else`. Assignment reaches locals and elements at any depth; compound assignment uses the same **checked** arithmetic, so `+=` overflows into a diagnostic. Fuel per **iteration** — the trap here is that an empty body evaluates nothing, so nothing else charges the budget at all. |
| **G6** aggregate values | *(this run)* | `Value::List` — CTFE goes from "compute a number" to "compute a **table**". Array literals/repeats evaluate; `[i]` and `.len` read. A `const` initialised by a comptime block yielding a list becomes an ordinary **static** (`static const JestyrArr_i64_8 jestyr_FIB = { { 0, 1, 1, 2, 3, 5, 8, 13 } };`) — the C compiler gets the answers, not the recursion. Emission detail with teeth: a static **must** be a brace initializer, since it cannot be initialised by the GNU statement-expression an expression-position aggregate uses; both paths exist and the test asserts no `({` reaches a static. Fuel is spent **per element**, so `[0; 10_000_000_000]` (and the nested `[[0; 100000]; 100000]`, where the *product* blows up) is a diagnostic in microseconds. |
| **G5** bounded generation | *(this run)* | `jestyrc plan … --emit`. A build script **computes the bytes of a file**; the plan records it by SHA-256, and the driver writes it only on explicit `--emit`. The evaluator gained no new power — it computed a string — so generation is a pure function whose result the *user* places, never an effect a script performs. Reproducible, attestable (hash in the plan), and bounded literally: size-capped, and an absolute or `..` path is **refused, not normalised**. A generated file can be named as a build target, so a program can be computed and compiled in one invocation. |
| **G4** `build.jestyr` | `29bd2bd` | `src/buildscript.rs` + `jestyrc plan <script> [--build]`. The build described in Jestyr and **evaluated, never run**. The plan is a *pure function of an index* (`const targets` + `fn source(i)`/`fn output(i)`) rather than the imperative `exe(…)`/`test(…)` shape, because that shape needs comptime **effects** — the one thing the ladder exists to forbid. Closes K's last leftover. `Interp::{eval_const, call_fn}` added. `examples/build.jestyr` is the canonical demo (a `.jestyr` extension, so no golden sweeps it). |
| **G3** reflection | `4a85bc6` | `@type_name` / `@field_count` / `@field_name` / `@field_type` over the **declared shape**, answered by this compiler and folded to literals. |
| **G2** comptime blocks | `b063ca4` | `comptime { … }` in user syntax, reusing the existing keyword. Typed as and emitted as the literal it folds to; **the body belongs to the interpreter alone**, so non-users are byte-identical with no gating flag. |
| *(chore)* seed refresh | `458911a` | `bootstrap_seed_is_current` was **already failing on master** — the committed flat was missing the ten import-placeholder blank lines. `jestyr_seed.c` was byte-identical, so it was cosmetic; the gcc-only bootstrap was never broken, only its drift guard. |
| **G1** CTFE interpreter | `ebf8397` | `src/comptime.rs` — a TOTAL comptime interpreter (step budget, call-depth cap, const-cycle detection, checked arithmetic). First consumer closed a silent miscompile: `[SIZE]i32` had been emitting a **zero-length** array with `assert(_ix < 0)` and no diagnostic. Zero emission change → no port mirror yet. |
| **54** `doc` renderer | `1a5ad67` | `jc <file> doc` byte-identical to `doc::generate` over 134 files; `at_guarantee_phrases` became the ONE guarantee extractor shared with attest. |
| **53** doc trivia | `1a5ad67` | `tokens.collect_docs` — finds comments by scanning the **gaps between tokens**, so the golden-pinned `tokenize` is untouched. `examples/std/doc_cli.jtr` added → corpus 134. |
| **52** attest diff/verify | `094bd6e` | `jc <old> attest-diff <new>` / `jc <file> attest-verify <manifest>` reproduce `attest::diff(…).render()` byte-for-byte, exit code included. Surfaced the `#line` divergence (§1). |
| **51** attest records | `e3c5fbe` | `jc <file> attest` emits the FULL manifest; ported `doc::{ty_str, fn_sig, extern_sig, const_sig, fn_guarantees}`. |

**Two findings from the O run worth carrying forward**, both recorded in place below:
the `#line` emission gap (§1 — invisible to every golden, so it hid for 50 increments),
and the fact that a map-free port of an ordered-container algorithm can still be exact
when the reference re-sorts its output by content (§2, O1).

### Findings from the G2–G4 run (read these before continuing CTFE)

1. ~~**`Value::List` — aggregate comptime values — is the single next unlock.**~~ —
   **DONE (G6).** It cost no totality, as predicted, *provided* fuel is spent per
   element — that one line is what separates a bounded evaluator from one that tries
   to allocate ten billion values. What it delivered: comptime **tables** as real
   statics. What it did *not* deliver on its own: building a table still meant spelling
   out `[f(0), f(1), …]` — the *values* were computed, the *shape* was not. ~~And
   tier-3 field iteration still needs comptime-only functions.~~ — **both closed by G7.**
2. ~~**Field iteration is emission-blocked, not L-blocked.**~~ — **DONE (G7), and the
   predicted fix was the wrong one.** The diagnosis was right: a top-level `fn` is also
   emitted as ordinary runtime code, where the index is a parameter and the query cannot
   fold. The prescription — **comptime-only functions** — was not needed. A comptime
   `for` binding is not a function parameter; it lives in the interpreter's own
   environment, and typeck never descends into a `comptime` body, so the loop form folds
   where the function form could not:

   ```jestyr
   const SHAPE: str = comptime {
       var acc = ""
       for i in 0..@field_count(Point) { acc += @field_name(Point, i) … }
       acc
   }
   ```

   That reaches C as one `JSTR(…)` constant (`a_comptime_loop_iterates_struct_fields_end_to_end`).
   **The transferable lesson:** when a feature is blocked because a value "is a
   parameter, not a constant", check whether some *other* binding form is already
   constant before building a new function kind. Comptime-only functions remain a
   reasonable convenience, but they block nothing now.
2b. **A design decision can be *unmade* by a later tier, and the docs must say so.**
   Tier 4 chose index functions (`source(i)`/`output(i)`) over a target list on the
   explicit ground that a list "costs the evaluator no new powers — no effects, no
   aggregate values, no comptime `for`". Two of those three arrived (G6, G7), so the
   ground was gone and the module docs were still asserting it. G8 added the list form
   *beside* the index form — `const targets` dispatches on its value's type, which made
   the change free of any migration — and rewrote the rationale rather than leaving a
   stale one. Worth a sweep after every tier: the ladder's earlier rungs contain
   "we can't do X yet, so Y" reasoning that expires silently.
3. **`size_of`/`align_of`/`offset_of` are C-deferred**, lowering to `sizeof`/
   `_Alignof`/`offsetof`. This compiler never learns the numbers, so exposing them as
   comptime *values* genuinely does require **L**. That is the real L dependency; the
   declared shape (names, field types, order) needed none of it.
4. **Bare-name intrinsics are a live hazard, and the compiler's own source proves it.**
   G3's first draft put reflection beside `size_of` as ordinary identifiers — but
   `examples/std/typeck.jtr:919` declares `fn field_type(…)`, which a bare-name
   `field_type` intrinsic silently hijacks, breaking the self-hosted build. Reflection
   moved to the **`@` namespace** (`@field_type`), which was already a callable form
   (`@address(0x…)`) and cannot collide. `size_of` and friends carry the same latent
   hazard; prefer `@` for anything new.

   **The same hazard recurred at M2, in a new place: a MODULE PATH.** The port mirror of
   `src/comptime.rs` could not be named `comptime.jtr`, because `comptime.run()` parses
   as a comptime block missing its `{` — the module path and the new expression form
   compete for the same token, and the parser resolves it in the keyword's favour. The
   reference is fine only because `comptime` is not a Rust keyword. It is now
   `examples/std/ctfe.jtr`. **Generalised rule: every time the language reserves a word,
   it also forbids that word as the first segment of a module path** — so check the
   keyword list before naming a module, not just before naming a function.
5. **Do not add names to cgen's `is_intrinsic` list casually.** That list stops a *bare
   value reference* being read as a closure capture — and the self-hosted compiler has
   several locals named `field_count`. Adding reflection there would have broken real
   code while buying nothing (reflection is only ever called). A comment now says so
   in place.
6. **`comptime` inherited the block-led statement rule for free**, which will look like
   a bug to the next reader: `comptime { comptime { 3 } + 1 }` is a parse error for the
   same reason `unsafe { unsafe { 3 } + 1 }` is — at statement position a block-led form
   parses as the block alone so a trailing operator cannot extend it. Nest in *value*
   position instead. Pinned in `comptime::tests`.
7. **A computed string needs real C escaping.** A source literal is passed through
   verbatim (Jestyr's escapes are C's), but a comptime-*computed* string has no source
   text. Two C rules bite: hex escapes are maximal-munch (`"\x41" "1"` → `\x411`), so
   non-printables use fixed-width three-digit octal; and `-std=c11` still honours
   trigraphs, so a literal `?` must be escaped. `c_string_literal` in cgen.rs, round-
   tripped through gcc. Note the NUL case can only be checked by *length* —
   `printf("%.*s")` stops at a NUL whatever precision it is given.
8. **Unit tests in the binary crate have no `CARGO_BIN_EXE_*`.** That is an
   integration-test variable. To invoke `jestyrc` as a subprocess from `proptests.rs`,
   walk up from `std::env::current_exe()` (`target/<profile>/deps/` → `../`).
9. **Every new block-led form needs its `dump_types` BLOCK-SHIM entry** (M3, and this
   will recur). The reference stores most blocks as embedded `Block` **structs** — fn
   bodies, if-THEN, `unsafe`/`concurrent`/`region` bodies, `for` body+else, select arms —
   while the port's parser materializes *every* block as a kind-23 arena node. The P3
   typeck golden compares the FULL expression stream in ExprId order, so `dump_types`
   skips the struct-position blocks to keep the two streams aligned. Adding `comptime`
   without adding kind 44 to that skip list shifted every subsequent type by one node per
   comptime block, which reads as a wall of unrelated type mismatches. **The diagnostic
   signature is an accumulating drift**: if the divergence count grows with the number of
   occurrences of the new construct, suspect the shim, not the typing.
10. **A new construct in a corpus file drags in the port's `eval_array_len` too.** A
   `comptime` block is legal as an array length, and the port's `eval_array_len` /
   `push_array_len` accepted only an integer *literal* — the exact pre-G1 reference
   behaviour, silently yielding `[0]T`. Both now route through the interpreter. Anything
   that can appear in a length position has to be folded on both sides.

11. **An unclosed capture segment is a SILENT divergence, not a crash.** `dc_begin`
   without a matching `dc_end` is absorbed by the next `dc_begin`'s `dc_glue`: the text
   is still emitted, at the right offset, with correct buffer coverage — it just loses
   its *name*, becoming anonymous glue that is pinned in place and invisible to
   `dc_find_dep`. That is how the port's slice/genref/array typedefs sat unregistered
   for the whole P5 series (M5). When auditing the port's `Dc` sites, check `dc_end`
   pairing, not just `dc_begin` presence — and remember the reference's `dep_of_cty`
   rule that makes the deps correct: a rendered type containing `*` contributes no edge,
   so a slice/genref typedef has no deps while a *field* of that type does.

12. **Still open (a both-sides gap, not a divergence): a struct declaring an ARRAY field
   whose instance is never constructed gets no `JestyrArr_<T>_<N>` typedef at all**, so
   the emitted C does not compile. `collect_arrays` keys off values (`info.expr_types`,
   fn signatures, monomorphized instances) and never walks struct field *declarations* —
   unlike `collect_slices`, which does scan the type arena. Both implementations agree,
   so every golden is green; `examples/def_order.jtr` sidesteps it by constructing a
   `[4]i64`. The fix is a field-declaration walk in `collect_arrays` + its port mirror.

13. **FUEL ACCOUNTING is part of the mirror contract, not an implementation detail**
   (M6). The budget decides *whether a program folds*, so if the two evaluators spend
   at different rates a borderline input folds under one and is refused by the other —
   a divergence no fixture would find unless it sat exactly at the boundary. The
   invariant that makes them agree is narrow and worth stating: **both spend one step on
   entry to the expression evaluator** (`Interp::eval_expr` / `ev`), and any additional
   per-element or per-iteration spend must be mirrored at the same place. Checked for
   tier 6: ArrayLit costs `1 + n` on both, ArrayRepeat `1 + 1 (count) + 1 (value) + n`
   on both, Index and `.len` `1 + children`. Tier 7 spends per *iteration*, so the
   mirror must too — and the recorded trap there is that an empty loop body evaluates
   nothing, so the per-iteration spend is the only thing charging the budget at all.

14. **TEMP-ALLOCATION ORDER is part of the mirror contract too, and it can stay dormant
   for years** (X1). The reference emits an array index's base and index into strings
   *before* taking its own `_a{n}`; the port took `n` first and emitted inline. Those
   two orders produce identical text for as long as no base or index allocates a temp
   of its own — which was true of every corpus file until `m[i][j]` (a base that is
   itself a checked index) existed. **The diagnostic signature is a pair of swapped
   temp numbers in otherwise identical C**, and no golden can see it until a corpus
   file reaches the shape. When mirroring any arm that takes a temp, mirror *when* the
   temp is taken, not just what is printed. **Still open, same shape:** the port's
   *slice*-read branch (`emit_expr`, TyData kind 8) also takes its temp before emitting
   the base, where the reference buffers first. Unreachable today (no corpus file has a
   temp-allocating slice base or index), so it is a latent divergence rather than a bug
   — a five-line restructure whenever something reaches it.

---

## 0. Discipline (unchanged — every increment)

- `cargo test` green (**700 default**, 757 with `--features selfhost-fixpoint`) +
  warning-clean; cross-impl goldens behind `--features c-oracle`; the
  fixpoint/self-build/seed family behind `--features selfhost-fixpoint`.
  **Auto-commit each green increment to `master` + `git push origin master`**
  (`git commit -F <file>`, Co-Authored-By trailer).
- One construct per increment with its golden slice. Never a big drop.
- **THE TWO-SIDED TAX (new since self-hosting — this is the thing that silently breaks):**
  any change to emitted C, an intrinsic, or a pass now has TWO implementations — the Rust
  reference (`src/*.rs`) and the port (`examples/std/*.jtr`). The full gate is:
  1. `jestyr_cgen_matches_reference` — **140** corpus files byte-identical;
  2. `jestyr_cgen_concat_matches_reference` + `jestyr_cgen_test_mode_matches_reference`;
  3. `selfhost_fixpoint_full` + `jestyr_driver_builds_itself` (the compiler must still
     compile ITSELF and must not trip its own refusal gates);
  4. `bootstrap_seed_is_current` — **whenever any `examples/std/*.jtr` source changes,
     rerun `REFRESH_SEED=1 cargo test --features selfhost-fixpoint bootstrap_seed_is_current`
     and commit the refreshed `bootstrap/` pair, else the drift guard fails.**
  New intrinsics/runtime fns must be GATED ON USE (the `uses_try_read` pattern) so every
  non-user program's C is byte-identical — and mirrored at all four port sites (gate
  helper + prelude line in cgen.jtr, helper-table entry, closure marker-name string,
  typeck.jtr return arm).
- **`.jtr` subset traps (all hit in real sessions — do not re-derive):** a `for`
  **dedup a per-type artifact by its MANGLED NAME, never by `TyId`** — the checker
  allocates a fresh `TyData` row per occurrence, so two `[]i32` sites hold different ids
  for one type; an id-keyed `seen` can agree in normal mode and double-emit in TEST mode,
  where the harness's extra code shifts the arena (Q-S2c); a `for`
  CONDITION may not start with `(` (parses as the zip-binding head — use
  `for i < n { if … { break } }`); a bare `{` block after a call-initializer parses as
  the `Name(args){…}` generic-ctor form (write flat, no scoping blocks); **a statement
  followed by a line starting with `[` parses as an INDEX, not a new statement** —
  `var a = [1, 2]` then `[a[0]]` on the next line is `[1, 2][a[0]]`, which refuses with
  "`a` is not a compile-time constant" and looks exactly like an interpreter bug (both
  implementations agree, because it is a parse; M7 lost time to this in a hand-written
  probe); NEVER chain
  `string_view(x).len` (bind `let v: str` first); `out`/`comptime`/`par`/`select` are
  reserved; author `.jtr` with the Write/Edit tools, never through shell heredocs
  (backslash mangling has produced real newlines inside string literals twice).
- The deep-dive levers: `DUMP_DIVERGE=1` on any cgen golden prints the first differing
  line; `TYPECK_FILE=<basename>` prints the aligned per-expr type stream.

## 1. Current state (what exists, so you don't rebuild it)

- **The self-hosted compiler** `examples/std/cgen.jtr` (**~12.7K lines**) + its 10 imports
  (`mem, intern, fs, env, list, tokens, parser, typeck, escape, sha256`). Full CLI:
  * `jc <file>` — raw C dump, the UNGATED golden mode (error files must keep emitting
    degenerately, so **never** add a refusal gate here);
  * `test [substr]` / `list [substr]` — the `@test`/`@bench` harness and its listing;
  * `build` / `run` — module-loading, refusal-gated, gcc-driving product modes;
  * `attest` — the FULL manifest (header + per-item records);
  * `<old> attest-diff <new>` / `<file> attest-verify <manifest>` — the breaking-change
    gate, exit 1 on a breaking change;
  * `doc` — the Markdown API page (single-file, typeck-free).
- **In-language modules**: the `ml_*` flatten loader (DFS deps-first, token-level
  rewrite, cross-module collision renames `name__<stem>`, `(merged, allsrc)` checkpoint
  map for exact per-file diagnostics).
- **The `_cli` split**: a module with `main` cannot be imported, so every pass that needs
  both a library and a dump driver has one — `parser_cli`, `typeck_cli`, `escape_cli`,
  `doc_cli`. Reach for it rather than adding a dump mode to `cgen.jtr`.
- **Bootstrap**: `bootstrap/jestyr_flat.jtr` (20,857 lines) + `jestyr_seed.c`
  (**31,658 lines**) + README; gcc-only build verified live, seed self-reproduces
  byte-for-byte.
- **Driver v1 limits already recorded** (P5 handoff, increments 44–50): no
  `-Wl,--stack` in driver gcc invocations; exe suffix fixed `.exe`; cmd.exe wants
  backslash paths for `run`; some item-level parse malformations recover Error-node-free
  and degrade to gcc; refusal messages are generic (location exact).
- **`#line` — the one KNOWN C-emission divergence** (surfaced by increment 52's
  `attest-verify`, recorded here because nothing else exposes it): the reference's
  module path (`module::load` → `TypeInfo::debug` → `mark_line`) emits `#line <n>
  "<file>"` directives, and **the port emits none**. No existing golden sees it — all
  134 corpus goldens, the concat, and the fixpoint use debug-free single-file/merged
  ASTs — but it means `jestyrc attest <f>` and `jc <f> attest` disagree on `c-sha256`
  (the reference hashes the `#line`-bearing C; `examples/api_v1.jtr`-shaped files show
  27 such lines), and port-built binaries carry no source-line mapping. `jc attest` /
  `attest-verify` are self-consistent, so the drift gate works — it's only cross-tool
  hash comparison that can't match. **Its own increment when wanted:** the port already
  has the input it needs (the `Ml.map` checkpoint pairs give per-file line/col), but
  there is NO golden for the module path's C today — build that first (`jestyrc emit-c`
  vs `jc <file> build`'s `.c` over a multi-module fixture), then port `mark_line`'s
  placement + dedup, then refresh the seed (the seed's C would gain the directives).

## 2. O-tooling — ✅ COMPLETE (kept as the record of how, not as a worklist)

> Nothing here is outstanding. It stays because the *how* is reusable: both items were
> ports of a Rust pass into `.jtr` under the two-sided tax, and the notes below name the
> arena layouts, the sharing invariant, and the traps that cost time.

### O1. Attest manifest RECORDS + `verify` — **DONE (increments 51–52)**
`jc <file> attest` now emits the FULL manifest — header plus per-item records — byte-equal
to `attest::manifest` (golden `jestyr_driver_attest_manifest_matches_reference`, 10 corpus
files), via the ported `doc::{fn_sig, fn_guarantees, const_sig, extern_sig, ty_str}` family
(`at_*` in cgen.jtr). And `jc <old> attest-diff <new>` / `jc <file> attest-verify
<manifest>` reproduce `attest::diff(…).render()` byte-for-byte, exit code included
(golden `jestyr_driver_attest_diff_matches_reference`, every verdict branch). What follows
is the original plan, kept for its reference pointers:

```
<kind> <name>
  vis: pub|priv
  sig: <reconstructed signature>
  guarantee: <…>          (0..n lines)
```

- The reference collects them in `attest.rs::collect_records`; **`sig` and `guarantees`
  come from `src/doc.rs` (`doc::fn_sig`, `doc::fn_guarantees`) — a signature
  RECONSTRUCTOR from the AST, not a span slice.** Port plan: a `sig`-renderer in `.jtr`
  over the parser's `iar` param 7-tuples + ret TypeId (the port already renders C
  signatures — this is the Jestyr-syntax analogue; `fn_guarantees` reads
  `requires`/`ensures` spans (`far` extras), the error set, `@no_panic`, refined params).
- Records sort by `(kind, name)`; kinds: fn / const / struct / enum / trait / distinct
  (read `collect_records` for the exact set + struct-method handling).
- Then `verify`: `jc <file> attest-verify <manifest>` — re-render and diff (the Rust
  side is `attest --diff` in main.rs; mirror the drift-report format).
- **Golden**: full-manifest byte-compare vs `attest::manifest` over a handful of corpus
  files (contracts.jtr — requires/ensures; records.jtr — methods; errors.jtr — error
  sets), grown file-by-file like every other golden.
- **As built (notes for the next reader):** `collect_records` covers fn/const/enum/
  struct-record-union/extern only — trait/impl/distinct/import are NOT records (the C
  hash attests them), and struct METHODS never get their own record. Sorting is a
  selection sort by (kind, name) emitting each record as its slot settles. The differ
  keeps the manifest text owned by the caller and models `ParsedItem` as spans
  (`struct Am`); the `BTreeSet`/`BTreeMap` fields are re-derived from the guarantee
  lines on demand rather than stored, and set ORDER never matters because the change
  list gets the reference's total `(item, verdict, detail)` re-sort at the end — that
  one observation is what makes a map-free port exact.

### O2. `doc` in-language — **DONE (increments 53–54)**
`jc <file> doc` renders the Markdown API page byte-for-byte identical to
`doc::generate(src, stem, html=false)` over all 134 corpus files (golden
`jestyr_doc_matches_reference`), with the dangling-doc lint located exactly.

- **The blocker was cleared without touching the token stream.** `tokens.jtr` gained
  `pub struct RawDoc` + `pub fn collect_docs`, a SECOND pass that finds comments by
  scanning the **gaps between tokens** — trivia by construction, so it needs none of the
  string/char-literal handling the tokenizer has, and `tokenize` stays byte-identical.
  (The load-bearing case: `"/// not a doc /* nor this */"` inside a string literal is
  never in a gap.) Golden `jestyr_doc_trivia_matches_reference` pins kind, block-ness,
  the comment span and the text span against `Lexer::tokenize_with_docs` corpus-wide,
  plus a fixture for both demotions (`////`, `/***`), `/**/`, nested plain comments and
  a trailing doc. `examples/std/doc_cli.jtr` is its dumper (the `_cli` split again).
- The renderer lives in cgen.jtr beside attest (`dc_*` + the `at_*` sig renderers),
  which is what makes the sharing real: **`at_guarantee_phrases` is now the ONE
  extractor** — attest wraps it as `  guarantee:` lines, doc as the `- ` bullets — so
  the attested ABI can never drift from the rendered docs, exactly the reference's
  design. Added for doc: `at_struct_sig` / `at_enum_sig` (note the reference prints
  `struct` for a `record`/`union` too, and lists fields only, never methods).
- **Not ported** (deliberate, recorded): `--html` mode; the fenced-`Example` extraction
  (collected by the reference for future doctests, never rendered in Markdown — the
  fence STATE is ported, since a `#` inside a fence must not open a section); and the
  reference's snippet decoration on the dangling-doc warning (its message text and
  `file:line:col` are ported, the `|`-gutter source excerpt is not). Doc prose using
  non-ASCII whitespace would trim differently (`char::is_whitespace` vs ASCII).
- **The trap this increment cost time on:** a struct member's tag-1 (method) tuple holds
  its fn `ItemId` in slot **1**, not slot 2 (slot 2 is a field's name-span start). Reading
  the wrong slot indexes the item arena with a source offset — an out-of-bounds read that
  surfaces only as a bare `Assertion failed!` from the generated C.

## 3. Group 3 — the three Rust-side workstreams (each multi-week; nothing blocks on them)

These change the REFERENCE first; the port inherits each construct later as its own
increment chain (same two-sided tax). Recommended order: **G → L → Q** (G unlocks the
most; L wants G's comptime for layout queries; Q builds on both).

### G. CTFE + reflection (~10% — the biggest unlock)
**What exists:** generics/type-param substitution; const-expr DISCRIMINANTS evaluate;
**the comptime interpreter (increment 1 — DONE)**.
**Build (increment chain):**
1. ~~A comptime **interpreter over the AST** (`src/comptime.rs`)~~ — **DONE.**
   `Interp::{eval, eval_usize}` over `Value::{Int, Bool, Str, Unit}`: literals (all
   bases, `_` separators, escapes), unary, checked arithmetic, comparisons,
   short-circuiting `and`/`or`, bitwise/shifts, string concat + compare, `if`/`else`,
   blocks with `let`, `const` references, int-to-int casts, and calls of pure fns with
   recursion. **Totality is the design point** — a compiler that hangs on
   `const A = B; const B = A` is worse than one that errors: a step budget (`FUEL`), a
   call-depth cap, and const-cycle detection make every input terminate with a
   diagnostic. Anything outside the value domain (floats, structs, intrinsics) is a
   clean `EvalError`, never a guess.
   **Its first consumer closed a real silent-miscompile bug:** `typeck::eval_array_len`
   and `cgen::array_len` accepted only an integer LITERAL and silently returned `0`
   otherwise, so `[SIZE]i32` emitted a zero-length array, a type-mismatched
   initialization and `assert(_ix < 0)` on every access — with no diagnostic. Both now
   evaluate, and a length that cannot be evaluated is an error
   (`check_array_len` from `audit_type_id` for signatures, from the `Stmt::Let` arm via
   `check_type_array_lens` for locals — item audits never visit local annotations, which
   is what the first draft got wrong). Zero emission change: a literal folds to the same
   number, so all 134 corpus goldens, the concat (31,658 lines) and the seed are
   untouched, and **no port mirror is needed yet**. Tests:
   `comptime::tests::*` (15, incl. every totality bound) plus
   `comptime_folds_array_lengths_end_to_end` (const / arithmetic / pure-fn-call lengths
   through gcc) and `comptime_rejects_a_non_constant_array_length`.
   **Port note for later:** the moment a corpus `.jtr` uses a non-literal array length,
   the port's `typeck.jtr`/`cgen.jtr` must fold it too (the P3 typeck golden compares
   `[N]T` renderings) — keep such files out of `examples/` until that mirror lands.
2. ~~`comptime { … }` blocks~~ — **DONE (G2, `b063ca4`)**, reference side. Reuses the
   existing `comptime` keyword (it could never start an expression, so no new reserved
   word and no changed parse). Typed as and emitted as the literal it folds to. **No
   `comptime const`** — top-level `const` is already comptime-evaluated, and a second
   way to say it violates §8. The design point: *the body belongs to the interpreter
   alone* — cgen/escape/attrs/dharht never descend into it, which is why non-users are
   byte-identical with no gating flag and why "typechecks" cannot disagree with
   "evaluates". **No corpus file yet, deliberately** (see the port-mirror trigger below).
3. ~~Reflection~~ — **DONE (G3, `4a85bc6`)**, reference side, *declared shape only*:
   `@type_name(T)`, `@field_count(T)`, `@field_name(T, i)`, `@field_type(T, i)`.
   Answered by this compiler and folded to literals; field types render through
   `doc::ty_str`, so reflection cannot drift from the docs. Arguments must be
   compile-time constants — see findings 2–4 above for what that excludes and why.
4. ~~**The executable `build.jestyr`**~~ — **DONE (G4)**, closes K's last leftover.
   `jestyrc plan <script> [--build]`. The plan is a **pure function of an index**
   (`const targets` + `fn source(i)`/`fn output(i)`), not the imperative
   `exe(…)`/`test(…)` shape, because that needs comptime effects. Determinism is
   structural: a non-deterministic build script cannot be *written*, since the
   evaluator has no arm for a clock or an environment read. `examples/build.jestyr` is
   the demo; a `.jestyr` extension means no golden sweeps it.
5. ~~**Tier 5 — bounded generation**~~ — **DONE (G5)**, foundation laid.
   `jestyrc plan … --emit`: a script *computes* an artifact's bytes, the plan records
   it by SHA-256, the driver writes it only on demand. The design point is where the
   boundary sits — **the evaluator gained no new power**; it computed a string, and the
   *driver* places the file. So generation is a pure function the user chooses to
   apply, not an effect a script can perform. Reproducible, attestable, and bounded
   literally (size cap; an absolute or `..` path is refused rather than normalised).
   Deliberately *artifact* generation, not source-string injection: what a script
   produces is a file you can read, diff and hash before anything acts on it.

**The outstanding CTFE work, in the order it should be done:**

| # | Work | Notes |
|---|---|---|
| 1 | **Comptime `for`** | the natural partner to G6: today a table's *values* are computed but its *shape* is spelled out (`[f(0), f(1), …]`). Bounded by the same fuel budget — spend per iteration, exactly as `ArrayRepeat` now does |
| 2 | **Comptime-only functions** | unblocks tier-3 field *iteration* end-to-end (finding 2). Either this or (1) closes the "walk a struct's fields" story |
| 3 | **Port mirrors for G2/G3/G6** | the big one — see below |
| 4 | `@size_of`/`@align_of`/`@offset_of` as comptime values | after **L** (finding 3) |

**On the port mirrors (item 3).** Nothing is broken today: G2–G4 changed no emitted C
for any existing program, so all 134 corpus goldens, the concat, test mode, the
fixpoint, the self-build and the seed are green *without* a mirror. The trigger is
unchanged and still the thing to respect — **the first `comptime`/reflection corpus
`.jtr` file drags in `parser.jtr` + `typeck.jtr` + `cgen.jtr` mirrors and a seed
refresh**, because the P2/P3 goldens sweep every `examples/**.jtr` with no allowlist.
The mirror is genuinely large: it means writing a total comptime interpreter in the
`.jtr` subset (G1's ~700 lines of Rust, plus the tier-2/3 surface) and threading a new
expression kind through the port's integer-tagged AST. Land it as its own increment
chain, and only when a corpus file needs it. `.jestyr` build scripts are exempt — the
goldens key on the `jtr` extension.
**Port impact:** each comptime construct that changes emitted C needs its cgen.jtr
mirror + golden growth; constructs that only FOLD at check time still need typeck.jtr
parity (the P3 golden compares every expression's resolved type).

### L. Memory-layout pass — ✅ **COMPLETE** (kept as the record of how)
size/align computation, **field reordering**, **enum niche-packing**, and
pass-large-aggregates-by-`const*` (today `read` params copy).
**The byte-identity constraint is the whole game here:** reordering fields changes the
emitted C for every existing program, which would invalidate 135 golden files, the
concat, the seed, and attest hashes at once. Land it OPT-IN:
1. ~~A pure ANALYSIS pass (`src/layout.rs`) computing size/align/waste + a report mode
   (`jestyrc layout <file>`)~~ — **DONE (L1).** Zero emission change, as predicted.
   Two things the next increment should inherit rather than rediscover:
   * **The C compiler is the authority, and the test says so.** `layout_matches_c_sizeof`
     emits the program's real C, appends a probe `main` printing `sizeof`/`_Alignof`/
     `offsetof`, compiles it with the locked `CC_FLAGS` and diffs against the model. The
     reordering increment will be *reasoning about offsets*, so it needs them to be
     known-true rather than believed. Extend that test's file list with each new shape.
   * **Two gaps are admitted, not guessed** — a record says `(incomplete)` when a
     component is generic/opaque, and when the struct has **bit-fields**, whose packing
     is implementation-defined in C (the model would otherwise report 4 bytes for
     `struct Packed { a: u8 : 1, … }`, which really occupies 1). `compute` takes the AST
     as well as the table precisely because bit widths are syntax, not type — the same
     hook `@layout(auto)` will need.
2. ~~`@layout(auto)` opt-in per struct~~ — **DONE (L2a `8d04770` + L2b `bff09d8`, both
   sides, corpus 141).** See the ledger.
3. ~~Niche-packing behind the same attribute; by-`const*` `read` params behind
   `@abi(ref)`~~ — **DONE (`cea779a`), and one half of it turned out not to need
   building.** `@abi(ref)` is the real increment. **Niche packing already existed on
   master**, automatic rather than attribute-gated — which is the right design, because
   the optimization is *provably free* (a pointer's null bit-pattern is unused), and
   gating a free win behind an opt-in asks users to request something that costs
   nothing. What was actually missing was the **layout model** knowing about it; fixed
   in `fbef2fb`. **This is the third time in this repo a "build X" item has turned out
   to be "X exists, check master first"** (Q discarded two duplicate parallelism builds).
4. ~~`@size_of`/`@align_of`/`@offset_of` as comptime values~~ — **DONE (`fbef2fb`).**

**Workstream L is therefore complete.** What is left is genuinely optional polish, not a
blocked increment:
   * **`@abi(ref)` on methods** — refused today by the target list. Needs method-call
     sugar, bound method values, trait vtables and `dyn` fat pointers to all agree on
     the convention. That is the whole indirect-call surface, so it is its own increment.
   * **The `@layout(auto)` / `@abi(ref)` port mirrors for `@abi`** — `@layout(auto)` is
     mirrored; `@abi(ref)` is not, and does not need to be until a corpus `.jtr` uses it
     (the standing trigger rule).
   * **Bit-fields** remain admitted-unmodellable in both the report and `@offset_of`.
     Closing that means picking a C ABI to model, which makes the model
     target-specific — worth doing only with a reason.

**Port impact:** every opt-in construct mirrors into cgen.jtr when its corpus file
lands; the seed refreshes.

### Q. SIMD → GPU (increment 1 DONE — the deterministic-acceleration frontier)

**Check master before building ANY parallelism feature — Q has twice discarded
duplicate builds** (the parallelism memory + `PARALLELISM-HANDOFF.md`). What is
already there, confirmed in-tree this run, so nobody rebuilds it a third time:
`par_map`/`par_scan` (`examples/std/parallel.jtr`), `par_reduce`, the
`par for … reduce(r)` surface (`ExprKind::ParFor`, the four-arm typeck check against
`DETERMINISTIC_REDUCTIONS`), dynamic-N spawn, `@deterministic`, the `par for` SHA
canary, and **`@span`** — the checked work-span model in `attrs.rs` (a `Cost { k, j }`
lattice over the AST's loop structure; a sequential loop multiplies by `n`, a
`par for` contributes `log n`, so serializing a reduction is a compile error).

**What did NOT exist before this run: anything SIMD-shaped at all.** No `@simd`, no
vector lowering, no thermal facet. (`dharht.rs`'s `simd_tier()` is an unrelated
blueprint string behind `--features dharht-experiment`.) So the frontier starts at
increment 1, and the mission is *not* to reinvent data parallelism — it is to make
SIMD deterministic first, then use the proven SIMD contract as the gate to GPU.

#### The promise being extended
Today: **bit-identical across thread counts and chunk sizes**. This workstream adds
**lane widths**, and eventually **legal GPU tile schedules**, to that list — without
loosening the FP contract (`-ffp-contract=off -fno-fast-math`, `CC_FLAGS`, locked by
`fp_contract_tests`) by a single flag.

#### Q-S1 — the checked `@simd` legality pass ✅ **DONE (this run)**
`src/simd.rs` + the `@simd` attribute + `jestyrc simd <file>`. **Zero emission change**,
which is the whole reason it is first: a vector lowering rewrites the emitted C of
every vectorized program at once, invalidating the corpus, the concat, the seed and
every attested hash in one commit. So this increment decides *what is legal*, proves
that decision against the real vector backend, and a later one lowers only what is
already certified. Exactly L1's route (measure the layout, verified by the C compiler,
before reordering a field).

The determinism argument splits cleanly in two, and only the second half was open:

1. **The reduction is lane-count independent** — `par for` already admits only
   `sum`/`min`/`max`/`xor` over `i64`, exactly associative *and* commutative on machine
   integers. A lane tree of any width equals the serial fold, bit for bit. This is why
   float is *excluded* rather than discouraged: `-ffp-contract=off` constrains
   contraction, not order, and a horizontal float add reassociates.
2. **The body is a total, elementwise integer expression** — what `simd::classify`
   decides, via a deliberately small whitelist: literals, names, `+ - * & | ^ ~ ! <<
   >>`, comparisons, `and`/`or`, `if`/`else`, blocks with `let`. Everything else is
   rejected with its **own** named cause (`Call`, `Memory`, `Float`, `Trapping`,
   `ShiftAmount`, `LaneWidth`, `Control`, `Unsupported`) at the **innermost** offending
   span, so the diagnostic points at the division and not at the loop.

`@simd` is a **contract, not a switch** — the `@span` shape, not a `#pragma`: it
changes no C, and a refactor that slips a call, a division or a float into a hot body
becomes a diagnostic instead of a silent fall-off-the-vector-path. `@simd` on a
function with no `par for` is an *error*: an attribute that quietly means nothing reads
like a guarantee.

**Tests (21 new; 785 default green, warning-clean).** `simd::tests` (15) cover each
accept and each distinct rejection; `proptests::simd_legality` (6) cover the attribute,
its diagnostics, that `@simd` emits byte-identical C, and that the shipped
`examples/std/par_for.jtr` is certified — a positive corpus case **without a new corpus
file**. The soundness half is `simd_lanes_match_scalar_bit_for_bit`
(`--features c-oracle`): every certified body is evaluated scalar-wise and through GCC
vector extensions at widths **2, 4 and 8**, reduced by all four declared reductions,
and every answer must be identical. **gcc is the authority**, the same way
`layout_matches_c_sizeof` makes it the authority on layout.

#### Findings from Q-S1 (read before Q-S2 — each cost real time or nearly did)

1. **The scalar tail of a vectorized loop needs the *scalar* lowering of a select.**
   The oracle shares ONE `#define F(x)` between the scalar and vector runs so there is
   no transcription risk — but a select cannot be shared, because a GNU vector
   comparison yields all-ones/all-zeros per lane while a scalar comparison yields `1`,
   and `?:` is not defined on vectors at all. The first draft flipped `SEL` to the mask
   blend for the whole vector section, tail included, and the tail (1003 elements, not
   a multiple of any width) silently computed garbage. **The lowering increment will
   have exactly this bug available to it**: `Q-S2` must emit the scalar select for the
   remainder loop, and `N % W != 0` must be in its first test, not its last. That the
   oracle *caught* it unprompted, with a precise message, is the teeth-verification
   this increment would otherwise have had to stage.
2. **`and`/`or` are legal only because the sublanguage is total.** A lane blend
   evaluates both sides; scalar short-circuits. That is value-preserving *only* because
   every faulting form (`/`, `%`, indexing, calls) is already excluded. **Admit one
   partial operation and the short-circuit operators stop being safe in the same
   motion** — the two rules are a single invariant wearing two hats. Same argument
   licenses `if`/`else` as a select.
3. **Division is the rule with teeth, and the reason is not the obvious one.** A vector
   computes every lane, so a divisor that is zero in a lane the scalar run would have
   skipped becomes a real SIGFPE — plus `INT64_MIN / -1`. `if x != 0 { 100 / x } else
   { 0 }` looks guarded and is not.
4. **A syntactic pass is sufficient here, and the argument for why is load-bearing.**
   Attributes are validated in the *parser*, before types exist (the constraint `@span`
   already works under), so `@simd` cannot consult `TypeInfo`. That is sound anyway:
   the loop variable is `i64`, and the only two routes from an integer to a float — a
   cast and a call — are both rejected, so **no float can depend on the loop variable**,
   and any float that could survive into a certified body is loop-invariant, broadcast,
   never reduced. `no_float_can_depend_on_the_loop_variable` pins both routes. **If
   either is ever admitted, the float exclusion needs types to stay sound.**
5. **Span containment beats a bespoke walker.** `sites_in_span` finds a function's
   `par for` loops by filtering the flat expression arena on span nesting, so no walker
   needs maintaining as new expression forms land — and a form that *contains* a
   `par for` cannot hide one from the check.
6. **ONE classifier, two consumers** — `@simd` and `jestyrc simd` both call
   `simd::classify`, so the report can never certify a loop the attribute rejects. The
   same rule that keeps M4's reflected/documented/attested type renderings from
   drifting (`at_ty`).
7. **The pass certifies a body cgen cannot lower — one form, and it is the interesting
   one** (found by Q-S2a). `classify` accepts `if c { a } else { b }`, and the *vector*
   half really does lower (a mask blend, written and tested). The **scalar remainder**
   cannot: cgen refuses a value-position `if`/`else` with "this control-flow expression is
   only supported in statement or return position". **It is diagnosed, not silent** —
   `emit-c` prints cgen's total-path `0` placeholder without surfacing cgen diagnostics,
   which is what made it read as a miscompile on first sight. So the legality pass is
   *optimistic relative to the backend* in exactly one place, which contradicts finding 8
   below until one of them moves. Fix by teaching cgen value-position control flow (the
   comptime-aggregate path already shows the shape) — that also fixes
   `let a = if c {…} else {…}` generally — or by narrowing `classify`.
8. **The pass is conservative, never optimistic**, and only one direction is a
   soundness claim. A rejected body may well vectorize; nothing depends on that, and it
   is not tested. Say this out loud in any diagnostic-tuning increment.

#### Q-S2 — deterministic SIMD lowering (**STARTED — Q-S2a has landed**)
`@simd` has flipped from *contract* to *contract + opt-in lowering*, for certified loops
only. Non-annotated programs stay byte-identical, so the corpus moves only for files that
opt in — the `@layout(auto)` discipline. **What Q-S2a emits**, for a certified site:

```c
size_t _pi0 = 0;
for (; _pi0 + 8 <= _pf0.len; _pi0 += 8) {                       /* vector head */
  JestyrVec_i32 _pv0; memcpy(&_pv0, _pf0.ptr + _pi0, sizeof(JestyrVec_i32));
  JestyrVec_i32 _pw0 = ((JestyrVec_i32){0}) + (<body over _pv0>);
  for (size_t _pk0 = 0; _pk0 < 8; _pk0++)
    _pm0[_pi0 + _pk0] = (int64_t)_pw0[_pk0];                    /* lane-by-lane widen */
}
for (; _pi0 < _pf0.len; _pi0++) {                               /* SCALAR remainder */
  int32_t j_x = _pf0.ptr[_pi0]; _pm0[_pi0] = (int64_t)(<body>);
}
```

Three decisions worth not re-deriving. The `((VT){0}) + (…)` wrapper **forces vector-ness**,
so a body that is a bare literal still yields a vector to subscript. Results are stored
**lane by lane** rather than converted as a vector, which keeps the `int64_t` widening exact
at every element width with no `__builtin_convertvector` dependency. And the remainder is
scalar *code* using scalar *forms* — a mask blend is not a conditional and a lane comparison
is not `0`/`1`, which is exactly the bug Q-S1's oracle caught in its own harness.

- Emit GCC vector extensions (`__attribute__((vector_size(N)))`), **not** `#pragma omp
  simd`, no `-march` change, no new `CC_FLAGS`. The lowering must be *chosen*, not
  begged for, or determinism is at the optimizer's discretion.
- **Lane count comes from the ELEMENT type, and Q-W1 made that a real choice**: a
  `par for` over `i32` fills twice the lanes of one over `i64`, `u8` eight times. The
  loop variable already carries the element's own type and only the per-element
  contribution is widened, so the vector body has the narrow type to work with — the
  scalar path is `{ecty} j_x = _pf.ptr[i]; _pm[i] = (int64_t)(body);` and the vector
  form has to preserve exactly that split.
- Shape: vector head + lane fold + **scalar remainder** (finding 1). Lane width is a
  fixed, recorded constant, not a host probe — a host-dependent width would make the
  emitted C depend on the build machine, which the attest hash would (rightly) flag.
- **Two-sided tax applies in full** for the first time in this workstream: an annotated
  corpus file drags in `parser.jtr` (attribute already generic), `typeck.jtr` and
  `cgen.jtr` mirrors plus `REFRESH_SEED=1`. Keep the first annotated file out of
  `examples/` until the mirror lands — the same trigger the CTFE M-series demonstrated.
- The determinism test already exists in the form it will need: extend
  `simd_lanes_match_scalar_bit_for_bit` to compare **`jestyrc run` of the annotated
  program against the unannotated one**, and add the annotated demo to the c-oracle SHA
  canary's demo set so lane width joins thread count and chunk size in the pinned digest.

#### Q-S3 — the `@span` thermal/energy facet (CJC CANA/PINN inheritance)
**Adapt, do not reinvent** (`MOTLEY.md` Part III; CJC `crates/cjc-cana`).

- Feed a `PhysicalCostQuery`-shaped record — flops, bytes read/written, allocation and
  working set, threads/**lanes**, batch/tile shape, and **float-op density**
  (`float_ops/flops`, PINN v2's dominant feature, corr ≈ +0.95) — into the closed-form
  v1 model (`heat = norm(flops)·(1+thread_amp·Δthreads)·(1+batch_amp·Δbatch)`, then
  `thermal = clamp01(heat·(1−cooling_rate))`, `cooling_rate = 0.05`, **no FMA, every
  product named**).
- `simd::Verdict::Legal { ops }` already carries the lane-op count *because* this is
  coming: it is the `flops` input, computed once, by the same classifier.
- **Deterministic or it does not ship**: no profiling, no clocks, no host probes, no
  `Date`/`rand` anywhere in the model. Container/cgroup/TDP inputs are future-facing and
  must be *explicit and recorded* — a normal build may not become host-dependent.
- **Ranking authority only, never legalization.** The facet may rank or diagnose legal
  lowerings ("this vectorization is thermally worse — here is the number"); it may never
  make a nondeterministic one legal. That veto split is CJC's own (`legality.rs` holds
  the veto; `pass_ranker.rs` only scores), and it is the line that keeps a *model* from
  quietly becoming a *policy*.
- Carry CJC's known debt forward rather than rediscovering it: the model is
  **hardware-generic** (per-window heat accumulation, not watts), and the energy
  residual is nonlinear — a linear head caps near R² 0.82.

#### Q-S4 — the GPU contract (design now, implement after Q-S2 is proven)
Write the deterministic contract while SIMD is fresh; build nothing GPU-facing until
the SIMD contract has tests behind it. The contract to state: **bit-identical across
every *legal* tile schedule**, by the same two-part argument — an exactly-associative
reduction plus a total elementwise body — with the tile/block shape occupying the role
lane width has here. `simd::classify`'s whitelist is the natural seed for the kernel
subset; a gather (`Reason::Memory`) is the first rule GPU will want to relax, and it
must be relaxed *with* its determinism argument, not before it.

#### Non-negotiables for every increment in this workstream
No fast-math, no reassociation, no FMA-dependent behaviour, no schedule-dependent float
results. No OpenMP pragmas, no work-stealing runtime, no hidden scheduler, no
nondeterministic reduction (not even opt-in — `PARALLELISM-HANDOFF.md` §"What NOT to
build" is still binding). Every user-visible feature ships with a determinism test that
proves scalar and accelerated paths are bit-identical. Emission changes bring goldens,
port mirrors and a seed refresh with them, in the same commit.

## 4. Sequencing (the one-line plan)

**Done:** ~~O1 records~~ (51–52) → ~~O2 doc~~ (53–54) → **workstream O complete** →
~~G1 the comptime interpreter~~ (`ebf8397`) → ~~G2 `comptime` blocks~~ (`b063ca4`) →
~~G3 reflection~~ (`4a85bc6`) → ~~G4 `build.jestyr`~~ (`29bd2bd`) → ~~G5 bounded
generation~~ (`bce5456`) → ~~G6 aggregate values / comptime tables~~ → ~~G7 comptime
`for` + mutation~~ — **the CTFE tier ladder (0–7) is done on the reference side**, and
documented in `docs/ctfe-tiers.md`.

**Port-mirror state: tiers 2, 3 and 6 are CLOSED on both sides** (M1 parse → M2 the
interpreter → M3 fold + emission → M4 reflection; M5 fixed the def-order divergence M4
surfaced; **M6 aggregates**). `ctfe` is the 12th module of the self-host closure, so the
flattened compiler and the bootstrap seed both contain a comptime evaluator — one that
now computes tables, not only numbers — and the shared type renderer. Tier 7 (the
comptime `for` + mutation) is the one interpreter tier still reference-only; tier 6's
*emission* (an aggregate `const` as a C static) is likewise still reference-only, and is
what will actually grow the corpus.

**Next, in order:**

1. ~~**The G6 port mirror — the representation change.**~~ — **DONE (M6).** The
   prediction held exactly: designing the representation *was* the increment, and the
   evaluation logic followed from the reference. The answer is a pair index into `lidx`
   naming a run of cells in `ltag`/`lval` — a tree in flat arenas, `Value` still a tag
   plus one `i64`. What the design had to decide rather than inherit was **staging**
   (elements go on a stack and move into the arena as one run when the literal closes,
   because a nested literal would otherwise interleave its cells with the run being
   built around it) and **when sharing becomes observable** (it is already present at
   `[v; n]`, and tier 6 simply has no way to detect it).

   ~~**G7 — the comptime `for` + mutation.**~~ — **DONE (M7). The CTFE ladder 0–7 is now
   closed on both sides.** The stated obligation came due exactly as written, and with
   one point more than the note had counted: the deep copy is owed at THREE places, not
   two — reading out of a binding, reading an element, and each element of `[v; n]`.
   That third one is what `[[0; 2]; 3]` turns on. The predicted payoff also landed: a
   `Place` is an env row or a cell index, so the write path is an address computation.

   ~~**Then M8: cgen emission + the first aggregate corpus file.**~~ — **DONE.** The
   brace-vs-statement-expression split was the detail with teeth, exactly as predicted,
   and both paths are mirrored; `examples/comptime_table.jtr` is corpus **138**.

   **The CTFE workstream (G) is therefore COMPLETE on both sides**, tiers 0–7, with the
   ladder documented in `docs/ctfe-tiers.md`.

**Next, in the order the user set:**

1. ~~**The lexer dogfood.**~~ — **DONE (M9).** `tokens.jtr`'s classifiers are one
   comptime-computed 256-entry table, the fixed point still holds (jc lexes its own
   source with a table its own evaluator computed), and the token stream is unchanged
   corpus-wide. **The verdict on the feature was measured, not asserted:** expressiveness
   yes — one declarative table replaces six functions that cross-called each other to
   avoid restating rules, and it forced out a conflation (`_` is an identifier character
   *and* separately the `1_000` separator; the branch chains got the right answer only
   by call order). Performance: a wash. Each classification is now a bounds-checked load
   where it was register comparisons, but both self-hosted compilers time at ~163 vs
   ~170 ms median on a 12.7K-line file with a 148–191 ms spread in both — indistinguish-
   able, because lexing is a small fraction of a compile. **The missing tool this named:
   there is no lexer-only benchmark**, so a per-stage cost like this cannot be resolved
   today; `selfbench` times the Rust reference's stages, not the port's.

2. ~~**Widen `par for` past `i64`.**~~ — **DONE (Q-W1), and the assumed blocker was not
   real.** This note had recorded the cap as following from "`spawn` targets cannot be
   generic", a constraint in workstream N. It does not: `emit_par_for` is
   map-then-reduce, so `core.par_reduce` only ever consumes the `int64_t` buffer and
   never sees the source slice. The restriction was a typeck rule. `par for` now
   iterates any integer element type with the loop variable carrying that type, the
   reduction domain stays `i64`, and the `i64` path is byte-identical — so no port
   mirror or seed refresh was due. **The lesson generalises: when a limit is attributed
   to another workstream's constraint, check whether the lowering actually depends on
   it before planning around it.**

   **Closed on both sides by Q-W2**: the port mirror plus `examples/std/par_for_width.jtr`
   (corpus **139**) — an `i32` sum-of-squares over an uneven chunk count checked against
   the serial fold, an identity max, and a `u8` sum, run on real OS threads 8× with
   identical tokens. Nothing here is outstanding; Q-S2 can start.

3. ~~**L2 `@layout(auto)`** … then L3 (niche packing, by-`const*` `read` params).~~ —
   ✅ **COMPLETE, both sides. WORKSTREAM L IS DONE.** L2a/L2b landed the opt-in field
   reordering (corpus **141**), `@abi(ref)` landed the by-`const*` `read` params, and
   `@size_of`/`@align_of`/`@offset_of` became comptime values. Niche packing needed no
   building — it already existed, automatically, and only the *model* was unaware of it.
   See the L section and the ledger for the findings; the three worth carrying into any
   workstream are:

   * **Cross-checking two implementations finds divergence, never shared blindness.**
     `the_two_layout_models_agree` caught the union bug on its first run and could not
     have caught the niche one, because both models were wrong together. Only an
     external authority (gcc) closes that class — the same argument
     `layout_matches_c_sizeof` was built on, now generalized.
   * **A Jestyr *place* is not automatically a C lvalue.** A bounds-checked index lowers
     to a GNU statement expression, which yields a value. Anything that wants to take an
     address must reason about the **emission**, not the source form.
   * **An attribute check that needs context beyond its item must run later.** Struct-body
     checks after the body parses (`validate_struct`); whole-program checks after the
     program does (`validate_program`). Written at the declaration, both silently pass
     the very programs they exist to reject.
4. ~~**Q-S2 deterministic SIMD lowering**~~ — ✅ **COMPLETE, both sides.** `@simd` lowers
   certified `par for` loops to GCC vector extensions (vector head + scalar remainder),
   the port mirrors it byte-identically, `examples/std/par_for_simd.jtr` is corpus **140**,
   and **lane width is pinned in the cross-OS SHA canary** alongside thread count and
   chunk size. The classify/cgen disagreement is closed — see the ledger for why the
   select was fixed in the backend while the multi-statement block narrowed the pass.

   **What remains in Q, and it is all deliberately deferred, not blocked:**
   * **Q-S3 — the CJC CANA/PINN thermal/energy facet on `@span`.** Feed a
     `PhysicalCostQuery`-shaped record (flops, bytes r/w, alloc, working set,
     threads/**lanes**, tile shape, float-op density) into the closed-form v1 model.
     `simd::Verdict::Legal { ops }` already carries the `flops` input, computed by the
     same classifier. **Deterministic or it does not ship** — no profiling, no clocks, no
     host probes. **Ranking authority only, never legalization**: the facet may say a
     lowering is thermally worse, never that a nondeterministic one is allowed. That veto
     split is CJC's own (`legality.rs` holds the veto, `pass_ranker.rs` only scores).
   * **Q-S4 — the GPU tile-schedule contract.** State it while SIMD is fresh:
     bit-identical across every *legal* tile schedule, by the same two-part argument
     (exactly-associative reduction + total elementwise body), with tile shape playing
     lane width's role. `simd::classify`'s whitelist is the kernel subset's seed; a gather
     (`Reason::Memory`) is the first rule GPU will want to relax, and it must be relaxed
     *with* its determinism argument, not before it. Jestyr emits C, so a GPU target is a
     genuinely new backend (OpenCL C / SPIR-V) — a months-scale lift, not an increment.
   * **Smaller, opportunistic:** SIMD over `u8`/`i16` bodies exists but has no corpus file;
     SOAC growth (`par_filter`, fused `par_map_reduce`); the `with schedule(…)` split,
     mostly enabled by N's dynamic-N spawn.

`@size_of`/`@align_of`/`@offset_of` as comptime *values* come after L (L1 unblocked the
computation; exposing it is its own slice). Comptime-only functions are a *convenience*,
not a blocker (finding 2). `#line` (§1) stays an independent, optional increment.

Keep every increment two-sided-green:
**corpus 140** + concat + test-mode + fixpoint + self-build + refreshed seed.

**The port-mirror trigger — now DEMONSTRATED, not just predicted.** It said: the moment a
`comptime` `.jtr` lands in `examples/`, the P2/P3 goldens sweep it with no allowlist and
the port must parse, check and emit it. That is exactly what M1–M3 did, and the predicted
order (reference side + Rust-only tests → mirror → corpus file) held. **Tier 2 is now
closed on both sides**: `examples/std/ctfe.jtr` is the self-hosted interpreter, `ctfe` is
the 12th module of the self-host closure, `examples/comptime_block.jtr` is corpus file
**135**, and the bootstrap seed carries a comptime evaluator.

What the next mirror should expect, in the order it will hit them: the parser needs its
`ref_dump_expr` arm in the harness (finding 6-adjacent — a missing one reads as a
divergent port); a block-led form needs its `dump_types` **block-shim** entry (finding 9,
whose signature is an *accumulating* drift); anything legal in an array-length position
needs `eval_array_len`/`push_array_len` folding on both sides (finding 10); and a new
`import` in `typeck.jtr`/`cgen.jtr` must be added to `SELFHOST_MODULES` in dependency
order or the concat build fails immediately (fast, not a divergence — a build error).
(`.jestyr` build scripts stay exempt: the goldens key on the `jtr` extension.)

**Tier 6 in the port was a REPRESENTATION change, and it is now done (M6).** `ctfe.jtr`'s
`Value` is still a tag plus one `i64`, Copy and allocator-free; a list payload is a pair
index into `lidx`, naming a run of cells in `ltag`/`lval`, and a cell tagged 4 names
another run — the reference's owned tree with ownership expressed as an index. The two
decisions that were not inherited from the reference: elements are **staged** on a stack
and moved into the arena as one contiguous run when the literal closes (otherwise a
nested literal interleaves its cells with the run being built around it), and **sharing
is already present at `[v; n]`** but is unobservable while nothing can write through a
handle. Tier 7 makes it observable and therefore owes a deep copy at the two points the
reference clones — binding a list, and reading one out of the environment — while
getting an easier write path in exchange, since walking an index path over a flat arena
is an address computation rather than a chain of `&mut` borrows.

## 5. V1 tier status — **verified against the tree**, not inherited

A status table circulating with an earlier handoff marked several finished things as
`NOT DONE`. That is the most expensive kind of stale note in this repo — Q discarded two
duplicate parallelism builds for exactly this reason — so the corrections are recorded
here with what actually exists. **Check this table, then check master, before building
anything below.**

| Row as previously reported | Reality |
|---|---|
| Cost Visibility tier 2 *layout report* — NOT DONE | **DONE.** `src/layout.rs` + `jestyrc layout <file>`, verified against gcc. |
| Cost Visibility tier 3 *`@layout(auto)`* — NOT DONE | **DONE, both sides** (`8d04770`, `bff09d8`), corpus 141. |
| Cost Visibility tier 4 *`@abi(ref)`* — NOT DONE | **DONE for free functions** (`cea779a`); methods refused by the target list, and that is the follow-up. |
| Cost Visibility tier 5 *SIMD* — NOT DONE | **DONE** (`4339d72`, `8f0e948`) — `@simd` lowers to GCC vector extensions, both sides, lane width pinned in the cross-OS canary. |
| Allocation tier 5 *`no_alloc`* — transitive NOT DONE | Direct/intraprocedural is enforced; **transitive is genuinely open.** |

**So the Cost Visibility ladder (tiers 0–5) is COMPLETE**, and workstream L with it.

### What is genuinely still open, in the order it is worth doing

These are the rows that survive the correction above. Each is its own increment chain
under the standing two-sided tax, and **none blocks any other**. Most have since been
closed; they are struck through with their commits, so the ordering rationale survives.

1. ~~**Error handling tier 3 — `catch` / recovery blocks.**~~ — **DONE on the reference
   side (`b67f2c2`).** `e catch fallback`: recovery, the half of the error story that
   decides *who* handles a failure, and the reason `catch` is legal in an **infallible**
   function where `?` is not. Right-associative, so a chain tries each alternative in
   turn; precedence between assignment and the binary operators. The fallback is
   evaluated **only** on the error path — a guarantee, not an optimization, which is why
   it lowers to C's conditional operator and is checked by *running* a fallback that
   prints. `catch` on an infallible expression is refused rather than accepted as a
   no-op. See `docs/error-handling.md`.
   **The port mirror is now DONE too** *(this session)* — parser.jtr (kind 45,
   `parse_catch_tail` mirroring the reference's `parse_assignment` → `parse_catch` →
   `parse_binary` layering), typeck.jtr (Result-unwrap to the ok type), cgen.jtr (the
   conditional lowering, with base and fallback emitted into buffers **before** the
   `_ct{n}` temp is allocated — the buffer-first temp-order rule X1 recorded). All
   byte-identical on the first run; `examples/error_catch.jtr` is corpus **145**; seed
   refreshed. Two things worth carrying: the reference's expression **collectors**
   (structs/moves/refs/closures/calls) each needed a `Catch` arm or a struct/closure/
   generic used *only in a fallback* would be silently dropped — grep `ExprKind::Try`
   to find every walker a new expression form owes an arm; and the P2 dump harness's
   catch-all prints `error` for an unhandled kind, so its `catch` arm went in **with**
   the construct, not after a golden failed.
   **`catch |e|` is now DONE on BOTH sides** (see the START HERE list) — the port
   mirror landed with the first corpus use, in `error_catch.jtr` itself.
2. ~~**Allocation — transitive `@no_alloc`.**~~ — **DONE (`f0e579d`).** The design
   question this note flagged (what to do at a call to an unannotated function) resolved
   as predicted: *infer per body* + a least fixpoint, since assume-allocates would make
   the attribute unusable. The implementation reuses the **existing** per-op rules by
   running the real checker as a probe, rather than restating "allocates" in a second
   walker that could drift. Known boundary, documented: free functions by name only —
   methods, closures and `fn(…)` pointers are not in the graph.
3. ~~**Diagnostics tier 5 — machine-readable JSON.**~~ — **DONE (`d6e34ba`).**
   `jestyrc check <file> --json`. See `docs/diagnostics-json.md` for the contract and the
   two deliberate non-choices (object not array; emission order not sorted).
4. ~~**Diagnostics tiers 2–3 — suggested rewrites.**~~ — **DONE (`b67f2c2`).** A store
   escape now names all three Jestyr answers (`take` / `region` / `genref`), because
   which one is right depends on whether the value is moved, long-lived or shared and
   the compiler cannot know that; a returned *closure* gets different advice; a
   non-constant array length is told about `const` **and** `comptime { … }`.
   **The invariant that made it free, and worth reusing:** suggestions go in `help`,
   never in the message, and the P4 escape golden compares span + **message** — so
   suggestions can be improved on the reference side forever without owing a port
   mirror. `adding_a_suggestion_does_not_change_any_message` pins it.
5. **Error handling tier 4 — debug error traces.** Now the largest remaining *feature*.
6. ~~**Correctness tier 5 — `@verified`.**~~ — **planning slice DONE (`fb18927`).**
   `src/obligations.rs` + `jestyrc obligations <file>`, and it produced the number the
   sizing decision needed: **7 declared obligations across the whole 144-file corpus.**
   So an SMT backend would have almost nothing to discharge, and the real prerequisite
   for `@verified` is **writing contracts**, not building a solver. Pinned as an *upper*
   bound (contracts should grow), firing at 100. See `docs/obligations.md`.
   **Still open:** the solver itself — now correctly sized as *not yet worth building*.
7. **Unsafe/provenance v2.** Start as documentation plus lints, per the design note —
   a written contract for raw-pointer validity, aliasing, and the C interop boundary,
   with diagnostics that enforce the parts that are checkable today. **The largest
   remaining item on this list.**
8. **The `#line` port** (§1) — its **prerequisite golden is now built** (`d6e34ba`,
   `jestyr_module_cgen_matches_reference_except_line_directives`), and building it
   corrected §1: **`#line` is not the only module-path divergence, there are three.**
   See the golden's doc comment. The emission port itself is still open, and the golden
   tightens in three steps as it lands (drop the `#line` filter, drop the task-name
   normalizer, set `strict_order` for every root).

### Also closed in passing

* **`xs[i].field = v` did not compile** — closed by **X1/X2** (the ledger's top two rows),
  *not* by this workstream. It was fixed twice, in parallel sessions: X1/X2 landed the
  complete version (both sides, corpus file, and the `mut`/`out` by-address half), and the
  duplicate reference-only fix was **dropped during the merge** rather than reconciled.
  Nothing of it survives; the ledger rows are the record.

### Integration note — two parallel sessions were merged here

The L workstream and the X1/X2 place-lowering work were developed **concurrently from
`8f0e948`** and merged by rebasing L onto X. Four things a future merge should expect,
because none was obvious in advance:

1. **The duplicate commit was dropped, not merged.** Two sessions independently fixed
   `xs[i].field = v`. X1's was strictly better (both sides + corpus file), so the
   reference-only twin was skipped during the rebase. *Check `origin/master` before
   starting a fix* — the same lesson workstream Q learned three times.
2. **The two `cgen.rs` changes composed, but only after a hand merge.** X2 replaced
   `&({e})` with `emit_addr_arg` for `mut`/`out` arguments; L3 added an `@abi(ref)`
   branch to the *same* two argument loops. The merged form is a three-way `if`
   (`mut`/`out` → address, `@abi(ref)` → `const T*`, else → value) at both sites. Git
   could not do this: both sides rewrote the same expression.
3. **The bootstrap seed's automatic merge was WRONG, and silently so.** `bootstrap/` is
   *generated*, so a textual three-way merge of two independently regenerated seeds
   produces a file that is neither. It auto-merged without conflict and had to be
   regenerated (`REFRESH_SEED=1`), which changed both files. **Never trust a merged
   seed — always regenerate and re-verify.**
4. **Both sessions added a corpus file** (`nested_place.jtr`, `layout_auto.jtr`) and both
   commit messages say "corpus 141". The allowlist now holds **139** entries against 144
   `.jtr` files on disk; the prose counts in the ledger have drifted from the allowlist
   and should be read as approximate. `CGEN_GOLDEN_ALLOWLIST` is the authority.

The full gate was re-run after the merge, not assumed: **839 default tests**, all three
cgen goldens (corpus + concat + test-mode), the fixpoint, the self-build, and a
regenerated seed.

**One standing caution for all of them:** anything that changes emitted C, an intrinsic,
or a pass owes the port mirror **and** a seed refresh in the same commit (§0). Anything
that only adds a refusal, a report, or an opt-in attribute nobody in `examples/` uses
does not — that boundary is what let all four L increments land without touching the
seed until the corpus file did.

## One-line
Self-hosting is finished and productized, **workstream O is complete in-language**, and
**the CTFE tier ladder (0–7) is now done on the reference side** — a total comptime
interpreter that closed a silent zero-length-array miscompile, `comptime { … }` in user
syntax, reflection over the declared shape in the collision-proof `@` namespace, a
`build.jestyr` that is *evaluated, never run*, bounded artifact generation, aggregate
values that make a computed lookup table an ordinary static, and a comptime `for` that
computes a table's **shape** as well as its values — a 256-entry CRC-32 table now
emerges from `for i in 0..256 { t[i] = crc_entry(i) }` as a plain C static, and the same
loop walks a struct's fields to generate a descriptor, which is what tier 3 had been
waiting on. The through-line is that **purity was never traded away to get power**: each
tier added a capability without giving the evaluator an effect, so determinism and
reproducibility stayed properties of the design rather than conventions to police — and
each tier's cost was one line of fuel accounting in a new place (per expression, per
element, per iteration). The ladder is documented in `docs/ctfe-tiers.md`. **Group 3's
other two workstreams have now opened the same way — by measuring before changing
anything**: L1 reports what your types cost with gcc as the authority on layout, and
**Q-S1 decides which `par for` loops may run a SIMD lane at a time, with gcc as the
authority on vectorization** (scalar ≡ 2 ≡ 4 ≡ 8 lanes, bit for bit, across all four
declared reductions) — `@simd` being a *checked contract* in `@span`'s shape, not a
`#pragma`, so it emits nothing and a body that stops vectorizing becomes a diagnostic
rather than a silent performance cliff. That the analysis-first ordering is not merely
cautious is now demonstrated twice over: L1's report is the thing reordering will
justify itself against, and Q-S1's oracle caught a real select-lowering miscompile in
its own harness before any lowering existed to be wrong. **Tier 6 is now closed in the
port too (M6)** — the self-hosted evaluator computes tables, not only numbers, and the
gcc-only bootstrap seed carries it; the representation question the note had flagged as
"the real work" turned out to be exactly that, answered by staging elements before
storing them and by naming precisely when a shared run becomes observable. What's left
is the **G7 port mirror** (the comptime `for` + mutation, which owes the deep copy tier
6 could defer) and **M8** (aggregate emission + the first corpus file), **L2** opt-in
layout, **Q-S2** opt-in SIMD lowering → the thermal facet → the GPU contract, and the
optional `#line` port — each landed increment-by-increment under the two-sided golden
discipline with the bootstrap seed refreshed at every `examples/std` change.
