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
{:,:{:{:,:1,:4,:{:,:{:{:},:{:},:{:},:{:,:[,,]},:{:,:2,:4,:{:,:{:{:},:{:}},:[],:false}},:{:}},:[,,,],:false}}},:[],:false}invalid inputquestions must contain 1 to 4 itemsid must be snake_case (a-z, 0-9, underscores, starting with a letter)question ids must be unique; "{0}" repeatsheader must be 1 to 12 charactersheader must be unique within one callquestion must be non-emptykind must be one of single, multi, texta text question takes no optionsa single or multi question needs optionsoptions must contain 2 to 4 itemsoption label must be non-emptyoption label must not contain control charactersoption description must be non-empty when presentoption description must not contain control characterspreview is allowed only on single and multi questionspreview must be 1 to 8192 bytes__texttextsinglemultitextask: {reason}singletext, {}: {}textmultiUnanswered: {}, 
{}: {}
askCARGO_PKG_VERSION
