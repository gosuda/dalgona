# procs

One deferred tool over the background jobs of this session. Other sessions' jobs are invisible. A first call by name promotes it, and a promoted tool never leaves the tool list mid-session. The battery asks for the `jobs` service only: it reads job rows and retained output text through that service and opens no file.

`op` `list` prints one line per job, live jobs oldest first, then at most ten terminal jobs newest first: `<id> <status> <age> <label>: <command>`. The status is `running`, `done` (exit code 0), `failed` (a nonzero exit, a failure, or a lost job), or `cancelled`. The age is the time since the job's last activity, in the jobs duration format (`48.0s`, `4m12s`, `1h02m05s`). The jobs service keeps one label per job, so the command text is the label, cut at 80 characters. `query` keeps only jobs whose label contains it as a substring, `status` keeps one phase, and `limit` (1 to 50, default 20) caps the rows. A filter that keeps no job answers with the no-match text below.

`op` `grep` matches `query` as a regex over the retained output text of each job, oldest job first and each job's text read once. Each match prints `<job id>:<line>: <text>`, the text cut at `match_bytes` bytes (1 to 2048, default 256) with control characters escaped. `max_matches` (1 to 200, default 50) and a 16384-byte limit on the whole result, tail included, bound what is shown; the tail `(<k> more matches elided; narrow query or raise max_matches.)` counts every further match in the session's retained output. The regex syntax has no look-around, and the error names the reason.

A number outside its range, an unknown field, or a field of the other op is rejected as `procs: invalid input: <reason>.`

Errors, exact: `procs: no jobs in this session.`, `procs: no match for "<query>" in <n> job(s).`, `procs: query is not a valid regex: <message>.`, and `procs: the jobs service is not granted to this battery: <Denied text>.`
