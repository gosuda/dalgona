# Syntax package

## Invariant

Only `internal/syntax` MUST import `github.com/odvcencio/gotreesitter`. Callers MUST see symbols, captures and reports, never trees, nodes or queries of the library. The package MUST import no grammar aggregate and no copyleft grammar. Parsing MUST run under a time limit, a memory budget and work limits, and every tree MUST be released after its rows are read.

## Scope

This decision applies to `internal/syntax`, its embedded queries under `internal/syntax/queries`, and the import rules `syntax` and `copyleft` in `.golangci.yml`. The search tool and the rewrite tool use the package through its exported API only. Each search mode that uses the package is specified in [search.md](../spec/search.md) by the step that adds that mode.

## Enforcement

`just lint` runs the `syntax` rule, which denies the library outside `internal/syntax`, and the `copyleft` rule, which denies the aggregate package and the copyleft grammars everywhere. `internal/syntax/syntax_test.go` checks the kind table, the extension table, the Go fixture rows, context cancellation and the stopped parse.

## Measurements

Baseline linux/amd64 binaries before the package existed, built with go1.27.4 from commit 30f8f047:

|Binary|Bytes|
|---|---|
|`dal`|36598376|
|`dalgona`|69772184|

The baseline sizes are the build at commit `30f8f047`, which has no `internal/syntax` and is an ancestor of `f0768f03d`. The first commit that adds `internal/syntax` is `b84c08d5c`. The change below also includes the search index commits between the two commits, so it is not the cost of the package alone. The flags of the baseline build were not recorded. Built with go1.27.4 and the default build flags at commit `f0768f03d`, where the search tool links the 23 grammars:

|Binary|Bytes|Change from baseline|
|---|---|---|
|`dal`|54021176|+17422800|
|`dalgona`|87163760|+17391576|

Probe binaries, linux/amd64, go1.27.4, built from the same tree with one `main` each:

|Link set|Bytes|
|---|---|
|the library root package `gotreesitter`|3507686|
|the Go grammar package `grammars/go` alone|4114522|
|the 23 grammars through `internal/syntax`|8325807|
|the aggregate package `grammars`|22030362|

The decision 3 figures in the code-symbols plan (2.4 MB, 17.7 MB, 22.7 MB and 36.9 MB) were not reproduced by these probes. The probe values above are the measured ones. The product delta is about twice the 23-grammar probe. This step did not split the rest of the delta.

## Bounds and measurements

The bounds of the symbols mode are unchanged by this record. Each value below is the value the code sets. Each measurement is an observation under load, not an idle-host result. The 1-minute load average on the 80-core host was 216 to 381 during the re-run. The walk, worker and peak-RSS rows ran earlier, under a load average of 130 to 300.

|Bound|Value|Where it is set|
|---|---|---|
|parse timeout|2 s|`parseTimeout`, `internal/syntax/language.go`|
|parse memory budget|256 MiB|`parseMemoryBudgetBytes`|
|parse iteration limit|200000|`parseIterationLimit`|
|parse stack depth limit|8192|`parseStackDepthLimit`|
|parse node limit|1000000|`parseNodeLimit`|
|files per call|256|`parseMaxFiles`, `internal/tool/search/parse.go`|
|bytes per file|256 KiB|`parseMaxFileBytes`|
|bytes per call|1 MiB|`parseMaxBytes`|
|call budget|10 s|`parseBudget`|
|child deadline|30 s|`parseDeadline`|
|workers|8|`parseWorkers`|

The throughput rows and the first-use rows were re-run on 2026-11-12. Throughput: `go test -run '^$' -bench <name> -benchtime=30x -cpu=1 -count=1`, once for each size group. First use: `-benchtime=1x` with the default `GOMAXPROCS`. The walk, worker and peak-RSS rows keep the method and values of commit `cb00c2e49`. The worker row ran before its output limit was set to 200. That limit changes only the printed rows, because symbol rows are filtered after parsing.

Parse throughput (`Parser.Symbols`, `GOMAXPROCS=1`, 30 samples per row, times in milliseconds). `stopped` counts samples whose parse stopped early for any reason, out of 30. `syntax errors` counts samples whose tree had syntax errors, out of 30. The source is a seeded template of ten definitions per language, repeated and cut at a definition boundary, with names from `rand.New(rand.NewPCG(1, 2))`. The benchmark setup fails when a 4 KB or 64 KB source parses with syntax errors.

