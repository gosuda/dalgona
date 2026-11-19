#### Schemas of the code-search workstream

Index root: `<home>/search/<hash16>`. `<home>` is `tool.Env.Home`. The agent sets it from `config.Config.Home` (`internal/agent/tools.go:390`), and `defaultConfig` takes that value from `Product.Home()` (`internal/config/config.go:253`). `Product.Home()` is `DAL_HOME` or `DALGONA_HOME` when set, and otherwise the user home with `.dal` or `.dalgona` (`internal/config/config.go:23`). `hash16` is the first 16 hex digits of the SHA-256 of the canonical workspace path, which is `filepath.EvalSymlinks` of `env.Cwd`.

`CURRENT`: a text file in the index root. It holds one generation name and one newline. It changes only by an atomic rename of a temporary file that `os.CreateTemp` made in the index root.

Generation: a directory `<root>/<name>`, made by the parent with mode 0o700 before the builder starts. The name is `%020d-%s`: the start in Unix nanoseconds, a hyphen, and 16 hex digits of random bytes. It holds two files: `trigram`, written by the codesearch v1.2.0 library, and `manifest`, which this workstream defines.

Manifest layout. All integers are big endian. Entries are in byte order of their paths, with no repeats.
```
magic: dalgona search manifest 2 codesearch v1.2.0, followed by one newline
start: int64, Unix nanoseconds, the build start
trigramSize: int64, byte length of the trigram file
count: uint32
for each of count entries:
  pathLen: uint32, from 1 to 4096
  path: pathLen bytes, slash separated, relative to the workspace root, no NUL byte
  size: int64
  mtime: int64, Unix nanoseconds
  ctime: int64, Unix nanoseconds
  indexed: one byte, 0 or 1
```

Covered rule. A record covers a file when `indexed` is 1, `size` equals the current size, `mtime` equals the current modification time in nanoseconds, `ctime` equals the current change time in nanoseconds, both `mtime` and `ctime` are less than `start - racy`, and the clock reading is at least `start`.

Skip rule. The engine skips a file only when it is covered, `all` is false, and `named` does not hold its path.

Count rule. The engine counts a read file as `unindexed` unless the file is covered, or the record exists with `indexed` 0 and its size, mtime and ctime equal the current values (decision 13). The member `unindexed` counts the bytes of counted files. The model never sees it.

Search child arguments. Without an index: `@self`, `__search`, and the JSON arguments. With an index: `@self`, `__search`, the JSON arguments, the index root, the generation name, the parent's clock in Unix nanoseconds, and the racy window in nanoseconds. Only the parent sets these.

Search child outcome JSON: the members text, captures and unindexed, in that order, where captures keep their current shape.

Builder arguments: `@self`, `__search-index`, the index root, the generation name, and the start in Unix nanoseconds. Its only extra writable root is the generation directory.

Exit statuses. Search child: 0 for success; 1 only when the index is unusable, on the index path only; 2 for any other error, including a failed write to stdout. Builder: 0 for success; 2 for a usage error; 1 for any other failure, including an unsupported platform.

Constants and variables. `racyWindow = 2 * time.Second` (`query.go`). `maxSearchFileBytes = 16 << 20` (`engine.go:20`). `maxBuildBytes = 1 << 30` (`build.go`). `indexRebuildBytes` (`index.go`, a variable whose value step 6 sets). `indexFailedStatus = 1` and `errIndex` (`query.go`). `type freshness struct { Now int64; Racy int64 }` (`query.go`).

Error texts. `search: invalid manifest: <reason>`, with the reasons of step 2 (`bad magic`, `truncated`, `paths not sorted`, `path length`, `empty path`, `NUL in path`, `bad indexed byte`, `count mismatch`, `trailing bytes`) and `trigram size` from step 4. `search: index unusable: <cause>` for `errIndex`. `search: index needs change times on this platform` from the builder on an unsupported platform. Usage lines: `usage: __search <json-arguments> [<root> <generation> <now-ns> <racy-ns>]` and `usage: __search-index <root> <generation> <start-ns>`.

Gating. The index applies when every condition holds: a local runner; `env.Home` is absolute; a sandbox mode of workspace write, with an empty mode counted as workspace write as `Local.spawn` does (`internal/runner/local.go:91`); `runner.Available()`; `ctimeSupported`; and an index root outside the containment list of decision 11.

Function signatures after this workstream: `execute(cwd string, args args, dir string, clock freshness) (outcome, error)`, where `dir` is the generation directory; `search.New() tool.Tool`, unchanged; `Main(argv []string, stdout io.Writer) int`; `BuildMain(argv []string, stdout io.Writer) int`; `buildIndex(cwd, dir string, start time.Time, limit int64) error`; `indexRoot(home, cwd string) (string, error)`; `indexDir(env tool.Env) (string, bool)`; `indexFor(env tool.Env) indexRef`.

