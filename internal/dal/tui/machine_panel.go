package tui

import (
	"cmp"
	"context"
	"fmt"
	"slices"
	"strings"
	"time"

	tea "charm.land/bubbletea/v2"

	"github.com/gosuda/dalgona/internal/dal/rpc"
)

const machineUsage = "/machines [status | select NAME | connect [NAME] | disconnect [NAME] | add ssh | add pair]"

const onboardingTimeout = 3 * time.Minute

var (
	sshFields     = [...]string{"Machine name", "SSH destination", "Local backend artifact (optional)"}
	pairingFields = [...]string{"Machine name", "HTTPS origin", "Machine ID", "TLS public-key pin", "Pairing code", "Device label"}
)

type machineAction struct {
	verb string
	id   string
}

type machineActionResult struct {
	action machineAction
	err    error
}

type machinePanel struct {
	machines  []Machine
	rows      []string
	cursor    int
	form      *machineForm
	working   bool
	stopSetup context.CancelFunc
	err       string
}

type machineForm struct {
	kind   string
	step   int
	values [len(pairingFields)]string
	field  textField
}

func parseMachineAction(argument string) machineAction {
	verb, id, _ := strings.Cut(strings.TrimSpace(argument), " ")
	return machineAction{verb: verb, id: strings.TrimSpace(id)}
}

func (m *machineModel) machineAction(action machineAction) tea.Cmd {
	if m.management == nil {
		m.active.model.noticef("Dal runs locally; use dalgona to manage machines")
		return m.refreshDock()
	}
	if m.machinePanelBusy() {
		return nil
	}
	switch action.verb {
	case "", "status":
		return m.openMachinePanel()
	case "add":
		if isMachineFormTarget(action.id) {
			return m.openMachineForm(action.id)
		}
	case "select", "connect", "disconnect":
		return m.targetMachine(action)
	}
	m.active.model.errorf("Usage: %s", machineUsage)
	return m.refreshDock()
}

func (m *machineModel) machinePanelBusy() bool {
	return m.panel != nil && m.panel.working
}

func isMachineFormTarget(id string) bool {
	return id == "" || id == "ssh" || id == "pair"
}

func (m *machineModel) targetMachine(action machineAction) tea.Cmd {
	machine, ok := m.machine(action.id)
	if !ok {
		m.active.model.errorf("Machine %q is not configured; use /machines to browse", sanitizeText(action.id))
		return m.refreshDock()
	}
	if action.verb == "select" {
		m.closeMachinePanel()
		return m.selectMachine(machine)
	}
	return m.connectionCmd(action.verb, machine.ID)
}

func (m *machineModel) connectionCmd(verb, id string) tea.Cmd {
	set := m.set
	return func() tea.Msg {
		var err error
		if verb == "disconnect" {
			err = set.Disconnect(id)
		} else {
			err = set.Connect(id)
		}
		return machineActionResult{action: machineAction{verb: verb, id: id}, err: err}
	}
}

func (m *machineModel) machineActionDone(result machineActionResult) tea.Cmd {
	if result.action.verb == "add" {
		return m.machineAdded(result)
	}
	if result.err != nil {
		m.reportMachineActionError(result)
	}
	return m.refreshMachinePanel()
}

func (m *machineModel) reportMachineActionError(result machineActionResult) {
	machine, ok := m.machine(result.action.id)
	if !ok {
		return
	}
	detail := fmt.Sprintf("Could not %s %s: %s", result.action.verb, sanitizeText(machine.Label), sanitizeText(result.err.Error()))
	if m.panel != nil {
		m.panel.err = detail
	}
	s := m.session(machine)
	s.model.errorf("%s", detail)
	if s == m.active && !s.model.out.started {
		s.model.commit("machine-open-error", []string{detail})
	}
}

