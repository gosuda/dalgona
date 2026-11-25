//go:build gui

package panel

import (
	"testing"

	"github.com/gosuda/dalgona/internal/dalgona/gui/shell"
)

func keyList(t *testing.T, context, goos string) *harness {
	t.Helper()
	h := newHarness(t)
	h.panels.help.goos = goos
	h.open(shell.Route{Panel: "help", Context: context})
	return h
}

func TestKeyListShowsTheFocusedContext(t *testing.T) {
	h := keyList(t, contextRequest, "linux")
	h.want("Accept the command or file change", "Accept for this thread", "Dismiss a question with no answers", "Scroll the request details")
	h.reject("Send, or steer while a turn runs", "Recall sent messages")
	h = keyList(t, contextComposer, "linux")
	h.want("Send, or steer while a turn runs", "Queue a follow-up while a turn runs", "Interrupt the running turn within two seconds")
	h.reject("Accept the command or file change")
}

func TestKeyListUsesPlatformModifierLabels(t *testing.T) {
	mac := keyList(t, contextComposer, "darwin")
	mac.want("Cmd+Z", "Option+Enter", "Ctrl+O")
	mac.reject("Ctrl+Z", "Alt+Enter")
	linux := keyList(t, contextComposer, "linux")
	linux.want("Ctrl+Z", "Alt+Enter", "Ctrl+O")
	linux.reject("Cmd+Z", "Option+Enter")
}

func TestKeyListExplainsCompositionAndTerminalOwnership(t *testing.T) {
	h := keyList(t, contextTerminal, "linux")
	h.want(imeNote, terminalNote, "Move between the controls", "Move focus out of the terminal")
	h.reject("Send, or steer while a turn runs")
}
