# locate: Search the workspace or read a file.

## Ported from
Original to dal; no pi counterpart.

## What it shows
One tool over two file operations. The `lookup` tool reads the file named by `path`; it returns the file text and passes the read view to `dal.output` for display. Without a path, it searches the workspace for `pattern` and returns the matches and the `truncated` flag. With neither, it returns a `locate` error. The command `/locate:locate` takes the pattern as its positional argument.

## What differs from pi
Pi gives the model separate read and search tools. Here one tool picks the operation from its arguments.

## Install
Copy to the dal plugins data root and add `locate` to `plugins`.

## Settings
None.
