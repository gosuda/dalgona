#### Specifications step 31. Correct `docs/spec/search.md`
Anchor: Richard Feynman, Edsger W. Dijkstra, Italo Calvino

Change `docs/spec/search.md` in one commit. Line numbers are from commit `3c4b6f94`.

Add these statements. The code already behaves this way.

| After line | Add | Evidence |
|---|---|---|
| 5 | Search MUST return at most `limit` results. The `limit` argument MUST be from 1 to 1000. | internal/tool/search/search.go:28 `find returns at most limit paths, sorted. grep returns at most limit matches`; search.go:29 schema `"limit":{"type":"integer","minimum":1,"maximum":1000}`; internal/tool/search/json.go: |
| 5 | Search MUST skip `.git` and every path that `.gitignore` or `.git/info/exclude` excludes. Search MUST NOT follow symbolic links. | internal/tool/search/engine.go:152-155 `func (w *walker) excluded` returns true for `entry.Type()&os.ModeSymlink != 0` and for a directory named `.git`; engine.go:156-158 loads each directory's ignore files; internal/too |
| 5 | The `grep` mode MUST skip binary files and files that are not valid UTF-8. At commit `3c4b6f94`, only the `grep` mode reads file contents. | internal/tool/search/engine.go:287-293 `func scanFile` calls `searchableFile` (line 288) and `readCapped` (line 291), then returns when `!searchableText(data)` (line 292); engine.go:313-314 `func searchableText` requires |

Structure:

- Call order resolved: engine.go:288-291 calls searchableFile (Lstat, regular file, size up to 16 MiB) before readCapped, so the 16 MiB rule holds before the read.
- Scope resolved: the 16 MiB and binary checks apply to grep only (engine.go:287-293). At commit `3c4b6f94`, the walker rules (.git, ignore files, symbolic links) apply to both modes (engine.go:152-158).
- The document still names no tool. The add block names grep in the binary rule and states which mode reads contents, which covers the audit's structure note.

Then make every sentence of the file follow rule 3 of this plan. Split each sentence that holds a semicolon outside code into two sentences. Keep every literal. The file has 0 style findings at commit `3c4b6f94`.

Proof: `uv run --no-project --python 3.12 docs/spec/check_style.py docs/spec/search.md` prints `style findings: 0`, and `just verify` passes.

