# web

Two model tools.

`web_fetch` fetches one http or https URL and returns the page as Markdown.
It follows at most `max_redirects` redirects (default 10), revalidates the
scheme after every hop, reads at most `max_bytes` body bytes (clamped into
1024..=`max_body_bytes`, default ceiling 2097152), converts `text/html` and
`application/xhtml+xml` to Markdown, passes `text/*` and `application/json`
through unchanged, and rejects every other media type. The Markdown is
capped at `max_markdown_bytes` (default 131072); a cut sets `truncated` and
appends a truncation marker. A transport failure (no connection, reset, TLS
failure) reports `web_fetch failed: no response from <url>`.

`web_fetch` never fetches a private address. Loopback, link-local, RFC 1918,
unique-local, unspecified, and the other non-public ranges refuse with
`web_fetch: <host> is a private address; fetch public URLs only.` The check
runs on every hop, redirects included, against the address the name
resolves to, not the name: a host name with any private address in its
answer refuses, and so does a literal such as `http://2130706433/`. A name
resolves once. An `http` hop connects to the first address the resolver
lists, which is the one that passed the check, and sends the original `Host`
header, so the answer cannot change between the check and the connection
(DNS rebinding). An `https` hop keeps the host name, so the certificate
check binds the connection to the name that was checked; that hop pins no
address. To fetch a private URL, use `exec` with `curl`.

`web_search` queries one provider, Brave Web Search or Tavily Search. The
query is 1 to 400 characters; `count` is 1 to 20 and defaults to 5. Results
carry `title`, `url`, and `snippet`, and `published_date` when the provider
sent one (Tavily's `published_date` passes through unchanged; Brave sends
none). Unknown fields in a provider reply are dropped. Brave sends
`GET /res/v1/web/search` with the `X-Subscription-Token` header; Tavily sends
`POST /search` with `Authorization: Bearer` and a JSON body of `query`,
`max_results`, `search_depth`, `topic` (`general`), `include_answer`
(`false`), and `include_raw_content` (`false`). The API key is read at call
time from the environment variable named by `api_key_env`; without it the
tool reports that it is not configured and sends nothing. The key never
appears in a result or an error: every occurrence is replaced with `***`.

Config: the `[plugin.web]` table with `enabled`, `provider` (`brave` or
`tavily`), `api_key_env` (default `BRAVE_API_KEY`; a `tavily` provider that
keeps that default reads `TAVILY_API_KEY`), `search_depth` (`basic` or
`advanced`, default `basic`, used by Tavily), `timeout_secs` (1 to 300,
default 30), `max_redirects` (0 to 20, default 10), `max_body_bytes`
(default 2097152), and `max_markdown_bytes` (default 131072). The battery
needs the `net` and `env` permissions; a missing permission makes each call
fail with a denied error naming the service. Requests go through the host
`net` service only.