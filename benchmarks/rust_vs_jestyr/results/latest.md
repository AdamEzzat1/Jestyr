# Latest results

- date: 2026-08-13 13:26
- rustc: rustc 1.97.1 (8bab26f4f 2026-07-14) / cargo 1.97.1 (c980f4866 2026-06-30)
- gcc: gcc.exe (x86_64-posix-seh, Built by strawberryperl.com project) 8.3.0
- jestyr: db62a7b
- timing: interleaved min-of-7 runs, first round discarded

| case | track | runtime (ms) | compile (ms) | binary (B) | LOC | outputs match |
|---|---|---:|---:|---:|---:|---|
| transient_borrow | rust-std | 520.9 | 1268 | 128512 | 101 | True |
| transient_borrow | jestyr | 529.5 | 1230 | 69068 | 91 | True |
| transient_borrow | jestyr-std | 557.7 | 1004 | 81608 | 92 | True |
| borrowed_projection | rust-std | 144.1 | 1138 | 128000 | 62 | True |
| borrowed_projection | jestyr | 144.4 | 886 | 70453 | 56 | True |
| borrowed_projection | jestyr-std | 160.3 | 1021 | 77459 | 60 | True |
| disjoint_mutation | rust-std | 663.2 | 1042 | 128000 | 63 | True |
| disjoint_mutation | jestyr | 790.9 | 1247 | 101843 | 59 | True |
| observer_registry | rust-std | 91.7 | 752 | 129024 | 109 | True |
| observer_registry | rust-idiomatic | 69.8 | 752 | 129024 | 63 | True |
| observer_registry | jestyr | 100.2 | 977 | 70838 | 74 | True |
| observer_registry | jestyr-std | 103.9 | 820 | 76827 | 74 | True |
| arena_ast | rust-std | 56.4 | 1181 | 128512 | 72 | True |
| arena_ast | rust-idiomatic | 57.1 | 1545 | 131072 | 85 | True |
| arena_ast | jestyr | 307.5 | 612 | 74971 | 77 | True |
| arena_ast | jestyr-std | 79 | 673 | 84304 | 76 | True |
| dlist | rust-std | 26.6 | 824 | 128000 | 82 | True |
| dlist | rust-idiomatic | 27 | 953 | 130048 | 80 | True |
| dlist | jestyr | 69.1 | 883 | 80614 | 101 | True |
| dlist | jestyr-std | 27 | 1290 | 80996 | 97 | True |
| resource_capabilities | rust-std | 33.4 | 684 | 126976 | 35 | True |
| resource_capabilities | jestyr | 28.7 | 755 | 60841 | 33 | True |
| structured_concurrency | rust-std | 94.7 | 1736 | 159744 | 38 | True |
| structured_concurrency | rust-idiomatic | 91.8 | 1718 | 212992 | 21 | True |
| structured_concurrency | jestyr | 102.5 | 1925 | 226267 | 29 | True |
| unsafe_boundary | rust-std | 135.8 | 759 | 127488 | 48 | True |
| unsafe_boundary | jestyr | 147.2 | 548 | 66809 | 47 | True |
