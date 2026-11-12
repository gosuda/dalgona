package tui

import (
	"context"
	"errors"
	"reflect"
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"
)

type failingSaveCaller struct {
	fakeCaller
}

func (f *failingSaveCaller) Call(ctx context.Context, method string, params, result any) error {
	if method == "config/batchWrite" {
		f.calls = append(f.calls, callRecord{method: method, params: params})
		return errors.New("disk full")
	}
	return f.fakeCaller.Call(ctx, method, params, result)
}

func openSettingsPanel(t *testing.T, m model, approval string) model {
	t.Helper()
	m, _ = drive(t, m, resultMsg{op: "settings", value: &settingsReply{Settings: []settingValue{{Key: "approval", Value: approval}}}})
	m.input.composer.SetValue("/settings")
	m, _ = drive(t, m, tea.KeyPressMsg{Code: tea.KeyEnter})
	return m
}

func pressKeys(t *testing.T, m model, keys ...tea.KeyPressMsg) model {
	t.Helper()
	for _, key := range keys {
		m, _ = drive(t, m, key)
	}
	return m
}

func batchEdits(client *fakeCaller) []settingEdit {
	var edits []settingEdit
	for _, call := range client.calls {
		if batch, ok := call.params.(settingsBatch); ok && call.method == "config/batchWrite" {
			edits = append(edits, batch.Edits...)
		}
	}
	return edits
}

func TestSettingsRowsInOrder(t *testing.T) {
	t.Parallel()
	m := openSettingsPanel(t, newTestModel(new(fakeCaller), Options{}, nil), "auto")
	text := dockText(m)
	if !strings.Contains(text, "Settings") {
		t.Fatalf("settings panel did not open:\n%s", text)
	}
	last := -1
	for _, label := range []string{"Approval", "Verbosity", "Theme", "Icons", "Status", "Edit review", "Notifications"} {
		at := strings.Index(text, label)
		if at <= last {
			t.Fatalf("%q is out of order in:\n%s", label, text)
		}
		last = at
	}
}

func TestSettingsPanelSavesChangedRows(t *testing.T) {
	t.Parallel()
	caller := new(fakeCaller)
	m := openSettingsPanel(t, newTestModel(caller, Options{}, nil), "ask")
	m = pressKeys(t, m, tea.KeyPressMsg{Code: tea.KeyRight})
	m, command := drive(t, m, tea.KeyPressMsg{Code: tea.KeyEnter})
	runPickCommand(t, m, command)
	edits := batchEdits(caller)
	if len(edits) != 1 || edits[0].KeyPath != "approval" || edits[0].Value != "auto" {
		t.Fatalf("batch edits = %#v, want one approval edit set to auto", edits)
	}
}

func TestSettingsPanelKeepsOpenOnError(t *testing.T) {
	t.Parallel()
	caller := new(failingSaveCaller)
	m := openSettingsPanel(t, newTestModel(caller, Options{}, nil), "ask")
	m = pressKeys(t, m, tea.KeyPressMsg{Code: tea.KeyRight})
	m, command := drive(t, m, tea.KeyPressMsg{Code: tea.KeyEnter})
	m = runPickCommand(t, m, command)
	if !strings.Contains(dockText(m), "Settings") {
		t.Fatalf("settings panel closed after a failed save:\n%s", dockText(m))
	}
	assertContains(t, stripANSI(m.liveText()), "Could not save settings: disk full")
}

func TestSettingsSaveErrorSurvivesKeyPress(t *testing.T) {
	t.Parallel()
	caller := new(failingSaveCaller)
	m := openSettingsPanel(t, newTestModel(caller, Options{}, nil), "ask")
	m = pressKeys(t, m, tea.KeyPressMsg{Code: tea.KeyRight})
	m, command := drive(t, m, tea.KeyPressMsg{Code: tea.KeyEnter})
	m = runPickCommand(t, m, command)
	m = pressKeys(t, m, tea.KeyPressMsg{Code: tea.KeyDown})
	assertContains(t, stripANSI(m.liveText()), "Could not save settings: disk full")
}

func TestSettingsSecondEnterWhileSavingSendsNothing(t *testing.T) {
	t.Parallel()
	caller := new(fakeCaller)
	m := openSettingsPanel(t, newTestModel(caller, Options{}, nil), "ask")
	m = pressKeys(t, m, tea.KeyPressMsg{Code: tea.KeyRight})
	m, first := drive(t, m, tea.KeyPressMsg{Code: tea.KeyEnter})
	m, second := drive(t, m, tea.KeyPressMsg{Code: tea.KeyEnter})
	if second != nil {
		t.Fatal("a second Enter while the save is in flight sent a command")
	}
	m = runPickCommand(t, m, first)
	saves := 0
	for _, call := range caller.calls {
		if call.method == "config/batchWrite" {
			saves++
		}
	}
	if saves != 1 {
		t.Fatalf("config/batchWrite calls = %d, want 1", saves)
	}
	if m.saving != nil {
		t.Fatal("save still in flight after its result")
	}
}

