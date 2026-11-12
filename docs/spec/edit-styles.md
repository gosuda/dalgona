# Edit styles

## Scope

The `patch` tool MUST support `anchor`, `replace`, `hashline`, `hashline-light`, `hashline-enhanced`, and `apply_patch`. The aliases MUST be `balanced` for `anchor`, `simple` for `replace`, and `strict` for `hashline`. `anchor` MUST be the default.

`apply_patch` MUST use tool name `apply_patch` and freeform input. Other styles MUST use tool name `patch`. Other styles MUST accept an `input` string, except `replace`, which MUST accept a `changes` array.

Symbol locators and symbol search are outside this contract. Parsers MUST reject `N*`, `Node`, `Symbol`, and `DefTag` operations.

## Common representation

Every parser MUST lower its input into the shared edit representation. The representation MUST support text, quoted span, line range, gap, and whole-file locators. It MUST support replacement, insertion before, and insertion after. It MUST support create, delete, and rename operations.

A parser MUST NOT read or write a target file. One resolver MUST validate guards and calculate byte ranges. One writer MUST apply every staged file.

Each file MUST be valid UTF-8 text of at most 16 MiB. The resolver MUST reject NUL bytes. It MUST preserve a BOM, mixed line endings, and an unterminated final line. Inserted text MUST use CRLF only when CRLF is dominant in the source file.

Paths MUST remain below the thread workspace. Absolute paths, NUL bytes, and parent traversal outside the workspace MUST fail. Rooted paths and paths with a volume name MUST fail the same way as absolute paths. A rename destination MUST NOT contain parent traversal.

## Grammars

### Replace

A `changes` entry MUST contain `path` and exactly one action:

- `old` with `new`, with optional `line` and `all`.
- `tag` with `new` for whole-file replacement.
- `new` without `old` for observed whole-file replacement.
- `create` containing complete file content.
- `delete` set to `true`.
- `rename` containing the destination path.

Unknown fields MUST fail. An empty `old` MUST fail. Non-`all` text MUST match exactly once, unless `line` selects a match. `all` MUST have whole-file proof.

### Anchor

`*** File: path` opens a file. An optional `#TAG` supplies whole-file proof. `*** Find` accepts exact text, `@N`, `N-M`, or `all`. `*** Replace`, `*** Insert Before`, and `*** Insert After` define the action. `*** Replace File` replaces the whole file. `*** New File`, `*** Delete File`, and `*** Move: old -> new` define lifecycle operations.

Every edit MUST address the original payload image. Edits on one file MUST NOT overlap.

### Hashline

`[PATH#TAG]` opens an existing file. The tag MUST contain four uppercase hexadecimal characters. `[PATH#NEW]` opens a new file. `PUT N:`, `PUT N-M:`, and `PUT N.=M:` replace source lines. `PUT <N:`, `PUT >N:`, and `PUT >$:` insert at source gaps. `CUT` removes lines. `REM` deletes a file. `MV DEST` renames after content edits.

Every `PUT` body MUST have at least one `+` row. The parser MUST remove exactly one leading `+`. Empty `PUT` bodies MUST fail. New files MUST have exactly one `PUT >$` operation. `REM` MUST NOT share a section with another operation.

### Hashline Light and Enhanced

Existing files MUST use `[PATH@ReadRef]`. New files MUST use `[PATH#NEW]`. Legacy tags, tagless headers, `latest`, and inferred references MUST fail. Quoted paths and quoted `MV` destinations MUST use one JSON string decoder.

A read reference MUST be `r<32 lowercase hex>.<nonzero base-36>`. It MUST identify an immutable snapshot and its path. A new store MUST reject an unknown reference. A store rebuilt from result metadata MUST restore the same reference, snapshot, path, and shown ranges.

Light permits an effective edit over unseen rows. Enhanced MUST require every affected source row in the shown ranges. Explicit `REM` is exempt from row coverage. Pure gap insertion is exempt unless it changes an unterminated final row. A valid no-op MUST NOT require new row coverage.

The grammar for `PUT`, `CUT`, `REM`, and `MV` follows hashline. For NEW, body rows MUST join with LF without a framing-derived final LF. One blank `+` creates zero bytes. A trailing blank row creates a final LF.

### Apply patch

Input MUST begin with `*** Begin Patch` and end with `*** End Patch`. File operations MUST be `*** Add File`, `*** Delete File`, or `*** Update File`. An update MAY include `*** Move to: <path>`. Hunks MUST start with `@@`. Hunk rows MUST start with space, `+`, or `-`. Added files MUST have at least one `+` row. `*** End of File` restricts the match to EOF.

Update chunks MUST resolve in payload order. Pure additions append at EOF. Other chunks replace quoted text after the previous chunk.

## References and lineage

A snapshot MUST contain complete source bytes, a SHA-256 digest, and complete shown line intervals. Read and search output and metadata MUST come from the same snapshot. Truncated rows MUST NOT receive shown coverage.

