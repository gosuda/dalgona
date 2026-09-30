//! Fixed user-facing strings and their small formatting rules.

use std::time::Duration;

/// Every fixed terminal string, keyed by its stable deck id.
pub mod ids {
    /// Product tagline.
    pub const APP_TAGLINE: &str = "dal {version} · {path}";
    /// First-run model prompt.
    pub const FIRST_RUN_TITLE: &str =
        "No model selected yet. dal needs a model before it can answer.";
    /// First-run authentication prompt.
    pub const FIRST_RUN_ACTION: &str = "Pick an API to sign in to:";
    /// Model picker title.
    pub const MODEL_PICKER_TITLE: &str =
        "Pick a model · {n} model|s from {provider} · type to filter";
    /// Model fetching state.
    pub const MODEL_PICKER_FETCHING: &str = "fetching models...";
    /// Model picker failure and retry instruction.
    pub const MODEL_PICKER_FAIL: &str =
        "Unable to load the model list. Check the network, then run /model to retry.";
    /// Typed model-id fallback instruction.
    pub const MODEL_PICKER_FALLBACK: &str = "You can also type a model id.";
    /// Idle hint when kitty keyboard keys are active.
    pub const HINT_IDLE: &str = "enter send · shift+enter newline · esc interrupt · f1 help";
    /// Idle hint for legacy terminals.
    pub const HINT_IDLE_LEGACY: &str = "enter send · ctrl+j newline · esc interrupt · f1 help";
    /// Short idle hint for narrow layouts.
    pub const HINT_IDLE_SHORT: &str = "enter send · f1 help";
    /// Empty composer placeholder.
    pub const COMPOSER_PLACEHOLDER: &str = "Ask dal to change code. / for commands.";
    /// Status shown without a selected model.
    pub const STATUS_NO_MODEL: &str = "no model · not signed in";
    /// Signed-in provider status template.
    pub const STATUS_SIGNED_IN: &str = "{provider} (signed in)";
    /// Full-width token usage template.
    pub const STATUS_TOKENS: &str = "in {in} out {out}";
    /// Context usage state templates.
    pub const STATUS_CONTEXT: &str = "ctx {pct}%";
    /// Context usage warning template.
    pub const STATUS_CONTEXT_RISING: &str = "ctx {pct}% · rising";
    /// Context usage high template.
    pub const STATUS_CONTEXT_HIGH: &str = "ctx {pct}% · high";
    /// Agent count template.
    pub const STATUS_AGENTS: &str = "{n} agent|s";
    /// Job count template.
    pub const STATUS_JOBS: &str = "{n} job|s";
    /// Thinking state word.
    pub const STATE_THINKING: &str = "thinking";
    /// Working state word.
    pub const STATE_WORKING: &str = "working";
    /// Fetching state word.
    pub const STATE_FETCHING: &str = "fetching";
    /// Compacting state word.
    pub const STATE_COMPACTING: &str = "compacting";
    /// Retrying state word.
    pub const STATE_RETRYING: &str = "retrying";
    /// Waiting-for-answer state word.
    pub const STATE_WAITING: &str = "waiting for you";
    /// Collapsed reasoning row.
    pub const THINKING_COLLAPSED: &str = "Thinking {dur} · ctrl+o to show reasoning";
    /// Expanded reasoning header.
    pub const THINKING_OPEN: &str = "Thinking · {n} step|s · {dur} · ctrl+o to hide";
    /// Remaining reasoning count.
    pub const THINKING_MORE: &str = "... {n} more step|s";
    /// Successful settled tool card.
    pub const TOOL_OK: &str = "ok  {name} {summary} · {dur}";
    /// Failed settled tool card.
    pub const TOOL_FAILED: &str = "failed  {name} · {reason} · {dur}";
    /// Tool expansion hint.
    pub const TOOL_EXPAND: &str = "ctrl+o expands";
    /// Tool collapse hint.
    pub const TOOL_COLLAPSE: &str = "ctrl+o collapses";
    /// Truncated tool output footer.
    pub const TOOL_MORE: &str = "... {n} more line|s in the full output";
    /// Patch summary header.
    pub const DIFF_HEADER: &str = "patch {path} · +{add} -{del}";
    /// Truncated diff footer.
    pub const DIFF_MORE: &str = "... {n} more hunk|s";
    /// Child-session aggregation header.
    pub const AGENTS_HEADER: &str = "{running} running · {failed} failed · {done} done";
    /// Dalgona run aggregation header.
    pub const AGENTS_RUN_HEADER: &str =
        "run {label} · {running} running · {queued} queued · {failed} failed · {done} done";
    /// Folded child-session summary.
    pub const AGENTS_MORE: &str = "({n} more: {breakdown})";
    /// Failed child-session row.
    pub const AGENTS_FAIL: &str = "failed · {reason}";
    /// Job aggregation header.
    pub const JOBS_HEADER: &str = "{n} job|s total · {running} running · {done} done";
    /// Folded running jobs summary.
    pub const JOBS_MORE: &str = "({n} more running)";
    /// Background exec completion notice.
    pub const JOB_EXEC_NOTICE: &str = "exec {id} \"{label}\": exit {code} in {duration}";
    /// Background exec cancellation notice.
    pub const JOB_EXEC_CANCELLED: &str = "exec {id} \"{label}\": cancelled";
    /// Empty job output label.
    pub const JOB_EXEC_NO_OUTPUT: &str = "no output";
    /// Extension activity row.
    pub const EXT_ROW: &str = "{ext}: {text}";
    /// Extension activity without details.
    pub const EXT_BUSY: &str = "{ext}: busy";
    /// Folded extension activity count.
    pub const EXT_MORE: &str = "({n} more extension|s busy)";
    /// Cancelled turn notice.
    pub const TURN_CANCELLED: &str =
        "The turn stopped. The session keeps everything up to the last completed step.";
    /// Length-limited turn notice.
    pub const TURN_LENGTH: &str = "The reply hit the length limit. Ask dal to continue.";
    /// Filtered turn notice.
    pub const TURN_FILTER: &str =
        "The reply was blocked by the content filter. Rephrase the request.";
    /// Generic turn failure template.
    pub const TURN_FAILED: &str = "{message}\n{hint}";
    /// Live diagram placeholder.
    pub const DIAGRAM_RENDERING: &str = "diagram {kind} · rendering...";
    /// Settled diagram header.
    pub const DIAGRAM_READY: &str = "diagram {kind} · {n} row|s · ctrl+o expands";
    /// Expanded diagram header.
    pub const DIAGRAM_READY_OPEN: &str = "diagram {kind} · {n} row|s · ctrl+o collapses";
    /// Diagram fallback header.
    pub const DIAGRAM_FAILED: &str = "diagram {kind}: render failed ({reason}) · source shown";
    /// Diagram too-wide reason.
    pub const DIAGRAM_REASON_WIDE: &str = "too wide";
    /// Diagram timeout reason.
    pub const DIAGRAM_REASON_TIMEOUT: &str = "timed out";
    /// Missing renderer reason.
    pub const DIAGRAM_REASON_NO_TOOL: &str = "{tool} is not installed";
    /// Invalid diagram source reason.
    pub const DIAGRAM_REASON_PARSE: &str = "invalid {kind}";
    /// Oversized diagram reason.
    pub const DIAGRAM_REASON_LARGE: &str = "too large";
    /// Image placeholder card.
    pub const IMAGE_CARD: &str = "image {name} · {w}x{h} · {size}";
    /// Expanded image note.
    pub const IMAGE_OPEN: &str =
        "The image was sent to the model. It is not shown in the terminal.";
    /// Expanded image note when shown inline.
    pub const IMAGE_OPEN_SHOWN: &str = "The image was sent to the model.";
    /// Active compaction header.
    pub const COMPACT_ACTIVE: &str = "Compacting the session. {pct}% of the context window used.";
    /// Active compaction body.
    pub const COMPACT_BODY: &str = "This takes a few seconds. The transcript above stays readable.";
    /// Compaction completion notice.
    pub const COMPACT_DONE: &str = "Compacted. {kept} of {window} tokens kept.";
    /// Compaction completion notice with older history images.
    pub const COMPACT_DONE_IMAGES: &str =
        "Compacted. {kept} of {window} tokens kept. Older history is in {n} images.";
    /// Compaction status word.
    pub const COMPACT_STATE: &str = "compacting";
    /// Rule fire notice.
    pub const RULE_FIRED: &str = "rule {name} fired. The {subject} matched /{pattern}/.";
    /// Rule retry notice.
    pub const RULE_RETRY: &str = "The turn retries with the rule applied.";
    /// Rule reminder notice.
    pub const RULE_REMIND: &str = "The model sees the rule with its next request.";
    /// Queued steering count.
    pub const STEER_QUEUED: &str = "{n} message|s queued for the next reply";
    /// Command approval title.
    pub const APPROVAL_TITLE_COMMAND: &str = "Allow this command?";
    /// Patch approval title.
    pub const APPROVAL_TITLE_PATCH: &str = "Allow this edit to {path}?";
    /// Extension evaluation approval title.
    pub const APPROVAL_TITLE_EVAL: &str = "Allow this eval cell to use {services}?";
    /// Tool approval title.
    pub const APPROVAL_TITLE_TOOL: &str = "Allow {tool}?";
    /// Call-scoped grant clause.
    pub const APPROVAL_GRANT_CLAUSE: &str = "also allows {argv} in {roots} until the job ends";
    /// One-call approval action.
    pub const APPROVAL_ONCE: &str = "Allow once";
    /// Session approval action.
    pub const APPROVAL_SESSION: &str = "Allow for this session";
    /// Denial action.
    pub const APPROVAL_DENY: &str = "Deny";
    /// Full-preview action.
    pub const APPROVAL_VIEW: &str = "View full preview";
    /// Approval escape hint.
    pub const APPROVAL_ESC_DENIES: &str = "esc denies";
    /// Extension grant title.
    pub const GRANT_TITLE: &str = "Allow {ext} to use {services}?";
    /// Extension grant body.
    pub const GRANT_BODY: &str =
        "The plugin {ext} from {origin} asks for these services. It gets only what you allow.";
    /// Short dialog action row.
    pub const DIALOG_ACTIONS_SHORT: &str = "y allow · a session · n deny";
    /// Dialog preview overflow footer.
    pub const DIALOG_BODY_MORE: &str = "... {n} more line|s · pgdn";
    /// Count of queued requests.
    pub const REQUEST_MORE: &str = "{n} more waiting";
    /// Request resolved by another client.
    pub const REQUEST_RESOLVED_BY: &str = "note: {client} answered \"{title}\".";
    /// Timed-out request notice.
    pub const REQUEST_TIMED_OUT: &str =
        "note: \"{title}\" timed out. dal used the default: {answer}.";
    /// Turn-cancelled request notice.
    pub const REQUEST_CANCELLED: &str = "note: \"{title}\" was cancelled with the turn.";
    /// Lost answer notice.
    pub const REQUEST_LOST: &str = "note: {error}";
    /// One-call answer word.
    pub const ANSWER_WORD_APPROVE: &str = "allow once";
    /// Session answer word.
    pub const ANSWER_WORD_SESSION: &str = "allow for this session";
    /// Decline answer word.
    pub const ANSWER_WORD_DECLINE: &str = "deny";
    /// Dismiss answer word.
    pub const ANSWER_WORD_CANCEL: &str = "dismiss";
    /// Single-select hint.
    pub const ASK_HINT_SINGLE: &str = "up/down move · enter choose · esc dismiss";
    /// Multi-select hint.
    pub const ASK_HINT_MULTI: &str = "up/down move · space toggle · enter submit · esc dismiss";
    /// Free-text hint.
    pub const ASK_HINT_TEXT: &str = "enter submit · shift+enter newline · esc dismiss";
    /// Confirm hint.
    pub const ASK_HINT_CONFIRM: &str = "y yes · n no · esc dismiss";
    /// Confirmation yes label.
    pub const ASK_YES: &str = "Yes";
    /// Confirmation no label.
    pub const ASK_NO: &str = "No";
    /// Empty free-text answer hint.
    pub const ASK_EMPTY_TEXT: &str = "Type an answer, or press esc to dismiss.";
    /// Preview overflow footer.
    pub const ASK_PREVIEW_MORE: &str = "... {n} more line|s · ctrl+o expands";
    /// Unreachable provider title.
    pub const ERROR_UNREACHABLE: &str = "Unable to reach {host}.";
    /// Provider failure body.
    pub const ERROR_BODY: &str = "The request failed. The session is saved and nothing was lost.";
    /// Retry action.
    pub const ERROR_RETRY: &str = "Retry now";
    /// Cancel-turn action.
    pub const ERROR_CANCEL_TURN: &str = "Cancel turn";
    /// Error details action.
    pub const ERROR_DETAILS: &str = "Show details";
    /// Escape cancellation hint.
    pub const ERROR_ESC_CANCEL: &str = "esc cancels the turn";
    /// Retry countdown.
    pub const RETRY_LINE: &str = "Unable to reach {host}. Retrying in {s}s (attempt {k} of {m}).";
    /// Retry immediately action.
    pub const RETRY_NOW: &str = "Retry now";
    /// Retry cancellation action.
    pub const RETRY_CANCEL: &str = "Cancel";
    /// Resume picker title.
    pub const RESUME_TITLE: &str = "Resume a session · type to filter";
    /// Resume picker row.
    pub const RESUME_ROW: &str = "{name} · {when} · {n} message|s · {dir}";
    /// Resume picker hint.
    pub const RESUME_HINT: &str = "enter resume · esc cancel · d delete";
    /// Session tree title.
    pub const TREE_TITLE: &str =
        "Session tree · enter moves the leaf · the model rewinds to that point";
    /// Active tree leaf marker.
    pub const TREE_LEAF: &str = "(current leaf)";
    /// Session tree hint.
    pub const TREE_HINT: &str = "enter move leaf · left/right collapse · esc cancel";
    /// Fork-point picker title.
    pub const FORK_PICKER_TITLE: &str = "Pick a user message to fork from";
    /// Picker key guide.
    pub const PICKER_HINT: &str = "↑/↓ move · type to filter · enter confirm · esc cancel";
    /// Empty picker notice.
    pub const PICKER_EMPTY: &str = "No choices are available.";
    /// Unsupported picker notice.
    pub const PICKER_UNSUPPORTED: &str =
        "This picker is not available in the terminal client. Use another client.";
    /// Settings title.
    pub const SETTINGS_TITLE: &str = "Settings · changes are session-only until saved";
    /// Model settings label.
    pub const SETTINGS_MODEL: &str = "model";
    /// Screen settings label.
    pub const SETTINGS_SCREEN: &str = "screen";
    /// Thinking settings label.
    pub const SETTINGS_THINKING: &str = "thinking";
    /// Approval settings label.
    pub const SETTINGS_APPROVAL: &str = "approval";
    /// Diagram-rendering setting label.
    pub const SETTINGS_DIAGRAMS: &str = "diagram rendering · {value}";
    /// Save the current diagram setting to the product configuration.
    pub const SETTINGS_DIAGRAMS_SAVE: &str = "save diagram setting to dal.toml";
    /// Ask-mode approval setting.
    pub const SETTINGS_APPROVAL_ASK: &str = "ask before patch and exec";
    /// Edit-mode approval setting.
    pub const SETTINGS_APPROVAL_EDITS: &str = "edits run, exec asks";
    /// Allow-all approval setting.
    pub const SETTINGS_APPROVAL_ALL: &str = "patch and exec run without asking";
    /// Theme settings label.
    pub const SETTINGS_THEME: &str = "theme";
    /// Terminal-palette theme label.
    pub const SETTINGS_THEME_PALETTE: &str = "terminal palette";
    /// Editor settings label.
    pub const SETTINGS_EDITOR: &str = "editor";
    /// Settings hint.
    pub const SETTINGS_HINT: &str = "arrows select · enter toggles or saves · esc close";
    /// Persisted-session exit line.
    pub const EXIT_SAVED: &str =
        "dalgon: session \"{name}\" saved · {n} message|s · run dalgon -r {name} to resume";
    /// Ephemeral-session exit line.
    pub const EXIT_EPHEMERAL: &str = "dalgon: ephemeral session · nothing saved";
    /// Draft discard dialog title.
    pub const EXIT_DRAFT_TITLE: &str = "Discard the draft and quit";
    /// Keep-editing action.
    pub const EXIT_KEEP_EDITING: &str = "Keep editing";
    /// Minimum terminal height notice.
    pub const NARROW_ROWS: &str = "dalgon needs at least 8 rows";
    /// Minimum terminal width notice.
    pub const NARROW_COLS: &str = "dalgon needs at least 12 columns";
    /// Plugin notice prefix.
    pub const NOTICE_PLUGIN: &str = "note: {text}";
    /// Large paste notice.
    pub const PASTE_LARGE: &str = "note: paste of {n} bytes is very large";
    /// Update-shedding notice.
    pub const PERF_SHED: &str = "note: shed {n} update|s to keep up";
    /// Copy success notice.
    pub const COPY_DONE: &str = "copied {n} character|s";
    /// Copy failure notice.
    pub const COPY_FAILED: &str = "copy failed: the terminal refused the selection";
    /// Detached transcript-follow notice.
    pub const FOLLOW_STOPPED: &str = "following stopped · press end to jump to the latest";
    /// Luna Reserve usage notice.
    pub const LUNA_OFFER: &str =
        "note: usage is blocked. Luna Reserve is available: type /model gpt-reserve to switch.";
}

