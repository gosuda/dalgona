# fetch: Fetch one URL over HTTP.

## Ported from
Original to dal; no pi counterpart. Pi reaches the network with `fetch` or an npm HTTP client.

## What it shows
The `net.fetch` operation and a failure the model can read. The `fetch` tool takes a `url`, sends an HTTP GET through `net.fetch`, and returns one line with the status and the body size. A transport failure returns a `fetch_failed` error naming the URL and the host message; a non-200 status returns a `fetch_failed` error naming the status. The command `/fetch:fetch` takes the URL as its positional argument.

## What differs from pi
Pi code awaits a response object and reads its body as text. This plugin reads `status` and the length of `body` from the host response record and never renders the body itself.

## Install
Copy to the dal plugins data root and add `fetch` to `plugins`.

## Settings
None.