|Language|Size|p50 ms|p95 ms|p99 ms|max ms|MB/s|allocs/op|stopped of 30|syntax errors of 30|
|---|---|---|---|---|---|---|---|---|---|
|bash|4KB|14.3|34.9|40.8|40.8|0.24|2647|0|0|
|bash|64KB|250.3|608.3|739.3|739.3|0.22|41932|0|0|
|c|4KB|8.2|12.5|15.4|15.4|0.47|4055|0|0|
|c|64KB|146.4|479.2|520.3|520.3|0.35|65102|0|0|
|c_sharp|4KB|14.1|42.5|61.3|61.3|0.21|2577|0|0|
|c_sharp|64KB|210.4|421.0|498.0|498.0|0.28|40852|0|0|
|cpp|4KB|9.3|22.9|81.7|81.7|0.33|5244|0|0|
|cpp|64KB|184.1|351.3|419.4|419.4|0.31|83566|0|0|
|go|4KB|9.2|14.7|15.2|15.2|0.42|4233|0|0|
|go|64KB|178.4|752.0|1344.0|1344.0|0.23|67840|0|0|
|java|4KB|6.4|9.7|16.4|16.4|0.59|2872|0|0|
|java|64KB|125.9|309.9|338.4|338.4|0.49|45673|0|0|
|javascript|4KB|10.4|26.9|28.6|28.6|0.33|3007|0|0|
|javascript|64KB|163.7|555.4|726.6|726.6|0.31|46600|0|0|
|kotlin|4KB|10.0|14.1|15.3|15.3|0.40|6526|0|0|
|kotlin|64KB|172.4|542.2|648.2|648.2|0.30|104096|0|0|
|lua|4KB|10.2|107.0|128.9|128.9|0.18|3495|0|0|
|lua|64KB|157.3|406.3|427.2|427.2|0.35|55731|0|0|
|markdown|4KB|18.6|412.7|517.1|517.1|0.06|2187|0|0|
|markdown|64KB|346.3|1287.3|2050.8|2050.8|0.13|32826|1|0|
|php|4KB|8.2|9.1|9.3|9.3|0.51|3013|0|0|
|php|64KB|141.5|247.3|479.1|479.1|0.41|47473|0|0|
|python|4KB|9.3|24.3|28.3|28.3|0.35|4626|0|0|
|python|64KB|178.6|359.4|442.1|442.1|0.33|71412|0|0|
|ruby|4KB|11.0|12.6|31.9|31.9|0.35|5212|0|0|
|ruby|64KB|213.4|569.9|922.1|922.1|0.24|81422|0|0|
|rust|4KB|10.3|22.2|23.7|23.7|0.35|3249|0|0|
|rust|64KB|169.0|275.0|310.6|310.6|0.39|51715|0|0|
|swift|4KB|13.4|46.6|56.5|56.5|0.22|6689|0|0|
|swift|64KB|270.5|422.6|429.3|429.3|0.24|106839|0|0|
|tsx|4KB|10.8|52.4|57.1|57.1|0.26|2811|0|0|
|tsx|64KB|324.7|634.1|657.4|657.4|0.19|44597|0|0|
|typescript|4KB|12.2|14.0|15.0|15.0|0.33|3591|0|0|
|typescript|64KB|216.2|338.6|924.9|924.9|0.27|56335|0|0|
|bash|1 MiB|1679.9|2073.3|2081.6|2081.6|0.62|68|30|0|
|c|1 MiB|588.0|1029.7|1142.8|1142.8|1.61|5793|30|0|
|c_sharp|1 MiB|2112.7|2712.3|2776.2|2776.2|0.47|3854|30|0|
|cpp|1 MiB|1979.0|2186.3|2282.6|2282.6|0.57|87|30|0|
|go|1 MiB|1833.0|2654.0|2681.8|2681.8|0.59|73|30|0|
|java|1 MiB|1331.8|2092.3|2419.8|2419.8|0.74|74|30|0|
|javascript|1 MiB|1477.4|2282.6|2371.4|2371.4|0.65|60|30|0|
|kotlin|1 MiB|2057.0|2431.7|2519.7|2519.7|0.52|49|30|0|
|lua|1 MiB|2024.3|2201.3|2365.2|2365.2|0.58|53|30|0|
|markdown|1 MiB|1787.1|2150.8|2182.2|2182.2|0.58|75|30|0|
|php|1 MiB|1837.2|2137.9|2164.3|2164.3|0.57|31861|30|0|
|python|1 MiB|1676.1|2057.0|2068.7|2068.7|0.60|104|30|0|
|ruby|1 MiB|2033.3|2080.9|2119.5|2119.5|0.53|49|30|0|
|rust|1 MiB|1370.1|2049.0|2080.8|2080.8|0.73|103|30|0|
|swift|1 MiB|2044.4|2116.0|2181.1|2181.1|0.51|43|30|0|
|tsx|1 MiB|1689.2|2051.9|2115.5|2115.5|0.61|102|30|0|
|typescript|1 MiB|2028.3|2109.3|2123.2|2123.2|0.53|8936|30|0|

