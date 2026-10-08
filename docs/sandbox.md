# The sandbox

With `sandbox = true` in dal.toml (or `--sandbox`), every `exec` child runs
through the platform sandbox helper: Landlock on Linux through the
`dalgon __sandbox` helper, Seatbelt on macOS, and an AppContainer on Windows.
The allowed roots are the workspace, the system temp directory, the platform
cache directory, and the `sandbox_writable` entries; nothing else under the
home directory is writable, and there is no network restriction. The Windows
backend matches the read-open model: the per-run `dalgon.sandbox.<guid>`
container inherits read and execute access wherever the user's own ACLs
allow it, and writes only under the granted roots. If the helper is missing
or the kernel cannot enforce the sandbox, dal refuses the command with the
setup error instead of running it unsandboxed. It does not cover plugins
running with your permissions.
