package tui

import (
	"errors"
	"fmt"
	"time"

	"charm.land/bubbles/v2/spinner"
	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/x/ansi"

	"github.com/gosuda/dalgona/internal/dal/textinput"
)

func (m model) Init() tea.Cmd {
	start := tea.Sequence(m.initialize(), m.readSettings(), m.loadKeywords(), m.loadLevels(), m.startThread())
	return tea.Batch(m.input.composer.Focus(), tea.RequestBackgroundColor, start)
}

func (m *model) handleMsg(msg tea.Msg) tea.Cmd {
	switch msg := msg.(type) {
	case tea.KeyPressMsg:
		return m.updateKey(msg)
	case notificationMsg, serverRequestMsg, resultMsg, disconnectedMsg, debounceMsg, searchResultMsg, shellResultMsg, imageMsg:
		return m.updateAsync(msg)
	case spinner.TickMsg:
		return m.updateTick(msg)
	case keywordTickMsg:
		return m.sendKeywordMatch(msg)
	case hintExpiredMsg:
		m.expireHint(msg)
		return nil
	case diagramMsg:
		m.applyDiagram(msg)
		return nil
	default:
		if command, handled := m.updateTerminal(msg); handled {
			return command
		}
		return m.updateComposer(msg)
	}
}

func (m model) Update(msg tea.Msg) (tea.Model, tea.Cmd) {
	m.anchorState().revision++
	before := m.input.composer.Value()
	notice := m.notice
	command := m.handleMsg(msg)
	m.syncHistory()
	command = tea.Batch(command, m.syncResumeSearch())
	if m.input.composer.Value() != before {
		command = tea.Batch(command, m.matchKeywords())
	}
	if m.notice != notice {
		m.noticePrinted = false
	}
	if !m.noticePrinted {
		m.commitOverflowNotice()
	}
	settle := m.settle()
	printCommand := m.drain()
	tick := m.scheduleTick()
	anchor := m.anchorDock()
	quit := m.finishQuit()
	suspend := m.finishSuspend()
	diagrams := m.takeDiagramCommands()
	return m, tea.Batch(command, tick, settle, printCommand, anchor, quit, suspend, diagrams)
}

func (m *model) wheel(msg tea.MouseWheelMsg) {
	switch {
	case m.review != nil:
		m.scrollReview(msg)
	case m.list != nil:
		m.wheelList(msg)
	}
}

func (m *model) updateTerminal(msg tea.Msg) (tea.Cmd, bool) {
	switch msg := msg.(type) {
	case tea.WindowSizeMsg:
		m.resizeTerminal(msg)
	case tea.CursorPositionMsg:
		return m.answerDock(msg), true
	case dockLandedMsg:
		return m.caretLanded(msg), true
	case dockQueryTimeoutMsg, dockMoveTimeoutMsg, dockHoldMsg, dockWrittenMsg:
		m.updateDockEvent(msg)
	case tea.FocusMsg, tea.BlurMsg:
		m.applyFocus(msg)
	case tea.MouseWheelMsg:
		m.wheel(msg)
	case tea.ModeReportMsg:
		m.applyModeReport(msg)
	default:
		return m.updateTerminalTail(msg)
	}
	return nil, true
}

func (m *model) resizeTerminal(msg tea.WindowSizeMsg) {
	m.screen.width = msg.Width
	m.screen.height = msg.Height
	m.input.composer.SetWidth(msg.Width)
	m.fitReview()
	if p := m.settings; p != nil && p.saveErr != "" && !p.saveErrPrinted {
		p.saveErrPrinted = m.commitOverflow(p.saveErr, p.saveErr)
	}
	if !m.noticePrinted {
		m.commitOverflowNotice()
	}
	if m.out.printing {
		m.out.frozen = m.presentedDock()
	}
}

func (m *model) updateDockEvent(msg tea.Msg) {
	switch msg := msg.(type) {
	case dockQueryTimeoutMsg:
		m.queryExpired(msg)
	case dockMoveTimeoutMsg:
		m.moveExpired(msg)
	case dockHoldMsg:
		m.holdExpired(msg)
	case dockWrittenMsg:
		anchor := m.anchorState()
		if anchor.moveRevision == msg.revision {
			anchor.moving = false
		}
	}
}

func (m *model) applyFocus(msg tea.Msg) {
	_, blurred := msg.(tea.BlurMsg)
	m.screen.focused = !blurred
	m.input.composer.focused = !blurred
}

