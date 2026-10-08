use super::decode::{
    blocks_member, bool_member, entry_id_member, i32_member, invalid, json_member, model_id_member,
    model_route_members, need, object_members, opt_entry_id_member, opt_raw_json_member,
    opt_text_member, owner_member, parts_member, purpose_member, text_member, u64_member,
    usage_member, want,
};
use super::scan::{Member, Members};
use super::{
    Answer, ApprovalMode, AssistantStop, ClientId, DecodeError, Decoded, Entry, EntryId, EntryKind,
    Header, JobEvent, JobOutcome, Mail, Mode, NonZeroU64, Product, RawJson, Record, ScannedHead,
    SessionId, Source, TreeKind, TurnEndStop, TurnId, VERSION,
};

pub(super) fn answer_member(member: &Member<'_>) -> Result<Answer, DecodeError> {
    let (offset, raw) = (member.offset, member.value);
    if let Ok(literal) = sonic_rs::from_slice::<&str>(raw.as_bytes()) {
        return match literal {
            "approve" => Ok(Answer::Approve),
            "approve_for_session" => Ok(Answer::ApproveForSession),
            "decline" => Ok(Answer::Decline),
            "cancel" => Ok(Answer::Cancel),
            other => Err(invalid(offset, format!("unknown answer literal `{other}`"))),
        };
    }
    let mut inner = object_members(raw, offset, &["value"])?;
    let value = want(&mut inner, "value")
        .ok_or_else(|| invalid(offset, "an `answer` object needs a `value` member"))?;
    let json =
        RawJson::parse(value.value).map_err(|error| invalid(value.offset, error.to_string()))?;
    Ok(Answer::Value(json))
}

pub(super) fn assistant_stop_member(member: &Member<'_>) -> Result<AssistantStop, DecodeError> {
    let (offset, raw) = (member.offset, member.value);
    if let Ok(literal) = sonic_rs::from_slice::<&str>(raw.as_bytes()) {
        return match literal {
            "done" => Ok(AssistantStop::Done),
            "length" => Ok(AssistantStop::Length),
            "filter" => Ok(AssistantStop::Filter),
            "tool_use" => Ok(AssistantStop::ToolUse),
            "cancelled" => Ok(AssistantStop::Cancelled),
            other => Err(invalid(
                offset,
                format!("unknown assistant stop literal `{other}`"),
            )),
        };
    }
    let mut inner = object_members(raw, offset, &["failed"])?;
    let failed = want(&mut inner, "failed")
        .map(|member| text_member(&member, "failed"))
        .transpose()?
        .ok_or_else(|| invalid(offset, "a `stop` object needs a `failed` member"))?;
    Ok(AssistantStop::Failed { message: failed })
}

pub(super) fn turn_end_stop_member(member: &Member<'_>) -> Result<TurnEndStop, DecodeError> {
    let (offset, raw) = (member.offset, member.value);
    if let Ok(literal) = sonic_rs::from_slice::<&str>(raw.as_bytes()) {
        return match literal {
            "done" => Ok(TurnEndStop::Done),
            "length" => Ok(TurnEndStop::Length),
            "filter" => Ok(TurnEndStop::Filter),
            "max_steps" => Ok(TurnEndStop::MaxSteps),
            "cancelled" => Ok(TurnEndStop::Cancelled),
            "aborted" => Ok(TurnEndStop::Aborted),
            other => Err(invalid(
                offset,
                format!("unknown turn stop literal `{other}`"),
            )),
        };
    }
    let mut inner = object_members(raw, offset, &["failed"])?;
    let failed = want(&mut inner, "failed")
        .map(|member| text_member(&member, "failed"))
        .transpose()?
        .ok_or_else(|| invalid(offset, "a `stop` object needs a `failed` member"))?;
    Ok(TurnEndStop::Failed { message: failed })
}

pub(super) fn job_outcome_member(member: &Member<'_>) -> Result<JobOutcome, DecodeError> {
    let (offset, raw) = (member.offset, member.value);
    if let Ok(literal) = sonic_rs::from_slice::<&str>(raw.as_bytes()) {
        return match literal {
            "cancelled" => Ok(JobOutcome::Cancelled),
            "lost" => Ok(JobOutcome::Lost),
            other => Err(invalid(
                offset,
                format!("unknown job outcome literal `{other}`"),
            )),
        };
    }
    let mut inner = object_members(raw, offset, &["exited", "failed"])?;
    if let Some(exited) = want(&mut inner, "exited") {
        return Ok(JobOutcome::Exited {
            code: i32_member(&exited, "exited")?,
        });
    }
    if let Some(failed) = want(&mut inner, "failed") {
        return Ok(JobOutcome::Failed {
            message: text_member(&failed, "failed")?,
        });
    }
    Err(invalid(
        offset,
        "a `outcome` object needs an `exited` or `failed` member",
    ))
}

