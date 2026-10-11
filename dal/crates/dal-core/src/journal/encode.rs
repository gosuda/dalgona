use super::wire::{
    AnswerWire, AssistantBlocksWire, AssistantStopWire, JobOutcomeWire, JournalPartsWire,
    OwnerWire, PurposeWire, TsWire, TurnEndStopWire, UsageWire, encode_json,
};
use super::wire_entries::{
    AllowAlwaysWire, ApprovalWire, ArchiveWire, AssistantWire, BeforeRequestMutWire, BootWire,
    BranchSummaryWire, CompactionWire, ExtWire, GrantGivenWire, InferredWire, JobWire, LabelWire,
    LeafWire, MailRecordWire, ModeValueWire, ModeWire, ModelRouteWire, ModelWire, NameWire,
    ReminderWire, ResolvedWire, RouteWire, RuleFiredWire, ScopedGrantEndedWire, ScopedGrantWire,
    SessionWire, SourceWire, ThinkingWire, ToolPromotedWire, ToolResultWire, ToolStartWire,
    TurnEndWire, TurnStartWire, UserWire, WakeAttemptWire,
};
use super::{EncodeError, Entry, EntryKind, JobEvent, ModelRoute, Record, RouteError};

/// Writes one record as a compact line: `"v":1` first, then `"type"`,
/// then the record's declared member order, then a single LF.
///
/// # Errors
/// Returns [`EncodeError::Json`] when a member fails to serialize,
/// [`EncodeError::InvalidCost`] when a usage member carries a reported
/// cost no integer micro-dollar count can hold,
/// [`EncodeError::InvalidRoute`] when a model record's synthetic or
/// harness id breaks its grammar, and [`EncodeError::MismatchedKind`]
/// when a tree record's variant does not match its entry kind.
pub fn encode(record: &Record) -> Result<Vec<u8>, EncodeError> {
    let body = encode_body(record)?;
    let mut line = Vec::with_capacity(body.len() + 8);
    line.extend_from_slice(b"{\"v\":1,");
    line.extend_from_slice(&body[1..]);
    line.push(b'\n');
    Ok(line)
}

/// Encodes one journal record body. The outer match is the record-kind
/// index: each group owns one domain's wire table, and adding a variant
/// to `Record` fails here until it joins a group.
pub(super) fn encode_body(record: &Record) -> Result<Vec<u8>, EncodeError> {
    match record {
        Record::User { .. }
        | Record::Assistant { .. }
        | Record::ToolResult { .. }
        | Record::Reminder { .. } => encode_message_entries(record),
        Record::Model { .. }
        | Record::Thinking { .. }
        | Record::Approval { .. }
        | Record::Mode { .. } => encode_setting_entries(record),
        Record::Compaction { .. } | Record::BranchSummary { .. } => encode_context_entries(record),
        Record::Session { .. }
        | Record::Boot { .. }
        | Record::Leaf { .. }
        | Record::Label { .. }
        | Record::Name { .. }
        | Record::Archive { .. } => encode_meta(record),
        Record::TurnStart { .. }
        | Record::ToolStart { .. }
        | Record::TurnEnd { .. }
        | Record::RuleFired { .. }
        | Record::Resolved { .. }
        | Record::BeforeRequestMut { .. }
        | Record::ToolPromoted { .. }
        | Record::WakeAttempt { .. } => encode_control(record),
        Record::AllowAlways { .. }
        | Record::GrantGiven { .. }
        | Record::ScopedGrant { .. }
        | Record::ScopedGrantEnded { .. } => encode_grants(record),
        Record::Job { .. } | Record::Ext { .. } | Record::Mail { .. } | Record::Inferred { .. } => {
            encode_effects(record)
        }
    }
}

