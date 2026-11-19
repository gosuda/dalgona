### Workstream: Comment removal

Every prose comment leaves the Go, hand-written TypeScript, Svelte and Python source of dalgona at commit `3c4b6f94`. Each rule that a comment held gets a home: a name or a type that makes the code say it, or one sentence in the topic file under `docs/spec`, added in the commit that deletes the comment. Compiler directives and `//nolint` lines with a reason stay. Generated TypeScript under `web/src/lib/codex/` keeps its banners and is skipped by directory. The last step adds `TestRepositoryHasNoProseComments` in `internal/commentgate`, which scans the four languages and fails when a prose comment returns. The inventory has 81 rows in 21 steps: 72 Go rows, 8 Python rows and 1 Svelte row. Hand-written TypeScript has no comment line.

