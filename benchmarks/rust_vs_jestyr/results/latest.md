# Latest results

- date: 2026-08-13 01:10
- rustc: rustc 1.97.1 (8bab26f4f 2026-07-14) / cargo 1.97.1 (c980f4866 2026-06-30)
- gcc: gcc.exe (x86_64-posix-seh, Built by strawberryperl.com project) 8.3.0
- jestyr: claude/rust-jestyr-ownership-benchmark-1bd785@f675669
- timing: interleaved min-of-7 runs, first round discarded

| case | track | runtime (ms) | compile (ms) | binary (B) | LOC | outputs match |
|---|---|---:|---:|---:|---:|---|
| transient_borrow | rust-std | 468.8 | 1033 | 128512 | 101 | True |
| transient_borrow | jestyr | 452.5 | 342 | 69068 | 91 | True |
| borrowed_projection | rust-std | 96.2 | 916 | 128000 | 62 | True |
| borrowed_projection | jestyr | 99.4 | 505 | 70453 | 56 | True |
| disjoint_mutation | rust-std | 479.4 | 799 | 128000 | 63 | True |
| disjoint_mutation | jestyr | 559 | 687 | 101843 | 59 | True |
| observer_registry | rust-std | 56.1 | 533 | 129024 | 109 | True |
| observer_registry | rust-idiomatic | 50 | 499 | 129024 | 63 | True |
| observer_registry | jestyr | 70.7 | 358 | 70838 | 74 | True |