/// Entry records carrying turn content.
fn encode_message_entries(record: &Record) -> Result<Vec<u8>, EncodeError> {
    match record {
        Record::User(entry) => {
            let EntryKind::User { parts } = &entry.kind else {
                return Err(EncodeError::MismatchedKind);
            };
            encode_json(&UserWire {
                r#type: "user",
                id: entry.id,
                parent: entry.parent,
                at: TsWire(&entry.at),
                parts: JournalPartsWire(parts),
            })
        }
        Record::Assistant(entry) => {
            let EntryKind::Assistant {
                api,
                model,
                content,
                usage,
                stop,
            } = &entry.kind
            else {
                return Err(EncodeError::MismatchedKind);
            };
            if model.is_empty() {
                return Err(EncodeError::EmptyModel);
            }
            encode_json(&AssistantWire {
                r#type: "assistant",
                id: entry.id,
                parent: entry.parent,
                at: TsWire(&entry.at),
                api: *api,
                model: model.as_ref(),
                content: AssistantBlocksWire(content),
                usage: UsageWire::new(usage)?,
                stop: AssistantStopWire(stop),
            })
        }
        Record::ToolResult(entry) => {
            let EntryKind::ToolResult {
                call,
                name,
                error,
                parts,
                changes,
            } = &entry.kind
            else {
                return Err(EncodeError::MismatchedKind);
            };
            encode_json(&ToolResultWire {
                r#type: "tool_result",
                id: entry.id,
                parent: entry.parent,
                at: TsWire(&entry.at),
                call,
                name: name.as_ref(),
                error: *error,
                parts: JournalPartsWire(parts),
                changes,
            })
        }
        Record::Reminder(entry) => {
            let EntryKind::Reminder { source, text } = &entry.kind else {
                return Err(EncodeError::MismatchedKind);
            };
            encode_json(&ReminderWire {
                r#type: "reminder",
                id: entry.id,
                parent: entry.parent,
                at: TsWire(&entry.at),
                source: source.as_ref(),
                text: text.as_ref(),
            })
        }
        _ => unreachable!("the encode_body index routes only encode_message_entries kinds here"),
    }
}

/// Entry records carrying turn settings.
fn encode_setting_entries(record: &Record) -> Result<Vec<u8>, EncodeError> {
    match record {
        Record::Model(entry) => {
            let EntryKind::Model { route } = &entry.kind else {
                return Err(EncodeError::MismatchedKind);
            };
            encode_model(entry, route)
        }
        Record::Thinking(entry) => {
            let EntryKind::Thinking { level } = &entry.kind else {
                return Err(EncodeError::MismatchedKind);
            };
            encode_json(&ThinkingWire {
                r#type: "thinking",
                id: entry.id,
                parent: entry.parent,
                at: TsWire(&entry.at),
                level: *level,
            })
        }
        Record::Approval(entry) => {
            let EntryKind::Approval { mode } = &entry.kind else {
                return Err(EncodeError::MismatchedKind);
            };
            encode_json(&ApprovalWire {
                r#type: "approval",
                id: entry.id,
                parent: entry.parent,
                at: TsWire(&entry.at),
                mode: ModeWire(mode),
            })
        }
        Record::Mode(entry) => {
            let EntryKind::Mode { mode } = &entry.kind else {
                return Err(EncodeError::MismatchedKind);
            };
            encode_json(&ModeValueWire {
                r#type: "mode",
                id: entry.id,
                parent: entry.parent,
                at: TsWire(&entry.at),
                mode: *mode,
            })
        }
        _ => unreachable!("the encode_body index routes only encode_setting_entries kinds here"),
    }
}

/// Entry records carrying compacted context.
fn encode_context_entries(record: &Record) -> Result<Vec<u8>, EncodeError> {
    match record {
        Record::Compaction(entry) => {
            let EntryKind::Compaction {
                summary,
                first_kept,
                tokens_before,
                replay,
                usage,
                parts,
                parts_tokens,
            } = &entry.kind
            else {
                return Err(EncodeError::MismatchedKind);
            };
            encode_json(&CompactionWire {
                r#type: "compaction",
                id: entry.id,
                parent: entry.parent,
                at: TsWire(&entry.at),
                summary,
                first_kept: *first_kept,
                tokens_before: *tokens_before,
                replay,
                usage: usage.as_ref().map(UsageWire::new).transpose()?,
                parts: (!parts.is_empty()).then_some(JournalPartsWire(parts)),
                parts_tokens: (!parts.is_empty()).then_some(*parts_tokens),
            })
        }
        Record::BranchSummary(entry) => {
            let EntryKind::BranchSummary { from, summary } = &entry.kind else {
                return Err(EncodeError::MismatchedKind);
            };
            encode_json(&BranchSummaryWire {
                r#type: "branch_summary",
                id: entry.id,
                parent: entry.parent,
                at: TsWire(&entry.at),
                from: *from,
                summary: summary.as_ref(),
            })
        }
        _ => unreachable!("the encode_body index routes only encode_context_entries kinds here"),
    }
}