func (m *model) updateTerminalTail(msg tea.Msg) (tea.Cmd, bool) {
	if m.updateFrame(msg) {
		return nil, true
	}
	return m.updateColour(msg)
}

func (m *model) updateFrame(msg tea.Msg) bool {
	switch msg := msg.(type) {
	case printedMsg:
		m.out.printing = false
		m.out.started = true
	case settledMsg:
		if msg.n == m.out.settleN {
			m.out.settling = false
		}
	case tea.ResumeMsg:
		m.leaving = false
	default:
		return false
	}
	return true
}

func (m *model) updateAsync(msg tea.Msg) tea.Cmd {
	switch msg := msg.(type) {
	case debounceMsg:
		return m.applyDebounce(msg)
	case searchResultMsg:
		m.applySearch(msg)
		return nil
	case notificationMsg:
		return m.applyNotification(msg)
	case serverRequestMsg:
		return m.applyRequest(msg)
	case resultMsg:
		return m.applyResultMessage(msg)
	case shellResultMsg:
		m.applyShell(msg)
	case disconnectedMsg:
		m.loseConnection(msg.err)
	case imageMsg:
		m.applyImage(msg)
	}
	return nil
}

func (m *model) applyResultMessage(msg resultMsg) tea.Cmd {
	switch msg.op {
	case "resumeSearch":
		m.applyResumeSearch(msg)
		return nil
	case "keywordMatch":
		m.applyKeywordMatch(msg)
		return nil
	}
	return m.applyResult(msg)
}

func (m *model) decisionKey(msg tea.KeyPressMsg) tea.Cmd {
	if m.approvals[0].card != nil {
		return m.answerQuestionKey(msg)
	}
	return m.answerApprovalKey(msg.String())
}

func (m *model) overlayKey(msg tea.KeyPressMsg) (tea.Cmd, bool) {
	key := msg.String()
	switch {
	case m.review != nil:
		m.reviewKey(msg)
		return nil, true
	case key == "ctrl+o":
		m.openReview()
		return nil, true
	case len(m.approvals) > 0:
		return m.decisionKey(msg), true
	case m.confirm != nil:
		return m.confirmKey(key), true
	case m.list != nil:
		return m.listKey(msg), true
	case m.settings != nil:
		return m.settingsKey(msg), true
	case m.login != nil:
		return m.loginKey(msg), true
	case m.help != nil:
		m.helpKey(key)
		return nil, true
	}
	return nil, false
}

func (m *model) composerKey(msg tea.KeyPressMsg, key string) tea.Cmd {
	switch key {
	case "up":
		if m.recallOlder() {
			return nil
		}
	case "down":
		if m.recallNewer() {
			return nil
		}
	}
	return m.updateComposer(msg)
}

func (m *model) keyCommand(msg tea.KeyPressMsg) tea.Cmd {
	m.noticef("")
	key := msg.String()
	if command, handled := m.guardKey(key); handled {
		return command
	}
	if command, handled := m.overlayKey(msg); handled {
		return command
	}
	if m.followupKey(msg) {
		return nil
	}
	if command, handled := m.shellKey(key, msg); handled {
		return command
	}
	if command, handled := m.popupKey(msg); handled {
		return command
	}
	return m.promptKey(msg, key)
}

func (m *model) promptKey(msg tea.KeyPressMsg, key string) tea.Cmd {
	switch key {
	case "enter":
		return m.enterPrompt()
	case "alt+enter":
		return m.queueFollowup()
	case "ctrl+v":
		return m.clipboardImage()
	case "ctrl+r":
		m.openHistorySearch()
		return nil
	case "?":
		return m.questionKey(msg, key)
	case "esc":
		return m.escape()
	default:
		return m.composerKey(msg, key)
	}
}

func (m *model) updateKey(msg tea.KeyPressMsg) tea.Cmd {
	command := m.keyCommand(msg)
	return tea.Batch(command, m.syncPopup(false))
}

func (m *model) followupKey(msg tea.KeyPressMsg) bool {
	switch msg.String() {
	case "alt+q":
		m.openQueue()
		return true
	case "alt+up":
		if m.input.composer.Value() == "" && len(m.queue) > 0 {
			m.dequeueFollowup()
			return true
		}
	}
	return false
}

func (m *model) guardKey(key string) (tea.Cmd, bool) {
	switch key {
	case "ctrl+c":
		return m.exitKey(), true
	case "ctrl+z":
		m.suspend()
		return nil, true
	}
	if key != "esc" {
		m.clearGuard()
	}
	return nil, false
}