pub(super) fn job_event_member(
    event: &Member<'_>,
    members: &mut Members<'_>,
) -> Result<JobEvent, DecodeError> {
    let (offset, raw) = (event.offset, event.value);
    let literal = sonic_rs::from_slice::<&str>(raw.as_bytes())
        .map_err(|_| invalid(offset, "member `event` must be a string"))?;
    match literal {
        "start" => Ok(JobEvent::Started {
            kind: want(members, "kind")
                .map(|member| text_member(&member, "kind"))
                .transpose()?,
        }),
        "end" => Ok(JobEvent::Settled {
            outcome: want(members, "outcome")
                .map(|member| job_outcome_member(&member))
                .transpose()?,
        }),
        "cancelled" => Ok(JobEvent::Cancelled {
            by: want(members, "by")
                .map(|member| json_member::<ClientId>(&member))
                .transpose()?,
        }),
        "killed" => Ok(JobEvent::Killed),
        "timed_out" => Ok(JobEvent::TimedOut),
        "orphaned" => Ok(JobEvent::Orphaned),
        other => Err(invalid(offset, format!("unknown job event `{other}`"))),
    }
}

pub(super) fn product_mode_member(member: &Member<'_>) -> Result<Mode, DecodeError> {
    let (offset, raw) = (member.offset, member.value);
    match sonic_rs::from_slice::<&str>(raw.as_bytes()) {
        Ok("normal") => Ok(Mode::Normal),
        Ok("eval-first") => Ok(Mode::EvalFirst),
        Ok("eval-only") => Ok(Mode::EvalOnly),
        Ok(other) => Err(invalid(offset, format!("unknown product mode `{other}`"))),
        Err(_) => Err(invalid(offset, "member `mode` must be a string")),
    }
}

pub(super) fn mode_member(member: &Member<'_>) -> Result<ApprovalMode, DecodeError> {
    let (offset, raw) = (member.offset, member.value);
    match sonic_rs::from_slice::<&str>(raw.as_bytes()) {
        Ok("ask") => Ok(ApprovalMode::Ask),
        Ok("auto-edit") => Ok(ApprovalMode::Edits),
        Ok("yolo") => Ok(ApprovalMode::All),
        Ok(other) => Err(invalid(offset, format!("unknown approval mode `{other}`"))),
        Err(_) => Err(invalid(offset, "member `mode` must be a string")),
    }
}

pub(super) fn product_member(member: &Member<'_>) -> Result<Product, DecodeError> {
    let (offset, raw) = (member.offset, member.value);
    match sonic_rs::from_slice::<&str>(raw.as_bytes()) {
        Ok("dal") => Ok(Product::Dal),
        Ok("dalgona") => Ok(Product::Dalgona),
        Ok(other) => Err(invalid(offset, format!("unknown product `{other}`"))),
        Err(_) => Err(invalid(offset, "member `product` must be a string")),
    }
}

pub(super) fn source_member(member: &Member<'_>) -> Result<Option<Source>, DecodeError> {
    let (offset, raw) = (member.offset, member.value);
    if raw == "null" {
        return Ok(None);
    }
    let mut inner = object_members(raw, offset, &["session", "entry"])?;
    let session = want(&mut inner, "session")
        .map(|member| json_member::<SessionId>(&member))
        .transpose()?
        .ok_or_else(|| invalid(offset, "a `from` object needs `session`"))?;
    let entry = opt_entry_id_member(&need(&mut inner, "entry")?, "entry")?;
    Ok(Some(Source { session, entry }))
}

