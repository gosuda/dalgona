# Search

## Scanning

Search MUST skip any file over the 16 MiB search ceiling before it reads the file. Search MUST scan only regular files.

Search MUST return at most `limit` results. The `limit` argument MUST be from 1 to 1000.

Search MUST skip `.git` and every path that `.gitignore` or `.git/info/exclude` excludes. Search MUST NOT follow symbolic links.

The `grep` mode MUST skip binary files and files that are not valid UTF-8. Only the `grep` mode reads file contents.

## Index

The index MUST live under the product home, in one directory for each workspace. The builder MUST index only the workspace root, and MUST write only its generation directory.

The builder MUST add only regular files that a grep call could read, each at most 16 MiB. The builder MUST stat a file before it reads the file, and MUST NOT follow a symbolic link.

The builder MUST write no record for a file that is not regular, is over 16 MiB, or has no change time. A file that the builder does not read or index MUST be recorded as not indexed.

The builder MUST leave unread each file whose size would take the total of indexed bytes past 1 GiB. The builder MUST keep reading the files after that file. The builder MUST sync the trigram file and the manifest before it exits with status 0.

Only the parent MUST publish a generation, by an atomic rename of `CURRENT`, after the builder exits with status 0. A publish MUST remove every other generation directory of the workspace.

On platforms without change times, including Windows, the index MUST NOT be built or used. The builder MUST run in the sandbox of the search call.

A grep call with an index MUST return the same output as a grep call without one. The index MUST let the search skip a file only when its trigram query cannot match the file.

A file MUST count as covered only when its size, modification time and change time equal its manifest record. A covered file MUST have both times more than 2 seconds older than the build start.

The search MUST read every file when its clock is earlier than the build start. A file without a covering record MUST be read as it is without an index.

A file that the last build did not index MUST NOT count toward the build threshold while its stat is unchanged.

The find mode MUST NOT use an index. The search child MUST exit with status 1 only when the index is unusable.

The search child MUST treat a generation deleted during the call as no index. The search child MUST write nothing to standard error when it succeeds. The search MUST NOT show the unindexed byte count to the model.

Search MUST use an index only for grep calls, only with a local runner, and only when the sandbox enforces its writable roots. Search MUST NOT read or build an index when the sandbox is off or the runner cannot enforce it.

The index root MUST be the product home, plus `search`, plus a 16-digit hash of the canonical workspace path. Search MUST NOT use an index when the index root lies inside the workspace, the temp directory, the user cache, or a writable root.

Search MUST pass the index root, the generation name, its clock, and the racy window to the search child as arguments. The model MUST NOT be able to choose the index root or the generation.

An index failure is exit status 1 whose output names an unusable index or a corrupt index. After an index failure, Search MUST remove `CURRENT` only when it still names the failed generation. Removing `CURRENT` reads the file and then removes it, so a publish between those two steps is removed too. The search then runs without an index until the next build, and its output does not change. Search MUST run the same call again without an index, under a new process id. Search MUST log an index failure at warn level, and the model MUST NOT receive it.

## Fuzzy file search

The `fuzzyFileSearch` method MUST list regular files under each requested root and MUST NOT list directories.

The method MUST skip `.git` and every path that `.gitignore` excludes. It MUST NOT follow symbolic links.

A root MUST hold at most 100000 file paths. A root list MUST be reused for 5 seconds after it is walked.

A query MUST match a path when its characters occur in the path in order, ignoring case. The matcher MUST choose the window with the fewest gaps. On a tie, the matcher MUST choose the earlier window.

The score MUST be 100000, minus 100 for each gap, minus the length of the path in characters. It MUST add 1000 when the last matched character lies in the base name. It MUST add 2000 when the base name starts with the query. A score below 0 MUST count as 0.

Results MUST sort by descending score, then by ascending path. The method MUST return at most 50 results.

A query MUST NOT be empty or longer than 256 bytes. Each root MUST be an absolute directory, and the method MUST take 1 to 8 roots.

A request with a `cancellationToken` MUST answer `{"files":[]}` when a newer request with the same token arrives.
