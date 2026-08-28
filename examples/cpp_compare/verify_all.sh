#!/usr/bin/env bash
# verify_all.sh — build every Jestyr/C++ pair in this directory, run both, and
# diff their output. Exits non-zero if any pair disagrees.
#
#   bash examples/cpp_compare/verify_all.sh
#
# Needs: a release build of the reference compiler (`cargo build --release` at
# the repo root) and `g++` on PATH. Works on Linux, macOS and Git Bash.
#
# `static_rejections` is skipped by the pair loop on purpose — it is a program
# that must FAIL to compile — and is checked separately at the end.

set -u
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../.." && pwd)"

jc="$repo/target/release/jestyrc"
[ -x "$jc" ] || jc="$repo/target/release/jestyrc.exe"
if [ ! -x "$jc" ]; then
  echo "error: no release compiler at target/release/jestyrc[.exe]" >&2
  echo "       run 'cargo build --release' at the repo root first" >&2
  exit 2
fi

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

pass=0; fail=0; failed=""

for jtr in "$here"/*.jtr; do
  b="$(basename "$jtr" .jtr)"
  [ "$b" = "static_rejections" ] && continue
  cpp="$here/$b.cpp"
  [ -f "$cpp" ] || continue

  # Flags mirror the README: floating-point contraction off wherever a program
  # prints a float, threads where a program uses them.
  extra="-ffp-contract=off -fno-fast-math"
  grep -q "thread" "$cpp" && extra="$extra -pthread"

  if ! g++ -O2 -std=c++17 $extra -o "$work/${b}_cpp" "$cpp" 2>"$work/$b.cpperr"; then
    echo "FAIL  $b — C++ did not build"; sed 's/^/      /' "$work/$b.cpperr" | head -5
    fail=$((fail+1)); failed="$failed $b"; continue
  fi
  if ! "$jc" build "$jtr" >"$work/$b.build" 2>&1; then
    echo "FAIL  $b — Jestyr did not build"; sed 's/^/      /' "$work/$b.build" | head -5
    fail=$((fail+1)); failed="$failed $b"; continue
  fi

  # `jestyrc build` prints:  built: <path> (via gcc)
  jexe="$(sed -n 's/^built: \(.*\) (via gcc)$/\1/p' "$work/$b.build" | head -1)"
  if [ -z "$jexe" ]; then
    echo "FAIL  $b — could not read the built path from: $(head -1 "$work/$b.build")"
    fail=$((fail+1)); failed="$failed $b"; continue
  fi

  "$jexe"            >"$work/$b.jestyr.out" 2>&1
  "$work/${b}_cpp"   >"$work/$b.cpp.out"    2>&1

  if diff -q "$work/$b.jestyr.out" "$work/$b.cpp.out" >/dev/null; then
    printf 'PASS  %-24s %s lines identical\n' "$b" "$(wc -l < "$work/$b.jestyr.out" | tr -d ' ')"
    pass=$((pass+1))
  else
    echo "DIFF  $b — the two languages disagree:"
    diff "$work/$b.jestyr.out" "$work/$b.cpp.out" | head -10 | sed 's/^/      /'
    fail=$((fail+1)); failed="$failed $b"
  fi
done

echo "----"
echo "$pass matched, $fail failed"
[ -n "$failed" ] && echo "failed:$failed"

# The other half of the comparison: programs C++ accepts and Jestyr refuses.
# This one must NOT check cleanly.
echo
echo "static_rejections (must be refused):"
if "$jc" check "$here/static_rejections.jtr" >"$work/rej.out" 2>&1; then
  echo "  FAIL — it compiled; the rejections have regressed"
  fail=$((fail+1))
else
  n=$(grep -c '^error' "$work/rej.out" || true)
  echo "  refused with $n error(s), as intended:"
  grep '^error' "$work/rej.out" | cut -c1-96 | sed 's/^/    /'
fi

[ "$fail" -eq 0 ] || exit 1
echo
echo "all pairs agree"
