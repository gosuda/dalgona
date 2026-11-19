#### Protocol step 17. `fs/*` file methods

- New `internal/rpc/serve_fs.go`: `fsReadFile`, `fsWriteFile` (temp file in the same directory plus rename, mode of the old file when it exists, else `0o644`), `fsCreateDirectory`, `fsGetMetadata` (`Lstat` for `isSymlink`, `Stat` for the rest, `createdAtMs` 0), `fsReadDirectory` (sorted by name, `Stat` through links), `fsRemove`, `fsCopy` (`os.CopyFS` for directories). Shared `func absPath(p string) (string, error)` returns `path must be absolute` for a relative path.
- `responseSchemas` gains the seven `v2/Fs*Response.json`.
- Test `internal/rpc` `TestFsMethodsRoundTrip` (write, read, metadata, list, copy, remove; relative path fails `path must be absolute`; missing file fails `no such file: <path>`). Fails before: `-32601`.
- Docs: `protocol.md`.

Anchor: Dieter Rams.

