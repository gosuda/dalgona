use super::session::MAILBOX_WAITING_LIMIT;
use super::types::{MAX_WAKE_RUN, PRODUCT_VERSION, PendingMail, Step, Tree, TurnStage};
use super::{
    BTreeMap, Block, CallId, Effect, Emit, Entry, EntryId, EntryKind, EntryView, JobEvent, JobId,
    JobKind, JobOutcome, JournalPart, MailMode, Name, NonZeroU64, Phase, Record, ReplayError,
    Session, TurnEndStop, TurnId,
};
use crate::journal::TurnSequence;

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

/// The synthetic result for a dangling `ask` call: the question died with
/// the turn, so the model continues on best judgment instead of asking again.
pub(super) const ASK_LOST: &str =
    "The question did not survive the restart. Continue on best judgment.";

/// The [`JobOutcome`] a terminal `JobEvent` declares. `Settled` carries
/// its outcome when the writer recorded one; the rest name their cause.
fn job_outcome(event: &JobEvent) -> JobOutcome {
    match event {
        JobEvent::Settled { outcome } => outcome.clone().unwrap_or(JobOutcome::Lost),
        JobEvent::Cancelled { .. } => JobOutcome::Cancelled,
        JobEvent::Killed => JobOutcome::Failed {
            message: "killed".into(),
        },
        JobEvent::TimedOut => JobOutcome::Failed {
            message: "timed out".into(),
        },
        JobEvent::Orphaned | JobEvent::Started { .. } => JobOutcome::Lost,
    }
}

pub(super) fn contradiction(detail: impl Into<Box<str>>) -> ReplayError {
    ReplayError::Contradiction {
        detail: detail.into(),
    }
}

pub(super) fn replay_tool_name(tool: &str, record: &str) -> Result<Name, ReplayError> {
    Name::parse_mapped_tool(tool)
        .map_err(|_| contradiction(format!("{record} record contains an invalid tool name")))
}

pub(super) struct PendingWake {
    pub(super) turn: TurnId,
    pub(super) jobs: Vec<JobId>,
}

