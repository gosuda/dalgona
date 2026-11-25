package tui

import (
	"cmp"
	"context"
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"slices"
	"time"

	"charm.land/bubbles/v2/spinner"
	tea "charm.land/bubbletea/v2"

	"github.com/gosuda/dalgona/internal/dal/rpc"
)

type Link struct {
	Endpoint   string
	Generation uint64
}

type Machine struct {
	ID    string
	Label string
	Kind  string
	Phase string
}

type MachinePhase struct {
	Link        Link
	Phase       string
	Initialized rpc.InitializeResult
	Err         error
}

type MachineEvents struct {
	Phase   func(MachinePhase)
	Notify  func(Link, string, jsontext.Value)
	Request func(context.Context, Link, string, jsontext.Value) (any, error)
}

type Machines interface {
	Endpoints() []Machine
	Connect(string) error
	Disconnect(string) error
	Call(context.Context, Link, string, any, any) error
	Close() error
}

type MachineOnboarding interface {
	Attach(string) (func(), error)
	OnboardSSH(ctx context.Context, name, target, artifact string) error
	PairDevice(ctx context.Context, name string, card rpc.PairingCard, code, label string) error
}

type OpenMachines func(MachineEvents) (Machines, error)

type machineCaller struct {
	set  Machines
	link Link
}

func (c machineCaller) Call(ctx context.Context, method string, params, result any) error {
	return c.set.Call(ctx, c.link, method, params, result)
}

type machineMsg struct {
	link Link
	body tea.Msg
}

func (l Link) scope(command tea.Cmd) tea.Cmd {
	if l.Endpoint == "" || command == nil {
		return command
	}
	return func() tea.Msg {
		if msg := command(); msg != nil {
			return machineMsg{link: l, body: msg}
		}
		return nil
	}
}

type machineSession struct {
	model     model
	observed  uint64
	automatic bool
	pinned    func()
}

type machineModel struct {
	set        Machines
	opts       Options
	done       <-chan struct{}
	active     *machineSession
	sessions   map[string]*machineSession
	initial    string
	panel      *machinePanel
	management MachineOnboarding
	selection  string
}

func newMachineModel(set Machines, opts Options, done <-chan struct{}) (*machineModel, error) {
	machines := set.Endpoints()
	initial := opts.InitialMachine
	if initial == "" && len(machines) > 0 {
		initial = machines[0].ID
	}
	i := slices.IndexFunc(machines, func(machine Machine) bool { return machine.ID == initial })
	if i == -1 {
		return nil, fmt.Errorf("machine %q is not configured; check --remote and [remotes]", initial)
	}
	management, _ := set.(MachineOnboarding)
	m := &machineModel{set: set, opts: opts, done: done, initial: initial, management: management, sessions: make(map[string]*machineSession)}
	m.active = m.session(machines[i])
	m.active.model.conn.foreground = true
	return m, nil
}

func (m *machineModel) session(machine Machine) *machineSession {
	if existing := m.sessions[machine.ID]; existing != nil {
		return existing
	}
	opts := m.opts
	if machine.ID != m.initial {
		opts.ThreadID, opts.Latest = "", false
		opts.Model, opts.Approval, opts.Sandbox = "", "", ""
		opts.ExplicitCwd = false
	}
	if machine.Kind != "local" && !opts.ExplicitCwd {
		opts.Cwd = ""
	}
	if machine.Kind != "local" {
		opts.OnChildSpawned = nil
	}
	s := &machineSession{model: newModel(nil, opts, m.done), automatic: machine.ID == m.initial}
	s.model.conn.link.Endpoint = machine.ID
	s.model.conn.machinePhase = machine.Phase
	if machine.Kind != "local" {
		s.model.conn.remote = machine.ID
	}
	if m.management != nil {
		s.model.conn.machine = cmp.Or(sanitizeText(machine.Label), sanitizeText(machine.ID))
	}
	s.model.conn.lost = "Machine is not connected; use /machines to connect"
	s.model.thread.state = stateDisconnected
	m.sessions[machine.ID] = s
	return s
}

func (m *machineModel) Init() tea.Cmd {
	return tea.Batch(m.active.model.Init(), m.connectionCmd("connect", m.initial))
}

