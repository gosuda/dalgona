# Rewrite

The `rewrite` tool replaces the text of one named capture in each match of a tree-sitter query. It is an edit-class tool.

## Arguments

The `lang` argument MUST name one of the grammars that `search` lists. The `pattern` argument MUST be a tree-sitter query that compiles with that grammar.

The `target` argument MUST name a capture of the query. The `replace` argument MUST be the new text.

The `path` and `glob` arguments MUST select files as they do in `search`.

The tool MUST reject any other argument.

## Replacement text

The replacement text MUST write `@@` as one literal `@`.

The replacement text MUST write `@name` or `@{name}` to insert the text of a capture of the same match. The tool MUST reject any other `@` and MUST name its byte offset.

The tool MUST refuse a match in which the target is absent, appears twice, or has zero width.

The tool MUST refuse two targets that overlap. The refusal MUST name the file and the line.

The tool MUST splice replacement bytes as written. The tool MUST NOT convert line endings.

## Refusals

A refused call MUST NOT write any file.

The tool MUST refuse a candidate file that has syntax errors before the call. The tool MUST refuse a changed file that has syntax errors after the rewrite.

The tool MUST refuse a candidate file over 1048576 bytes, and the refusal MUST name that file.

The tool MUST refuse a call that selects more than 256 files. The tool MUST refuse a call that changes more than 32 files or makes more than 256 replacements.

The tool MUST refuse a call on a runner that is not local.

The tool MUST stop planning when 90 seconds have passed.

## Secrets

The tool MUST unmask secret placeholders in `pattern` and `replace` only. The tool MUST NOT unmask `path` or `glob`.

## Result

The result MUST begin with a summary line. It MUST then give one line for each changed file, in path order, and the post-image of each changed file.

A call with no changed file MUST return the text `No matches for` followed by the pattern.

A replacement that still matches the query MUST be allowed. Each call changes the file again.

## Preview

The tool MUST NOT offer a preview option. The `syntax` mode of `search` MUST serve as the preview.

The `syntax` preview MUST list a file over its own size cap in its footer.
