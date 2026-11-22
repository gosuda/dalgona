package tui

import (
	"cmp"
	"context"
	"encoding/json/jsontext"
	"time"

	"charm.land/bubbles/v2/spinner"
	tea "charm.land/bubbletea/v2"
	"charm.land/glamour/v2"
	"github.com/charmbracelet/colorprofile"
	"github.com/charmbracelet/x/ansi"

	"github.com/gosuda/dalgona/internal/dal/rpc"
)

const (
	stateIdle             = "idle"
	stateRunning          = "running"
	stateAwaitingApproval = "awaiting approval"
	stateInterrupted      = "interrupted"
	stateFailed           = "failed"
	stateDisconnected     = "disconnected"
)

type hintState struct {
	text    string
	id      int
	pending string
}

type hintExpiredMsg struct {
	id int
}

func (m *model) setHint(text, pending string) tea.Cmd {
	m.hint.id++
	m.hint.text, m.hint.pending = text, pending
	id := m.hint.id
	return tea.Tick(hintWindow, func(time.Time) tea.Msg { return hintExpiredMsg{id: id} })
}

func (m *model) clearHint() {
	m.hint.id++
	m.hint.text, m.hint.pending = "", ""
}

type caller interface {
	Call(context.Context, string, any, any) error
	Notify(string, any) error
}

type model struct {
	conn        connInfo
	thread      threadInfo
	live        liveState
	input       inputState
	screen      screenState
	out         outState
	notice      string
	noticeError bool
	noticeN     int
	hint        hintState
	queue       []followup
	leaving     bool
	quitting    bool
	suspending  bool
	approvals   []approval
	list        *listPanel
	confirm     *confirmation
	login       *loginPanel
	help        *helpPanel
	matchSeq    int
	listSeq     int
	review      *reviewState
	settings    *settingsPanel
	saving      *settingsPanel
	prefs       map[string]string
	pending     *pendingResume
}

const pendingResumeLimit = 4096

type pendingResume struct {
	target string
	seq    int
	failed bool
	held   []notificationMsg
}

type connInfo struct {
	client         caller
	opts           Options
	done           <-chan struct{}
	resumeSlot     chan struct{}
	clock          func() time.Time
	product        string
	version        string
	platformFamily string
	lost           string
}

type threadInfo struct {
	id              string
	cwd             string
	turnID          string
	state           string
	seq             uint64
	completedTurns  map[string]struct{}
	title           string
	modelName       string
	approvalMode    string
	sandbox         string
	turnStartedAt   time.Time
	lastTurn        time.Duration
	effort          string
	turnEffort      *string
	turnRevision    uint64
	usage           rpc.ThreadTokenUsage
	levels          map[string][]string
	inputModalities map[string][]string
}

type liveState struct {
	items      []liveItem
	byID       map[string]int
	deltas     *deltaDecoder
	plan       []rpc.TurnPlanStep
	retryUntil time.Time
	group      []rpc.Item
	groupN     int
	planN      int
	planTurn   string
	patches    map[patchKey][]rpc.FileChange
}

type inputState struct {
	composer    editor
	history     history
	keywords    []keyword
	marks       []keywordSpan
	marksFor    string
	field       textField
	mode        inputMode
	popup       *completion
	fileSeq     int
	fileMissing bool
	shells      int
}

type screenState struct {
	width         int
	height        int
	focused       bool
	method        ansi.Method
	themeSetting  string
	theme         theme
	color         bool
	profile       colorprofile.Profile
	dark          bool
	hasDark       bool
	motion        bool
	spin          spinner.Model
	ticking       bool
	iconsSetting  string
	statusHide    []string
	kittyAsked    bool
	kittyWindow   bool
	kittyNerd     bool
	imgProto      string
	verbosity     string
	env           tea.EnvMsg
	notifications bool
}

type outState struct {
	markdown map[mdKey]*glamour.TermRenderer
	printed  map[string]struct{}
	entries  []entry
	pending  []string
	printing bool
	frozen   tea.View
	geometry frameGeometry
	started  bool
	settling bool
	settleN  int
	anchor   *dockAnchor
	seq      int
	loads    int
	draw     drawFunc
	renders  chan struct{}
	held     []heldCommit
	launch   []tea.Cmd
}

type notificationMsg struct {
	method string
	params jsontext.Value
}

type serverRequestMsg struct {
	method    string
	requestID string
	params    jsontext.Value
	answer    chan requestAnswer
}

type requestAnswer struct {
	value any
	err   error
}

type debounceMsg struct {
	seq int
}

type searchResultMsg struct {
	seq   int
	files []fileHit
	err   error
}

type resultMsg struct {
	op       string
	value    any
	text     string
	verbatim bool
	seq      int
	turnID   string
	err      error
}

type followup struct {
	text     string
	verbatim bool
}

type disconnectedMsg struct {
	err error
}

type shellResultMsg struct {
	n       int
	command string
	result  rpc.CommandExecResponse
	err     error
	elapsed time.Duration
}

func newModel(client caller, opts Options, done <-chan struct{}) model {
	approvalMode := opts.Approval
	sandbox := cmp.Or(opts.Sandbox, "workspace-write")
	if approvalMode == "yolo" {
		sandbox = "off"
	}
	return model{
		conn:    connInfo{client: client, opts: opts, done: done, clock: time.Now, resumeSlot: make(chan struct{}, 1)},
		input:   inputState{composer: newEditor()},
		screen:  screenState{width: 80, focused: true, color: true, profile: colorprofile.ASCII, motion: true, spin: newSpinner(true), notifications: true},
		out:     outState{markdown: make(map[mdKey]*glamour.TermRenderer), printed: make(map[string]struct{}), renders: make(chan struct{}, diagramRenders)},
		live:    liveState{deltas: new(deltaDecoder)},
		thread:  threadInfo{state: stateIdle, title: "new thread", modelName: opts.Model, approvalMode: approvalMode, sandbox: sandbox},
		pending: &pendingResume{},
	}
}
