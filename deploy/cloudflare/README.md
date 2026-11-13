# dalgona-box on Cloudflare

## Box tokens

The Go client sends the worker secret, which is the value of
`DALGONA_TOKEN` on the worker, as the bearer token. It reads that value
from `DALGONA_CLOUDFLARE_TOKEN`, or else from the `cloudflare` entry in
`auth.json`. The worker checks it in constant time and accepts only that
value. Each box gets its own
token. The token is HMAC-SHA256 over the box name keyed by
`DALGONA_TOKEN`. The durable object reads the box name from
`ctx.id.name` and fails with 500 when it is missing. It sets
`DALGONA_BOX_TOKEN` to the box token and replaces the Authorization
header with the box token before proxying into the container. A box
token cannot call worker routes.

A box that an earlier worker version started still holds `DALGONA_TOKEN`
as its box token. After you deploy this version, destroy and create each
existing box again. Until then the worker sends that box a token it does
not accept, and the box keeps a credential that opens every box.
Destroying a box discards its changes that you did not pull. First run
`dalgona pull <thread-id>` for each thread whose changes you need. Then
archive the threads of the box and run `dalgona runner gc --yes`, which
deletes each box whose threads are all archived. `dalgona cloud down`
refuses Cloudflare boxes.

Release flow for the box container image.

## Publish

Push a `v*` tag. `.github/workflows/box-image.yml` builds the
multi-arch image and pushes two tags: the version and the commit sha.
Backport and prerelease tags must not move a shared reference, so the
workflow never publishes `latest`.

## Pin

Cloudflare must run an immutable reference. After the release
finishes, resolve the published digest and pin it:

    just cf-pin <tag>

`<tag>` is the version or commit sha the release published. The
recipe reads the manifest digest with `docker buildx imagetools
inspect --format '{{.Manifest.Digest}}'` and writes
`ghcr.io/gosuda/dalgona-box@sha256:<digest>` into `wrangler.jsonc`. It
then runs `just cf-deploy-check` to prove the pin. Commit the result.

`just cf-deploy-check` fails unless the image is a digest pin, so run
it as the deploy step before `wrangler deploy`. `just cf-check` stays
limited to typecheck, lint, and tests and never blocks on the pin.

Run the worker route tests with `pnpm -C deploy/cloudflare test`.
