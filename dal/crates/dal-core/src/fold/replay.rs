use super::helpers::{entry_weight, parse_job_kind};
use super::types::{MAX_WAKE_RUN, PRODUCT_VERSION, Tree};
use super::{
    BTreeMap, Block, CallId, Effect, Emit, Entry, EntryId, EntryKind, EntryView, JobEvent, JobId,
    JobKind, JournalPart, Name, NonZeroU64, Phase, Record, ReplayError, Session, TurnEndStop,
    TurnId,
};

impl Tree {
    pub(super) fn new() -> Self {
        Self {
            entries: BTreeMap::new(),
            leaf: None,
            labels: BTreeMap::new(),
        }
    }

    pub(super) fn append(&mut self, entry: Entry) -> EntryView {
        let view = EntryView {
            id: entry.id,
            parent: entry.parent,
            kind: entry.kind.clone(),
        };
        self.leaf = Some(entry.id);
        self.entries.insert(entry.id, entry);
        view
    }

    pub(super) fn ancestors(&self, from: Option<EntryId>) -> Vec<EntryId> {
        let mut branch = Vec::new();
        let mut cursor = from;
        while let Some(id) = cursor {
            let Some(entry) = self.entries.get(&id) else {
                break;
            };
            branch.push(id);
            cursor = entry.parent;
        }
        branch.reverse();
        branch
    }
}

/// One assistant tool call tracked while replaying its owning turn.
#[derive(Clone, Debug)]
pub(super) struct ReplayCall {
    pub(super) call: CallId,
    pub(super) name: Box<str>,
    pub(super) started: bool,
    pub(super) settled: bool,
}

pub(super) const TOOL_LOST: &str =
    "Tool call was not completed: dalgon stopped before it finished.";

pub(super) fn contradiction(detail: impl Into<Box<str>>) -> ReplayError {
    ReplayError::Contradiction {
        detail: detail.into(),
    }
}

pub(super) fn replay_tool_name(tool: &str, record: &str) -> Result<Name, ReplayError> {
    Name::parse_mapped_tool(tool)
        .map_err(|_| contradiction(format!("{record} record contains an invalid tool name")))
}

/// Record-order state carried while folding one journal into a session.
pub(super) struct Replay {
    pub(super) session: Session,
    pub(super) active_turn: Option<TurnId>,
    pub(super) calls: Vec<ReplayCall>,
    pub(super) started_jobs: Vec<(JobId, Option<JobKind>)>,
    pub(super) max_turn: u64,
    pub(super) last_completed_turn: u64,
    pub(super) max_entry: u64,
    pub(super) saw_record: bool,
}

impl Replay {
    pub(super) fn new() -> Self {
        Self {
            session: Session::empty(),
            active_turn: None,
            calls: Vec::new(),
            started_jobs: Vec::new(),
            max_turn: 0,
            last_completed_turn: 0,
            max_entry: 0,
            saw_record: false,
        }
    }

    pub(super) fn record(&mut self, record: &Record) -> Result<(), ReplayError> {
        if let Record::Session(header) = record {
            if self.saw_record {
                return Err(contradiction(
                    "session header is not first or is duplicated",
                ));
            }
            self.session.id = Some(header.id);
        }
        self.saw_record = true;
        self.protocol(record)?;
        if let Some(entry) = record.entry() {
            self.max_entry = self.max_entry.max(entry.id.get());
            self.session.tree.append(entry.clone());
            self.session.projected_bytes = self
                .session
                .projected_bytes
                .saturating_add(entry_weight(entry));
            self.session.replay_setting(entry);
        }
        self.session_state(record)
    }

