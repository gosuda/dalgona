package tui

import (
	"context"
	"fmt"
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"

	"github.com/gosuda/dalgona/internal/dal/rpc"
)

type lateCaller struct {
	shellCaller
}

func (f *lateCaller) Call(ctx context.Context, method string, params, result any) error {
	switch out := result.(type) {
	case *rpc.ModelListResponse:
		f.calls = append(f.calls, callRecord{method: method, params: params})
		out.Data = []rpc.Model{{ID: "late-model"}}
		return nil
	case *rpc.ThreadListResponse:
		f.calls = append(f.calls, callRecord{method: method, params: params})
		out.Data = []rpc.Thread{{ID: "t2", Name: "other work"}}
		return nil
	case *rpc.ThreadTreeResponse:
		f.calls = append(f.calls, callRecord{method: method, params: params})
		*out = rewindTree()
		return nil
	case *rpc.ThreadStartResponse:
		f.calls = append(f.calls, callRecord{method: method, params: params})
		out.Thread = rpc.Thread{ID: "fresh", Name: "fresh thread"}
		if asked, ok := params.(rpc.ThreadResumeParams); ok {
			out.Thread = rpc.Thread{ID: asked.ThreadID, Name: "resumed " + asked.ThreadID}
		}
		if _, ok := params.(rpc.ThreadForkParams); ok {
			out.Thread = rpc.Thread{ID: "forked", Name: "forked thread"}
		}
		return nil
	}
	return f.shellCaller.Call(ctx, method, params, result)
}

func lateModel(t *testing.T) (model, *lateCaller) {
	t.Helper()
	caller := &lateCaller{shellCaller: shellCaller{result: rpc.CommandExecResponse{Stdout: "hi\n"}}}
	m := newTestModel(caller, Options{Cwd: "/work"})
	m, _ = drive(t, m, tea.WindowSizeMsg{Width: 100, Height: 24})
	m.thread.id = "t1"
	return m, caller
}

func TestLateShellResultDoesNotJoinNewDraft(t *testing.T) {
	t.Parallel()
	for _, test := range []struct {
		name  string
		draft func(t *testing.T, m model) model
		want  string
	}{
		{name: "prompt mode", want: "next draft", draft: func(t *testing.T, m model) model {
			m, _ = drive(t, m, press(tea.KeyEscape, 0, ""))
			return shellType(t, m, "next draft")
		}},
		{name: "shell mode", want: "next draft", draft: func(t *testing.T, m model) model {
			return shellType(t, m, "next draft")
		}},
		{name: "paste chip", want: "[Paste #1 +12 lines]", draft: func(t *testing.T, m model) model {
			m, _ = drive(t, m, press(tea.KeyEscape, 0, ""))
			m, _ = drive(t, m, tea.PasteMsg{Content: strings.TrimSuffix(strings.Repeat("line\n", 12), "\n")})
			return m
		}},
	} {
		t.Run(test.name, func(t *testing.T) {
			t.Parallel()
			m, _ := lateModel(t)
			m = shellType(t, m, "!echo hi")
			m, command := drive(t, m, press(tea.KeyEnter, 0, ""))
			if command == nil {
				t.Fatal("Enter in shell mode produced no command")
			}
			m = test.draft(t, m)
			m, _ = drive(t, m, command())
			if got := m.input.composer.firstLine(); got != test.want {
				t.Fatalf("draft after the late shell result = %q, want %q", got, test.want)
			}
			for _, held := range m.input.composer.chips {
				if held.shell {
					t.Fatalf("draft holds the shell chip %q after the late shell result", held.label)
				}
			}
			assertContains(t, lastEntry(t, m), "echo hi")
		})
	}
}