/// Record-order state carried while folding one journal into a session.
pub(super) struct Replay {
    pub(super) session: Session,
    pub(super) active_turn: Option<TurnId>,
    pub(super) calls: Vec<ReplayCall>,
    pub(super) started_jobs: Vec<(JobId, Option<JobKind>)>,
    pub(super) pending_wakes: Vec<PendingWake>,
    pub(super) opening_wake: Option<Vec<JobId>>,
    pub(super) turns: TurnSequence,
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
            pending_wakes: Vec::new(),
            opening_wake: None,
            turns: TurnSequence::default(),
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
            self.session.replay_setting(entry);
        }
        self.session_state(record)
    }

    pub(super) fn protocol(&mut self, record: &Record) -> Result<(), ReplayError> {
        self.feed_totals(record)?;
        let opening_wake = self.opening_wake.take();
        match record {
            Record::Boot { r#gen, .. } => {
                self.session.generation = Some(*r#gen);
                self.turns.expire_pending();
                self.pending_wakes.clear();
            }
            Record::TurnReserved { turn, .. } => self
                .turns
                .reserve(*turn)
                .map_err(|error| contradiction(error.to_string()))?,
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
            } => self.wake_attempt(*turn, *count, jobs)?,
            Record::MailDelivered {
                at,
                turn,
                entry,
                mails,
            } => {
                if self.active_turn != Some(*turn) {
                    return Err(contradiction(
                        "mail delivered outside the turn that started it",
                    ));
                }
                // The fold holds the authoritative copy of every waiting
                // row, so the delivery must match it byte for byte before
                // anything mutates: a tampered or foreign line consumes
                // nothing instead of replaying text the retained row never
                // held. A pending position exists only for a `NextTurn` row
                // this session addressed to itself, which is the recipient
                // a stored position implies.
                for (index, mail) in mails.iter().enumerate() {
                    if mails[..index]
                        .iter()
                        .any(|prior| prior.position == mail.position)
                    {
                        return Err(contradiction("mail delivered repeats a position"));
                    }
                    let pending = self
                        .session
                        .pending_mail
                        .iter()
                        .find(|pending| pending.position == mail.position)
                        .ok_or_else(|| contradiction("mail delivered was never waiting"))?;
                    if pending.text != mail.text {
                        return Err(contradiction(
                            "mail delivered text differs from the waiting mail",
                        ));
                    }
                }
                if self.session.tree.entries.contains_key(entry) {
                    return Err(contradiction("mail delivery reused a tree entry"));
                }
                // Validated whole: now consume exactly the validated
                // positions in one sweep, then materialize the entry they
                // name with the parent the live fold stamped (the leaf so
                // far).
                self.session
                    .pending_mail
                    .retain(|pending| !mails.iter().any(|mail| mail.position == pending.position));
                let parts = mails
                    .iter()
                    .map(|mail| JournalPart::Text {
                        text: mail.text.clone(),
                    })
                    .collect();
                let derived = Entry {
                    id: *entry,
                    parent: self.session.tree.leaf,
                    at: *at,
                    kind: EntryKind::User { parts },
                };
                self.max_entry = self.max_entry.max(entry.get());
                self.session.tree.append(derived);
            }
            Record::User(_) => {
                if let Some(jobs) = opening_wake {
                    self.session.delivered_jobs.extend(jobs);
                }
            }
            Record::Job { job, event, .. } => self.job(*job, event)?,
            Record::Reminder(Entry {
                kind: EntryKind::Reminder { source, .. },
                ..
            }) if source.as_ref() == "jobs.finished" => {
                // The live path writes this reminder and then drains
                // `ended_jobs`; replaying the record consumes the pending
                // entries the same way, so a reopened session does not
                // re-emit reminders the journal already delivered.
                self.session.ended_jobs.clear();
            }
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
        self.turns
            .start(turn)
            .map_err(|error| contradiction(error.to_string()))?;
        self.active_turn = Some(turn);
        self.session.turn_totals.reset();
        if self
            .session
            .wake_attempt_turn
            .is_some_and(|wake_turn| wake_turn.get() < turn.get())
        {
            self.session.wake_run = 0;
            self.session.wake_attempt_turn = None;
            self.pending_wakes.clear();
        }
        self.calls.clear();
        self.opening_wake = self.claim_wake_attempt(turn);
        Ok(())
    }

    fn claim_wake_attempt(&mut self, turn: TurnId) -> Option<Vec<JobId>> {
        let index = self
            .pending_wakes
            .iter()
            .position(|pending| pending.turn == turn)?;
        Some(self.pending_wakes.swap_remove(index).jobs)
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

    pub(super) fn wake_attempt(
        &mut self,
        turn: TurnId,
        count: u32,
        jobs: &[JobId],
    ) -> Result<(), ReplayError> {
        if count == 0 || count > MAX_WAKE_RUN {
            return Err(contradiction("wake count is outside the configured limit"));
        }
        if self
            .pending_wakes
            .iter()
            .any(|pending| pending.turn == turn)
        {
            return Err(contradiction("wake attempt repeats a turn"));
        }
        self.session.wake_run = count;
        self.session.wake_attempt_turn = Some(turn);
        if self.turns.next().is_some_and(|next| turn >= next) {
            self.turns
                .reserve(turn)
                .map_err(|error| contradiction(error.to_string()))?;
        }
        self.pending_wakes.push(PendingWake {
            turn,
            jobs: jobs.to_vec(),
        });
        Ok(())
    }

    pub(super) fn job(&mut self, job: JobId, event: &JobEvent) -> Result<(), ReplayError> {
        match event {
            JobEvent::Started { kind } => {
                let kind = kind
                    .as_deref()
                    .map(|value| {
                        match value {
                            "exec" => Some(JobKind::Exec),
                            "child" => Some(JobKind::Child),
                            "compaction" => Some(JobKind::Compaction),
                            _ => None,
                        }
                        .ok_or_else(|| contradiction("job start has an unknown kind"))
                    })
                    .transpose()?;
                if self.started_jobs.iter().any(|(started, _)| *started == job) {
                    return Err(contradiction("job started more than once"));
                }
                self.started_jobs.push((job, kind));
                // Journal job records are a declared source too: mirror
                // them into the session so live/ended jobs inspect the
                // written history, not just update records.
                match kind {
                    Some(kind) => {
                        // Declared replay projects ledgers, not client
                        // updates: the emitted job updates are scratch.
                        let mut emit = Emit {
                            records: Vec::new(),
                            updates: Vec::new(),
                        };
                        self.session.job_started(job, kind, &mut emit);
                    }
                    None => self.session.live_jobs.push((job, None)),
                }
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
                let mut emit = Emit {
                    records: Vec::new(),
                    updates: Vec::new(),
                };
                self.session.job_settled(job, job_outcome(event), &mut emit);
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
            Record::RuleFired {
                turn, rule, entry, ..
            } => {
                session.rule_facts.push(super::GateSeed {
                    rule: rule.clone(),
                    turn: *turn,
                    entry: *entry,
                });
            }
            Record::Ext {
                ext, kind, body, ..
            } => session.fold_ext(ext, kind, body),
            Record::PrivateLoop {
                r#loop, who, turn, ..
            } => {
                if let Err(conflict) = session.check_private_loop(*r#loop) {
                    return Err(contradiction(conflict.detail()));
                }
                session.fold_private_loop(*r#loop, who.clone(), *turn);
            }
            Record::PrivateStart {
                r#loop,
                token,
                who,
                turn,
                round,
                name,
                ..
            } => {
                if let Err(conflict) = session.check_private_start(*r#loop, *token, who, *turn) {
                    return Err(contradiction(conflict.detail()));
                }
                session.fold_private_start(*r#loop, *token, who, *turn, *round, name);
            }
            Record::PrivateEnd {
                r#loop,
                token,
                outcome,
                ..
            } => {
                if let Err(conflict) = session.check_private_end(*r#loop, *token) {
                    return Err(contradiction(conflict.detail()));
                }
                session.fold_private_end(*r#loop, *token, outcome.clone());
            }
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
            Record::Mail(mail) => self.note_mail(mail)?,
            _ => {}
        }
        Ok(())
    }

    /// Folds one journaled mail row: every row mints its journal-wide
    /// position, while only this session's own undelivered `NextTurn` rows
    /// become pending; `Aside` and `Steer` rows, foreign recipients, and
    /// delivered history never count against the bound. The waiting check
    /// precedes the position mint, so a rejected row mutates nothing.
    pub(super) fn note_mail(&mut self, mail: &crate::Mail) -> Result<(), ReplayError> {
        let session = &mut self.session;
        let waiting = mail.mode == MailMode::NextTurn && session.id.is_some_and(|id| mail.to == id);
        if waiting && session.pending_mail.len() >= MAILBOX_WAITING_LIMIT {
            return Err(contradiction(
                "waiting mail exceeds the recipient's pending limit",
            ));
        }
        let Some(position) = session.next_mail_position() else {
            return Err(contradiction("mail position space exhausted"));
        };
        session.mail_rows = position.get();
        if waiting {
            session.pending_mail.push(PendingMail {
                position,
                text: mail.text.clone(),
            });
        }
        Ok(())
    }

    /// Applies the declared-fold tail to the running session: reserves
    /// entry and turn counters, surfaces an unterminated turn as the
    /// running phase the journal declares, and restores branch state.
    /// Idempotent, so an incremental fold can re-apply it after every
    /// pushed record.
    pub(super) fn declared_tail(&mut self) -> Result<(), ReplayError> {
        self.calls.retain(|known| !known.settled);
        self.reserve_counters()?;
        self.session.phase = match self.active_turn {
            Some(turn) => Phase::Running {
                turn,
                round: Step(0),
                stage: TurnStage::Boundary,
            },
            None => Phase::Idle,
        };
        self.session.restore_branch_state(self.session.tree.leaf)
    }

    /// Folds the declared records into a session without crash-recovery
    /// synthesis: open turns stay open and no repair, orphan, or boot
    /// records are minted, so journal inspection attributes state to the
    /// written records only.
    pub(super) fn finish_declared(mut self) -> Result<Session, ReplayError> {
        self.declared_tail()?;
        Ok(self.session)
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
        // The synthesized orphans are the jobs' terminal records: settle them
        // through the session too, or a reopened session lists them as live.
        // They stay in `ended_jobs` so the next turn's `jobs.finished`
        // reminder reports the loss once.
        for (job, _) in std::mem::take(&mut self.started_jobs) {
            let mut emit = Emit {
                records: Vec::new(),
                updates: Vec::new(),
            };
            self.session.job_settled(job, JobOutcome::Lost, &mut emit);
        }
        self.session.repair_private_ends(now, &mut records);
        let mut session = self.session;
        session.restore_branch_state(session.tree.leaf)?;
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
            .turns
            .next()
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
            let lost = if name.as_ref() == "ask" {
                ASK_LOST
            } else {
                TOOL_LOST
            };
            let entry = self
                .session
                .entry(
                    now,
                    EntryKind::ToolResult {
                        call,
                        name,
                        error: true,
                        parts: vec![JournalPart::Text { text: lost.into() }],
                        changes: Vec::new(),
                        elapsed_ms: None,
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