func (m *machineModel) openMachinePanel() tea.Cmd {
	if m.panel == nil {
		m.panel = &machinePanel{}
		m.active.model.machines = m.panel
		m.updateMachineRows()
		m.panel.selectRow(m.active.model.conn.link.Endpoint)
		m.active.model.out.started = true
	}
	return m.refreshDock()
}

func (m *machineModel) closeMachinePanel() {
	if m.panel != nil && m.panel.form != nil {
		clear(m.panel.form.field.text)
		clear(m.panel.form.values[:])
	}
	m.panel, m.active.model.machines = nil, nil
}

func (m *machineModel) refreshMachinePanel() tea.Cmd {
	m.updateMachineRows()
	return m.refreshDock()
}

func (m *machineModel) updateMachineRows() {
	if m.panel == nil {
		return
	}
	p := m.panel
	p.machines = m.set.Endpoints()
	p.cursor = min(p.cursor, max(0, len(p.machines)-1))
	p.rows = p.rows[:0]
	for _, machine := range p.machines {
		phase := machine.Phase
		if session := m.sessions[machine.ID]; session != nil && len(session.model.approvals) > 0 {
			phase = fmt.Sprintf("%s · %d decisions", phase, len(session.model.approvals))
		}
		if machine.ID == m.active.model.conn.link.Endpoint {
			phase += " · selected"
		}
		p.rows = append(p.rows, sanitizeText(machine.Label)+" · "+phase+" · "+machine.Kind)
	}
}

func (m *machineModel) machineControl(msg tea.Msg) (tea.Cmd, bool) {
	if m.panel == nil {
		key, pressed := msg.(tea.KeyPressMsg)
		if pressed && key.String() == "ctrl+g" && m.management != nil && !m.active.model.quitting {
			return m.openMachinePanel(), true
		}
		return nil, false
	}
	return m.handleMachinePanel(msg)
}

func (m *machineModel) handleMachinePanel(msg tea.Msg) (tea.Cmd, bool) {
	switch msg := msg.(type) {
	case tea.KeyPressMsg:
		return m.machineKey(msg), true
	case tea.PasteMsg:
		m.panel.paste(msg.Content)
		return nil, true
	case terminalPasteRefused:
		m.panel.err = "Paste is too large for a machine field"
		return nil, true
	}
	return nil, false
}

func (p *machinePanel) paste(text string) {
	if form := p.form; form != nil && form.step < len(form.fields()) && !p.working {
		p.insert(text)
	}
}

func (m *machineModel) machineKey(msg tea.KeyPressMsg) tea.Cmd {
	p := m.panel
	key := msg.String()
	if p.working {
		return m.setupKey(key)
	}
	if key == "esc" {
		return m.cancelMachinePanel()
	}
	if key == "ctrl+c" || key == "ctrl+g" {
		m.closeMachinePanel()
		return nil
	}
	if p.form != nil {
		return m.machineFormKey(msg)
	}
	return m.machineBrowseKey(key)
}

func (m *machineModel) setupKey(key string) tea.Cmd {
	switch key {
	case "ctrl+c":
		return m.active.model.exitKey()
	case "esc":
		if stop := m.panel.stopSetup; stop != nil {
			stop()
			m.panel.stopSetup = nil
			m.panel.err = "Canceling setup…"
		}
	}
	return nil
}

func (m *machineModel) cancelMachinePanel() tea.Cmd {
	p := m.panel
	if p.form != nil {
		clear(p.form.field.text)
		clear(p.form.values[:])
		p.form, p.err = nil, ""
	} else {
		m.closeMachinePanel()
	}
	return nil
}

func (m *machineModel) machineBrowseKey(key string) tea.Cmd {
	p := m.panel
	switch key {
	case "up", "ctrl+p":
		p.cursor = max(0, p.cursor-1)
	case "down", "ctrl+n":
		p.cursor = min(max(0, len(p.machines)-1), p.cursor+1)
	case "a":
		return m.openMachineForm("ssh")
	case "p":
		return m.openMachineForm("pair")
	case "enter", "c", "d":
		if p.cursor >= len(p.machines) {
			return nil
		}
		verb := machineBrowseVerb[key]
		return m.targetMachine(machineAction{verb: verb, id: p.machines[p.cursor].ID})
	}
	return nil
}

