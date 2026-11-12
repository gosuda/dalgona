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

	"github.com/gosuda/dalgona/internal/rpc"
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
	queue       []string
	leaving     bool
	quitting    bool
	suspending  bool
	approvals   []approval
	selector    *selector
	confirm     *confirmation
	matchSeq    int
	review      *reviewState
}

type connInfo struct {
	client         caller
	opts           Options
	done           <-chan struct{}
	clock          func() time.Time
	product        string
	version        string
	platformFamily string
}

type threadInfo struct {
	id             string
	turnID         string
	state          string
	seq            uint64
	completedTurns map[string]struct{}
	title          string
	modelName      string
	approvalMode   string
	sandbox        string
	turnStartedAt  time.Time
	lastTurn       time.Duration
	effort         string
	usage          rpc.ThreadTokenUsage
	levels         map[string][]string
}

type liveState struct {
	items      []liveItem
	deltas     *deltaDecoder
	plan       []rpc.TurnPlanStep
	retryUntil time.Time
	group      []rpc.Item
	groupN     int
	planN      int
	planTurn   string
}

type inputState struct {
	composer editor
	keywords []Keyword
	marks    []keywordSpan
	marksFor string
}

type screenState struct {
	width        int
	height       int
	focused      bool
	method       ansi.Method
	themeSetting string
	theme        theme
	color        bool
	profile      colorprofile.Profile
	dark         bool
	hasDark      bool
	motion       bool
	spin         spinner.Model
	ticking      bool
	iconsSetting string
	statusHide   []string
	kittyAsked   bool
	kittyWindow  bool
	kittyNerd    bool
	verbosity    string
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
	seq      int
	loads    int
}

type notificationMsg struct {
	method string
	params jsontext.Value
}

type serverRequestMsg struct {
	method string
	params jsontext.Value
	answer chan requestAnswer
}

type requestAnswer struct {
	value any
	err   error
}

type resultMsg struct {
	op     string
	value  any
	text   string
	seq    int
	turnID string
	err    error
}

type disconnectedMsg struct {
	err error
}

func newModel(client caller, opts Options, done <-chan struct{}) model {
	approvalMode := cmp.Or(opts.Approval, "auto")
	sandbox := cmp.Or(opts.Sandbox, "workspace-write")
	if approvalMode == "yolo" {
		sandbox = "off"
	}
	return model{
		conn:   connInfo{client: client, opts: opts, done: done, clock: time.Now},
		input:  inputState{composer: newEditor()},
		screen: screenState{width: 80, focused: true, color: true, profile: colorprofile.ASCII, motion: true, spin: newSpinner(true)},
		out:    outState{markdown: make(map[mdKey]*glamour.TermRenderer), printed: make(map[string]struct{})},
		live:   liveState{deltas: new(deltaDecoder)},
		thread: threadInfo{state: stateIdle, title: "new thread", modelName: opts.Model, approvalMode: approvalMode, sandbox: sandbox},
	}
}
