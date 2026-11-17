# Connect wire

## Invariant

One package MUST implement Connect envelope framing, trailer decoding, and protowire field helpers for the provider wire packages. The `cursor` and `devin` wire packages MUST import that package and MUST NOT keep their own copies of those helpers.

## Scope

This decision applies to `internal/dal/model/connectwire`, `internal/dal/model/cursor`, and `internal/dal/model/devin`. The float and packed-integer helpers that only the `devin` wire uses stay in `internal/dal/model/devin/wire`.

## Enforcement

`internal/dal/model/connectwire/frame_test.go` checks frame splitting, the frame length limit, and trailer decoding. The `dupl` linter in `just lint` fails when the two wire packages repeat a block of helper code.