var machineBrowseVerb = map[string]string{"enter": "select", "c": "connect", "d": "disconnect"}

func (m *machineModel) openMachineForm(kind string) tea.Cmd {
	if kind == "" {
		return m.openMachinePanel()
	}
	if m.panel == nil {
		m.panel = &machinePanel{}
		m.active.model.machines = m.panel
		m.active.model.out.started = true
	}
	m.panel.form, m.panel.err = &machineForm{kind: kind}, ""
	return m.refreshDock()
}

func (f *machineForm) fields() []string {
	if f.kind == "pair" {
		return pairingFields[:]
	}
	return sshFields[:]
}

func (f *machineForm) secret() bool { return f.kind == "pair" && f.step == 4 }

func (p *machinePanel) insert(text string) {
	if refusal := p.form.field.pasteRefusal(text); refusal != "" {
		p.err = "Enter one valid line for this field"
		return
	}
	p.err = ""
	if err := p.form.field.insert(text); err != nil {
		p.err = "Enter one valid line for this field"
	}
}

func (m *machineModel) machineFormKey(msg tea.KeyPressMsg) tea.Cmd {
	p, key := m.panel, msg.String()
	f := p.form
	if key == "enter" {
		return m.acceptMachineField()
	}
	if f.step < len(f.fields()) {
		if msg.Text != "" {
			p.insert(msg.Text)
		} else {
			f.field.edit(key)
		}
	}
	return nil
}

func (m *machineModel) acceptMachineField() tea.Cmd {
	p := m.panel
	f := p.form
	if f.step < len(f.fields()) {
		text := strings.TrimSpace(f.field.value())
		if text == "" && (f.kind != "ssh" || f.step != 2) {
			p.err = "Enter " + strings.ToLower(f.fields()[f.step])
			return nil
		}
		f.values[f.step], f.step = text, f.step+1
		clear(f.field.text)
		f.field, p.err = textField{}, ""
		return nil
	}
	if !f.consentVisible(&m.active.model.screen) {
		return nil
	}
	kind, values := f.kind, f.values
	clear(f.values[:])
	ctx, cancel := boundedContext(m.done, onboardingTimeout)
	p.working, p.stopSetup, p.err = true, cancel, ""
	return m.addMachine(ctx, cancel, kind, values)
}

func (m *machineModel) addMachine(ctx context.Context, cancel context.CancelFunc, kind string, values [len(pairingFields)]string) tea.Cmd {
	manage := m.management
	return func() tea.Msg {
		defer cancel()
		name := values[0]
		var err error
		if kind == "ssh" {
			err = manage.OnboardSSH(ctx, name, values[1], values[2])
		} else {
			card := rpc.PairingCard{Origin: values[1], MachineID: values[2], PublicKeyPin: values[3]}
			err = manage.PairDevice(ctx, name, card, values[4], values[5])
			if err != nil {
				err = fmt.Errorf("%s", strings.ReplaceAll(err.Error(), values[4], "[redacted]"))
			}
		}
		return machineActionResult{action: machineAction{verb: "add", id: name}, err: err}
	}
}

func (m *machineModel) machineAdded(result machineActionResult) tea.Cmd {
	p := m.panel
	canceled := p != nil && p.working && p.stopSetup == nil
	if p != nil {
		p.working, p.stopSetup = false, nil
	}
	if result.err != nil {
		text := "Could not add machine: " + sanitizeText(result.err.Error())
		if p == nil {
			m.active.model.errorf("%s", text)
		} else {
			p.form, p.err = nil, text
		}
		return m.refreshMachinePanel()
	}
	if canceled {
		p.form, p.err = nil, "Setup canceled after the machine was saved. Connect it from Machines."
		return m.refreshMachinePanel()
	}
	machine, ok := m.machine(result.action.id)
	if !ok {
		m.active.model.errorf("Machine was saved but is not in the current list; reopen /machines")
		return m.refreshMachinePanel()
	}
	m.closeMachinePanel()
	return tea.Batch(m.selectMachine(machine), m.connectionCmd("connect", machine.ID))
}