func (m *machineModel) View() tea.View { return m.active.model.View() }

func (m *machineModel) Update(msg tea.Msg) (tea.Model, tea.Cmd) {
	var command tea.Cmd
	switch msg := msg.(type) {
	case MachinePhase:
		command = m.phaseChanged(msg)
	case machineMsg:
		command = m.applyMachineMsg(msg)
	case machineAction:
		command = m.machineAction(msg)
	case machineActionResult:
		command = m.machineActionDone(msg)
	default:
		command = m.terminalUpdate(msg)
	}
	return m, command
}

func (m *machineModel) terminalUpdate(msg tea.Msg) tea.Cmd {
	if _, printed := msg.(printedMsg); printed && m.selection != "" {
		id := m.selection
		m.selection = ""
		m.active.model.updateFrame(msg)
		if machine, ok := m.machine(id); ok {
			return m.selectMachine(machine)
		}
	}
	if command, handled := m.machineControl(msg); handled {
		return tea.Batch(command, m.refreshDock())
	}
	return m.updateActive(msg)
}

func (m *machineModel) updateActive(msg tea.Msg) tea.Cmd {
	next, command := m.active.model.update(msg)
	m.active.model = next
	m.syncPin(m.active)
	m.updateMachineRows()
	return command
}

func (m *machineModel) refreshDock() tea.Cmd {
	return m.updateActive(machineRefresh{})
}

type machineRefresh struct{}

func (m *machineModel) applyMachineMsg(msg machineMsg) tea.Cmd {
	s := m.sessions[msg.link.Endpoint]
	_, settledDiagram := msg.body.(diagramMsg)
	if s == nil || (!settledDiagram && s.model.conn.link != msg.link) {
		if request, ok := msg.body.(serverRequestMsg); ok {
			request.answer <- requestAnswer{err: errors.New("the machine connection has ended")}
		}
		return nil
	}
	if s == m.active {
		return m.updateActive(msg.body)
	}
	if _, ticking := msg.body.(spinner.TickMsg); ticking {
		s.model.screen.ticking = false
		return nil
	}
	copyTerminal(&s.model, &m.active.model)
	command := s.model.reduce(msg.body)
	m.syncPin(s)
	m.updateMachineRows()
	return tea.Batch(command, s.model.takeDiagramCommands())
}

func (m *machineModel) syncPin(s *machineSession) {
	if m.management == nil {
		return
	}
	busy := m.pinSessionBusy(s)
	if s.pinned == nil && busy && s.model.conn.client != nil {
		s.pinned, _ = m.management.Attach(s.model.conn.link.Endpoint)
	}
	if s.pinned != nil && (!busy || s.model.conn.client == nil) {
		s.pinned()
		s.pinned = nil
	}
}

func (m *machineModel) pinSessionBusy(s *machineSession) bool {
	return s == m.active || s.model.turnLive() || len(s.model.approvals) > 0
}

func (m *machineModel) phaseChanged(change MachinePhase) tea.Cmd {
	machine, found := m.machine(change.Link.Endpoint)
	if !found {
		return nil
	}
	s := m.session(machine)
	if s.observed > change.Link.Generation || (s.model.conn.link == change.Link && change.Phase == "connected") {
		return nil
	}
	s.observed, s.model.conn.machinePhase = change.Link.Generation, change.Phase
	copyTerminal(&s.model, &m.active.model)
	var start tea.Cmd
	if change.Phase == "connected" {
		start = m.machineConnected(s, change)
	} else {
		m.machineUnavailable(s, change.Err)
	}
	m.syncPin(s)
	m.updateMachineRows()
	return tea.Batch(start, m.refreshDock())
}

func (m *machineModel) machineConnected(s *machineSession, change MachinePhase) tea.Cmd {
	view := &s.model
	view.conn.link = change.Link
	view.conn.client = machineCaller{set: m.set, link: change.Link}
	view.conn.lost = ""
	view.applyResult(resultMsg{op: "initialize", value: &change.Initialized})
	view.thread.state = stateIdle
	view.emitStatus()
	var thread tea.Cmd
	switch {
	case view.thread.id != "":
		thread = view.resumeThread(view.thread.id)
	case s.automatic:
		s.automatic = false
		thread = view.startThread()
	default:
		view.noticef("Use /resume to open a thread or /new to start one")
	}
	return tea.Sequence(view.readSettings(), view.loadKeywords(), view.loadLevels(), thread)
}