func TestLateRewindKeepsNewDraft(t *testing.T) {
	t.Parallel()
	m, caller := lateModel(t)
	m, _ = drive(t, m, press(tea.KeyEscape, 0, ""))
	m, command := drive(t, m, press(tea.KeyEscape, 0, ""))
	m = runCommand(t, m, command, "second esc produced no command")
	m, _ = drive(t, m, press(tea.KeyEnter, 0, ""))
	m, revert := drive(t, m, press(tea.KeyEnter, 0, ""))
	if revert == nil {
		t.Fatal("confirming the rewind produced no command")
	}
	m = shellType(t, m, "new draft")
	m, _ = drive(t, m, revert())
	if got := m.input.composer.Value(); got != "new draft" {
		t.Fatalf("draft after the late rewind = %q, want %q", got, "new draft")
	}
	if want := "Rewound; the editor holds a draft, so the prompt was not restored"; m.notice != want {
		t.Fatalf("notice after the late rewind = %q, want %q", m.notice, want)
	}
	if last := caller.calls[len(caller.calls)-1]; last.method != "thread/revert" {
		t.Fatalf("last call = %q, want thread/revert", last.method)
	}
}

func lateListRequest(m *model, name string) tea.Cmd {
	switch name {
	case "model":
		return m.modelCommand("")
	case "resume":
		return m.resumeCommand("")
	case "advisor":
		return m.advisorCommand("")
	}
	return m.treeCommand("tree")
}

func TestLateListReplyDoesNotReplaceOpenPanel(t *testing.T) {
	t.Parallel()
	panels := []struct {
		name string
		open func(m *model)
		kind string
	}{
		{name: "queue list", open: func(m *model) { m.openQueue() }, kind: queueKind},
		{name: "confirm", open: func(m *model) { m.openRevert("turn-1", "first prompt") }},
		{name: "help", open: func(m *model) { m.openHelp() }},
		{name: "review", open: func(m *model) {
			m.commit("row", []string{"an earlier row"})
			m.openReview()
		}},
	}
	for _, panel := range panels {
		for _, reply := range []string{"model", "resume", "tree", "advisor"} {
			t.Run(panel.name+" "+reply, func(t *testing.T) {
				t.Parallel()
				m, _ := lateModel(t)
				command := lateListRequest(&m, reply)
				panel.open(&m)
				if !m.panelOpen() && m.review == nil {
					t.Fatalf("%s did not open", panel.name)
				}
				m, _ = drive(t, m, command())
				assertOpenList(t, m, panel.kind)
			})
		}
	}
}

func assertOpenList(t *testing.T, m model, kind string) {
	t.Helper()
	switch {
	case kind != "" && (m.list == nil || m.list.kind != kind):
		t.Fatalf("list after the late reply = %#v, want kind %q", m.list, kind)
	case kind == "" && m.list != nil:
		t.Fatalf("the late reply opened a %q list over another panel", m.list.kind)
	}
}

func TestLateListReplyYieldsToNewerRequest(t *testing.T) {
	t.Parallel()
	m, _ := lateModel(t)
	models := m.modelCommand("")
	threads := m.resumeCommand("")
	m, _ = drive(t, m, models())
	if m.list != nil {
		t.Fatalf("the older model reply opened a %q list while a newer request is pending", m.list.kind)
	}
	m, _ = drive(t, m, threads())
	if m.list == nil || m.list.kind != "resume" {
		t.Fatalf("list after the newer reply = %#v, want the resume list", m.list)
	}
}

func confirmNewThread(t *testing.T, m model) (model, tea.Cmd) {
	t.Helper()
	m.executeSlash("new", "")
	m, started := drive(t, m, press(tea.KeyEnter, 0, ""))
	if started == nil {
		t.Fatal("confirming /new produced no command")
	}
	return m, started
}

func assertNoEntry(t *testing.T, m model, text string) {
	t.Helper()
	for _, entry := range m.out.entries {
		if strings.Contains(entry.text, text) {
			t.Fatalf("a dropped result committed %q", entry.text)
		}
	}
}