/// Stable copy-deck IDs and their complete templates.
pub const DECK: &[(&str, &str)] = &[
    ("app.tagline", ids::APP_TAGLINE),
    ("firstRun.title", ids::FIRST_RUN_TITLE),
    ("firstRun.action", ids::FIRST_RUN_ACTION),
    ("modelPicker.title", ids::MODEL_PICKER_TITLE),
    ("modelPicker.fetching", ids::MODEL_PICKER_FETCHING),
    ("modelPicker.fail", ids::MODEL_PICKER_FAIL),
    ("modelPicker.fallback", ids::MODEL_PICKER_FALLBACK),
    ("hint.idle", ids::HINT_IDLE),
    ("hint.idle.legacy", ids::HINT_IDLE_LEGACY),
    ("hint.idle.short", ids::HINT_IDLE_SHORT),
    ("composer.placeholder", ids::COMPOSER_PLACEHOLDER),
    ("status.nomodel", ids::STATUS_NO_MODEL),
    ("status.signedin", ids::STATUS_SIGNED_IN),
    ("status.tokens", ids::STATUS_TOKENS),
    ("status.ctx", ids::STATUS_CONTEXT),
    ("status.ctx.rising", ids::STATUS_CONTEXT_RISING),
    ("status.ctx.high", ids::STATUS_CONTEXT_HIGH),
    ("status.agents", ids::STATUS_AGENTS),
    ("status.jobs", ids::STATUS_JOBS),
    ("state.thinking", ids::STATE_THINKING),
    ("state.working", ids::STATE_WORKING),
    ("state.fetching", ids::STATE_FETCHING),
    ("state.compacting", ids::STATE_COMPACTING),
    ("state.retrying", ids::STATE_RETRYING),
    ("state.waiting", ids::STATE_WAITING),
    ("thinking.collapsed", ids::THINKING_COLLAPSED),
    ("thinking.open", ids::THINKING_OPEN),
    ("thinking.more", ids::THINKING_MORE),
    ("tool.ok", ids::TOOL_OK),
    ("tool.failed", ids::TOOL_FAILED),
    ("tool.expand", ids::TOOL_EXPAND),
    ("tool.collapse", ids::TOOL_COLLAPSE),
    ("tool.more", ids::TOOL_MORE),
    ("diff.header", ids::DIFF_HEADER),
    ("diff.more", ids::DIFF_MORE),
    ("agents.header", ids::AGENTS_HEADER),
    ("agents.run.header", ids::AGENTS_RUN_HEADER),
    ("agents.more", ids::AGENTS_MORE),
    ("agents.fail", ids::AGENTS_FAIL),
    ("jobs.header", ids::JOBS_HEADER),
    ("jobs.more", ids::JOBS_MORE),
    ("job.exec.notice", ids::JOB_EXEC_NOTICE),
    ("job.exec.cancelled", ids::JOB_EXEC_CANCELLED),
    ("job.exec.noOutput", ids::JOB_EXEC_NO_OUTPUT),
    ("ext.row", ids::EXT_ROW),
    ("ext.busy", ids::EXT_BUSY),
    ("ext.more", ids::EXT_MORE),
    ("turn.cancelled", ids::TURN_CANCELLED),
    ("turn.length", ids::TURN_LENGTH),
    ("turn.filter", ids::TURN_FILTER),
    ("turn.failed", ids::TURN_FAILED),
    ("diagram.rendering", ids::DIAGRAM_RENDERING),
    ("diagram.ready", ids::DIAGRAM_READY),
    ("diagram.ready.open", ids::DIAGRAM_READY_OPEN),
    ("diagram.failed", ids::DIAGRAM_FAILED),
    ("diagram.reason.wide", ids::DIAGRAM_REASON_WIDE),
    ("diagram.reason.timeout", ids::DIAGRAM_REASON_TIMEOUT),
    ("diagram.reason.noTool", ids::DIAGRAM_REASON_NO_TOOL),
    ("diagram.reason.parse", ids::DIAGRAM_REASON_PARSE),
    ("diagram.reason.large", ids::DIAGRAM_REASON_LARGE),
    ("image.card", ids::IMAGE_CARD),
    ("image.open", ids::IMAGE_OPEN),
    ("image.open.shown", ids::IMAGE_OPEN_SHOWN),
    ("compact.active", ids::COMPACT_ACTIVE),
    ("compact.body", ids::COMPACT_BODY),
    ("compact.done", ids::COMPACT_DONE),
    ("compact.done_images", ids::COMPACT_DONE_IMAGES),
    ("compact.state", ids::COMPACT_STATE),
    ("rule.fired", ids::RULE_FIRED),
    ("rule.retry", ids::RULE_RETRY),
    ("rule.remind", ids::RULE_REMIND),
    ("steer.queued", ids::STEER_QUEUED),
    ("approval.title.command", ids::APPROVAL_TITLE_COMMAND),
    ("approval.title.patch", ids::APPROVAL_TITLE_PATCH),
    ("approval.title.eval", ids::APPROVAL_TITLE_EVAL),
    ("approval.title.tool", ids::APPROVAL_TITLE_TOOL),
    ("approval.grantClause", ids::APPROVAL_GRANT_CLAUSE),
    ("approval.once", ids::APPROVAL_ONCE),
    ("approval.session", ids::APPROVAL_SESSION),
    ("approval.deny", ids::APPROVAL_DENY),
    ("approval.view", ids::APPROVAL_VIEW),
    ("approval.escDenies", ids::APPROVAL_ESC_DENIES),
    ("grant.title", ids::GRANT_TITLE),
    ("grant.body", ids::GRANT_BODY),
    ("dialog.actions.short", ids::DIALOG_ACTIONS_SHORT),
    ("dialog.bodyMore", ids::DIALOG_BODY_MORE),
    ("request.more", ids::REQUEST_MORE),
    ("request.resolvedBy", ids::REQUEST_RESOLVED_BY),
    ("request.timedOut", ids::REQUEST_TIMED_OUT),
    ("request.cancelled", ids::REQUEST_CANCELLED),
    ("request.lost", ids::REQUEST_LOST),
    ("answer.word.approve", ids::ANSWER_WORD_APPROVE),
    ("answer.word.session", ids::ANSWER_WORD_SESSION),
    ("answer.word.decline", ids::ANSWER_WORD_DECLINE),
    ("answer.word.cancel", ids::ANSWER_WORD_CANCEL),
    ("ask.hint.single", ids::ASK_HINT_SINGLE),
    ("ask.hint.multi", ids::ASK_HINT_MULTI),
    ("ask.hint.text", ids::ASK_HINT_TEXT),
    ("ask.hint.confirm", ids::ASK_HINT_CONFIRM),
    ("ask.yes", ids::ASK_YES),
    ("ask.no", ids::ASK_NO),
    ("ask.emptyText", ids::ASK_EMPTY_TEXT),
    ("ask.previewMore", ids::ASK_PREVIEW_MORE),
    ("error.unreachable", ids::ERROR_UNREACHABLE),
    ("error.body", ids::ERROR_BODY),
    ("error.retry", ids::ERROR_RETRY),
    ("error.cancelTurn", ids::ERROR_CANCEL_TURN),
    ("error.details", ids::ERROR_DETAILS),
    ("error.escCancel", ids::ERROR_ESC_CANCEL),
    ("retry.line", ids::RETRY_LINE),
    ("retry.now", ids::RETRY_NOW),
    ("retry.cancel", ids::RETRY_CANCEL),
    ("resume.title", ids::RESUME_TITLE),
    ("resume.row", ids::RESUME_ROW),
    ("resume.hint", ids::RESUME_HINT),
    ("tree.title", ids::TREE_TITLE),
    ("tree.leaf", ids::TREE_LEAF),
    ("picker.fork.title", ids::FORK_PICKER_TITLE),
    ("picker.hint", ids::PICKER_HINT),
    ("picker.empty", ids::PICKER_EMPTY),
    ("picker.unsupported", ids::PICKER_UNSUPPORTED),
    ("tree.hint", ids::TREE_HINT),
    ("settings.title", ids::SETTINGS_TITLE),
    ("settings.model", ids::SETTINGS_MODEL),
    ("settings.screen", ids::SETTINGS_SCREEN),
    ("settings.thinking", ids::SETTINGS_THINKING),
    ("settings.approval", ids::SETTINGS_APPROVAL),
    ("settings.diagrams", ids::SETTINGS_DIAGRAMS),
    ("settings.diagrams.save", ids::SETTINGS_DIAGRAMS_SAVE),
    ("settings.approval.ask", ids::SETTINGS_APPROVAL_ASK),
    ("settings.approval.edits", ids::SETTINGS_APPROVAL_EDITS),
    ("settings.approval.all", ids::SETTINGS_APPROVAL_ALL),
    ("settings.theme", ids::SETTINGS_THEME),
    ("settings.theme.palette", ids::SETTINGS_THEME_PALETTE),
    ("settings.editor", ids::SETTINGS_EDITOR),
    ("settings.hint", ids::SETTINGS_HINT),
    ("exit.saved", ids::EXIT_SAVED),
    ("exit.ephemeral", ids::EXIT_EPHEMERAL),
    ("exit.draftTitle", ids::EXIT_DRAFT_TITLE),
    ("exit.keepEditing", ids::EXIT_KEEP_EDITING),
    ("narrow.rows", ids::NARROW_ROWS),
    ("narrow.cols", ids::NARROW_COLS),
    ("notice.plugin", ids::NOTICE_PLUGIN),
    ("paste.large", ids::PASTE_LARGE),
    ("perf.shed", ids::PERF_SHED),
    ("copy.done", ids::COPY_DONE),
    ("copy.failed", ids::COPY_FAILED),
    ("follow.stopped", ids::FOLLOW_STOPPED),
    ("luna.offer", ids::LUNA_OFFER),
];