func (m *model) exitKey() tea.Cmd {
	if m.hint.pending == "exit" {
		m.quit()
		return nil
	}
	draft := m.input.composer.Expanded()
	if textinput.Blank(draft) {
		return m.setHint("Ctrl+C again to exit", "exit")
	}
	m.input.history.saveDraft(m.input.composer, draft)
	m.input.history.rewind()
	hint := "Draft saved to history · Ctrl+C again to exit"
	if len(m.input.composer.images()) > 0 {
		hint = "Draft saved to history, images dropped · Ctrl+C again to exit"
	}
	m.input.composer.SetValue("")
	return m.setHint(hint, "exit")
}

func (m *model) recallOlder() bool {
	if !m.input.composer.onFirstRow(&m.screen) {
		return false
	}
	return m.input.history.older(&m.input.composer)
}

func (m *model) recallNewer() bool {
	if !m.input.composer.onLastRow(&m.screen) {
		return false
	}
	return m.input.history.newer(&m.input.composer)
}

func (m *model) escape() tea.Cmd {
	if m.thread.state == stateDisconnected {
		m.errorf("%s", m.conn.lost)
		return nil
	}
	if !m.turnLive() {
		if m.thread.state == stateIdle {
			return m.idleEscape()
		}
		return m.interrupt()
	}
	if _, waiting := m.retryRemaining(); waiting || m.hint.pending == "interrupt" {
		m.clearHint()
		return m.interrupt()
	}
	return m.setHint("Esc again to interrupt", "interrupt")
}

func (m *model) loseConnection(cause error) {
	m.setState(stateDisconnected)
	m.conn.lost = fmt.Sprintf("connection lost: %v; restart the TUI, then %s", cause, m.resumeHint())
	m.live.retryUntil = time.Time{}
	m.clearHint()
	m.abandonApprovals(fmt.Errorf("connection lost: %w", cause))
	m.errorf("%s", m.conn.lost)
}

func (m *model) idleEscape() tea.Cmd {
	if !textinput.Blank(m.input.composer.Value()) {
		m.quietNotice("idle, draft kept")
		return nil
	}
	if m.thread.id == "" {
		return nil
	}
	if m.hint.pending == "rewind" {
		m.clearHint()
		return m.rewindCommand()
	}
	return m.setHint("Esc again to rewind", "rewind")
}

func (m *model) clearGuard() {
	if m.hint.pending != "" {
		m.clearHint()
	}
}

func (m *model) quit() {
	m.closeReview()
	m.leaving = true
	m.quitting = true
	m.commit("exit", []string{m.exitLine()})
}

func (m *model) exitLine() string {
	if m.thread.id == "" {
		return "No thread was started"
	}
	return fmt.Sprintf("Thread %s saved · resume with %s --resume %s", sanitizeText(m.thread.title), m.conn.product, sanitizeText(m.thread.id))
}

func (m *model) dockInFlight() bool {
	anchor := m.anchorState()
	return m.screen.height > 0 && (anchor.moving || anchor.querying)
}

func (m *model) finishQuit() tea.Cmd {
	if !m.quitting || m.dockInFlight() {
		return nil
	}
	if m.screen.height == 0 || (!m.out.printing && !m.out.settling && len(m.out.pending) == 0 && len(m.out.held) == 0) {
		return tea.Quit
	}
	return nil
}

func (m *model) suspend() {
	m.closeReview()
	m.leaving = true
	m.suspending = true
}

func (m *model) finishSuspend() tea.Cmd {
	if !m.suspending || m.dockInFlight() {
		return nil
	}
	if m.screen.height == 0 || (!m.out.printing && len(m.out.pending) == 0 && len(m.out.held) == 0) {
		m.suspending = false
		return tea.Suspend
	}
	return nil
}

func (m *model) updateComposer(msg tea.Msg) tea.Cmd {
	var imageCmd tea.Cmd
	if paste, ok := msg.(tea.PasteMsg); ok {
		if len(paste.Content) > pasteLimit {
			m.noticef("Paste is %d bytes, over the 1 MiB limit; save it to a file and mention it with @path", len(paste.Content))
			return nil
		}
		if m.pasteField(paste.Content) {
			return nil
		}
		if m.textMode() == inputPrompt {
			imageCmd = m.imagePasteCmd(paste.Content, m.input.composer.cursor)
		}
	}
	updated, command := m.input.composer.Update(&m.screen, msg)
	m.input.composer = updated
	return tea.Batch(command, m.syncPopup(typedText(msg)), imageCmd)
}