func TestSettingsEscRevertsPreview(t *testing.T) {
	t.Parallel()
	m := openSettingsPanel(t, newTestModel(new(fakeCaller), Options{}, nil), "auto")
	before := m.screen.dockTheme().name
	m = pressKeys(t, m,
		tea.KeyPressMsg{Code: tea.KeyDown},
		tea.KeyPressMsg{Code: tea.KeyDown},
		tea.KeyPressMsg{Code: tea.KeyRight},
		tea.KeyPressMsg{Code: tea.KeyRight},
	)
	if m.screen.themeSetting != "day" || m.screen.dockTheme().name == before {
		t.Fatalf("theme preview = %q (%s), want day applied at once", m.screen.themeSetting, m.screen.dockTheme().name)
	}
	m = pressKeys(t, m, tea.KeyPressMsg{Code: tea.KeyEscape})
	if m.screen.themeSetting != "auto" || m.screen.dockTheme().name != before {
		t.Fatalf("after Esc theme = %q (%s), want the saved auto theme (%s)", m.screen.themeSetting, m.screen.dockTheme().name, before)
	}
	if strings.Contains(dockText(m), "Settings") {
		t.Fatalf("Esc did not close the settings panel:\n%s", dockText(m))
	}
}

func TestSettingsSavesIconsAndStatusHide(t *testing.T) {
	t.Parallel()
	caller := new(fakeCaller)
	m := openSettingsPanel(t, newTestModel(caller, Options{}, nil), "auto")
	m = pressKeys(t, m,
		tea.KeyPressMsg{Code: tea.KeyDown},
		tea.KeyPressMsg{Code: tea.KeyDown},
		tea.KeyPressMsg{Code: tea.KeyDown},
		tea.KeyPressMsg{Code: tea.KeyRight},
		tea.KeyPressMsg{Code: tea.KeyRight},
		tea.KeyPressMsg{Code: tea.KeyDown},
		tea.KeyPressMsg{Code: tea.KeyRight},
		tea.KeyPressMsg{Code: tea.KeyRight},
		tea.KeyPressMsg{Code: tea.KeyRight},
		tea.KeyPressMsg{Code: tea.KeySpace, Text: " "},
		tea.KeyPressMsg{Code: tea.KeyRight},
		tea.KeyPressMsg{Code: tea.KeyRight},
		tea.KeyPressMsg{Code: tea.KeyRight},
		tea.KeyPressMsg{Code: tea.KeySpace, Text: " "},
	)
	m, command := drive(t, m, tea.KeyPressMsg{Code: tea.KeyEnter})
	runPickCommand(t, m, command)
	values := map[string]any{}
	for _, edit := range batchEdits(caller) {
		values[edit.KeyPath] = edit.Value
	}
	if got := values["icons"]; got != "nerd" {
		t.Fatalf("icons edit = %#v, want nerd in %#v", got, values)
	}
	if got := values["statusHide"]; !reflect.DeepEqual(got, []string{"folder", "mark"}) {
		t.Fatalf("statusHide edit = %#v, want [folder mark]", got)
	}
}

func TestSettingsApprovalNotice(t *testing.T) {
	t.Parallel()
	m := openSettingsPanel(t, newTestModel(new(fakeCaller), Options{}, nil), "ask")
	m = pressKeys(t, m, tea.KeyPressMsg{Code: tea.KeyRight})
	m, command := drive(t, m, tea.KeyPressMsg{Code: tea.KeyEnter})
	m = runPickCommand(t, m, command)
	if want := "saved; open threads keep their approval and sandbox"; m.notice != want {
		t.Fatalf("notice = %q, want %q", m.notice, want)
	}
}

func TestSettingsDockSteady(t *testing.T) {
	t.Parallel()
	m := openSettingsPanel(t, newTestModel(new(fakeCaller), Options{}, nil), "auto")
	m = pressKeys(t, m,
		tea.KeyPressMsg{Code: tea.KeyDown},
		tea.KeyPressMsg{Code: tea.KeyDown},
		tea.KeyPressMsg{Code: tea.KeyDown},
		tea.KeyPressMsg{Code: tea.KeyDown},
	)
	var keys []tea.KeyPressMsg
	for range 50 {
		keys = append(keys, tea.KeyPressMsg{Code: tea.KeyRight}, tea.KeyPressMsg{Code: tea.KeyLeft})
	}
	for range 50 {
		keys = append(keys, tea.KeyPressMsg{Code: tea.KeyDown})
	}
	m = assertSteadyDock(t, m, keys)
	if got := len(m.dockLines()); got != 20 {
		t.Fatalf("dock has %d rows, want 20 (input 1, settings 17, live 1, band 1)", got)
	}
}
