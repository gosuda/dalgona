package tui

import (
	"path/filepath"
	"slices"
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/x/ansi"

	"github.com/gosuda/dalgona/internal/dal/rpc"
)

func initializedModel(t *testing.T, cwd string) model {
	t.Helper()
	m := newTestModel(nil, Options{Cwd: cwd})
	return rawUpdate(t, m, resultMsg{op: "initialize", value: &rpc.InitializeResult{UserAgent: "dalgona/1.2.3"}})
}

func entriesWithKey(m model, key string) []entry {
	var found []entry
	for _, e := range m.out.entries {
		if e.key == key {
			found = append(found, e)
		}
	}
	return found
}

func TestWelcomeLineOnce(t *testing.T) {
	t.Parallel()
	m := initializedModel(t, "/work")
	m = rawUpdate(t, m, resultMsg{op: "initialize", value: &rpc.InitializeResult{UserAgent: "dalgona/1.2.3"}})
	if got := len(entriesWithKey(m, "welcome")); got != 1 {
		t.Fatalf("welcome rows after two initialize results = %d, want 1", got)
	}
}

func TestWelcomeLinesNameProductModelAndDirectory(t *testing.T) {
	t.Parallel()
	m := initializedModel(t, "/work")
	welcome := entriesWithKey(m, "welcome")
	if len(welcome) != 1 {
		t.Fatalf("welcome rows = %d, want 1", len(welcome))
	}
	lines := strings.Split(ansi.Strip(welcome[0].text), "\n")
	want := []string{"dalgona 1.2.3 · default · /work", "Type a prompt · / commands · ? keys"}
	if !slices.Equal(lines, want) {
		t.Fatalf("welcome lines = %q, want %q", lines, want)
	}
}

func TestWelcomeShowsHomeAsTilde(t *testing.T) {
	home := filepath.Clean("/home/u")
	t.Setenv("HOME", home)
	t.Setenv("USERPROFILE", home)
	m := initializedModel(t, filepath.Join(home, "proj"))
	welcome := entriesWithKey(m, "welcome")
	if len(welcome) != 1 || !strings.Contains(ansi.Strip(welcome[0].text), filepath.Join("~", "proj")) {
		t.Fatalf("welcome row does not show the home directory as ~: %+v", welcome)
	}
}

func TestWelcomeUsesOnlyDefaultColours(t *testing.T) {
	t.Parallel()
	m := initializedModel(t, "/work")
	welcome := entriesWithKey(m, "welcome")
	if len(welcome) != 1 {
		t.Fatalf("welcome rows = %d, want 1", len(welcome))
	}
	allowed := []string{"", "0", "1", "2"}
	for _, match := range sgrPattern.FindAllStringSubmatch(welcome[0].text, -1) {
		for param := range strings.SplitSeq(match[1], ";") {
			if !slices.Contains(allowed, param) {
				t.Errorf("welcome row holds SGR %q, want only bold, dim or reset", match[0])
			}
		}
	}
}

func TestExitLineNamesResumeCommand(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct{ name, product, machine, remote, command string }{
		{name: "Dal remains local", product: "dal", command: "dal"},
		{name: "Dalgona local", product: "dalgona", machine: "local", command: "dalgona --local"},
		{name: "Dalgona named remote", product: "dalgona", machine: "Remote label", remote: "work", command: "dalgona --remote work"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			m := initializedModel(t, "/work")
			m.conn.product = tc.product
			m.conn.machine, m.conn.remote = tc.machine, tc.remote
			m.thread.id, m.thread.title = "t1", "Plan"
			m = rawUpdate(t, m, press('c', tea.ModCtrl, ""))
			m, _ = updateWithCommand(t, m, press('c', tea.ModCtrl, ""))
			assertContains(t, strings.Join(m.out.pending, "\n"), "Thread Plan saved · resume with "+tc.command+" --resume t1")
		})
	}
}

func TestExitLineWithoutThread(t *testing.T) {
	t.Parallel()
	m := initializedModel(t, "/work")
	m = rawUpdate(t, m, press('c', tea.ModCtrl, ""))
	m, _ = updateWithCommand(t, m, press('c', tea.ModCtrl, ""))
	if !strings.Contains(strings.Join(m.out.pending, "\n"), "No thread was started") {
		t.Fatalf("exit row without a thread missing in pending %q", m.out.pending)
	}
}
