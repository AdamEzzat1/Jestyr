# Latest results

- date: 2026-08-13 01:39
- rustc: rustc 1.97.1 (8bab26f4f 2026-07-14) / cargo 1.97.1 (c980f4866 2026-06-30)
- gcc: gcc.exe (x86_64-posix-seh, Built by strawberryperl.com project) 8.3.0
- jestyr: claude/rust-jestyr-ownership-benchmark-1bd785@321b9fb
- timing: interleaved min-of-7 runs, first round discarded

| case | track | runtime (ms) | compile (ms) | binary (B) | LOC | outputs match |
|---|---|---:|---:|---:|---:|---|
| transient_borrow | rust-std | 457.9 | 1305 | 128512 | 101 | True |
| transient_borrow | jestyr | 494.4 | 288 | 69068 | 91 | True |
| borrowed_projection | rust-std | 92.2 | 850 | 128000 | 62 | True |
| borrowed_projection | jestyr | 95.8 | 261 | 69941 | 56 | True |
| disjoint_mutation | rust-std | 535.4 | 860 | 128000 | 63 | True |
| disjoint_mutation | jestyr | 555.7 | 408 | 101843 | 59 | True |
| observer_registry | rust-std | 56 | 664 | 129024 | 109 | True |
| observer_registry | rust-idiomatic | 49.4 | 687 | 129024 | 63 | True |
| observer_registry | jestyr | 70.3 | 294 | 70838 | 74 | True |
| arena_ast | rust-std | 39.2 | 1077 | 128512 | 72 | True |
| arena_ast | rust-idiomatic | 38.6 | 1154 | 131072 | 85 | True |
| arena_ast | jestyr | 210.1 | 272 | 74971 | 77 | True |
| dlist | rust-std | 16.2 | 668 | 128000 | 82 | True |
| dlist | rust-idiomatic | 18.3 | 981 | 130048 | 80 | True |
| dlist | jestyr | 51.8 | 445 | 80102 | 101 | True |
| resource_capabilities | rust-std | 24 | 506 | 126976 | 35 | True |
| resource_capabilities | jestyr | 20.9 | 364 | 60841 | 33 | True |
| structured_concurrency | rust-std | 94.7 | 1287 | 159744 | 38 | True |
| structured_concurrency | rust-idiomatic | 100.5 | 1349 | 212992 | 21 | True |
| structured_concurrency | jestyr | 106.4 | 1447 | 226267 | 29 | True |
| unsafe_boundary | rust-std | 102.6 | 570 | 127488 | 48 | True |
| unsafe_boundary | jestyr | 110.5 | 336 | 66809 | 47 | True |
