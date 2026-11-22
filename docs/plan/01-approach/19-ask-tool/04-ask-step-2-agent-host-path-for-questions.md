#### Ask step 2. Agent host path for questions

Update the existing question lifecycle in `internal/dal/agent/question.go` and its host and subscriber callers.
[Ask tool](../../../dal/spec/ask.md#host-path) owns the outcome and timeout contract.
The shared [input contract](../../../dal/spec/input-text.md) owns text preservation and validation.

The actor remains the single owner of pending questions, subscriber membership, deadlines, and the winning outcome.
Each pending question retains its request ID, call ID, original offered values, subscribers, response channel, and first-publication deadline.
Validation precedes publication. Required model fields must be nonblank, bounded, well-formed, and free of NUL.
Apply the existing secret masker before offering the question, and validate the offered values before publication.
Masking failure must offer nothing. Returned option values identify the exact masked labels that the user saw.

Publication and replay follow these rules:

- No subscriber means the existing no-client outcome without an open question.
- First publication starts one five-minute deadline.
- A later subscriber receives every still-open question in order, under its original request ID.
- Replay, corrections, and later subscribers never restart the deadline.
- Slow-subscriber resubscription preserves the existing refusal accounting and question outcome rules.
- `Status.Questions` counts pending questions until an authoritative outcome wins.

For an otherwise structurally valid answer, validate all answer text before claiming the outcome.
A text error leaves the actor's pending entry, deadline, subscriber set, and accepted source unchanged.
The transport sends the correction warning defined in [Protocol](../../../dal/spec/protocol.md#server-requests).
Only a valid answer or an existing cancellation, timeout, interruption, or no-client outcome can remove the pending entry.
A losing outcome must not publish a second resolution or send another reply.

The host must serialize these races:

- Two valid answers.
- An invalid answer followed by a corrected answer from the same connection.
- A corrected answer against another client's valid answer.
- An answer against interruption, deadline expiry, or last-subscriber loss.
- Subscription replay against an already chosen outcome.

Last-subscriber loss resolves open questions through the actor, not independent per-connection subscriber-count checks.
Do not promise that reconnecting preserves a question after its sole subscriber leaves.
Stop the timer when any outcome wins. A consumer other than `ask` treats timeout as no answer.
The ask tool alone formats the specified recommended-option fallback.

Tests in `internal/dal/agent` must use production ownership and a controlled clock.
Keep channel deadlines for hung-test detection, without sleeps as the event oracle.
Test first publication, replay, unchanged deadline after rejection, exact offered labels, first-valid-answer behavior, and every race above.
Each expected result comes from the current ask contract.
The test must show that invalid text cannot consume the host's decision or append an answer result.

Lands after Step 1's validation boundary and alongside Step 3's typed transport lifecycle.
No new question journal or reconnect lease is introduced.
