# dalgona-box on Cloudflare

Release flow for the box container image.

## Publish

Push a `v*` tag. `.github/workflows/box-image.yml` builds the
multi-arch image and pushes three tags: the version, the commit sha,
and `latest`. `latest` is a convenience tag only and is never
deployed.

## Pin

Cloudflare must run an immutable reference. After the release
finishes, resolve the published digest and pin it:

    just cf-pin <tag>

`<tag>` is the version or commit sha the release published. The
recipe hashes the raw manifest with `docker buildx imagetools inspect
--raw` and writes `ghcr.io/gosuda/dalgona-box@sha256:<digest>` into
`wrangler.jsonc`. It then runs `just cf-deploy-check` to prove the
pin. Commit the result.

`just cf-deploy-check` fails unless the image is a digest pin, so run
it as the deploy step before `wrangler deploy`. `just cf-check` stays
limited to typecheck and lint and never blocks on the pin.
