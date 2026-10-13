# judged

The judged battery adds five judge-fed features. All of them consult the
shared judge (`[judge]`, gate `auto|on|off`). The battery adds no second
on/off switch.

## Features and keys

| key | default | feature |
|---|---|---|
| thinking | true | Auto-thinking classification |
| ranking | true | Judged search ranking |
| ask_anchor | true | Anchored asks |
| claim_check | true | Omniscience claim verification |
| dedup | true | Prompt dedup |

Set a key to `false` under `[plugin.judged]` to turn one feature off.
Unknown keys and wrong types fail startup:
`judged: [plugin.judged] has an unknown key "<key>".` plus, for a near
miss, ` Did you mean "<suggestion>"?`
`judged: [plugin.judged].<key> must be true or false.`

## When each feature acts

Auto-thinking classifies the first request of a top-level turn. It picks one
thinking level from the model's allowed levels and waits at most 2000 ms.
Three consecutive failed classifications disable it for the session with
this notice:
`judged: auto-thinking disabled for this session after 3 failed classifications.`
A level you set with `/thinking` or the model picker wins for the session.

Judged search ranking rescores the first 20 search candidates when a search
returns 3 or more. It judges each path once per session and returns a stable
order. When the order changes, the search result ends with:
`judged: re-ranked by relevance; <n> of <m> candidates rescored`

Anchored asks classify each `ask` call that the user did not ask for
directly. `owner-decision` allows the call. The other verdicts block it with
one of:
`ask anchored: the evidence you already have can answer this. Explore first, and ask again only if it stays open.`
`ask anchored: your goal state settles this. Take the resolving step instead of asking.`
Any judge failure allows the call.

Claim verification checks a settled reply of at least 512 bytes. On a
positive verdict it parks one reminder for the next turn, at most one at a
time:
`Your previous reply stated facts with confidence. Treat them as claims, not evidence: verify each before you build on it, or say plainly that it is unverified.`

Prompt dedup refuses to re-inject reminder text the model already has. The
deterministic half always runs: exact match after whitespace normalization,
or an 8-word-shingle Jaccard of 0.9 or more against an injection of the same
kind from the last 50 turns, refuses with no judge call. A borderline
candidate gets one judge call, at most 4 per turn.

## Constants

`classify_wait_ms` 2000; `thinking_shared_bytes` 4000; `digest_assistant_bytes`
16384; `digest_tool_previews` 8 at 4096 bytes each; `claim_min_reply_bytes`
512; `claim_reminder_bytes` 400; `rank_max_candidates` 20; `rank_memo_entries`
512; `dedup_shingle_words` 8; `dedup_jaccard` 0.9; `dedup_window_turns` 50;
`dedup_max_calls_per_turn` 4; `thinking_breaker_streak` 3.

Children never run auto-thinking or claim verification; their level is what
their spawn parameters carried. Ranking and dedup act per call as anywhere.
