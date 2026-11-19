#### Code search step 3. Index builder, publish rules and hidden build command

Anchor: Tony Hoare, Edsger W. Dijkstra

- Files:
  - new `internal/tool/search/build.go`, `build_test.go`, `build_atime_linux_test.go`, `ctime_linux.go`, `ctime_darwin.go`, `ctime_other.go` (build constraint `!linux && !darwin`), and `bench_index_test.go`.
  - `internal/tool/search/engine.go`: add `searchableInfo` and `readSearchable` (below). `searchableFile` calls `searchableInfo`. `scanFile` calls `readSearchable` after `searchableFile`. Its behaviour does not change, and its tests keep their bodies.
  - `internal/tool/search/main_test.go`: `TestMain` gets a branch that exits with `BuildMain(os.Args[2:], os.Stdout)` when `os.Args[1]` is `__search-index`. The existing branches for `__shim` and `__search` stay.
  - `cmd/dal/main.go`: in `run`, after `case "__search"`, add `case "__search-index": return search.BuildMain(args[1:], stdout)`.
  - `cmd/dalgona/main.go`: in `var hidden`, after the `__search` entry, add `"__search-index": func(args []string, std stdio) int { return search.BuildMain(args, std.out) },`.
  - `go.mod` and `go.sum`: add `github.com/google/codesearch v1.2.0` to the direct require block, run `go mod tidy`, and commit what it writes. The module has no requirements, so tidy adds only its `go.sum` lines.
  - `docs/decisions/dependencies.md`: in the codesearch row that the Version pins workstream wrote, replace the Decision cell `Not added here; add at v1.2.0 in the step that imports it` with `Search index for the grep mode, see docs/decisions/search-index.md`.
  - new `docs/decisions/search-index.md` with the sections `## Invariant`, `## Scope` and `## Rejected alternatives`, as described in Spec below.
  - `THIRD_PARTY_NOTICES.md`: if the file is absent, create it with the heading `# Third-party notices`. Then add the section `## codesearch (BSD-3-Clause)`, which copies verbatim the file `github.com/google/codesearch@v1.2.0/LICENSE`, with its copyright line `Copyright (c) 2011 The Go Authors. All rights reserved.` and its conditions and disclaimer. If the file is present, append the section.
  - `docs/spec/search.md`: insert the section `## Index` directly after `## Scanning`. Code symbols step 6 places `## Symbols and syntax` after it.
  - Vocabulary: add the terms `generation`, `manifest` and `trigram` to the terms list that Tests-and-docs step 15 creates in `docs/README.md`, with one line each, so that rule 3 of this plan holds for the new sentences.