func TestNewThreadResultAfterResumeIsDropped(t *testing.T) {
	t.Parallel()
	for _, resumeFirst := range []bool{true, false} {
		t.Run(fmt.Sprintf("resume result first %t", resumeFirst), func(t *testing.T) {
			t.Parallel()
			m, _ := lateModel(t)
			m, started := confirmNewThread(t, m)
			resumed := m.executeSlash("resume", "t2")
			if resumed == nil {
				t.Fatal("/resume t2 produced no command")
			}
			first, second := started, resumed
			if resumeFirst {
				first, second = resumed, started
			}
			m, _ = drive(t, m, first())
			m, _ = drive(t, m, second())
			if m.thread.id != "t2" {
				t.Fatalf("thread after /new then /resume t2 = %q, want t2", m.thread.id)
			}
			assertNoEntry(t, m, "started fresh thread")
		})
	}
}

func TestResumeResultAfterNewThreadRequestIsDropped(t *testing.T) {
	t.Parallel()
	m, _ := lateModel(t)
	resumed := m.executeSlash("resume", "t2")
	if resumed == nil {
		t.Fatal("/resume t2 produced no command")
	}
	m, started := confirmNewThread(t, m)
	m, _ = drive(t, m, resumed())
	m, _ = drive(t, m, started())
	if m.thread.id != "fresh" {
		t.Fatalf("thread after /resume t2 then /new = %q, want fresh", m.thread.id)
	}
	assertNoEntry(t, m, "resumed t2")
}

func confirmFork(t *testing.T, m model) (model, tea.Cmd) {
	t.Helper()
	m.executeSlash("fork", "")
	turns := &rpc.ThreadTreeResponse{Turns: []rpc.TreeTurn{{TurnID: "turn-1", Preview: "first prompt", Active: true}}}
	m, _ = drive(t, m, resultMsg{op: "forkTree", seq: m.listSeq, value: turns})
	m, forked := drive(t, m, press(tea.KeyEnter, 0, ""))
	if forked == nil {
		t.Fatal("choosing a turn in /fork produced no command")
	}
	return m, forked
}

func runResults(t *testing.T, m model, command tea.Cmd) model {
	t.Helper()
	if command == nil {
		return m
	}
	switch msg := command().(type) {
	case tea.BatchMsg:
		for _, next := range msg {
			m = runResults(t, m, next)
		}
	case resultMsg:
		var next tea.Cmd
		m, next = drive(t, m, msg)
		m = runResults(t, m, next)
	}
	return m
}

func TestForkResultAfterResumeIsDropped(t *testing.T) {
	t.Parallel()
	for _, resumeFirst := range []bool{true, false} {
		t.Run(fmt.Sprintf("resume result first %t", resumeFirst), func(t *testing.T) {
			t.Parallel()
			m, _ := lateModel(t)
			m, forked := confirmFork(t, m)
			resumed := m.executeSlash("resume", "t2")
			if resumed == nil {
				t.Fatal("/resume t2 produced no command")
			}
			first, second := forked, resumed
			if resumeFirst {
				first, second = resumed, forked
			}
			m = runResults(t, m, first)
			m = runResults(t, m, second)
			if m.thread.id != "t2" {
				t.Fatalf("thread after /fork then /resume t2 = %q, want t2", m.thread.id)
			}
		})
	}
}

func TestResumeResultAfterForkRequestIsDropped(t *testing.T) {
	t.Parallel()
	m, _ := lateModel(t)
	resumed := m.executeSlash("resume", "t2")
	if resumed == nil {
		t.Fatal("/resume t2 produced no command")
	}
	m, forked := confirmFork(t, m)
	m = runResults(t, m, resumed)
	if m.thread.id != "t1" {
		t.Fatalf("thread after the older /resume t2 answered = %q, want t1 while /fork is pending", m.thread.id)
	}
	m = runResults(t, m, forked)
	if m.thread.id != "forked" {
		t.Fatalf("thread after /resume t2 then /fork = %q, want forked", m.thread.id)
	}
}
