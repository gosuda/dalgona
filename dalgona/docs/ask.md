# ask

The ask tool raises one to four typed questions through the shared ask
service and waits for exactly one answer. A question is single-select,
multi-select, or free text, carries a header of at most 12 characters, and
may attach a preview of at most 8192 bytes.

Apply two filters before asking: if the evidence already collected can
answer the question, explore instead; if the ideal state settles it, resolve
to that instead. An owner decision always survives as a question.

When no controller can answer, the ask resolves immediately to the
fail-closed default: the tool reports that there was no answer and says to
continue on best judgment without asking again this turn. Print, JSON, and
child sessions are never answerers, so a question raised there fails closed
the same way.
