package tui

import (
	"slices"
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/x/ansi"

	"github.com/gosuda/dalgona/internal/rpc"
)

func initializedModel(t *testing.T, cwd string) model {
	t.Helper()
	m := newTestModel(nil, Options{Cwd: cwd}, nil)
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
	t.Setenv("HOME", "/home/u")
	m := initializedModel(t, "/home/u/proj")
	welcome := entriesWithKey(m, "welcome")
	if len(welcome) != 1 || !strings.Contains(ansi.Strip(welcome[0].text), "~/proj") {
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
	m := initializedModel(t, "/work")
	m.thread.id = "t1"
	m.thread.title = "Plan"
	m = rawUpdate(t, m, press('c', tea.ModCtrl, ""))
	m, _ = updateWithCommand(t, m, press('c', tea.ModCtrl, ""))
	if !strings.Contains(strings.Join(m.out.pending, "\n"), "Thread Plan saved · resume with dalgona --resume t1") {
		t.Fatalf("exit row missing or wrong in pending %q", m.out.pending)
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
