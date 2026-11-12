# Search index

## Invariant

A grep call with an index MUST return the same output as a grep call without one. The index MUST name only files that the search may skip, and the search MUST keep its own matcher and output.

The index MUST NOT be built or used where the sandbox does not enforce its writable roots, or where the platform has no change times.

## Scope

The index belongs to the grep mode of the search tool in `internal/tool/search`. It covers the workspace root only. The builder runs as a hidden command, `__search-index`, in both products. The library is `github.com/google/codesearch` at v1.2.0, licensed under BSD-3-Clause. It has no module requirements.

## Enforcement

`TestIndexedGrepEqualsScan` and `FuzzTrigramCandidates` in `internal/tool/search` check that an indexed grep returns the scan's output and names every file that can match. `TestCoveredFileWithRestoredMtimeIsRead` and `TestRacyFileIsReadEvenWhenStatMatches` check the freshness rule. `TestChildWritesNothingToStderrOnSuccess` and the exit-status tests in `query_exit_test.go` check the child's contract.

## Threshold

A grep call that counts at least 32 MiB of uncovered bytes starts one background build. The value is `indexRebuildBytes` in `internal/tool/search/index.go`. An unusable index blocks new builds for 10 minutes in every process, through a backoff file in the index root. A failed build blocks further builds only in its own process. A build that runs longer than 10 minutes is killed.

The 4 GB sweep of `BenchmarkThresholdSweep` has not run on this host, so the value stays at 32 MiB until its output exists. The final value is the smallest uncovered size whose indexed p95 is below its scan p95, or 4 GiB when no size qualifies.

## Rejected alternatives

- `github.com/sourcegraph/zoekt` as the engine. It adds 58 direct requirements, and its default cuts file content above 2 MiB.
- Building the index in the host process. A library crash would end the agent, and the build reads the whole workspace.
- A cross-process file lock for each workspace. It adds platform code, and the later rename of `CURRENT` settles a race between two builds.
- A checksum over the whole index on each call. It reads every byte on every search.
- An index trusted by modification time only. A restored modification time would hide a changed file.
- `encoding/gob` for the manifest. Reflection is barred from wire and journal codecs.

## Rulings

- No cross-process lock guards the index build of a workspace. The review row that asked for one is closed by decisions 14 and 15 of the code-search plan: one build starts in each process, and each build publishes by an atomic rename of `CURRENT` after its own generation directory is complete. When two processes build, the later rename wins, and the publisher removes the other generation directories.
- The index format is named in the manifest magic, `codesearch v1.2.0`, and not in the directory name. The directory is `search/<hash16>`, and the generation directory is `<start-ns>-<random>`, as the plan's schemas define.
