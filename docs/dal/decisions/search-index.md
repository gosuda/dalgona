# Search index

## Invariant

A grep call with an index MUST return the bytes of a grep call without one.

## Scope

The index covers the workspace tree only, so a grep scope inside the workspace uses the same index. The index applies on local runners only, in the sandboxed search child only, and on platforms with change times. [search.md](../spec/search.md) defines the index requirement.

The index is advice. It names files that the search may skip, and the search keeps its own matcher and output.

The index belongs to the grep mode of the search tool in `internal/dal/tool/search`. The builder runs as a hidden command, `__search-index`, in both products. The library is `github.com/google/codesearch` at v1.2.0, licensed under BSD-3-Clause. It has no module requirements.

On macOS, `indexDir` returns no index root, so grep scans every eligible file and starts no background index build. Native macOS sandbox runtime checks have not been proven. This scan-only fallback stays until those checks pass. It does not change output byte identity or the platform's change-time support.

## Enforcement

`TestIndexedGrepEqualsScan` and `FuzzTrigramCandidates` in `internal/dal/tool/search` check that an indexed grep returns the scan's output and names every file that can match. The property test builds an index, then changes, adds and removes files, and covers a build start before, at and after its clock. `TestCoveredFileWithRestoredMtimeIsRead` and `TestRacyFileIsReadEvenWhenStatMatches` check the freshness rule. `TestChildWritesNothingToStderrOnSuccess` and the exit-status tests in `query_exit_test.go` check the child's contract.

`TestGenerationRemovedBeforeTrigramReadFallsBackToScan` and `TestPinnedTrigramSurvivesRemovedGeneration` check the rule for a generation deleted during the call. The child opens the trigram file before it names candidates, and the library opens that file through the descriptor of the held file. A generation removed before the read yields the scan, and a removal after the read cannot end the child. A host without a descriptor path uses no index.

## Threshold

A grep call that counts at least 32 MiB of uncovered bytes starts one background build. The value is `indexRebuildBytes` in `internal/dal/tool/search/index.go`. An unusable index blocks new builds for 10 minutes in every process, through a backoff file in the index root. A failed build blocks further builds only in its own process. A build that runs longer than 10 minutes is killed.

The 4 GB sweep of `BenchmarkThresholdSweep` has not run on this host, so the value stays at 32 MiB until its output exists. The final value is the smallest uncovered size whose indexed p95 is below its scan p95, or 4 GiB when no size qualifies.

`rejected_files` counts manifest records whose `Indexed` value is false. `BenchmarkBuildIndexWorkloadA` and `BenchmarkBuildIndexWorkloadB` report this count for the built generation. Both the scan and indexed sub-benchmarks of `BenchmarkGrepRareLiteral` and `BenchmarkGrepRareLiteralWorkloadB` report the same generation's count. The manifest is read and counted outside the timed loop. This metric describes index coverage, not the number of files scanned by a call.

## Rulings

- No cross-process lock guards the index build of a workspace. One build starts at most once in each process, and each build publishes by an atomic rename of `CURRENT` after its own generation directory is complete. When two processes build, the later rename wins, and the publisher removes the other generation directories.
- The index format is named in the manifest magic, `codesearch v1.2.0`, and not in the directory name. The directory is `search/<hash16>`, and the generation directory is `<start-ns>-<random>`, where `<start-ns>` is the build start in 20 decimal digits of Unix nanoseconds and `<random>` is 16 hex digits.
- The builder stops reading files once the next file would take the indexed bytes past 1 GiB. Each later file keeps a not-indexed record, so a search reads it on every call and never counts it.

## Rejected alternatives

- `github.com/sourcegraph/zoekt` as the engine. It adds 58 direct requirements, and its default cuts file content above 2 MiB.
- An index trusted for the answer. A stale or damaged index would hide matches.
- Size and modification time only as the coverage rule. A restored modification time would hide a changed file.
- A hash of each file on each call as the coverage rule. It reads every byte on every search.
- A filesystem watcher as the coverage rule. It needs a process that always runs.
- Opening the index in the host process. The library calls `log.Fatal` on a corrupt index, and that exit would end the agent.
- Building the index in the host process. A library crash would end the agent, and the build reads the whole workspace.
- A cross-process file lock for each workspace. It adds platform code, and the later rename of `CURRENT` settles a race between two builds.
- A checksum over the whole index on each call. It reads every byte on every search.
- Index fields in the JSON arguments. The model writes those arguments, and the parser rejects unknown keys.
- Environment variables for the index arguments. They need a second mechanism for the tests to check.
- `encoding/gob` for the manifest. Reflection is barred from wire and journal codecs.
- No record for a file past the 1 GiB cap. Such a file would count on every call, and each new process would start a build.
- No build cap. The library allocates a 64 MiB posting buffer and a 64 MiB sparse set, and the cap bounds build time and index size.
