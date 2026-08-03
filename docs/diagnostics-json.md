# Machine-readable diagnostics — `jestyrc check <file> --json`

The Diagnostics tier ladder's top rung: one JSON report on stdout instead of the human
caret rendering, for editors, CI, and anything that would otherwise have to parse
`error: …` prose.

```bash
jestyrc check examples/typeerr.jtr --json
```

```json
{"version":1,"diagnostics":[{"severity":"error","message":"no field `z` on struct `Point`","file":"examples/typeerr.jtr","line":8,"col":5,"endLine":8,"endCol":8,"code":null,"help":null}]}
```

## The contract

| Field | Meaning |
|---|---|
| `version` | Schema version (`1`). Bumped for any non-additive change, so a consumer can refuse a format it does not understand. |
| `severity` | `"error"` \| `"warning"` \| `"note"` — the same word the human renderer prints. |
| `message` | The diagnostic text, without the severity prefix. |
| `file` | The file that **owns** the diagnostic, not the root — forward slashes on every platform. |
| `line` / `col` | 1-based start position, matching the caret renderer and every editor. |
| `endLine` / `endCol` | 1-based end position, so a consumer can underline exactly what the caret renderer underlines. |
| `code` | A stable error code (`"E0042"`), or `null`. |
| `help` | The `= help:` suggestion, or `null`. |

Four properties are guaranteed, and each is tested:

* **Always emitted.** A clean program produces `{"version":1,"diagnostics":[]}`, not
  silence — a consumer never has to distinguish "no problems" from "the tool did not
  run". The **exit code** still reports success or failure.
* **Always valid JSON.** Messages quote user identifiers, rendered types and source
  text, so they can contain quotes, backslashes, newlines and control bytes. One
  unescaped character would make the *whole* report unparseable — losing every
  diagnostic, not one — so escaping is checked as a property over arbitrary strings
  (`a_diagnostic_always_renders_valid_json`) as well as over every corpus file
  (`json_diagnostics_are_wellformed_over_the_corpus`).
* **Deterministic.** Byte-identical across runs, so a report can be diffed and checked
  in like every other artifact this compiler produces.
* **Attributed per file.** A diagnostic from an imported module names *that* module, via
  the same span-to-region translation the human renderer uses.

## Two deliberate non-choices

**The shape is an object, not a bare array.** A top-level array cannot gain a field. The
object can grow a summary, timings, or a pass name without breaking a consumer that
already reads `diagnostics`.

**Diagnostics keep their emission order**, which is the compiler's deterministic pass
order — parse, then type check, then ownership. A tool that wants them sorted by position
can sort them; a tool that wants to know what the compiler saw *first* cannot recover that
if we sort here. Ordering is a consumer's policy, not the format's.

## Implementation notes

The escaper is hand-written (`diag::json_escape`) because the compiler has **zero runtime
dependencies** — a deliberate property, not an oversight. It escapes `"`, `\`, and the C0
controls (with the short forms for `\n`/`\r`/`\t`), and passes non-ASCII through, since
JSON is UTF-8 by definition and Rust strings are already valid UTF-8. `DEL` (0x7F) is not
a JSON control character and is left alone.

The test-side validator (`json_strings_wellformed`) is likewise hand-written, and checks
the one thing that can actually go wrong when emitting JSON without a library: a string
body that was not escaped.