func (m *model) pasteField(text string) bool {
	switch m.textMode() {
	case inputFilter:
		if m.typeIntoField(text) {
			m.list.cursor, m.list.scroll = 0, 0
		}
	case inputTarget, inputAnswer, inputAPIKey:
		m.typeIntoField(text)
	case inputPrompt, inputShell:
		if err := (textinput.Field{Name: "paste"}).Check(text); err != nil {
			m.refuseInsert(fieldNames[m.textMode()], err)
			return true
		}
		return false
	}
	return true
}

var fieldNames = map[inputMode]string{
	inputPrompt: "Prompt", inputShell: "Shell command", inputFilter: "Filter",
	inputTarget: "Review target", inputAnswer: "Answer", inputAPIKey: "API key",
}

func (m *model) typeIntoField(text string) bool {
	if err := m.input.field.insert(text); err != nil {
		m.refuseInsert(fieldNames[m.textMode()], err)
		return false
	}
	return true
}

func (m *model) refuseInsert(name string, err error) {
	textErr, _ := errors.AsType[*textinput.Error](err)
	switch textErr.Reason {
	case textinput.LineBreakNotAllowed:
		m.noticef("%s takes one line; nothing was inserted", name)
	case textinput.TextTooLarge:
		m.noticef("%s holds at most %d characters; nothing was inserted", name, m.input.field.limit)
	case textinput.DisallowedControl:
		m.noticef("%s cannot hold NUL; nothing was inserted", name)
	case textinput.InvalidEncoding, textinput.InvalidOffset:
		m.noticef("%s needs valid UTF-8; nothing was inserted", name)
	}
}

func (m model) View() tea.View {
	view := m.frame()
	view.ReportFocus = true
	view.WindowTitle = m.windowTitle()
	return view
}

func (m model) rawFrame() tea.View {
	switch {
	case m.out.printing:
		return m.out.frozen
	case m.leaving:
		view := tea.NewView(" ")
		view.ProgramStatus = &tea.ProgramStatus{State: tea.ProgramStateIdle}
		return view
	case !m.out.started:
		view := tea.NewView("")
		view.ReportFocus = true
		return view
	case m.review != nil:
		view := tea.NewView(m.screen.emit(m.reviewView()))
		view.AltScreen = true
		view.ReportFocus = true
		return view
	default:
		view := m.dockView()
		if m.caretLanding() {
			view.Cursor.X, view.Cursor.Y = 0, 0
		}
		view.Content = m.screen.emit(view.Content) + m.markOverlay()
		return view
	}
}

func (m model) presentedDock() tea.View {
	view := m.dockView()
	view.Content = m.screen.emit(view.Content)
	view.MouseMode = tea.MouseModeNone
	if !m.leaving && (m.review != nil || m.list != nil) {
		view.MouseMode = tea.MouseModeCellMotion
	}
	view.ReportFocus = true
	view.WindowTitle = m.windowTitle()
	return view
}

func (m model) frame() tea.View {
	view := m.rawFrame()
	view.MouseMode = tea.MouseModeNone
	if !m.leaving && (m.review != nil || m.list != nil) {
		view.MouseMode = tea.MouseModeCellMotion
	}
	return view
}

func (m *model) noticef(format string, args ...any) {
	m.notice = sanitizeText(fmt.Sprintf(format, args...))
	m.noticeError = false
}

func (m *model) errorf(format string, args ...any) {
	m.noticef(format, args...)
	m.noticeError = true
}

func (m *model) commitOverflowNotice() {
	if m.notice == "" || !m.noticeShown() {
		return
	}
	m.noticePrinted = m.commitOverflow(m.notice, m.noticeLine())
}

func (m *model) commitOverflow(text, line string) bool {
	if m.screen.cells(line) <= m.liveFreeWidth() {
		return false
	}
	m.commit(fmt.Sprintf("note:%d", m.noticeN), m.screen.wrap(text, terminalWidth(m.screen.width)))
	m.noticeN++
	return true
}

func (m *model) applyModeReport(msg tea.ModeReportMsg) {
	if msg.Mode != ansi.ModeUnicodeCore {
		return
	}
	if msg.Value == ansi.ModeReset || msg.Value == ansi.ModeSet || msg.Value == ansi.ModePermanentlySet {
		m.screen.method = ansi.GraphemeWidth
	}
}
