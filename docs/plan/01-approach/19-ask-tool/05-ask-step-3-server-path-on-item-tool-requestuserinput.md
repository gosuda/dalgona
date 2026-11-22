#### Ask step 3. Server path on `item/tool/requestUserInput`

Update `internal/dal/rpc/conn.go`, `serve_input.go`, and their client consumers together.
Use the current [server-request contract](../../../dal/spec/protocol.md#server-requests) and [ask contract](../../../dal/spec/ask.md).
Keep the existing request method, identifiers, schemas, and subscription ordering.

The wire response remains `{"answers":{"q1":{"answers":["value"]}}}`.
Each selected option returns its exact offered label, including its original line endings.
Only newly authored free-form drafts pass through the interactive newline adapter.
The daemon never repeats that conversion or trims accepted answers.

The transport must validate a typed question response before deleting its pending response waiter.
The current one-response dispatch path must migrate with first-party clients to a lifecycle that supports correction.
Keeping only the server waiter while a client callback returns once is insufficient.

For an otherwise structurally valid response with invalid text:

- Keep the original pending waiter and host question.
- Send `warning` to the answering connection with `threadId`, `requestId`, `field`, `reason`, and the specified bounded message.
- Preserve the original deadline and subscriber set.
- Send neither `serverRequest/resolved` nor a JSON-RPC error response to the response.
- Accept a corrected response under the same ID after the warning, while that question is still pending.

Keep existing outcomes for other responses:

|Input or event|Outcome|
|---|---|
|Valid answer map|Attempt the host's first-valid-answer decision.|
|`{"answers":{}}`|Explicit cancellation.|
|Every named answer list empty|Answered with the existing skipped result.|
|Wrong answer shape|Existing unreadable-answer failure.|
|Client error response|This subscriber cannot answer, subject to existing refusal accounting.|
|Interruption, timeout, or last-subscriber loss|The host's existing authoritative outcome.|

Resolution uses `serverRequest/resolved` with `answered`, `cancelled`, `interrupted`, `timedOut`, or `declined`.
Resolution closes the typed lifecycle and prevents late corrections or warnings from reopening the card.
First-party senders remain available after submission and are released only when the lifecycle ends.
Legacy clients may lack correction UI. Preserve host correctness without promising reconnect recovery.

Verification must exercise both production sides of the boundary:

- Validate requests, nested responses, warnings, and resolutions against the vendored schemas.
- Reject 2001-scalar text, then accept a corrected response from the same first-party client under the original ID.
- Accept 2000 supplementary scalars and preserve every accepted scalar.
- Return an offered label containing CRLF unchanged and match it as that option.
- Verify no waiter removal, answer outcome, or journal append occurs on text rejection.
- Race correction against another client's answer, timeout, interruption, and disconnect.
- Deliver a stale warning after resolution and prove that no card or sender is recreated.
- Retain the unreadable-answer and client-error tests to prove those outcomes remain distinct.

Lands with Step 2's actor validation and Steps 7 and 8's retained client senders.
The GUI inherits this lifecycle through the shared native connection owner.
