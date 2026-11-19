package tui

import (
	"fmt"
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"

	"github.com/gosuda/dalgona/internal/dal/rpc"
)

const maskedQuestionJSON = `{"threadId":"t1","turnId":"turn-1","itemId":"q","isBlocking":true,"questions":[{"id":"q1","header":"Secret","question":"Which token?","isOther":true,"isSecret":true,"multiSelect":false,"options":[]}]}`

type panelOpener struct {
	name   string
	inline bool
	open   func(t *testing.T, m model) model
}

func openSlash(name string) func(*testing.T, model) model {
	return func(_ *testing.T, m model) model {
		m.executeSlash(name, "")
		return m
	}
}

func openResult(name, op string, value any) panelOpener {
	return panelOpener{name: name, open: func(t *testing.T, m model) model {
		m, _ = drive(t, m, resultMsg{op: op, value: value})
		return m
	}}
}

func openRequest(name string, request func() serverRequestMsg) panelOpener {
	return panelOpener{name: name, open: func(t *testing.T, m model) model {
		m, _ = drive(t, m, request())
		return m
	}}
}

func openTyped(name, text string, popup bool) panelOpener {
	return panelOpener{name: name, inline: !popup, open: func(t *testing.T, m model) model {
		return typeText(t, m, text)
	}}
}

func narrowThreads() *rpc.ThreadListResponse {
	return &rpc.ThreadListResponse{Data: []rpc.Thread{{ID: "t1", Name: "demo", Preview: "a preview with a tab\tinside"}, {ID: "t2"}}}
}

func narrowTree() *rpc.ThreadTreeResponse {
	return &rpc.ThreadTreeResponse{Turns: []rpc.TreeTurn{{TurnID: "turn-8", Preview: "old", Active: true}, {TurnID: "turn-9", Preview: "work", Active: true}}}
}

func narrowPanelOpeners() []panelOpener {
	argument := make(chan requestAnswer, 4)
	return []panelOpener{
		{name: "composer", inline: true, open: func(_ *testing.T, m model) model { return m }},
		openTyped("draft", "hello world", false),
		openTyped("shell draft", "!ls -la", false),
		openTyped("slash popup", "/", true),
		openTyped("file popup", "@", true),
		{name: "history", open: func(t *testing.T, m model) model {
			m, _ = drive(t, m, press('r', tea.ModCtrl, ""))
			return m
		}},
		{name: "help", open: openSlash("help")},
		{name: "settings", open: openSlash("settings")},
		{name: "login loading", open: openSlash("login")},
		{name: "login providers", open: func(t *testing.T, m model) model {
			m.executeSlash("login", "")
			m, _ = drive(t, m, resultMsg{op: "providers", value: providerList()})
			return m
		}},
		{name: "login api key", open: func(t *testing.T, m model) model {
			m.executeSlash("login", "openai")
			m, _ = drive(t, m, resultMsg{op: "providers", value: providerList()})
			m, _ = drive(t, m, press(tea.KeyEnter, 0, ""))
			return typeText(t, m, "sk-live-test")
		}},
		{name: "confirm new", open: openSlash("new")},
		{name: "confirm compact", open: openSlash("compact")},
		{name: "list review", open: openSlash("review")},
		openResult("list models", "models", &rpc.ModelListResponse{Data: []rpc.Model{{ID: "model-1"}, {ID: "한글-model"}}}),
		openResult("list resume", "threads", narrowThreads()),
		openResult("list tree", "tree", narrowTree()),
		openResult("list revert", "revertTree", narrowTree()),
		openResult("list fork", "forkTree", narrowTree()),
		openResult("list advisor", "advisorPanel", &[]rpc.AdvisorState{{Name: "advisor", Status: "disabled", Model: "gpt-test"}}),
		openRequest("approval command", func() serverRequestMsg { return commandApprovalRequest(argument) }),
		openRequest("approval file", func() serverRequestMsg { return fileApprovalRequest(argument) }),
		openRequest("approval long command", func() serverRequestMsg {
			command := strings.Repeat("go test ./internal/tui/... -run TestFrame -count=1\\n", 40)
			return serverRequestMsg{method: "item/commandExecution/requestApproval", requestID: "r2", answer: argument, params: []byte(`{"threadId":"t1","turnId":"turn-1","itemId":"cmd2","startedAtMs":1,"command":"` + command + `","cwd":"/work","reason":"한글 reason\twith a tab"}`)}
		}),
		openRequest("question", func() serverRequestMsg { return questionRequest(argument, questionParamsJSON) }),
		openRequest("secret question", func() serverRequestMsg { return questionRequest(argument, maskedQuestionJSON) }),
	}
}

func narrowPanelModel(t *testing.T, width, height int) model {
	t.Helper()
	m := newTestModel(new(fakeCaller), Options{})
	m, _ = drive(t, m, tea.WindowSizeMsg{Width: width, Height: height})
	m.thread.id = "t1"
	return m
}

func requireDockWithin(t *testing.T, m model, opener panelOpener, width, height int) {
	t.Helper()
	if !opener.inline && !m.panelOpen() && !m.popupShown() {
		t.Fatalf("%s left no panel open", opener.name)
	}
	lines := strings.Split(m.View().Content, "\n")
	assertFrame(t, lines, max(width, 2), height)
}

func TestPanelRowsFitNarrowWindows(t *testing.T) {
	t.Parallel()
	for _, opener := range narrowPanelOpeners() {
		t.Run(opener.name, func(t *testing.T) {
			t.Parallel()
			for _, height := range []int{24, 8, 6, 5} {
				for width := 1; width <= 5; width++ {
					t.Run(fmt.Sprintf("%dx%d resized after opening", width, height), func(t *testing.T) {
						m := opener.open(t, narrowPanelModel(t, 80, 24))
						m, _ = drive(t, m, tea.WindowSizeMsg{Width: width, Height: height})
						requireDockWithin(t, m, opener, width, height)
					})
					t.Run(fmt.Sprintf("%dx%d opened after resizing", width, height), func(t *testing.T) {
						m := opener.open(t, narrowPanelModel(t, width, height))
						requireDockWithin(t, m, opener, width, height)
					})
				}
			}
		})
	}
}
