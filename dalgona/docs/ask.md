# ask

The ask tool raises one to four typed questions through the shared ask
service and waits for exactly one answer. A question is single-select,
multi-select, or free text, carries a header of at most 12 characters, and
may attach a preview of at most 8192 bytes. Question ids are snake_case
and unique per call; select kinds carry two to four options whose labels
and descriptions are non-empty and free of control characters.

Select and text answers arrive as {"selected": [label, ...]} and
{"text": answer} objects; an empty submission is a dismiss. Add
"explicit": true only when the user asked for this question directly.

Apply two filters before asking: if the evidence already collected can
answer the question, explore instead; if the ideal state settles it, resolve
to that instead. An owner decision always survives as a question.

The open-question timeout is `[ask] timeout` in dal.toml; it defaults to
30 minutes and clamps into 1 to 120 minutes at load, with one startup
notice when clamped. Each raised question keeps one absolute deadline from
its raise; dismissal, timeout, and a missing controller all reach the tool
as one merged no answer: continue on best judgment and do not ask again
this turn. Print, JSON, and child sessions are never answerers, so a
question raised there fails closed the same way.

The TUI shows one question at a time with a count for the queued rest,
each select offering its options plus an always-present Other free-text
row. Wire clients with the question capability receive the whole request
and answer `session/answer`; a losing answer is refused after the first
resolution, and a client that attaches late reads the open requests in
the session view.
