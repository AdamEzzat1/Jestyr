#!/usr/bin/env bash
# examples/distinct_corpus/record.sh — replay the `distinct` anti-regression corpus
# and print one TSV row per program:
#
#     <program>\t<verdict>\t<detail>
#
# verdicts
#   TYPECK_REJECT  `jestyrc check` exits non-zero. `detail` is the diagnostic text.
#   CGEN_REJECT    check passes; the backend refuses. `detail` is the diagnostic.
#   GCC_REJECT     check passes; the emitted C does not compile/link. `detail` is
#                  the first gcc/ld line. THIS IS A HOLE, NOT A REJECTION: the
#                  language accepted the program and the C compiler caught it.
#   RUN_OK         built and ran. `detail` is stdout, whitespace-collapsed.
#   RUN_FAIL       built, ran, exited non-zero. `detail` is stdout+exit code.
#
# usage:  bash examples/distinct_corpus/record.sh [path/to/jestyrc.exe] > HEAD.tsv
# diff the output against baseline-HEAD-e293e8b.tsv to see what a change moved.
set -u
HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
JC="${1:-$ROOT/target/release/jestyrc.exe}"

norm() {
  # collapse whitespace, strip absolute paths and temp filenames
  tr '\n' ' ' | sed -e 's#[A-Za-z]:[\\/][^ ]*[\\/]##g' -e 's/  */ /g' -e 's/^ //' -e 's/ $//'
}

for f in "$HERE"/*.jtr; do
  name="$(basename "$f" .jtr)"
  chk="$("$JC" check "$f" 2>&1)"; chk_rc=$?
  if [ $chk_rc -ne 0 ]; then
    detail="$(printf '%s' "$chk" | grep -m1 '^error:' | norm)"
    printf '%s\tTYPECK_REJECT\t%s\n' "$name" "$detail"
    continue
  fi
  out="$("$JC" run "$f" 2>&1)"; run_rc=$?
  if printf '%s' "$out" | grep -q 'gcc failed to compile\|ld returned\|undefined reference'; then
    detail="$(printf '%s' "$out" | grep -m1 'error:' | norm)"
    printf '%s\tGCC_REJECT\t%s\n' "$name" "$detail"
  elif [ $run_rc -ne 0 ] && printf '%s' "$out" | grep -q '^error:\|^ *error:'; then
    detail="$(printf '%s' "$out" | grep -m1 'error:' | norm)"
    printf '%s\tCGEN_REJECT\t%s\n' "$name" "$detail"
  elif [ $run_rc -ne 0 ]; then
    detail="$(printf '%s' "$out" | sed -n '3,$p' | norm)"
    printf '%s\tRUN_FAIL\trc=%d %s\n' "$name" "$run_rc" "$detail"
  else
    detail="$(printf '%s' "$out" | sed -n '3,$p' | norm)"
    printf '%s\tRUN_OK\t%s\n' "$name" "$detail"
  fi
done