/// Session lifecycle records.
fn encode_meta(record: &Record) -> Result<Vec<u8>, EncodeError> {
    match record {
        Record::Session(header) => encode_json(&SessionWire {
            r#type: "session",
            id: &header.id,
            at: TsWire(&header.at),
            workspace: &header.workspace,
            product: &header.product,
            from: header.from.map(|source| SourceWire {
                session: source.session,
                entry: source.entry,
            }),
        }),
        Record::Boot { at, r#gen, version } => encode_json(&BootWire {
            r#type: "boot",
            at: TsWire(at),
            r#gen: *r#gen,
            version: version.as_ref(),
        }),
        Record::Leaf { at, to } => encode_json(&LeafWire {
            r#type: "leaf",
            at: TsWire(at),
            to,
        }),
        Record::Label { at, entry, label } => encode_json(&LabelWire {
            r#type: "label",
            at: TsWire(at),
            entry: *entry,
            label,
        }),
        Record::Name { at, name } => encode_json(&NameWire {
            r#type: "name",
            at: TsWire(at),
            name,
        }),
        Record::Archive { at, archived } => encode_json(&ArchiveWire {
            r#type: "archive",
            at: TsWire(at),
            archived: *archived,
        }),
        _ => unreachable!("the encode_body index routes only encode_meta kinds here"),
    }
}

/// Turn control records.
fn encode_control(record: &Record) -> Result<Vec<u8>, EncodeError> {
    match record {
        Record::TurnStart { at, turn } => encode_json(&TurnStartWire {
            r#type: "turn_start",
            at: TsWire(at),
            turn: *turn,
        }),
        Record::ToolStart { at, turn, call } => encode_json(&ToolStartWire {
            r#type: "tool_start",
            at: TsWire(at),
            turn: *turn,
            call,
        }),
        Record::TurnEnd {
            at,
            turn,
            stop,
            usage,
            changes,
        } => encode_json(&TurnEndWire {
            r#type: "turn_end",
            at: TsWire(at),
            turn: *turn,
            stop: TurnEndStopWire(stop),
            usage: usage.as_ref().map(UsageWire::new).transpose()?,
            changes,
        }),
        Record::RuleFired {
            at,
            turn,
            rule,
            entry,
        } => encode_json(&RuleFiredWire {
            r#type: "rule_fired",
            at: TsWire(at),
            turn: *turn,
            rule: rule.as_ref(),
            entry: *entry,
        }),
        Record::Resolved {
            at,
            request,
            answer,
            by,
            was_default,
        } => encode_json(&ResolvedWire {
            r#type: "resolved",
            at: TsWire(at),
            request,
            answer: AnswerWire(answer),
            by,
            was_default: *was_default,
        }),
        Record::BeforeRequestMut {
            at,
            turn,
            ext,
            field,
            old,
            new,
        } => encode_json(&BeforeRequestMutWire {
            r#type: "before_request_mut",
            at: TsWire(at),
            turn: *turn,
            ext: ext.as_ref(),
            field: field.as_ref(),
            old: old.as_ref(),
            new: new.as_ref(),
        }),
        Record::ToolPromoted {
            at,
            tool,
            turn,
            leaf,
        } => encode_json(&ToolPromotedWire {
            r#type: "tool_promoted",
            at: TsWire(at),
            tool: tool.as_ref(),
            turn: *turn,
            leaf: *leaf,
        }),
        Record::WakeAttempt {
            at,
            turn,
            count,
            jobs,
        } => encode_json(&WakeAttemptWire {
            r#type: "wake_attempt",
            at: TsWire(at),
            turn: *turn,
            count: *count,
            jobs,
        }),
        _ => unreachable!("the encode_body index routes only encode_control kinds here"),
    }
}

