package tui

import (
	"fmt"
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"
)

func shortApprovalModel(t *testing.T, width, height int, request func(chan requestAnswer) serverRequestMsg) model {
	t.Helper()
	m := newTestModel(new(fakeCaller), Options{})
	m, _ = drive(t, m, tea.WindowSizeMsg{Width: width, Height: height})
	m.thread.id = "t1"
	m, _ = drive(t, m, request(make(chan requestAnswer, 1)))
	if len(m.approvals) != 1 {
		t.Fatalf("approval was not queued: %#v", m.approvals)
	}
	return m
}

func longCommandApproval(answer chan requestAnswer) serverRequestMsg {
	command := strings.Repeat("go test ./internal/tui/... -run TestFrame -count=1\\n", 12)
	return serverRequestMsg{method: "item/commandExecution/requestApproval", requestID: "r2", answer: answer, params: []byte(`{"threadId":"t1","turnId":"turn-1","itemId":"cmd2","startedAtMs":1,"command":"` + command + `","cwd":"/work","reason":"reason"}`)}
}

type shortCard struct {
	height  int
	panel   int
	accept  bool
	command bool
	live    bool
	band    bool
}

func (c shortCard) check(t *testing.T, lines []string) {
	t.Helper()
	c.checkCard(t, checkShortDock(t, lines, "┌ Approval", c.panel, c.live, c.band))
}

func (c shortCard) checkCard(t *testing.T, card string) {
	t.Helper()
	if !strings.Contains(card, "[n] decline") {
		t.Fatalf("card lacks %q:\n%s", "[n] decline", card)
	}
	if got := strings.Contains(card, "[y] accept"); got != c.accept {
		t.Fatalf("card shows [y] accept = %v, want %v:\n%s", got, c.accept, card)
	}
	if got := strings.Contains(card, "$ go test"); got != c.command {
		t.Fatalf("card shows the command = %v, want %v:\n%s", got, c.command, card)
	}
}

func TestApprovalCardKeepsActionsAtHeights(t *testing.T) {
	t.Parallel()
	for _, tc := range []shortCard{
		{height: 4, panel: 2},
		{height: 5, panel: 3, accept: true},
		{height: 6, panel: 3, accept: true, live: true},
		{height: 7, panel: 3, accept: true, live: true, band: true},
		{height: 8, panel: 4, accept: true, command: true, live: true, band: true},
	} {
		for _, width := range []int{20, 40} {
			t.Run(fmt.Sprintf("%dx%d", width, tc.height), func(t *testing.T) {
				t.Parallel()
				m := shortApprovalModel(t, width, tc.height, commandApprovalRequest)
				lines := strings.Split(stripANSI(m.View().Content), "\n")
				assertFrame(t, lines, width, tc.height)
				tc.check(t, lines)
			})
		}
	}
}

func TestApprovalCardNeedsFourRows(t *testing.T) {
	t.Parallel()
	for _, height := range []int{2, 3} {
		t.Run(fmt.Sprintf("height %d", height), func(t *testing.T) {
			t.Parallel()
			m := shortApprovalModel(t, 40, height, commandApprovalRequest)
			lines := strings.Split(stripANSI(m.View().Content), "\n")
			assertFrame(t, lines, 40, height)
			if joined := strings.Join(lines, "\n"); strings.Contains(joined, "Approval") || strings.Contains(joined, "[n] decline") {
				t.Fatalf("a window of %d rows shows the card:\n%s", height, joined)
			}
		})
	}
}

func TestApprovalCardFramesAtShortHeights(t *testing.T) {
	t.Parallel()
	for _, request := range []struct {
		name string
		open func(chan requestAnswer) serverRequestMsg
	}{
		{"command", commandApprovalRequest},
		{"file", fileApprovalRequest},
		{"long command", longCommandApproval},
	} {
		for height := 4; height <= 9; height++ {
			for _, width := range []int{20, 40, 100} {
				t.Run(fmt.Sprintf("%s %dx%d", request.name, width, height), func(t *testing.T) {
					t.Parallel()
					m := shortApprovalModel(t, width, height, request.open)
					before := len(m.dockLines())
					assertFrame(t, strings.Split(m.View().Content, "\n"), width, height)
					m, _ = drive(t, m, press(tea.KeyDown, 0, ""))
					assertFrame(t, strings.Split(m.View().Content, "\n"), width, height)
					if got := len(m.dockLines()); got != before {
						t.Fatalf("dock changed from %d to %d rows after a scroll key", before, got)
					}
				})
			}
		}
	}
}
