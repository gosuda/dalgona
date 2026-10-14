# The complexity guard

Four report groups: turn growth, per-file absolute metrics, per-function metric changes, best current. The guard is off by default in dal and on in dalgona. Notices name the file and the metric. To turn it off, set `enabled` in the `[guard]` table in `dal.toml`:

```toml
[guard]
enabled = false
```