/// Formats a duration using the terminal copy-deck format.
#[must_use]
pub fn dur(duration: Duration) -> String {
    let millis = duration.as_millis();
    if duration < Duration::from_secs(1) {
        return format!("{millis} ms");
    }
    if duration < Duration::from_secs(120) {
        return format!("{:.1}s", duration.as_secs_f64());
    }
    let seconds = duration.as_secs();
    format!("{}m{:02}s", seconds / 60, seconds % 60)
}

/// Formats token counts as whole thousands from one thousand tokens onward.
#[must_use]
pub fn tokens(count: u64) -> String {
    if count < 1_000 {
        return count.to_string();
    }
    format!("{}k", count / 1_000)
}

/// Fills a full template and selects plural forms by `plural_count`.
#[must_use]
pub fn render(template: &str, values: &[(&str, &str)], plural_count: u64) -> String {
    let plural = if plural_count == 1 { "" } else { "s" };
    let mut rendered = template.replace("|s", plural).replace('|', "");
    for (name, value) in values {
        rendered = rendered.replace(&format!("{{{name}}}"), value);
    }
    rendered
}

#[cfg(test)]
mod tests {
    use super::{DECK, dur, ids, render, tokens};
    use std::time::Duration;

    #[test]
    fn duration_boundaries_use_three_display_bands() {
        assert_eq!(dur(Duration::from_millis(999)), "999 ms");
        assert_eq!(dur(Duration::from_millis(1_000)), "1.0s");
        assert_eq!(dur(Duration::from_secs(120)), "2m00s");
    }

    #[test]
    fn token_counts_switch_to_thousands_at_one_thousand() {
        assert_eq!(tokens(999), "999");
        assert_eq!(tokens(1_000), "1k");
    }

    #[test]
    fn plural_forms_cover_zero_one_and_many() {
        let template = ids::ASK_PREVIEW_MORE;
        assert_eq!(
            render(template, &[("n", "0")], 0),
            "... 0 more lines · ctrl+o expands"
        );
        assert_eq!(
            render(template, &[("n", "1")], 1),
            "... 1 more line · ctrl+o expands"
        );
        assert_eq!(
            render(template, &[("n", "2")], 2),
            "... 2 more lines · ctrl+o expands"
        );
    }

    #[test]
    fn deck_has_unique_ids_and_no_obsolete_wake_entry() {
        let unique: std::collections::BTreeSet<_> = DECK.iter().map(|(id, _)| *id).collect();
        assert_eq!(unique.len(), DECK.len());
        assert!(!DECK.iter().any(|(id, _)| *id == "jobs.wake"));
    }
}
