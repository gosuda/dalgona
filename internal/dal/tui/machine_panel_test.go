package tui

import (
	"context"
	"errors"
	"strings"
	"sync"
	"testing"
	"testing/synctest"

	tea "charm.land/bubbletea/v2"

	"github.com/gosuda/dalgona/internal/dal/rpc"
)

func TestSelectedRemoteRemainsVisibleWhenThreadStartFails(t *testing.T) {
	t.Parallel()
	m, err := newMachineModel(machineViewCatalog{}, Options{InitialMachine: "proof"}, nil)
	if err != nil {
		t.Fatal(err)
	}
	m.active.model.screen.width, m.active.model.screen.height = 100, 30
	m.active.model = playTerminal(m.active.model)
	link := Link{Endpoint: "proof", Generation: 1}
	m.Update(MachinePhase{Link: link, Phase: "connected", Initialized: rpc.InitializeResult{UserAgent: "dalgona/test"}})
	m.Update(machineMsg{link: link, body: resultMsg{op: "new", err: &rpc.RPCError{Code: -32600, Message: "agent: no model is configured; pick one with /model"}}})
	m.active.model = settleFrames(m.active.model)
	assertShown(t, m.active.model, "no model is configured")
	m.Update(tea.KeyPressMsg{Code: 'x', Text: "x"})
	assertContains(t, stripANSI(m.View().Content), "@proof [connected]", "› x")
}

type machineViewCatalog struct {
	Machines
	MachineOnboarding
}

func (machineViewCatalog) Endpoints() []Machine {
	return []Machine{
		{ID: "local", Label: "local", Kind: "local", Phase: "idle"},
		{ID: "proof", Label: "Proof machine", Kind: "SSH daemon", Phase: "idle"},
	}
}

func (machineViewCatalog) Attach(string) (func(), error) { return nil, nil }

func TestEscapeCancelsSetupWithoutConnectingLateSavedMachine(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		set := &lateSetupCatalog{entered: make(chan context.Context), finish: make(chan struct{})}
		release := sync.OnceFunc(func() { close(set.finish) })
		defer release()
		m, err := newMachineModel(set, Options{}, nil)
		if err != nil {
			t.Fatal(err)
		}
		m.active.model.screen.width, m.active.model.screen.height = 104, 30
		m.active.model.input.composer.SetValue("local draft stays")
		m.openMachineForm("pair")
		m.panel.form.values = [len(pairingFields)]string{
			"paired", "https://loopback.invalid", strings.Repeat("1", 64), strings.Repeat("2", 64), strings.Repeat("a", 43), "test device",
		}
		m.panel.form.step = len(pairingFields)
		start := m.machineFormKey(tea.KeyPressMsg{Code: tea.KeyEnter})
		if start == nil {
			t.Fatal("consent did not start pairing")
		}
		result := make(chan tea.Msg, 1)
		go func() { result <- start() }()
		ctx := <-set.entered
		m.Update(tea.KeyPressMsg{Code: tea.KeyEscape})
		if !errors.Is(ctx.Err(), context.Canceled) {
			t.Fatalf("Escape did not cancel the in-flight setup: %v", ctx.Err())
		}
		assertContains(t, strings.Join(m.active.model.panelLines(), "\n"), "Canceling setup")
		release()
		_, follow := m.Update(<-result)
		commandMsgs(follow)
		assertSavedWithoutSelecting(t, m, set)
	})
}

func assertSavedWithoutSelecting(t *testing.T, m *machineModel, set *lateSetupCatalog) {
	t.Helper()
	if m.active.model.conn.link.Endpoint != "local" || m.active.model.input.composer.Value() != "local draft stays" {
		t.Fatal("late setup completion changed machine focus or edited the current draft")
	}
	if len(set.actions) != 0 {
		t.Fatalf("canceled setup performed lifecycle actions: %v", set.actions)
	}
	panel := strings.Join(m.active.model.panelLines(), "\n")
	assertContains(t, panel, "local · connected · selected", "paired · idle", "machine was saved")
}

type lateSetupCatalog struct {
	Machines
	MachineOnboarding
	entered   chan context.Context
	finish    chan struct{}
	published bool
	actions   []string
}

func (s *lateSetupCatalog) Endpoints() []Machine {
	endpoints := []Machine{{ID: "local", Label: "local", Kind: "local", Phase: "connected"}}
	if s.published {
		endpoints = append(endpoints, Machine{ID: "paired", Label: "paired", Kind: "paired device", Phase: "idle"})
	}
	return endpoints
}

func (s *lateSetupCatalog) PairDevice(ctx context.Context, _ string, _ rpc.PairingCard, _, _ string) error {
	s.published = true
	s.entered <- ctx
	<-s.finish
	return nil
}

func (s *lateSetupCatalog) Connect(id string) error {
	s.actions = append(s.actions, "connect "+id)
	return errors.New("unexpected connection during the cancellation scenario")
}

func (s *lateSetupCatalog) Disconnect(id string) error {
	s.actions = append(s.actions, "disconnect "+id)
	return errors.New("unexpected disconnection during the cancellation scenario")
}
