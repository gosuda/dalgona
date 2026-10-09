# web

Two model tools.

`web_fetch` fetches one http or https URL and returns the page as Markdown.
It follows at most `max_redirects` redirects (default 10), revalidates the
scheme after every hop, reads at most `max_bytes` body bytes (clamped into
1024..=`max_body_bytes`, default ceiling 2097152), converts `text/html` and
`application/xhtml+xml` to Markdown, passes `text/*` and `application/json`
through unchanged, and rejects every other media type. The Markdown is
capped at `max_markdown_bytes` (default 131072); a cut sets `truncated` and
appends a truncation marker.

`web_search` queries Brave Web Search. The query is 1 to 400 characters;
`count` is 1 to 20 and defaults to 5. Results carry `title`, `url`, and
`snippet`. The API key is read at call time from the environment variable
named by `api_key_env` (default `BRAVE_API_KEY`); without it the tool
reports that it is not configured.

Config: the `[plugin.web]` table with `enabled`, `provider` (`brave` is
the only value), `api_key_env`, `timeout_secs` (1 to 300, default 30),
`max_redirects` (0 to 20, default 10), `max_body_bytes` (default
2097152), and `max_markdown_bytes` (default 131072). The battery needs
the `net` and `env` permissions; a missing permission makes each call
fail with a denied error naming the service. Requests go through the
host `net` service only; loopback HTTP(S) URLs use that same service
and remain subject to the host's network policy.