func (m *machineModel) machineUnavailable(s *machineSession, cause error) {
	view := &s.model
	wasConnected := view.conn.client != nil
	view.conn.client = nil
	view.conn.link.Generation = 0
	view.conn.threadOpen = false
	if m.management == nil && cause != nil {
		if wasConnected {
			view.loseConnection(cause)
		} else {
			view.applyResult(resultMsg{op: "initialize", err: cause})
		}
		return
	}
	if wasConnected {
		view.expireMachineLink()
	}
	view.setState(stateDisconnected)
	view.conn.lost = s.phaseMessage(cause)
	view.noticef("%s", view.conn.lost)
	if phase := view.conn.machinePhase; !view.out.started && (cause != nil || phase == "offline") {
		view.commit("machine:"+phase, []string{view.conn.lost})
	}
}

func (s *machineSession) phaseMessage(cause error) string {
	name, phase := s.model.conn.machine, s.model.conn.machinePhase
	if name == "" {
		if cause == nil {
			return "Local backend: " + phase
		}
		return "Could not connect to the local backend: " + sanitizeText(cause.Error())
	}
	detail := fmt.Sprintf("Machine %s: %s", name, phase)
	if cause != nil {
		detail += ": " + sanitizeText(cause.Error())
	}
	switch phase {
	case "idle", "offline", "failed", "incompatible":
		return detail + "; use /machines to connect"
	}
	return detail
}

func (m *model) expireMachineLink() {
	m.abandonApprovals(errors.New("the machine connection has ended"))
	m.live.retryUntil = time.Time{}
	m.thread.turnStartedAt = time.Time{}
	m.thread.turnSent = false
	m.resetPendingResume("")
	m.closeList()
	m.screen.ticking = false
	m.confirm, m.login, m.saving = nil, nil, nil
	m.input.popup = nil
	m.clearHint()
}

func (m *machineModel) machine(id string) (Machine, bool) {
	machines := m.set.Endpoints()
	i := slices.IndexFunc(machines, func(machine Machine) bool { return machine.ID == id })
	if i == -1 {
		return Machine{}, false
	}
	return machines[i], true
}

func copyTerminal(to, from *model) {
	to.screen.width, to.screen.height = from.screen.width, from.screen.height
	to.screen.focused, to.input.composer.focused = from.screen.focused, from.screen.focused
	to.screen.method, to.screen.profile = from.screen.method, from.screen.profile
	to.screen.dark, to.screen.hasDark = from.screen.dark, from.screen.hasDark
	to.screen.env, to.screen.kittyWindow = from.screen.env, from.screen.kittyWindow
	to.screen.kittyAsked, to.screen.kittyNerd = from.screen.kittyAsked, from.screen.kittyNerd
	to.screen.imgProto = from.screen.imgProto
	if to.screen.motion != from.screen.motion {
		to.screen.motion = from.screen.motion
		to.screen.spin, to.screen.ticking = newSpinner(from.screen.motion), false
	}
	to.input.composer.SetWidth(to.screen.width)
	to.screen.setTheme()
}

func (m *machineModel) selectMachine(machine Machine) tea.Cmd {
	to := m.session(machine)
	if to == m.active {
		return m.refreshDock()
	}
	from := &m.active.model
	if m.active.model.out.printing {
		m.selection = machine.ID
		return nil
	}
	view := &to.model
	copyTerminal(view, from)
	view.out.anchor, from.out.anchor = from.out.anchor, nil
	view.out.started, view.out.geometry = from.out.started, from.out.geometry
	view.out.printing, view.out.frozen = false, tea.View{}
	view.out.settling, view.out.settleN = from.out.settling, from.out.settleN
	from.conn.foreground, view.conn.foreground = false, true
	previous := m.active
	m.active = to
	m.syncPin(previous)
	view.out.pending = append([]string{"── " + view.conn.machine + " ──"}, view.out.pending...)
	view.emitStatus()
	return m.refreshDock()
}
