# Search

## Scanning

Search MUST skip any file over the 16 MiB search ceiling before it reads the file. Search MUST scan only regular files.

Search MUST return at most `limit` results for the find and grep modes. The definition mode MUST print at most `limit` source lines. The `limit` argument MUST be from 1 to 1000.

Search MUST skip `.git` and every path that `.gitignore` or `.git/info/exclude` excludes. Search MUST NOT follow symbolic links.

The `grep` mode MUST skip binary files and files that are not valid UTF-8. Only the `grep` mode reads file contents.

## Index

The index MUST live under the product home, in one directory for each workspace. The builder MUST index only the workspace root, and MUST write only its generation directory.

The builder MUST add only regular files that a grep call could read, each at most 16 MiB. The builder MUST stat a file before it reads the file, and MUST NOT follow a symbolic link.

The builder MUST write no record for a file that is not regular, is over 16 MiB, or has no change time. A file that the builder does not read or index MUST be recorded as not indexed.

The builder MUST leave unread each file whose size would take the total of indexed bytes past 1 GiB. The builder MUST keep reading the files after that file. The builder MUST sync the trigram file and the manifest before it exits with status 0.

Only the parent MUST publish a generation, by an atomic rename of `CURRENT`, after the builder exits with status 0. A publish MUST remove every other generation directory of the workspace.

On platforms without change times, including Windows, the index MUST NOT be built or used. The builder MUST run in the sandbox of the search call.

On macOS, Search MUST scan without an index until native sandbox runtime checks pass. Search MUST NOT start an index build there.

A grep call with an index MUST return the same output as a grep call without one. The index MUST let the search skip a file only when its trigram query cannot match the file.

A file MUST count as covered only when its size, modification time and change time equal its manifest record. A covered file MUST have both times more than 2 seconds older than the build start.

The search MUST read every file when its clock is earlier than the build start. A file without a covering record MUST be read as it is without an index.

A file that the last build did not index MUST NOT count toward the build threshold while its stat is unchanged.

The find mode MUST NOT use an index. The search child MUST exit with status 1 only when the index is unusable.

The search child MUST treat a generation deleted during the call as no index. The search child MUST write nothing to standard error when it succeeds. The search MUST NOT show the unindexed byte count to the model.

Search MUST use an index only for grep calls, only with a local runner, and only when the sandbox enforces its writable roots. Search MUST NOT read or build an index when the sandbox is off or the runner cannot enforce it.

The index root MUST be the product home, plus `search`, plus a 16-digit hash of the canonical workspace path. Search MUST NOT use an index when the index root lies inside the workspace, the temp directory, the user cache, or a writable root.

Search MUST pass the index root, the generation name, its clock, and the racy window to the search child as arguments. The model MUST NOT be able to choose the index root or the generation. A generation is the directory that `CURRENT` names. The trigram file and manifest are files inside it. The racy window is the 2-second freshness margin. A file is covered only when its modification and change times are more than that long before the build start.

An index failure is exit status 1 whose output names an unusable index or a corrupt index. After an index failure, Search MUST remove `CURRENT` only when it still names the failed generation. Removing `CURRENT` reads the file and then removes it, so a publish between those two steps is removed too. The search then runs without an index until the next build, and its output does not change. Search MUST run the same call again without an index, under a new process id. Search MUST log an index failure at warn level, and the model MUST NOT receive it.

When a grep call counts at least `indexRebuildBytes` uncovered bytes, Search MUST start one build for the workspace.

The search call MUST NOT wait for a build. A build that runs longer than 10 minutes MUST be killed. Any build failure MUST kill the build child and wait for its exit. The generation MUST be removed only after that exit is confirmed.

A workspace MUST have at most one build start in each process, even when that build fails.

After an unusable index, Search MUST NOT start a build for that workspace in any process for 10 minutes. A failed build MUST NOT block a build in a new process.

Deleting the directory `search` under the product home MUST restore the scan, and no other state changes.

## Symbols and syntax

The symbols mode MUST list the definitions under `path` as `PATH:START-END: KIND TEXT` rows.