- Symbols:
  - `const ctimeSupported = true` in `ctime_linux.go` and `ctime_darwin.go`, and `const ctimeSupported = false` in `ctime_other.go`.
  - `func ctimeNS(info fs.FileInfo) (int64, bool)`. On linux it reads the `Ctim` field of `*syscall.Stat_t`, and on darwin the `Ctimespec` field. Both convert with `time.Unix(int64(ts.Sec), int64(ts.Nsec)).UnixNano()`. A failed type assertion returns `0, false`. `ctime_other.go` returns `0, false` and nothing else.
  - `func searchableInfo(info fs.FileInfo) bool { return info.Mode().IsRegular() && info.Size() <= maxSearchFileBytes }`.
  - `func searchableFile(path string) bool`: `os.Lstat`, then `searchableInfo`.
  - `func readSearchable(path string) ([]byte, bool)`: `readCapped(path)`, then `searchableText`. It returns `nil, false` on a read error or when `searchableText` is false.
  - `const maxBuildBytes = 1 << 30`. `const currentFile = "CURRENT"` is written as the name `CURRENT`.
  - `func parseBuildArgs(argv []string) (root, gen string, start time.Time, err error)`: exactly three arguments. `gen` is one path element and is not `.` or `..`. `start` is a decimal count of Unix nanoseconds.
  - `func BuildMain(argv []string, stdout io.Writer) int`. Any argument count other than three writes the usage line `usage: __search-index <root> <generation> <start-ns>` and returns 2. A bad `gen` or `start` returns 2. When `ctimeSupported` is false, it writes `search: index needs change times on this platform` and returns 1. Otherwise it calls `os.Getwd` and then `buildIndex(cwd, filepath.Join(root, gen), time.Unix(0, start), maxBuildBytes)`. An error writes its text to stdout and returns 1. Success returns 0.
  - `func buildIndex(cwd, dir string, start time.Time, limit int64) error`:
    1. `entries, err := walkScope(cwd, cwd)`. The builder indexes only the workspace root.
    2. `writer := index.Create(filepath.Join(dir, "trigram"))`. `index.Create` returns no error, and the library exits the process on a write failure (`index/write.go:474`).
    3. For each entry, in walk order, call `recordFile(writer, entry, total, limit)`. It returns the record and the bytes it added. `recordFile` calls `os.Lstat(entry.absolute)`, and skips the entry when that fails, when `searchableInfo` is false, or when `ctimeNS` is not ok. It builds the record with `Size` from `info.Size()`, `ModNS` from `info.ModTime().UnixNano()`, `CtimeNS` from `ctimeNS`, and `Indexed` false. If the total plus `info.Size()` would pass `limit`, it returns the record unread. It calls `readSearchable(entry.absolute)`. When that returns false, it returns the record unread. Otherwise it calls `writer.Add(entry.display, bytes.NewReader(data))` and returns the record with `Indexed` false and the length of the data.
    4. Call `writer.Flush()`, then `indexedNames(dir)`. Set `Indexed` to true for each record whose path is in that set.
    5. Call `syncFile` on the trigram file, read its size with `os.Stat`, call `writeManifest(filepath.Join(dir, "manifest"), manifest{Start: start.UnixNano(), TrigramSize: size, Entries: records})`, and then call `syncDir(dir)`.
  - `func indexedNames(dir string) (map[string]bool, error)`: `index.Open(filepath.Join(dir, "trigram"))`, then `PostingQuery(&index.Query{Op: index.QAll})`, which lists every file id, and `Name(id)` for each id. `PostingQuery` is at `index/read.go:333`, and the `QAll` case is at `index/read.go:342-349`.
  - `func syncDir(path string) error`: opens the directory and calls `Sync`.
  - `func createGeneration(root string, start time.Time) (gen, dir string, err error)`: `os.MkdirAll(root, 0o700)`, then `generationName(start)`, then `os.Mkdir(dir, 0o700)`.
  - `func generationName(start time.Time) (string, error)`: the text `%020d-%s` with the start in Unix nanoseconds and 8 random bytes from `crypto/rand`, written as 16 hex digits.
  - `func publishGeneration(root, gen string) error`: `os.CreateTemp(root, "CURRENT-*.tmp")` (the pattern is `CURRENT-*.tmp`), writes `gen` and one newline, calls `Sync`, closes the file, and then calls `os.Rename` onto `CURRENT`. On any error the temporary file is removed.
  - `func removeOtherGenerations(root, keep string) error`: removes every directory in `root` whose name is not `keep`. It leaves files, `CURRENT` among them, and joins the errors.
  - Each function stays at cyclomatic complexity nine or less. Split `recordFile` out of `buildIndex` as above.
