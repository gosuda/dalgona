# judged

The judged battery adds five judge-fed features. All of them consult the
shared judge (`[judge]`, gate `auto|on|off`). The battery adds no second
on/off switch: a feature acts only when its key is true and the judge's
resolved gate is ready. With the gate off, every hook returns its no-op
verdict, the search reranker returns its input unchanged, and the battery
sends no judge request.

## Features and keys

| key | default | feature |
|---|---|---|
| thinking | true | Auto-thinking classification |
| ranking | true | Judged search ranking |
| ask_anchor | true | Anchored asks |
| claim_check | true | Omniscience claim verification |
| dedup | true | Prompt dedup |

Set a key to `false` under `[plugin.judged]` to turn one feature off.
There are no other keys. Unknown keys and wrong types fail startup:
`judged: [plugin.judged] has an unknown key "<key>".` plus, for a near
miss, ` Did you mean "<suggestion>"?`
`judged: [plugin.judged].<key> must be true or false.`

## When each feature acts, and when it stays silent

Every judge call goes through the one judge handle, so the judge's
`max_concurrent` and `max_per_turn` bound the whole battery. Any judge
failure applies the feature's fallback below and lands one ledger row.

Auto-thinking runs in the `before_request` hook.
It classifies the first request of a top-level turn and picks one thinking level from the model's
allowed levels. It waits at most 2000 ms. The level applies to every round
of that turn; the core clamps it to the model's caps. It stays silent when
the model offers fewer than two levels, when the turn has no user text (a
wake-started turn keeps the carried level), and in a child session, whose
level is what its spawn parameters carried. A level you set with `/thinking`
or `SetThinking` wins from then on: the latch moves one way, and a resumed
session whose journal holds your level starts latched. Three consecutive
failed classifications (timeout, parse, or provider) disable it for the
session with this notice, and a later success does not re-enable it:
`judged: auto-thinking disabled for this session after 3 failed classifications.`
A failed classification keeps the session default.

Judged search ranking rescores the first 20 search candidates when a
search returns 3 or more. It judges each path once per session (a 512-entry
memo) and returns a stable order: score descending, then the search
tool's own order. Candidates with no display line and paths that fail to
score stay ambient (score 1). When every score is equal, or the judge
fails, the input order stays and no note appears. When the order changes,
the search result ends with the ranking note below.

Anchored asks classify each `ask` call that the user did not ask for
directly, with one judge call per ask call however many questions it holds.
`owner-decision` allows the call. `explore-first` and `ideal-state` block it
with the exact reasons below. The anchor stays silent, and allows the
call, for another tool, for `explicit = true`, for arguments that are not
valid JSON, in a child session (children never ask), and on any judge
failure.

Claim verification checks a settled top-level reply of at least 512 bytes.
The reply is the whole turn's assistant text across every model request of
the turn. On a positive verdict it parks one reminder, which the next
`before_turn` delivers once; at most one is outstanding. A second positive
verdict while one is parked parks nothing and gets one dedup consult
against the parked text. It stays silent for shorter replies, in a child
session, and when the judge fails.

Prompt dedup refuses to re-inject reminder text the model already has,
for the claim reminder and for any dalgona battery that uses the shared
admission function. The deterministic half always runs: an exact match
after whitespace normalization, or an 8-word-shingle Jaccard of 0.9 or
more against an injection of the same kind from the last 50 turns,
refuses with no judge call and no ledger row. A borderline candidate gets
one judge call against the closest outstanding injection (highest
shingle Jaccard, newest on a tie), at most 4 per turn. The judged half is
inert with the toggle or the gate off, and any judge failure admits.

## Judge prompts (exact)

Auto-thinking is a choice question over the model's allowed levels in the
model's own order:

```text
How much reasoning does this turn need? Pick the smallest level that fits; pick the current default, medium, when unsure.
```

Shared context, with the user text cut at 4000 bytes:

