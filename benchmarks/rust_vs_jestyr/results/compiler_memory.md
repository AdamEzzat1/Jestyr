# Peak compiler memory

Peak working set of the compiler process, polled every 5 ms.
rustc: direct invocation at -O (whole compiler incl. LLVM, single process).
jestyrc: emit-c only -- gcc (forked cc1) is NOT included. Footnotes matter;
see METHODOLOGY.md. Idiomatic-track crates skipped (extern plumbing).

| case | rustc peak (MB) | jestyrc peak (MB) |
|---|---:|---:|
| transient_borrow | 57.5 | 2.6 |
| borrowed_projection | 57.2 | 3.6 |
| disjoint_mutation | 57.5 | 2.6 |
| observer_registry | 61.8 | 2.6 |
| arena_ast | 58.9 | 4 |
| dlist | 56.8 | 2.6 |
| resource_capabilities | 50.2 | 5.7 |
| structured_concurrency | 66.1 | 8 |
| unsafe_boundary | 58.9 | 2.6 |
