#### Code symbols step 9. Benchmarks and the parse bounds

Anchor: John Carmack, Richard Feynman

- Files: `internal/syntax/bench_parse_test.go` for benchmarks (i) and (ii). `internal/tool/search/bench_symbols_test.go` for benchmarks (iii) and (iv). Each benchmark generates its input in memory or in `b.TempDir()`. `docs/decisions/syntax-package.md`: a table of the bounds, the measured values, the 23-name grammar sizes and the binary sizes of `dal` and `dalgona` after Step 8, next to the sizes of Step 1.
- Benchmarks. Each records p50, p95, p99 and max over its samples with `b.ReportMetric`. Each runs with `GOMAXPROCS=1` unless stated, on an idle host, with `-count=10`, and each compares runs with `benchstat` at alpha 0.05.
  - (i) Parse throughput for each of the 17 definition languages on generated source of 4 KB, 64 KB and 1 MB. Workload: a template of ten definitions per language, repeated, with names from `rand.New(rand.NewPCG(1, 2))`. Warm-up: one untimed parse. Metric: ns per op, MB per second with `b.SetBytes`, and allocations per op.
  - (ii) First-use cost of a grammar. Workload: a 4 KB file for go, python and cpp. Harness: a test binary started 30 times with `-test.run` for one test per language. Metric: wall time of the first `Symbols` call in a new process, with p50, p95 and max over the 30 processes.
  - (iii) Walk and narrowing. The test writes 200,000 files of 512 bytes into 2,000 directories, with content from `rand.New(rand.NewPCG(1, 2))`, in `b.TempDir()`, outside the timed loop. A rare name occurs in three files. Metric: wall time per `symbols` call with the pattern, and the number of files parsed, with two warm-up calls and `GOMAXPROCS=8`.
  - (iv) Worker count 1, 4, 8 and `runtime.GOMAXPROCS(0)`, through the package variable `parseWorkerCount`, which the benchmark sets and restores with `b.Cleanup`. Workload: 64 files of 64 KB each, in Go. Metric: wall time per call, and peak resident memory of the child.
- Binary sizes. Build `bin/dal` and `bin/dalgona` as in Step 1, and record the sizes after Step 8 next to the sizes of Step 1. Record also the sizes of decision 3 next to the new measurement of the 23 grammars.
- Memory. Record the peak resident memory of one call with 8 workers over 64 files of 256 KiB each. The record is the input of decision 10.
- Rule for the constants. A measured p99 of the 1 MiB parse at `GOMAXPROCS=1` above `parseTimeout` is recorded as a failure, and it needs a new entry in `docs/decisions/syntax-package.md` before any constant changes. No constant changes in this step.
- Specification text: none. The bounds are contract values that Step 6 states in `docs/spec/search.md`.
- Lands after: Code symbols step 8. The syntax mode of search

