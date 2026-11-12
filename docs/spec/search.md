# Search

## Scanning

Search MUST skip any file over the 16 MiB search ceiling before it reads the file. Search MUST scan only regular files.

Search MUST return at most `limit` results. The `limit` argument MUST be from 1 to 1000.

Search MUST skip `.git` and every path that `.gitignore` or `.git/info/exclude` excludes. Search MUST NOT follow symbolic links.

The `grep` mode MUST skip binary files and files that are not valid UTF-8. Only the `grep` mode reads file contents.
