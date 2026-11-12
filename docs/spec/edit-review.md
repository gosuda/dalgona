# Edit review

The edit review judges each edit on one diff.

## Diff

The package `internal/textdiff` holds one function, `Unified`. `Unified(oldName, newName string, pre, post []byte, context int)` returns the diff text, a `Stats` value, and an error. `Stats` holds `Added` and `Removed`. `Added` counts the `+` lines of all hunks. `Removed` counts the `-` lines of all hunks.

The package is built on `github.com/aymanbagabas/go-udiff v0.4.1`. The diff text starts with `--- <old label>` and `+++ <new label>`. Callers MUST pass `a/<path>`, `b/<path>`, or `/dev/null` as the labels. Each hunk starts with a `@@` header. Equal inputs return an empty text and a zero `Stats`.

The library writes `\ No newline at end of file` after a line without a final newline. The function MUST NOT write its own marker.

`textdiff.Limit` is `1 << 20` bytes. A side above `textdiff.Limit` MUST NOT be diffed. The caller MUST skip that file.

The edit unit MUST pass 3 for `context`. The turn unit of the edit review MUST pass 0.