```text
User turn:
<<<TURN
<user text, cap 4000>
TURN>>>
```

Search ranking asks one score question (0 to 3) per candidate; each prompt
is the candidate display line capped at 200 bytes. Shared context:

```text
Search query: <query>
Mode: <mode>
Score each candidate: 3 = it would change the next action, 2 = it supports it, 1 = it is ambient, 0 = it is irrelevant.
```

Anchored asks are a choice question with the options `explore-first`,
`ideal-state`, `owner-decision`, in that order:

```text
Classify the pending ask in the shared context. Pick the first option that applies.
```

```text
Ask call:
<<<ASK
<ask tool arguments, cap 8000>
ASK>>>

Recent context:
<<<CTX
<digest, cap 4000>
CTX>>>
```

Claim verification is a bool question:

```text
Does the reply in the shared context assert unverified facts that the next step would act on?
```

```text
Reply:
<<<REPLY
<assistant text, cap 8000>
REPLY>>>
```

Prompt dedup is a bool question:

```text
Does the candidate text in the shared context repeat advice the model already has?
```

```text
Already injected:
<<<OLD
<closest outstanding injection, cap 8000>
OLD>>>

Candidate:
<<<NEW
<candidate, cap 4000>
NEW>>>
```

A cap cuts at a UTF-8 boundary and keeps at most that many bytes. The
battery builds its recent-context digest only from hook observations: the
last user text (4000 bytes), the current turn's assistant text (a rolling
16384 bytes), and the last 8 tool previews (4096 bytes each). It never
reads the journal, the raw payload, or another extension's state.

## Verdict mapping (exact)

- Auto-thinking: the chosen index names a level in the model's allowed
  levels; the battery replaces only the `thinking` level of the request
  parameters and the core clamps the result.
- Ranking: scores 0 to 3; a stable sort by score descending, then by the
  search tool's order.
- Ask anchor: `owner-decision` allows. `explore-first` blocks with:
  `ask anchored: the evidence you already have can answer this. Explore first, and ask again only if it stays open.`
  `ideal-state` blocks with:
  `ask anchored: your goal state settles this. Take the resolving step instead of asking.`
- Claim check: `true` parks this reminder, capped at 400 bytes:
  `Your previous reply stated facts with confidence. Treat them as claims, not evidence: verify each before you build on it, or say plainly that it is unverified.`
  `false` parks nothing.
- Dedup: `true` refuses the injection (the model sees nothing); `false`
  admits.

## Ranking note (exact)

When the order changed, the search tool appends this as the last line of
its result, where `<n>` counts the candidates that got a score and `<m>`
is the input count:

`judged: re-ranked by relevance; <n> of <m> candidates rescored`

## Ledger feature values (exact)

Each judge call writes one judge ledger row through the shared judge,
with one of these `feature` values: `thinking`, `search-rank`,
`ask-anchor`, `claim-check`, `prompt-dedup`. A deterministic dedup
refusal is not a call and writes no row. The row format, redaction, and
1024-byte cap belong to the shared judge.

## The `explicit` field on the ask tool (exact)

The ask tool declares this top-level optional boolean, and the anchor
reads a missing field as false:

```json
{"type": "boolean", "default": false, "description": "true when the user asked for this question directly"}
```

## Constants

`classify_wait_ms` 2000; `thinking_shared_bytes` 4000; `digest_assistant_bytes`
16384; `digest_tool_previews` 8 at 4096 bytes each; `claim_min_reply_bytes`
512; `claim_reminder_bytes` 400; `rank_max_candidates` 20; `rank_memo_entries`
512; `dedup_shingle_words` 8; `dedup_jaccard` 0.9; `dedup_window_turns` 50;
`dedup_max_calls_per_turn` 4; `thinking_breaker_streak` 3.

## Children

Children never run auto-thinking or claim verification, and an anchored
ask never fires in one because children never ask. Ranking and dedup act
per call as anywhere.