/// Reads the fixed tree-record prefix without allocating.
///
/// The byte string must start exactly `{"v":1,"type":"<tag>","id":<int>,
/// "parent":<int or null>,` with a tree `tag`. Returns `None` for every
/// other shape; callers then decode the whole line.
#[must_use]
pub fn scan_head(line: &[u8]) -> Option<ScannedHead> {
    let prefix = br#"{"v":1,"type":""#;
    if line.len() < prefix.len() + 4 || !line.starts_with(prefix) {
        return None;
    }
    let mut cursor = prefix.len();
    let tag_start = cursor;
    while cursor < line.len() && line[cursor] != b'"' {
        cursor += 1;
    }
    if cursor == tag_start || cursor >= line.len() {
        return None;
    }
    let tag = std::str::from_utf8(&line[tag_start..cursor]).ok()?;
    let kind = TreeKind::from_tag(tag)?;
    let mut rest = &line[cursor..];
    if !rest.starts_with(br#"","id":"#) {
        return None;
    }
    rest = &rest[7..];
    let mut id: u64 = 0;
    let mut digits = 0_usize;
    while let Some(&byte) = rest.first() {
        if byte.is_ascii_digit() {
            id = id.checked_mul(10)?.checked_add(u64::from(byte - b'0'))?;
            digits += 1;
            rest = &rest[1..];
        } else {
            break;
        }
    }
    if digits == 0 || !rest.starts_with(br#","parent":"#) {
        return None;
    }
    rest = &rest[10..];
    let parent = if rest.starts_with(b"null") {
        rest = &rest[4..];
        None
    } else {
        let mut value: u64 = 0;
        let mut seen = 0_usize;
        while let Some(&byte) = rest.first() {
            if byte.is_ascii_digit() {
                value = value.checked_mul(10)?.checked_add(u64::from(byte - b'0'))?;
                seen += 1;
                rest = &rest[1..];
            } else {
                break;
            }
        }
        if seen == 0 {
            return None;
        }
        Some(EntryId::new(NonZeroU64::new(value)?))
    };
    if !rest.starts_with(b",") {
        return None;
    }
    Some(ScannedHead {
        id: EntryId::new(NonZeroU64::new(id)?),
        parent,
        kind,
    })
}