Snapshot metadata MUST be the object `{"ref","path","blob","shown","revision","epoch"}`, where `shown` is a list of `[first,last]` line pairs. A `read` result carries one such object. A `search` result carries `{"captures":[...]}` with at most 64 KiB of metadata. The `search` output MUST print `[PATH#TAG]`, `[PATH#VERSION]`, and `[PATH@ReadRef]` only for each capture in the `captures` array. Restore MUST reject metadata with unknown, duplicate, or missing keys. Restore MUST reject trailing data. Restore MUST reject a blob that does not match the bytes. Restore MUST reject shown lines outside the bytes. Restore MUST reject a zero revision or epoch. Restore MUST NOT move a path head to an older revision or epoch. The next issued reference MUST be greater than every restored one.

The store MUST issue monotonic references. It MUST NOT reuse a reference after eviction or counter exhaustion. Equal captures MUST have distinct references. Unchanged captures MAY share a revision.

The store MUST record exact splices for sound line edits. A span insertion at its start MUST shift the span without expanding it. An insertion at its end MUST leave the span unchanged. An insertion inside a span MUST conflict. A replacement overlapping any target cell MUST conflict.

A gap MUST survive only while both original neighbors remain adjacent. An insertion at the same gap MUST conflict. A changed neighbor MUST conflict. A later undo MUST NOT revive a conflict. Missing history, an external digest change, or an unsound splice MUST end the usable lineage.

## Omission placeholders

A write MUST NOT stand in for omitted code with a comment that holds an ellipsis.
An ellipsis is three periods or the character U+2026.
A comment is a placeholder when its text holds only dots or brackets.
A comment is also a placeholder when its text starts with a listed phrase. A space or the end of the text MUST follow the phrase.
The phrases are `rest of`, `existing code`, `existing implementation`, `existing methods`, `remaining code`, `remaining implementation`, `remaining methods`, `unchanged`, and `previous code`.
The phrases are also `same as before`, `code omitted`, `implementation omitted`, `omitted`, `other methods`, `keep existing`, `keep the rest`, and `etc`.
The comment prefixes MUST be `//`, `#`, `--`, `;`, `/*`, `(*`, or `<!--`.
A placeholder MUST be refused only when the old file holds fewer copies of the same line.
Files with the extensions `.md`, `.markdown`, `.mdx`, `.rst`, `.txt`, and `.adoc` MUST NOT be checked.
A refused call MUST name the file and the line, and MUST write nothing.

## Planning and recovery

`Plan` MUST calculate every pre-image and post-image before any write. It MUST set `PreSHA` and `PostSHA` to SHA-256 hex. An absent file MUST have an empty pre-image. A deleted file MUST have an empty post-image. `Pre` MUST contain exact pre-image bytes.

All parse, binding, coverage, shape, overlap, and path checks MUST finish before the first write. Valid no-ops MUST return `no_change`. Mixed batches MUST apply only effective edits. A plan that fails after preparation MUST release its pinned directory handles. A second release MUST succeed.

A plan that will not be applied, such as a declined approval, MUST be released through `Discard`.

A call MUST contain at least one edit. A call MUST NOT contain more than 64 edits. A call MUST NOT contain more than 8 MiB of input.

`Plan` MUST unmask secret placeholders after parsing and before resolution. It MUST unmask only locator text, apply_patch context, and new content. It MUST NOT unmask paths, rename destinations, tags, or references. A placeholder with no known value MUST fail the call. The error MUST name the placeholder.

## Writer

The writer MUST recheck every source digest and every required absence before publication. It MUST stage complete post-images and pre-image backups in sibling temporary files. Publication MUST use a temporary file and atomic rename for replacement. Creation and rename destinations MUST use no-replace publication where the runner supports it. Other runners MUST return an error whose text contains `no_replace_unavailable` for those operations.

A call MUST NOT stage more than 64 MiB of file content.

A mid-commit failure MUST attempt reverse-order restoration of every completed effect. The error MUST name every affected source and destination path. A rollback failure MUST name each failed restoration and every retained backup. The writer MUST NOT report `nothing was written` after a completed write and successful restore.

Cancellation before the first target mutation MUST leave every target unchanged. Cancellation after the first mutation MUST NOT interrupt commit or rollback between effect boundaries.

A successful apply MUST print `[PATH#DELETED]` for each deleted file.

Existing file modes MUST survive updates. Renames MUST carry the source mode. New files MUST use mode `0644`, subject to the process umask.

For the local runner, every platform MUST write through a directory handle held from planning.
Unix MUST use `openat`-family calls. Other platforms MUST use `os.Root`, so no write resolves outside the workspace.
A symbolic link MUST be refused when the pin is taken.
On Windows, a read-only target MUST stay read-only after replacement.

## Errors

Every parse error MUST end with this exact suffix, with the selected style substituted:

```text
Current edit_style is "<style>" for this model: past calls in history may use another dialect. Do not imitate them.
```

A stale tag or reference MUST fail without a write. An Enhanced refusal MUST NOT downgrade to Light.
