#### Code search step 2. Manifest file format

Anchor: Claude Shannon, David Parnas

- Files: new `internal/tool/search/manifest.go`, `manifest_test.go`, and `bench_manifest_test.go`.
- Symbols:
  - The constant `manifestMagic` holds the text `dalgona search manifest 2 codesearch v1.2.0` followed by one newline byte.
  - `type manifestEntry struct { Path string; Size int64; ModNS int64; CtimeNS int64; Indexed bool }`.
  - `type manifest struct { Start int64; TrigramSize int64; Entries []manifestEntry }`. `Start` is the build start in Unix nanoseconds. `TrigramSize` is the byte length of the trigram file.
  - `var errManifest = errors.New("search: invalid manifest")`.
  - `func encodeManifest(m manifest) ([]byte, error)`: returns an error that wraps `errManifest` with a reason when a path is empty (`empty path`), longer than 4096 bytes (`path length`), contains a NUL byte (`NUL in path`), or is not greater than the path before it (`paths not sorted`). It also rejects a negative `TrigramSize` (`trigram size`).
  - `func decodeManifest(data []byte) (manifest, error)`: the reverse. It rejects a wrong magic (`bad magic`), a truncated header or entry (`truncated`), an indexed byte other than 0 or 1 (`bad indexed byte`), a count that does not match the entries (`count mismatch`), and trailing bytes (`trailing bytes`). Each error has the form `search: invalid manifest: <reason>` and wraps `errManifest`.
  - `func writeManifest(path string, m manifest) error`: encodes, writes the bytes with mode 0o600, and then calls `syncFile`.
  - `func readManifest(path string) (manifest, error)`: reads the whole file, then decodes it.
  - `func (m manifest) lookup(path string) (manifestEntry, bool)`: a binary search with `sort.Search` over `Entries` by `Path`.
  - `func syncFile(path string) error`: opens the path read-only, calls `Sync`, and closes it.
  - The byte layout is in the Schemas part of this workstream.
- Tests that fail before the change (compile failure, because the functions do not exist):
  - `TestManifestRoundTrip` in `manifest_test.go`. Input: `manifest{Start: 1700000100000000000, TrigramSize: 4096, Entries: [a.go with Size 3, ModNS 1700000000000000001, CtimeNS 1700000000000000002, Indexed true; b/c.txt with Size 0, ModNS 5, CtimeNS 6, Indexed false]}`. Expected: `decodeManifest(encodeManifest(m))` equals the input. Source: the layout in the Schemas part.
  - `TestManifestRejectsBadMagic`. Input: the encoding with its first byte changed to `x`. Expected: an error that wraps `errManifest` and contains `bad magic`. Source: the magic rule.
  - `TestManifestRejectsOtherLibraryVersion`. Input: the encoding with `v1.2.0` replaced by `v1.3.0-rc.1` in the magic. Expected: an error that contains `bad magic`. Source: the magic names the library version.
  - `TestManifestRejectsUnsortedPaths`. Input: entries `b` and then `a`. Expected: `encodeManifest` returns an error that wraps `errManifest` and contains `paths not sorted`. Source: the byte-order rule.
  - `TestManifestRejectsLongPath`. Input: one entry whose path is 4097 bytes of `a`. Expected: `encodeManifest` returns an error that contains `path length`. Source: the 4096-byte limit.
  - `TestManifestRejectsTrailingBytes`. Input: the encoding with one extra zero byte. Expected: an error that contains `trailing bytes`. Source: the layout.
  - `TestManifestRejectsTruncatedEntry`. Input: the encoding without its last byte. Expected: an error that contains `truncated`. Source: the layout.
  - `TestManifestLookup`. Input: the round-trip manifest, looked up with `b/c.txt` and with `missing`. Expected: `b/c.txt` is found with CtimeNS 6, and `missing` is not found. Source: the input.
- Benchmark: `BenchmarkManifestLoad` in `bench_manifest_test.go`. Workloads: manifests of 2,000 entries and of 200,000 entries, with paths of depth 3 to 8 from `rand.NewPCG(1, 2)`, written with `writeManifest` before the timer. Metric: ns per `readManifest` call with p50, p95, p99 and max; B/op; allocs/op. Baseline: none, the benchmark measures the function. Warm-up: one read. `GOMAXPROCS=1`, count 10.
- Spec: none. The format has no behaviour until step 4.
- Lands after: Code search step 1. Baseline benchmarks of the scan path.