A child row MUST be indented by two spaces for each level of nesting.

Definitions inside a function, method, constant, or variable MUST NOT be listed.

The symbols mode MUST parse only files whose extension belongs to a language with a definition query.

The `.h` extension MUST belong to the `c` language, and it MUST NOT select `cpp`.

A non-empty pattern MUST first select the files in which some line matches it.

A name pattern that is not literal MUST NOT use `^`, `$`, `\A`, `\z`, `\b`, or `\B`.

The owner of a Go method MUST be its receiver type.

The owner of a nested symbol MUST be the qualified name of its parent.

A Markdown section MUST name its parent section as owner.

A Rust `impl` block MUST give no row and MUST name the owner of its members.

A name pattern MUST split at its last dot. The split applies when the text before the dot is not empty. The text after the dot MUST be a plain name.

A plain name holds only letters, digits, underscore and dollar. A pattern that ends with a dot MUST NOT split.

A symbol MUST match when the whole pattern matches its name. A symbol with an owner MUST also match when the owner part matches its owner and the name part matches its name.

File selection MUST accept a file when some line matches the whole pattern or the name part. A qualified name never sits on one line, so the owner part alone MUST NOT select a file.

The `kind` argument MUST be one of the eleven kinds. It MUST NOT be set in any mode other than `symbols` and `definition`.

The `pattern` argument MAY be empty only in the symbols mode.

The symbols mode MUST parse at most 256 files and 1 MiB of source in one call.

A file over 256 KiB MUST NOT be parsed.

Each parse MUST stop after 2 seconds. A call MUST stop starting new files after 10 seconds, and it MUST count each remaining file of a supported language as stopped.

The search child MUST stop after 30 seconds, and the search MUST return an error that names the deadline.

A file whose parse stopped MUST give no rows, and it MUST appear in a `[Parse: stopped, no rows:]` line.

A file whose tree has syntax errors MUST keep its rows, and it MUST appear in a `[Parse: syntax errors, rows kept:]` line.

The call MUST print `[Stopped: N files not parsed. Narrow path or glob.]` when the start budget stops dispatch. The call MUST print `[Not parsed: N files. Narrow path or glob.]` when files overflow the parse budgets. The call MUST print `[Syntax only: N files. Use mode syntax with lang.]` for files whose language has no definition query.

Symbol rows MUST be sorted by path and then by start line.

Symbol rows MUST NOT carry snapshot tags.

Markdown setext headings MUST NOT give a symbol. Only ATX headings give a section symbol.

### Definition

The definition mode MUST print each matching definition as a header row, followed by its source lines.

Each body row MUST use the form `PATH:LINE: TEXT`.

The definition mode MUST cap the body rows at `limit` for one call, and it MUST end the body with `[Truncated: N more lines]` when it cuts.

The definition mode MUST NOT accept an empty pattern.

A header row MAY be read with the `read` tool, from line START through line END.

A definition longer than 2000 lines needs one `read` call for each 2000 lines.

### Syntax

The syntax mode MUST run a tree-sitter query on each file of the language named by `lang`.

A query that does not compile MUST return the library message after the prefix `syntax: `.

Each capture row MUST show the file, the line, the capture name, and the captured text.

The `lang` argument MUST be set in the syntax mode, and it MUST NOT be set in any other mode.

The syntax mode MUST NOT carry snapshot tags.

## References and imports

The references mode MUST list each call and each type use of one bare name.

The importers mode MUST list each import whose path matches one import path.

Both modes MUST parse only files of Go, TypeScript, Python, Rust and Java.

Both modes MUST match by name. Both modes MUST NOT resolve types.

A reference MUST have the kind `call` or `type`. A plain read of an identifier MUST NOT give a row. Use the grep mode for it.

A name inside a comment or a string MUST NOT give a row.

The name of a definition MUST NOT give a row, even when the definition is local.

In Go, a type use of a predeclared type MUST NOT give a row.

When a call and a type use share one place, the row MUST be a `call` row.