/// Grant and approval records.
fn encode_grants(record: &Record) -> Result<Vec<u8>, EncodeError> {
    match record {
        Record::AllowAlways { at, tool, by } => encode_json(&AllowAlwaysWire {
            r#type: "allow_always",
            at: TsWire(at),
            tool: tool.as_ref(),
            by,
        }),
        Record::GrantGiven {
            at,
            ext,
            set,
            scope,
            by,
        } => encode_json(&GrantGivenWire {
            r#type: "grant_given",
            at: TsWire(at),
            ext: ext.as_ref(),
            set,
            scope: scope.as_ref(),
            by,
        }),
        Record::ScopedGrant {
            at,
            call,
            prefix,
            roots,
            job,
            by,
        } => encode_json(&ScopedGrantWire {
            r#type: "scoped_grant",
            at: TsWire(at),
            call,
            prefix,
            roots,
            job,
            by,
        }),
        Record::ScopedGrantEnded { at, job } => encode_json(&ScopedGrantEndedWire {
            r#type: "scoped_grant_ended",
            at: TsWire(at),
            job,
        }),
        _ => unreachable!("the encode_body index routes only encode_grants kinds here"),
    }
}

/// Async effect records.
fn encode_effects(record: &Record) -> Result<Vec<u8>, EncodeError> {
    match record {
        Record::Job { at, job, event } => {
            let (literal, kind, outcome, by) = match event {
                JobEvent::Started { kind } => ("start", kind.as_deref(), None, None),
                JobEvent::Settled { outcome } => {
                    ("end", None, outcome.as_ref().map(JobOutcomeWire), None)
                }
                JobEvent::Cancelled { by } => ("cancelled", None, None, by.as_ref()),
                JobEvent::Killed => ("killed", None, None, None),
                JobEvent::TimedOut => ("timed_out", None, None, None),
                JobEvent::Orphaned => ("orphaned", None, None, None),
            };
            encode_json(&JobWire {
                r#type: "job",
                at: TsWire(at),
                job,
                event: literal,
                kind,
                outcome,
                by,
            })
        }
        Record::Ext {
            at,
            ext,
            kind,
            body,
        } => encode_json(&ExtWire {
            r#type: "ext",
            at: TsWire(at),
            ext: ext.as_ref(),
            kind: kind.as_ref(),
            body,
        }),
        Record::Mail(mail) => encode_json(&MailRecordWire {
            r#type: "mail",
            at: TsWire(&mail.at),
            from: &mail.from,
            to: &mail.to,
            mode: mail.mode,
            text: mail.text.as_ref(),
            reply_to: &mail.reply_to,
        }),
        Record::Inferred {
            at,
            who,
            purpose,
            usage,
        } => encode_json(&InferredWire {
            r#type: "inferred",
            at: TsWire(at),
            who: OwnerWire(who),
            purpose: PurposeWire(purpose),
            usage: UsageWire::new(usage)?,
        }),
        _ => unreachable!("the encode_body index routes only encode_effects kinds here"),
    }
}

/// Writes a model record. An API route keeps the byte-exact format-1
/// `api`, `model` members; a synthetic or harness route writes one
/// `route` member after its id passes the same grammar the decoder checks.
pub(super) fn encode_model(entry: &Entry, route: &ModelRoute) -> Result<Vec<u8>, EncodeError> {
    let route = match route {
        ModelRoute::Api { model, .. } if model.is_empty() => {
            return Err(EncodeError::EmptyModel);
        }
        ModelRoute::Api { family, model } => {
            return encode_json(&ModelWire {
                r#type: "model",
                id: entry.id,
                parent: entry.parent,
                at: TsWire(&entry.at),
                api: *family,
                model: model.as_ref(),
            });
        }
        ModelRoute::Synthetic { id } => {
            if !ModelRoute::is_valid_synthetic_id(id) {
                return Err(EncodeError::InvalidRoute(RouteError::InvalidSyntheticId {
                    id: id.clone(),
                }));
            }
            RouteWire::Synthetic { id }
        }
        ModelRoute::Harness { id } => {
            ModelRoute::harness(&**id).map_err(EncodeError::InvalidRoute)?;
            RouteWire::Harness { id }
        }
    };
    encode_json(&ModelRouteWire {
        r#type: "model",
        id: entry.id,
        parent: entry.parent,
        at: TsWire(&entry.at),
        route,
    })
}
