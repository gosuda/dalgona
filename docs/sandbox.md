# The sandbox

With `sandbox = true` in dal.toml (or `--sandbox`), every `exec` child runs
through the platform sandbox helper: Landlock on Linux through the
`dalgon __sandbox` helper, Seatbelt on macOS, and an AppContainer on Windows.
The allowed roots are the workspace, the system temp directory, the platform
cache directory, and the `sandbox_writable` entries; nothing else under the
home directory is writable, and there is no network restriction. The Windows
backend matches the read-open model as far as DACL editing safely reaches:
the per-run `dalgon.sandbox.<guid>` container reads and executes wherever
system ACLs already permit application containers (`ALL APPLICATION
PACKAGES`), plus the launch directory and every `PATH` directory. Each
writable root's ancestors get traverse access only (POSIX `+x` parity): the
container resolves through them but cannot list or read sibling files —
a failed grant there fails the launch, since a partially installed read
policy is worse than none. Drive roots are never
touched: an inheritable ACE on a drive root would propagate to every file
on the volume. Writes stay under the granted roots, which get full
control. If the helper is missing
or the kernel cannot enforce the sandbox, dal refuses the command with the
setup error instead of running it unsandboxed. It does not cover plugins
running with your permissions.
