# Search

## Scanning

Search MUST skip any file over the 16 MiB search ceiling before it reads the file. Search MUST scan only regular files.

Search MUST return at most `limit` results. The `limit` argument MUST be from 1 to 1000.

Search MUST skip `.git` and every path that `.gitignore` or `.git/info/exclude` excludes. Search MUST NOT follow symbolic links.

The `grep` mode MUST skip binary files and files that are not valid UTF-8. Only the `grep` mode reads file contents.

## Fuzzy file search

The `fuzzyFileSearch` method MUST list regular files under each requested root and MUST NOT list directories.

The method MUST skip `.git` and every path that `.gitignore` excludes. It MUST NOT follow symbolic links.

A root MUST hold at most 100000 file paths. A root list MUST be reused for 5 seconds after it is walked.

A query MUST match a path when its characters occur in the path in order, ignoring case. The matcher MUST choose the window with the fewest gaps. On a tie, the matcher MUST choose the earlier window.

The score MUST be 100000, minus 100 for each gap, minus the length of the path in characters. It MUST add 1000 when the last matched character lies in the base name. It MUST add 2000 when the base name starts with the query. A score below 0 MUST count as 0.

Results MUST sort by descending score, then by ascending path. The method MUST return at most 50 results.

A query MUST NOT be empty or longer than 256 bytes. Each root MUST be an absolute directory, and the method MUST take 1 to 8 roots.

A request with a `cancellationToken` MUST answer `{"files":[]}` when a newer request with the same token arrives.
