# dalgon serve: the model router and A2A

Loopback only; non-loopback binds are refused. `--public` requires `dalgon serve token` on the same loopback listener; remote access goes through an HTTPS/WSS reverse proxy on the same machine, which must set `X-Forwarded-Proto: https` so the agent card advertises the public scheme; any other declared proto refuses the card rather than advertising a bearer endpoint over plaintext. Routes pick model ids; continuation resumes sessions; errors name the fix. `[serve] approval` gates router actions. A2A with `--a2a` serves the agent card and methods.
