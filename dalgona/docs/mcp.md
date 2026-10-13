# mcp

The MCP client battery maps declared server tools into the session.
Declarations come from skill frontmatter; nothing is reachable until the
session grants the full declared server set and the call passes the normal
approval ladder.

stdio and streamable-HTTP transports are supported, on protocol revisions
2026-07-28 and 2025-11-25. Mapped tools stay deferred to the model and are
promoted at a turn boundary under `<skill>.<server>.<tool>` names. Calls
run on the exec-class approval ladder; HTTP servers authenticate through
OAuth with issuer and resource binding. OAuth discovery and token requests
reject non-public resolved addresses unless the configured MCP endpoint is
explicitly local, and HTTP redirects are not followed. The authorization
callback does not authorize an exchange by itself: an affirmative prompt and
the validated callback are both required.
The client speaks the tools-only protocol; there is no HTTP+SSE-only transport.

Limits: discovery 5 s, start 10 s, list 15 s, call 60 s (600 s with
progress), one restart per session and isolation key, 50 list pages,
result text capped at 524288 bytes. Token files are stored at mode 0600.
