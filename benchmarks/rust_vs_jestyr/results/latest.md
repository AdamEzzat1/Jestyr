# Latest results

- date: 2026-08-27 13:28
- rustc: rustc 1.97.1 (8bab26f4f 2026-07-14) / cargo 1.97.1 (c980f4866 2026-06-30)
- gcc: gcc.exe (x86_64-posix-seh, Built by strawberryperl.com project) 8.3.0
- jestyr: master@a23e826
- timing: interleaved min-of-7 runs, first round discarded

| case | track | runtime (ms) | compile (ms) | binary (B) | LOC | outputs match |
|---|---|---:|---:|---:|---:|---|
| transient_borrow | rust-std | 372 | 748 | 128512 | 101 | True |
| transient_borrow | jestyr | 393.9 | 990 | 69068 | 91 | True |
| transient_borrow | jestyr-std | 410.7 | 671 | 81608 | 92 | True |
| borrowed_projection | rust-std | 75.1 | 1405 | 128000 | 62 | True |
| borrowed_projection | jestyr | 77.8 | 559 | 69941 | 56 | True |
| borrowed_projection | jestyr-std | 84.5 | 703 | 77459 | 60 | True |
| disjoint_mutation | rust-std | 399.4 | 1434 | 128000 | 63 | True |
| disjoint_mutation | jestyr | 458.5 | 1037 | 101843 | 59 | True |
| observer_registry | rust-std | 42.1 | 447 | 129024 | 109 | True |
| observer_registry | rust-idiomatic | 41.5 | 585 | 129024 | 63 | True |
| observer_registry | jestyr | 54.8 | 661 | 70838 | 74 | True |
| observer_registry | jestyr-std | 53.6 | 544 | 76827 | 74 | True |
| arena_ast | rust-std | 32.4 | 717 | 128512 | 72 | True |
| arena_ast | rust-idiomatic | 31.6 | 1324 | 131072 | 85 | True |
| arena_ast | jestyr | 180.7 | 457 | 74971 | 77 | True |
| arena_ast | jestyr-std | 45.1 | 622 | 84304 | 76 | True |
| dlist | rust-std | 10.8 | 463 | 128000 | 82 | True |
| dlist | rust-idiomatic | 12 | 643 | 130048 | 80 | True |
| dlist | jestyr | 36.2 | 601 | 79590 | 101 | True |
| dlist | jestyr-std | 11.7 | 663 | 80996 | 97 | True |
| resource_capabilities | rust-std | 17.5 | 597 | 126976 | 35 | True |
| resource_capabilities | jestyr | 14.8 | 539 | 60841 | 33 | True |
| structured_concurrency | rust-std | 54.6 | 2727 | 159744 | 38 | True |
| structured_concurrency | rust-idiomatic | 55.5 | 1099 | 212992 | 21 | True |
| structured_concurrency | jestyr | 62.7 | 1271 | 226267 | 29 | True |
| unsafe_boundary | rust-std | 85.7 | 753 | 127488 | 48 | True |
| unsafe_boundary | jestyr | 95.1 | 497 | 66809 | 47 | True |