A references row MUST have the form `PATH:LINE: KIND NAME in CALLER by name`.

A reference outside every listed definition MUST read `PATH:LINE: KIND NAME at top level by name`.

The `CALLER` value MUST be the qualified name of the innermost listed definition that contains the reference.

References rows MUST sort by path, then by line, then by column.

An importers row MUST have the form `PATH:LINE: import SPEC by name`. Importers rows MUST sort by path, then by line.

The words `by name` MUST end every row. A row MUST NOT carry a confidence number or a snapshot tag.

The `pattern` argument MUST NOT be empty.

The references pattern MUST be one bare name of letters, digits, underscore and dollar. The match MUST be exact and case-sensitive.

A qualified pattern such as `Server.Run` MUST be refused in the references mode.

The importers pattern MUST be one import path without white space.

A spec MUST match when it equals the pattern. A spec MUST also match when it ends with the pattern.

The character before that ending pattern MUST be `/`, `.` or `:`.

The `literal` and `ignore_case` arguments MUST be refused in both modes.

The `kind` and `lang` arguments MUST be refused in both modes.

The `path` and `glob` arguments MUST bound the files. The `limit` argument MUST bound the rows.

A cut MUST end the result with `[Truncated: N more rows]`.

Both modes MUST apply the file, byte, parse time and call time limits of the symbols mode.

The references mode MUST parse only files that contain the exact spelling of the name.

The importers mode MUST parse every file of a language with a graph, until a limit stops it.

All loads in one call MUST share the same file, byte and time limits.

A file skipped only because the name is absent MUST remain eligible for a later load.

An admission refusal MUST count each file once.

Rust imports MUST expand braces into leaf paths and remove aliases. A `self` leaf MUST name its parent path.

Go and TypeScript imports MUST omit their enclosing quotes. Python imports MUST keep their module text.

Java imports MUST omit `import`, `static` and the final semicolon. A trailing `.*` MUST remain.

A file whose parse stopped MUST give no rows. It MUST appear in a `[Parse: stopped, no rows:]` line.

A file that fails to parse for any other reason MUST give no rows. The call MUST go on with the next file.

A file whose tree has syntax errors MUST keep its rows. It MUST appear in a `[Parse: syntax errors, rows kept:]` line.

A cancelled call and a call that reaches the 30 second deadline MUST return an error. They MUST NOT return an empty result.

A call without rows MUST print `No matches for NAME by name. Searched N files of languages with a graph.`

The value N MUST count the files in scope whose language has a graph.

A call MUST print `[No graph: N files of LANGS. Use grep.]` when files in scope have a supported language without a graph.

LANGS MUST list those languages in sorted order, separated by a comma and a space.

The footers `[Not parsed:]`, `[Stopped:]` and `[Parse:]` of the symbols mode MUST keep their texts.

## Fuzzy file search

The `fuzzyFileSearch` method, defined in [protocol.md](protocol.md), MUST list regular files under each requested root and MUST NOT list directories.

The method MUST skip `.git` and every path that `.gitignore` excludes. It MUST NOT follow symbolic links.

A root MUST hold at most 100000 file paths. A root list MUST be reused for 5 seconds after it is walked.

A query MUST match a path when its characters occur in the path in order, ignoring case. The matcher MUST scan the path from left to right. It MUST match each query character to its first occurrence after the previous matched character.

The `indices` member MUST list the matched positions, counted in characters of the path from 0.

The number of gaps MUST be the last matched index, minus the first matched index, plus 1, minus the length of the query.

The score MUST be 100000, minus 100 for each gap, minus the length of the path in characters. It MUST add 1000 when the last matched character lies in the base name. It MUST add 2000 when the base name starts with the query. A score below 0 MUST count as 0.

Results MUST sort by descending score, then by ascending path. The method MUST return at most 50 results.

A query MUST NOT be empty or longer than 256 bytes. Each root MUST be an absolute directory, and the method MUST take 1 to 8 roots.

A request with a `cancellationToken` MUST answer `{"files":[]}` when a newer request with the same token arrives.