- Tests that fail before the change (compile failure, because the functions do not exist):
  - `TestBuildWritesManifestAndTrigram` in `build_test.go`. Input: `src/a.go` with `needle one` and a newline, `b.txt` with `needle two` and a newline, a root from `t.TempDir()`, `createGeneration(root, time.Now())`, then `buildIndex(cwd, dir, time.Now(), 1<<30)`. Expected: the manifest lists `b.txt` and then `src/a.go`; both have Size 11 and Indexed true; each ModNS equals the `os.Stat` modification time taken after the build; `TrigramSize` equals the `os.Stat` size of `trigram`. Source: the byte count of each file (11) and `os.Stat`.
  - `TestBuildMarksLibraryRejectedFilesUnindexed` in `build_test.go`. Input: `long.txt` with one line of 2001 bytes of `x` and a newline, and `ok.txt` with `needle` and a newline. Expected: `long.txt` has Indexed false, and `ok.txt` has Indexed true. Source: `index/write.go:164-168`, which skips a line over 2000 bytes.
  - `TestBuildRecordsUnreadableTextUnindexed` in `build_test.go`. Input: `bin.dat` with the bytes `a`, a zero byte, `b` and a newline. Expected: one record for `bin.dat` with Indexed false, and no error. Source: `searchableText` rejects a NUL byte in the first 8192 bytes (`engine.go`, function `searchableText`).
  - `TestBuildSkipsSymbolicLinks` in `build_test.go`, skipped unless the platform is linux or darwin. Input: `link.txt`, a symbolic link to `outside.txt` in another temporary directory. Expected: no record for `link.txt`. Source: `walker.excluded` in `engine.go` excludes symbolic links from the walk.
  - `TestBuildRespectsByteLimit` in `build_test.go`. Input: `a.txt` with ten `x` bytes and a newline, `b.txt` with ten `y` bytes and a newline, and `limit` 20. Expected: two records. `a.txt` has Indexed true. `b.txt` has Indexed false, because the next file would pass the limit and the builder does not read it. Source: this step's rule that a file past the limit gets a record and is not read.
  - `TestBuildNeverReadsOversizedFile` in `build_atime_linux_test.go`. Input: `sparse.txt`, 64 MiB, starting with `hit` and a newline, with its access time set 48 hours back and its modification time set to now, using the setup of `bound_atime_linux_test.go:29-30`. Expected: no record for `sparse.txt`, and its access time is still 48 hours back within one second. Source: `engine.go`, constant `maxSearchFileBytes` (16 MiB, line 20).
  - `TestPublishLaterSwitchWins` in `build_test.go`. Input: two generations `g1` and `g2` made with `createGeneration`, then `publishGeneration(root, g1)` and `publishGeneration(root, g2)`. Expected: `CURRENT` holds `g2` and one newline, and after `removeOtherGenerations(root, g2)` only the directory `g2` remains. Source: the rule that the later rename wins.
  - `TestGenerationNamesDiffer` in `build_test.go`. Input: `generationName` called twice with the same start time. Expected: the two names differ, and both begin with the 20-digit nanosecond count. Source: the naming rule of this step.
  - `TestBuildMainArguments` in `build_test.go`. Input: two arguments, and then three arguments whose generation is `../x`. Expected: both calls return 2, and the first writes the usage line. Source: `parseBuildArgs`.
- Benchmark: `BenchmarkBuildIndex` in `bench_index_test.go`. Workloads A and B. `-benchtime=1x -count=5`, `GOMAXPROCS=1`. Metric: wall ns per build with p50, p95, p99 and max; allocs/op; the index size reported as `index_bytes`; and the count of records with Indexed false reported as `rejected_files`. Hypothesis [unmeasured]: one build reads each eligible file once, and the index stays below the 4 GiB limit (`index/write.go:523`). Baseline: none.
- Spec: the section `## Index` of `docs/spec/search.md`. Each sentence has at most 25 words and no semicolon.
  - The index MUST live under the product home, in one directory for each workspace.
  - The builder MUST index only the workspace root, and MUST write only its generation directory.
  - The builder MUST add only regular files that a grep call could read, each at most 16 MiB.
  - The builder MUST stat a file before it reads the file, and MUST NOT follow a symbolic link.
  - The builder MUST write no record for a file that is not regular, is over 16 MiB, or has no change time.
  - A file that the builder does not read or index MUST be recorded as not indexed.
  - Search MUST read a file recorded as not indexed on every call.
  - The builder MUST stop reading files when the next file would take the total past 1 GiB.
  - The builder MUST sync the trigram file and the manifest before it exits with status 0.
  - Only the parent MUST publish a generation, by an atomic rename of `CURRENT`, after the builder exits with status 0.
  - A publish MUST remove every other generation directory of the workspace.
  - On platforms without change times, including Windows, the index MUST NOT be built or used.
  - The builder MUST run in the sandbox of the search call.
- `docs/decisions/search-index.md` in this step: `## Invariant` holds one sentence, that a grep call with an index returns the bytes of a grep call without one. `## Scope` names the workspace tree only, local runners only, and sandboxed children only. `## Rejected alternatives` names the rivals of decisions 1 to 4 and 7, each once.
- Lands after: Code search step 2. Manifest file format; Tests-and-docs step 15. Terms list; Specifications step 31 (the rewrite of `docs/spec/search.md`); the Version pins workstream (its table in `docs/decisions/dependencies.md`).