func (p *machinePanel) view(screen *screenState) panelView {
	if p.form != nil {
		return p.form.view(screen, p.working, p.err)
	}
	actions := []string{"↑/↓ choose · Enter select · C connect · D disconnect", "A add SSH · P pair device · Esc close"}
	if p.err != "" {
		actions = append(screen.wrap(p.err, max(1, terminalWidth(screen.width)-3)), actions...)
	}
	return panelView{kind: "machines", layout: panelSettings, title: "Machines", body: p.rows, actions: actions, cursor: p.cursor}
}

func (f *machineForm) view(screen *screenState, working bool, notice string) panelView {
	view := panelView{kind: "machine-form", layout: panelSettings, title: "Add SSH machine", cursor: -1}
	if f.kind == "pair" {
		view.title = "Pair this device"
	}
	if working {
		view.body = []string{cmp.Or(notice, "Adding machine…")}
		if notice == "" {
			view.actions = []string{"Esc cancel setup"}
		}
		return view
	}
	if f.step < len(f.fields()) {
		view.body = []string{f.fields()[f.step]}
		view.actions = []string{"Enter next · Esc cancel"}
		if notice != "" {
			view.body = append(view.body, notice)
		}
		return view
	}
	view.body = f.consentLines(screen)
	view.actions = []string{"Enter confirm · Esc cancel"}
	if !f.consentVisible(screen) {
		view.body = []string{"Enlarge the terminal to review and confirm"}
		view.actions = []string{"Esc cancel"}
	}
	return view
}

func (f *machineForm) consentLines(s *screenState) []string {
	body := []string{"Connect to " + sanitizeText(f.values[1]), "Uses SSH host keys you already trust", "May install a verified local backend artifact"}
	if f.kind == "pair" {
		body = []string{
			"Pair with " + sanitizeText(f.values[1]),
			"This device gets FULL access to the daemon account",
			"Same access as trusted SSH into that account",
			"It can run commands and read or change files",
			"It can use that access to create other ways in",
			"Revoking blocks this credential and its connections",
			"It cannot undo changes or remove other access",
		}
	}
	var lines []string
	for _, line := range body {
		lines = append(lines, s.wrap(line, max(1, panelRowWidth(s)-4))...)
	}
	return lines
}

func (f *machineForm) consentVisible(s *screenState) bool {
	return panelRows(panelSettings, s.height) >= len(f.consentLines(s))+2
}

func (m *model) machineInput(width int) (string, int) {
	f := m.machines.form
	if f == nil || m.machines.working || f.step == len(f.fields()) {
		return m.draftLine(width)
	}
	label := f.fields()[f.step] + ": "
	text := f.field.text
	if f.secret() {
		text = []rune(strings.Repeat("*", len(text)))
	}
	room := terminalWidth(width) - m.screen.cells(label)
	window, caret := fieldWindow(&m.screen, text, f.field.cursor, room)
	return m.screen.fit(label+window, terminalWidth(width)), m.screen.cells(label) + caret
}

func (m *model) machineOffline() bool {
	if m.conn.link.Endpoint == "" || m.conn.client != nil {
		return false
	}
	m.errorf("%s", m.conn.lost)
	return true
}

func (m *model) threadReady() bool {
	if m.thread.id != "" && (m.conn.link.Endpoint == "" || m.conn.threadOpen) {
		return true
	}
	m.noticef("Thread is not ready; wait for startup or use /resume or /new")
	return false
}

func (p *machinePanel) selectRow(id string) {
	if i := slices.IndexFunc(p.machines, func(machine Machine) bool { return machine.ID == id }); i >= 0 {
		p.cursor = i
	}
}