/// Decodes one complete journal line.
///
/// The `"v"` member is validated before anything else; `"type"` must then
/// name a format-1 kind. Members the record does not declare are dropped
/// so a reader survives fields a later format adds; the closed nested
/// payloads (`usage`, parts, blocks, `who`, `purpose`, `answer`, `from`,
/// `outcome`, `stop`) still reject unknown members, and `cost_usd` inside
/// `usage` is rejected rather than silently dropped.
///
/// # Errors
/// Returns [`DecodeError`] for a missing or unsupported version, an
/// unknown record kind, malformed JSON, or an invalid member.
pub fn decode(line: &[u8]) -> Result<Decoded, DecodeError> {
    let text = std::str::from_utf8(line.trim_ascii_end())
        .map_err(|_| invalid(0, "journal lines are UTF-8"))?;
    let mut members: Members<'_> = Vec::new();
    let mut version_member: Option<Member<'_>> = None;
    let mut type_member: Option<Member<'_>> = None;
    let mut duplicate_member: Option<DecodeError> = None;
    for member in super::scan::scan_members(text, 0)? {
        let offset = member.offset;
        match member.name.as_ref() {
            "v" => {
                if version_member.is_some() {
                    return Err(invalid(offset, "duplicate member `v`"));
                }
                version_member = Some(member);
            }
            "type" => {
                if type_member.is_some() {
                    duplicate_member
                        .get_or_insert_with(|| invalid(offset, "duplicate member `type`"));
                } else {
                    type_member = Some(member);
                }
            }
            _ => {
                if members
                    .iter()
                    .any(|known: &Member<'_>| known.name == member.name)
                {
                    duplicate_member.get_or_insert_with(|| {
                        invalid(offset, format!("duplicate member `{}`", member.name))
                    });
                } else {
                    members.push(member);
                }
            }
        }
    }
    let Some(version) = version_member else {
        return Err(DecodeError::MissingVersion);
    };
    let version = sonic_rs::from_slice::<u64>(version.value.as_bytes())
        .map_err(|_| invalid(version.offset, "member `v` must be an unsigned integer"))?;
    if version != u64::from(VERSION) {
        return Err(DecodeError::UnsupportedVersion { found: version });
    }
    if let Some(error) = duplicate_member {
        return Err(error);
    }
    let Some(record_type) = type_member else {
        return Err(invalid(0, "a record needs a `type` member"));
    };
    let kind = sonic_rs::from_slice::<&str>(record_type.value.as_bytes())
        .map_err(|_| invalid(record_type.offset, "member `type` must be a string"))?;
    let record = decode_record(kind, &mut members)?;
    Ok(Decoded { record })
}

/// Pulls the `id`, `parent`, and `at` members every tree entry shares.
pub(super) fn entry_prefix_members(
    members: &mut Members<'_>,
) -> Result<(EntryId, Option<EntryId>, jiff::Timestamp), DecodeError> {
    let id = entry_id_member(&need(members, "id")?, "id")?;
    let parent = opt_entry_id_member(&need(members, "parent")?, "parent")?;
    let at = json_member(&need(members, "at")?)?;
    Ok((id, parent, at))
}

/// Decodes one record body. After the entry-prefix fast path, each
/// group owns one domain's tag table; an unrecognized tag falls through
/// every group to `UnknownRecordKind`.
pub(super) fn decode_record(kind: &str, members: &mut Members<'_>) -> Result<Record, DecodeError> {
    if TreeKind::from_tag(kind).is_some() {
        let (id, parent, at) = entry_prefix_members(members)?;
        let kind = decode_entry_kind(kind, members)?;
        let entry = Entry {
            id,
            parent,
            at,
            kind,
        };
        return Ok(match entry.kind {
            EntryKind::User { .. } => Record::User(entry),
            EntryKind::Assistant { .. } => Record::Assistant(entry),
            EntryKind::ToolResult { .. } => Record::ToolResult(entry),
            EntryKind::Reminder { .. } => Record::Reminder(entry),
            EntryKind::Model { .. } => Record::Model(entry),
            EntryKind::Thinking { .. } => Record::Thinking(entry),
            EntryKind::Approval { .. } => Record::Approval(entry),
            EntryKind::Mode { .. } => Record::Mode(entry),
            EntryKind::Compaction { .. } => Record::Compaction(entry),
            EntryKind::BranchSummary { .. } => Record::BranchSummary(entry),
        });
    }
    decode_meta(kind, members)
        .or_else(|| decode_control(kind, members))
        .or_else(|| decode_grants(kind, members))
        .or_else(|| decode_effects(kind, members))
        .unwrap_or_else(|| Err(DecodeError::UnknownRecordKind { kind: kind.into() }))
}

/// Session lifecycle records.
fn decode_meta(kind: &str, members: &mut Members<'_>) -> Option<Result<Record, DecodeError>> {
    if !matches!(
        kind,
        "session" | "boot" | "leaf" | "label" | "name" | "archive"
    ) {
        return None;
    }
    let record = (|| -> Result<Record, DecodeError> {
        match kind {
            "session" => Ok(Record::Session(Header {
                id: json_member(&need(members, "id")?)?,
                at: json_member(&need(members, "at")?)?,
                workspace: json_member(&need(members, "workspace")?)?,
                product: product_member(&need(members, "product")?)?,
                from: source_member(&need(members, "from")?)?,
            })),
            "boot" => Ok(Record::Boot {
                at: json_member(&need(members, "at")?)?,
                r#gen: json_member(&need(members, "gen")?)?,
                version: text_member(&need(members, "version")?, "version")?,
            }),
            "leaf" => Ok(Record::Leaf {
                at: json_member(&need(members, "at")?)?,
                to: opt_entry_id_member(&need(members, "to")?, "to")?,
            }),
            "label" => Ok(Record::Label {
                at: json_member(&need(members, "at")?)?,
                entry: entry_id_member(&need(members, "entry")?, "entry")?,
                label: opt_text_member(&need(members, "label")?, "label")?,
            }),
            "name" => Ok(Record::Name {
                at: json_member(&need(members, "at")?)?,
                name: opt_text_member(&need(members, "name")?, "name")?,
            }),
            "archive" => Ok(Record::Archive {
                at: json_member(&need(members, "at")?)?,
                archived: bool_member(&need(members, "archived")?, "archived")?,
            }),
            _ => unreachable!("the tag filter above admits only this group's kinds"),
        }
    })();
    Some(record)
}

/// Turn control records.
fn decode_control(kind: &str, members: &mut Members<'_>) -> Option<Result<Record, DecodeError>> {
    if !matches!(
        kind,
        "turn_start"
            | "tool_start"
            | "turn_end"
            | "rule_fired"
            | "resolved"
            | "before_request_mut"
            | "tool_promoted"
            | "wake_attempt"
    ) {
        return None;
    }
    let record = (|| -> Result<Record, DecodeError> {
        match kind {
            "turn_start" => Ok(Record::TurnStart {
                at: json_member(&need(members, "at")?)?,
                turn: json_member(&need(members, "turn")?)?,
            }),
            "tool_start" => Ok(Record::ToolStart {
                at: json_member(&need(members, "at")?)?,
                turn: json_member(&need(members, "turn")?)?,
                call: json_member(&need(members, "call")?)?,
            }),
            "turn_end" => Ok(Record::TurnEnd {
                at: json_member(&need(members, "at")?)?,
                turn: json_member(&need(members, "turn")?)?,
                stop: turn_end_stop_member(&need(members, "stop")?)?,
                usage: usage_member(&need(members, "usage")?)?,
                changes: json_member(&need(members, "changes")?)?,
            }),
            "rule_fired" => Ok(Record::RuleFired {
                at: json_member(&need(members, "at")?)?,
                turn: json_member(&need(members, "turn")?)?,
                rule: text_member(&need(members, "rule")?, "rule")?,
                entry: entry_id_member(&need(members, "entry")?, "entry")?,
            }),
            "resolved" => Ok(Record::Resolved {
                at: json_member(&need(members, "at")?)?,
                request: json_member(&need(members, "request")?)?,
                answer: answer_member(&need(members, "answer")?)?,
                by: json_member(&need(members, "by")?)?,
                was_default: match want(members, "was_default") {
                    Some(member) => bool_member(&member, "was_default")?,
                    None => false,
                },
            }),
            "before_request_mut" => Ok(Record::BeforeRequestMut {
                at: json_member(&need(members, "at")?)?,
                turn: json_member(&need(members, "turn")?)?,
                ext: text_member(&need(members, "ext")?, "ext")?,
                field: text_member(&need(members, "field")?, "field")?,
                old: text_member(&need(members, "old")?, "old")?,
                new: text_member(&need(members, "new")?, "new")?,
            }),
            "tool_promoted" => Ok(Record::ToolPromoted {
                at: json_member(&need(members, "at")?)?,
                tool: text_member(&need(members, "tool")?, "tool")?,
                turn: match want(members, "turn") {
                    Some(member) => Some(json_member::<TurnId>(&member)?),
                    None => None,
                },
                leaf: match want(members, "leaf") {
                    Some(member) => Some(entry_id_member(&member, "leaf")?),
                    None => None,
                },
            }),
            "wake_attempt" => Ok(Record::WakeAttempt {
                at: json_member(&need(members, "at")?)?,
                turn: json_member(&need(members, "turn")?)?,
                count: u32::try_from(u64_member(&need(members, "count")?, "count")?)
                    .map_err(|_| invalid(0, "member `count` exceeds u32"))?,
                jobs: match want(members, "jobs") {
                    Some(member) => json_member(&member)?,
                    None => Vec::new(),
                },
            }),
            _ => unreachable!("the tag filter above admits only this group's kinds"),
        }
    })();
    Some(record)
}

/// Grant and approval records.
fn decode_grants(kind: &str, members: &mut Members<'_>) -> Option<Result<Record, DecodeError>> {
    if !matches!(
        kind,
        "allow_always" | "grant_given" | "scoped_grant" | "scoped_grant_ended"
    ) {
        return None;
    }
    let record = (|| -> Result<Record, DecodeError> {
        match kind {
            "allow_always" => Ok(Record::AllowAlways {
                at: json_member(&need(members, "at")?)?,
                tool: text_member(&need(members, "tool")?, "tool")?,
                by: json_member(&need(members, "by")?)?,
            }),
            "grant_given" => Ok(Record::GrantGiven {
                at: json_member(&need(members, "at")?)?,
                ext: text_member(&need(members, "ext")?, "ext")?,
                set: json_member(&need(members, "set")?)?,
                scope: text_member(&need(members, "scope")?, "scope")?,
                by: json_member(&need(members, "by")?)?,
            }),
            "scoped_grant" => Ok(Record::ScopedGrant {
                at: json_member(&need(members, "at")?)?,
                call: json_member(&need(members, "call")?)?,
                prefix: json_member(&need(members, "prefix")?)?,
                roots: json_member(&need(members, "roots")?)?,
                job: json_member(&need(members, "job")?)?,
                by: json_member(&need(members, "by")?)?,
            }),
            "scoped_grant_ended" => Ok(Record::ScopedGrantEnded {
                at: json_member(&need(members, "at")?)?,
                job: json_member(&need(members, "job")?)?,
            }),
            _ => unreachable!("the tag filter above admits only this group's kinds"),
        }
    })();
    Some(record)
}

/// Async effect records.
fn decode_effects(kind: &str, members: &mut Members<'_>) -> Option<Result<Record, DecodeError>> {
    if !matches!(kind, "job" | "ext" | "mail" | "inferred") {
        return None;
    }
    let record = (|| -> Result<Record, DecodeError> {
        match kind {
            "job" => Ok(Record::Job {
                at: json_member(&need(members, "at")?)?,
                job: json_member(&need(members, "job")?)?,
                event: job_event_member(&need(members, "event")?, members)?,
            }),
            "ext" => Ok(Record::Ext {
                at: json_member(&need(members, "at")?)?,
                ext: text_member(&need(members, "ext")?, "ext")?,
                kind: text_member(&need(members, "kind")?, "kind")?,
                body: {
                    let member = need(members, "body")?;
                    RawJson::parse(member.value)
                        .map_err(|error| invalid(member.offset, error.to_string()))?
                },
            }),
            "mail" => Ok(Record::Mail(Mail {
                at: json_member(&need(members, "at")?)?,
                from: json_member(&need(members, "from")?)?,
                to: json_member(&need(members, "to")?)?,
                mode: json_member(&need(members, "mode")?)?,
                text: text_member(&need(members, "text")?, "text")?,
                reply_to: opt_text_member(&need(members, "reply_to")?, "reply_to")?,
            })),
            "inferred" => Ok(Record::Inferred {
                at: json_member(&need(members, "at")?)?,
                who: owner_member(&need(members, "who")?)?,
                purpose: purpose_member(&need(members, "purpose")?)?,
                usage: usage_member(&need(members, "usage")?)?
                    .ok_or_else(|| invalid(0, "member `usage` must be an object"))?,
            }),
            _ => unreachable!("the tag filter above admits only this group's kinds"),
        }
    })();
    Some(record)
}

pub(super) fn decode_entry_kind(
    kind: &str,
    members: &mut Members<'_>,
) -> Result<EntryKind, DecodeError> {
    Ok(match kind {
        "user" => EntryKind::User {
            parts: parts_member(&need(members, "parts")?)?,
        },
        "assistant" => EntryKind::Assistant {
            api: json_member(&need(members, "api")?)?,
            model: model_id_member(&need(members, "model")?)?,
            content: blocks_member(&need(members, "content")?)?,
            usage: usage_member(&need(members, "usage")?)?
                .ok_or_else(|| invalid(0, "member `usage` must be an object"))?,
            stop: assistant_stop_member(&need(members, "stop")?)?,
        },
        "tool_result" => EntryKind::ToolResult {
            call: json_member(&need(members, "call")?)?,
            name: text_member(&need(members, "name")?, "name")?,
            error: bool_member(&need(members, "error")?, "error")?,
            parts: parts_member(&need(members, "parts")?)?,
            changes: json_member(&need(members, "changes")?)?,
        },
        "reminder" => EntryKind::Reminder {
            source: text_member(&need(members, "source")?, "source")?,
            text: text_member(&need(members, "text")?, "text")?,
        },
        "model" => EntryKind::Model {
            route: model_route_members(members)?,
        },
        "thinking" => EntryKind::Thinking {
            level: json_member(&need(members, "level")?)?,
        },
        "approval" => EntryKind::Approval {
            mode: mode_member(&need(members, "mode")?)?,
        },
        "mode" => EntryKind::Mode {
            mode: product_mode_member(&need(members, "mode")?)?,
        },
        "compaction" => EntryKind::Compaction {
            summary: opt_text_member(&need(members, "summary")?, "summary")?,
            first_kept: opt_entry_id_member(&need(members, "first_kept")?, "first_kept")?,
            tokens_before: u64_member(&need(members, "tokens_before")?, "tokens_before")?,
            replay: opt_raw_json_member(&need(members, "replay")?, "replay")?,
            usage: usage_member(&need(members, "usage")?)?,
            parts: want(members, "parts")
                .map(|member| parts_member(&member))
                .transpose()?
                .unwrap_or_default(),
            parts_tokens: want(members, "parts_tokens")
                .map(|member| u64_member(&member, "parts_tokens"))
                .transpose()?
                .unwrap_or_default(),
        },
        "branch_summary" => EntryKind::BranchSummary {
            from: entry_id_member(&need(members, "from")?, "from")?,
            summary: text_member(&need(members, "summary")?, "summary")?,
        },
        other => {
            return Err(DecodeError::UnknownRecordKind { kind: other.into() });
        }
    })
}