    pub(super) fn protocol(&mut self, record: &Record) -> Result<(), ReplayError> {
        self.feed_totals(record)?;
        match record {
            Record::Boot { r#gen, .. } => self.session.generation = Some(*r#gen),
            Record::TurnStart { turn, .. } => self.turn_start(*turn)?,
            Record::Assistant(entry) => self.assistant_calls(entry),
            Record::ToolStart { turn, call, .. } => self.tool_start(*turn, call)?,
            Record::ToolResult(Entry {
                kind: EntryKind::ToolResult { call, .. },
                ..
            }) => self.tool_result(call)?,
            Record::TurnEnd { turn, .. } => self.turn_end(*turn)?,
            Record::WakeAttempt {
                turn, count, jobs, ..
            } => {
                self.wake_attempt(*turn, *count)?;
                self.session.delivered_jobs.extend(jobs.iter().copied());
            }
            Record::Job { job, event, .. } => self.job(*job, event)?,
            _ => {}
        }
        Ok(())
    }

    /// Folds a replayed record into the open turn's totals, mirroring the
    /// live journal paths so a replayed session ends its turn with the same
    /// `Record::TurnEnd` totals.
    pub(super) fn feed_totals(&mut self, record: &Record) -> Result<(), ReplayError> {
        if self.active_turn.is_none() {
            return Ok(());
        }
        let totals = &mut self.session.turn_totals;
        let result = match record {
            Record::Assistant(entry) => match &entry.kind {
                EntryKind::Assistant { usage, .. } => totals.add_usage(*usage),
                _ => Ok(()),
            },
            Record::ToolResult(entry) => match &entry.kind {
                EntryKind::ToolResult { changes, .. } => totals.add_changes(changes),
                _ => Ok(()),
            },
            Record::Compaction(entry) => match &entry.kind {
                EntryKind::Compaction {
                    usage: Some(usage), ..
                } => totals.add_usage(*usage),
                _ => Ok(()),
            },
            // The store validator counts this record inside the turn window,
            // so replay includes it to keep repair totals aligned.
            Record::Inferred { usage, .. } => totals.add_usage(*usage),
            _ => Ok(()),
        };
        result.map_err(|error| contradiction(error.to_string()))
    }

    pub(super) fn turn_start(&mut self, turn: TurnId) -> Result<(), ReplayError> {
        if self.active_turn.is_some() {
            return Err(contradiction("a turn started before the prior turn ended"));
        }
        if turn.get() <= self.last_completed_turn {
            return Err(contradiction("turn ids are not strictly increasing"));
        }
        self.active_turn = Some(turn);
        self.session.turn_totals.reset();
        if self
            .session
            .wake_attempt_turn
            .is_some_and(|wake_turn| wake_turn.get() < turn.get())
        {
            self.session.wake_run = 0;
            self.session.wake_attempt_turn = None;
        }
        self.max_turn = self.max_turn.max(turn.get());
        self.calls.clear();
        Ok(())
    }

    pub(super) fn assistant_calls(&mut self, entry: &Entry) {
        let EntryKind::Assistant { content, .. } = &entry.kind else {
            return;
        };
        for block in content {
            let Block::ToolCall { id, name, .. } = block else {
                continue;
            };
            if self
                .calls
                .iter()
                .any(|known| known.call == *id && !known.settled)
            {
                continue;
            }
            self.calls.push(ReplayCall {
                call: id.clone(),
                name: name.clone(),
                started: false,
                settled: false,
            });
        }
    }

    pub(super) fn open_call(&mut self, call: &CallId) -> Option<&mut ReplayCall> {
        self.calls
            .iter_mut()
            .find(|known| known.call == *call && !known.settled)
    }

    pub(super) fn tool_start(&mut self, turn: TurnId, call: &CallId) -> Result<(), ReplayError> {
        if self.active_turn != Some(turn) {
            return Err(contradiction("tool call started outside its owning turn"));
        }
        let Some(known) = self.open_call(call) else {
            return Err(contradiction(format!(
                "tool call {} started without an open assistant tool call",
                call.as_str()
            )));
        };
        if known.started {
            return Err(contradiction(format!(
                "tool call {} started more than once",
                call.as_str()
            )));
        }
        known.started = true;
        Ok(())
    }

    pub(super) fn tool_result(&mut self, call: &CallId) -> Result<(), ReplayError> {
        if let Some(known) = self.open_call(call) {
            known.settled = true;
            return Ok(());
        }
        let detail = if self.calls.iter().any(|known| known.call == *call) {
            format!("repeated tool result for call {}", call.as_str())
        } else {
            format!("tool result for unknown call {}", call.as_str())
        };
        Err(contradiction(detail))
    }

    pub(super) fn turn_end(&mut self, turn: TurnId) -> Result<(), ReplayError> {
        if self.active_turn != Some(turn) {
            return Err(contradiction("turn ended without a matching start"));
        }
        if let Some(open) = self.calls.iter().find(|known| !known.settled) {
            return Err(contradiction(format!(
                "turn ended with tool call {} missing its result",
                open.call.as_str()
            )));
        }
        self.active_turn = None;
        self.calls.clear();
        self.session.turn_totals.reset();
        self.last_completed_turn = self.last_completed_turn.max(turn.get());
        Ok(())
    }

    pub(super) fn wake_attempt(&mut self, turn: TurnId, count: u32) -> Result<(), ReplayError> {
        if count == 0 || count > MAX_WAKE_RUN {
            return Err(contradiction("wake count is outside the configured limit"));
        }
        self.session.wake_run = count;
        self.session.wake_attempt_turn = Some(turn);
        self.max_turn = self.max_turn.max(turn.get());
        Ok(())
    }

    pub(super) fn job(&mut self, job: JobId, event: &JobEvent) -> Result<(), ReplayError> {
        match event {
            JobEvent::Started { kind } => {
                let kind = kind
                    .as_deref()
                    .map(|value| {
                        parse_job_kind(Some(value))
                            .ok_or_else(|| contradiction("job start has an unknown kind"))
                    })
                    .transpose()?;
                if self.started_jobs.iter().any(|(started, _)| *started == job) {
                    return Err(contradiction("job started more than once"));
                }
                self.started_jobs.push((job, kind));
            }
            JobEvent::Settled { .. }
            | JobEvent::Cancelled { .. }
            | JobEvent::Killed
            | JobEvent::TimedOut
            | JobEvent::Orphaned => {
                if !self.started_jobs.iter().any(|(started, _)| *started == job) {
                    return Err(contradiction("job ended without a start"));
                }
                self.started_jobs.retain(|(started, _)| *started != job);
            }
        }
        Ok(())
    }

    pub(super) fn session_state(&mut self, record: &Record) -> Result<(), ReplayError> {
        let session = &mut self.session;
        match record {
            Record::Leaf { to: Some(id), .. } if !session.tree.entries.contains_key(id) => {
                return Err(contradiction("leaf points to an unknown entry"));
            }
            Record::Leaf { to, .. } => session.tree.leaf = *to,
            Record::Ext {
                ext, kind, body, ..
            } => session.fold_ext(ext, kind, body),
            Record::Label {
                entry,
                label: Some(label),
                ..
            } => {
                session.tree.labels.insert(*entry, label.clone());
            }
            Record::Label {
                entry, label: None, ..
            } => {
                session.tree.labels.remove(entry);
            }
            Record::Name { name, .. } => session.settings.name.clone_from(name),
            Record::AllowAlways { tool, .. } => {
                session
                    .allow_always
                    .insert(replay_tool_name(tool, "allow-always")?);
            }
            Record::ToolPromoted { tool, .. } => {
                session
                    .promoted
                    .insert(replay_tool_name(tool, "tool-promoted")?);
            }
            _ => {}
        }
        Ok(())
    }

    pub(super) fn finish(
        mut self,
        now: jiff::Timestamp,
    ) -> Result<(Session, Vec<Effect>), ReplayError> {
        self.calls.retain(|known| !known.settled);
        self.reserve_counters()?;
        let mut records = Vec::new();
        self.repair_open_turn(now, &mut records)?;
        records.extend(self.started_jobs.iter().map(|(job, _)| Record::Job {
            at: now,
            job: *job,
            event: JobEvent::Orphaned,
        }));
        let mut session = self.session;
        session.restore_branch_state()?;
        let generation = session.next_generation()?;
        records.push(Record::Boot {
            at: now,
            r#gen: generation,
            version: PRODUCT_VERSION.into(),
        });
        session.generation = Some(generation);
        session.phase = Phase::Idle;
        let emit = Emit {
            records,
            updates: Vec::new(),
        };
        Ok((session, vec![Effect::Emit(emit)]))
    }

    pub(super) fn reserve_counters(&mut self) -> Result<(), ReplayError> {
        let repair_entries = u64::try_from(self.calls.len())
            .map_err(|_| contradiction("too many dangling calls to repair"))?;
        let next_entry = self
            .max_entry
            .checked_add(1)
            .and_then(NonZeroU64::new)
            .map(EntryId::new)
            .ok_or_else(|| contradiction("entry id space exhausted"))?;
        if self.max_entry.checked_add(repair_entries).is_none() {
            return Err(contradiction(
                "entry id space exhausted during crash repair",
            ));
        }
        let next_turn = self
            .max_turn
            .checked_add(1)
            .and_then(NonZeroU64::new)
            .map(TurnId::new)
            .ok_or_else(|| contradiction("turn id space exhausted"))?;
        self.session.next_entry = Some(next_entry);
        self.session.next_turn = Some(next_turn);
        self.session.last_turn = self.last_completed_turn;
        Ok(())
    }

    pub(super) fn repair_open_turn(
        &mut self,
        now: jiff::Timestamp,
        records: &mut Vec<Record>,
    ) -> Result<(), ReplayError> {
        let Some(turn) = self.active_turn else {
            return Ok(());
        };
        for ReplayCall { call, name, .. } in std::mem::take(&mut self.calls) {
            let entry = self
                .session
                .entry(
                    now,
                    EntryKind::ToolResult {
                        call,
                        name,
                        error: true,
                        parts: vec![JournalPart::Text {
                            text: TOOL_LOST.into(),
                        }],
                        changes: Vec::new(),
                    },
                )
                .map_err(|_| contradiction("entry id space exhausted during crash repair"))?;
            self.session.tree.append(entry.clone());
            records.push(Record::ToolResult(entry));
        }
        records.push(Record::TurnEnd {
            at: now,
            turn,
            stop: TurnEndStop::Aborted,
            usage: self.session.turn_totals.usage(),
            changes: self.session.turn_totals.changes(),
        });
        self.session.last_turn = turn.get();
        Ok(())
    }
}
