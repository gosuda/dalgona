# mcp

The MCP client battery maps declared server tools into the session.
Declarations come from skill frontmatter; nothing is reachable until the
session grants the full declared server set and the call passes the normal
approval ladder.

## Declare a server

Each entry under `mcp.servers` in skill frontmatter has exactly one
`launch` or `url`. These JSON objects show the accepted server shapes;
replace the example programs and packages with your server's values.

```json
{"launch":{"kind":"command","argv":["local-mcp-server","--workspace","/srv/project"]}}
{"launch":{"kind":"npx","package":"files-server","version":"1.2.3","args":["/srv/project"]}}
{"launch":{"kind":"uvx","package":"files-server","executable":"files-server","args":[]}}
{"url":"https://mcp.example/server"}
```

`version` is optional for `npx` and `uvx`; omitting it is shown as an
unpinned selection. `args` defaults to an empty list. `uvx` requires an
explicit executable. A launch may also have an `env` object beside it;
its string values are literal, not expanded. `url` does not accept `env`.
The former `command` server field is rejected. Move an arbitrary argv
list into `launch` with `kind: command`. Unknown fields and null optional
values are rejected. Public URLs must use HTTPS and must not contain
userinfo or a fragment.

## Review the target

Grant and tool approval previews show the declared package and version,
plus the executable for `uvx`, or the HTTPS scheme and authority. Arbitrary
commands remain opaque, including their executable and every operand.
All environment names and values, URL paths and queries, and remote tool
argument JSON stay hidden. A package label identifies the requested
target; it does not attest to installed code. Grant identity covers the
full declaration, so different hidden operands do not share a grant merely
because their previews match.

## Transports and limits

stdio and streamable-HTTP transports are supported, on protocol revisions
2026-07-28 and 2025-11-25. Mapped tools stay deferred to the model and are
promoted at a turn boundary under `<skill>.<server>.<tool>` names. Calls
run on the exec-class approval ladder; HTTP servers authenticate through
OAuth with issuer and resource binding. OAuth discovery and token requests
use public resolved addresses unless the configured MCP endpoint is
explicitly local, and HTTP redirects are not followed. Each OAuth request
to an origin other than the configured endpoint asks for an exact-origin
approval immediately before the operation that needs it: metadata fetches
during discovery, the token endpoint before a refresh or code exchange,
and the registration endpoint before a dynamic client-registration POST.
A declined approval skips only the operation it named; a declined
registration falls back to the stored client ID or a manual entry. The
browser authorization URL is opened by the user, not fetched by the
client, so it asks for no network grant. The authorization callback does
not authorize an exchange by itself; an affirmative prompt and the
validated callback are both required. The client speaks the tools-only
protocol subset; there is no HTTP+SSE-only transport.

Limits: discovery 5 s, start 10 s, list 15 s, call 60 s (600 s with
progress), one restart per session and isolation key, 50 list pages,
result text capped at 524288 bytes. Token files are stored at mode 0600.