All 30 samples of every 1 MiB row stopped early. The 4 KB and 64 KB rows have no syntax errors. One 64 KB markdown sample stopped early, and the slowest sample of that row took 2.05 s.

First use of a grammar in a new process. The run measured 30 child processes for each language, and the timed call includes `NewParser`. In milliseconds: go p50 152.5, p95 310.7, p99 379.4, max 379.4; python p50 99.2, p95 145.6, p99 150.3, max 150.3; cpp p50 711.7, p95 1074.2, p99 1143.6, max 1143.6. With 30 samples, p99 is the largest sample.

Symbols mode over 200,000 files of 512 bytes in 2,000 directories, with a literal name in three files (`BenchmarkSymbolsWalkNarrowing`, `-cpu 8`, three runs of three iterations): three files parsed per call, and a median call of about 10.0 s (p50 10.01 s, 10.01 s and 10.66 s across the three runs). A call walks and reads every file, so this figure is the walk and read cost, not the parse cost.

Parse worker count (`BenchmarkSymbolsWorkers`, 64 files of 64 KiB, median of ten runs of one call, seconds): 1 worker 1.85, 4 workers 0.85, 8 workers 0.87, 80 workers 0.53. The 1 MiB per-call byte cap parses only 16 of the 64 files, so the worker count spreads 16 parses, not 64.

Child peak resident set (`BenchmarkSymbolsChildPeakRSS`, 64 files of 256 KiB, three runs): 72152, 74212 and 63792 KiB, with 60 files reported as not parsed in each run. The 1 MiB per-call byte cap parses four of the 64 files. An earlier single run of the same benchmark, in the same session, measured 301276 KiB, so the peak varies with the collector and the load.

Every sample of every 1 MiB row stopped early, for all 17 languages. The record does not separate the stop reasons. A stop can come from the 2 s timeout, from the 256 MiB memory budget, or from a work limit. So the 1 MiB rows measure the time to a stop, not a complete parse. The plan rule records a p99 of the 1 MiB parse above `parseTimeout` as a failure. The 1 MiB p99 is above 2 s in 16 of 17 languages, so 16 failures are recorded. The c row is the exception at 1142.8 ms. No constant changes in this record. A measured p99 above `parseTimeout` for a complete parse is a failure that needs a new entry here before any constant changes.

## Known gaps

Setext headings give no symbol on gotreesitter v0.57.0. Observed on 2026-11-12 with `Outliner.OutlineTree` over the markdown fixture:

- The plan's row `(section (setext_heading heading_content: (paragraph) @name)) @definition.section` names the enclosing section of each setext heading. The setext heading is a child of the atx section that holds it, so that row and the atx row name the same node. The outliner reports `OmittedNameConflict` 2 and drops `Setup`, so the row is removed.
- The row `(setext_heading heading_content: (paragraph) @name) @definition.section` avoids the conflict and gives `Heading text` at rows 8 to 10. Its span is the heading and its underline, so `definition` would print no body. It is not used, because decision 36 requires a heading span that holds its body.
- Code symbols step 6 MUST state in `docs/spec/search.md` that setext headings give no symbol.

## Rejected alternatives

- A wrapper that returns trees and nodes. It lost because each caller would then repeat the parser setup, the time limit and the release rule, and the library import would spread across packages.
- The library's aggregate grammar package. It lost because it embeds the four copyleft grammars (caddy, disassembly, jq under GPL-3.0 and nim under MPL-2.0) in every build, per `licenses/grammars.json`.
- The library's inferred tags queries. They lost because they name kinds this package does not choose and they cover only some languages, so the closed kind set would not hold.
