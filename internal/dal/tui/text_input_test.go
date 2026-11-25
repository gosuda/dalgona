package tui

import (
	"context"
	"encoding/json/jsontext"
	json "encoding/json/v2"
	"errors"
	"net"
	"reflect"
	"strings"
	"testing"
	"time"

	tea "charm.land/bubbletea/v2"

	"github.com/gosuda/dalgona/internal/dal/rpc"
)

func TestComposerSendsTheExactDraftAndRefusesABlankOne(t *testing.T) {
	t.Parallel()
	caller := new(fakeCaller)
	m := newModel(caller, Options{}, nil)
	m.thread.id = "t1"
	m = typeText(t, m, " \u3000 ")
	if _, command := drive(t, m, press(tea.KeyEnter, 0, "")); command != nil || len(caller.calls) != 0 {
		t.Fatalf("a blank draft sent %#v", caller.calls)
	}
	m.input.composer.SetValue("")
	m = typeText(t, m, "  hi \u3000")
	m, command := drive(t, m, press(tea.KeyEnter, 0, ""))
	m = runCommand(t, m, command, "Enter did not send the prompt")
	assertTurnStartText(t, caller, "  hi \u3000")
	if got := historyTexts(m); len(got) != 1 || got[0] != "  hi \u3000" {
		t.Fatalf("history = %q, want the exact prompt", got)
	}
}

func TestSteerAndFollowUpKeepTheExactDraft(t *testing.T) {
	t.Parallel()
	caller := new(fakeCaller)
	m := newModel(caller, Options{}, nil)
	m.thread.id, m.thread.turnID = "t1", "turn-1"
	m.setState(stateRunning)
	m = typeText(t, m, " later ")
	m, _ = drive(t, m, press(tea.KeyEnter, tea.ModAlt, ""))
	if len(m.queue) != 1 || m.queue[0].text != " later " {
		t.Fatalf("queue = %#v, want the exact follow-up", m.queue)
	}
	m = typeText(t, m, "  more\u00a0")
	m, command := drive(t, m, press(tea.KeyEnter, 0, ""))
	runCommand(t, m, command, "Enter did not steer the turn")
	assertLastCallText(t, caller, 1, "turn/steer", "  more\u00a0")
}

func TestShellCommandKeepsItsExactText(t *testing.T) {
	t.Parallel()
	caller := &shellCaller{result: rpc.CommandExecResponse{Stdout: "hi\n"}}
	m := newTestModel(caller, Options{Cwd: "/work"})
	shellSubmit(t, shellType(t, m, "!  echo hi  "))
	calls := shellCalls(caller)
	if len(calls) != 1 || !strings.Contains(calls[0], `"command":["sh","-c","  echo hi  "]`) {
		t.Fatalf("command/exec calls = %v, want the command with its spaces", calls)
	}
}

func TestPrivateUseTextIsTextAndNeverAChip(t *testing.T) {
	t.Parallel()
	caller := new(fakeCaller)
	m := newTestModel(caller, Options{})
	m, _ = drive(t, m, tea.WindowSizeMsg{Width: 80, Height: 24})
	m.thread.id = "t1"
	text := "\U000F0000\U000F0001x\ue000\U0010FFFD\ufffc"
	m, _ = drive(t, m, tea.PasteMsg{Content: text})
	if len(m.input.composer.chips) != 0 || m.input.composer.Expanded() != text {
		t.Fatalf("draft = %q with %d chips, want the pasted scalars as text", m.input.composer.Expanded(), len(m.input.composer.chips))
	}
	pasted := pasteLines(11) + "\n" + text
	m, _ = drive(t, m, tea.PasteMsg{Content: pasted})
	if len(m.input.composer.chips) != 1 || m.input.composer.Expanded() != text+pasted {
		t.Fatalf("draft = %q with %d chips, want the text and one chip", m.input.composer.Expanded(), len(m.input.composer.chips))
	}
	m, command := drive(t, m, press(tea.KeyEnter, 0, ""))
	runCommand(t, m, command, "Enter did not send the prompt")
	assertTurnStartText(t, caller, text+pasted)
}

func TestQuestionLabelsStayDistinctAndSendTheirSource(t *testing.T) {
	t.Parallel()
	data := `{"threadId":"t1","turnId":"turn-1","itemId":"call-q","isBlocking":true,"questions":[` +
		`{"id":"q1","header":"Pick","question":"Which?","isOther":false,"isSecret":false,"multiSelect":false,` +
		`"options":[{"label":"Yes","description":""},{"label":"Yes\u200b","description":""},{"label":"\u001b[1mBold","description":""}]}]}`
	for key, want := range map[rune]string{'2': "Yes\u200b", '3': "\x1b[1mBold"} {
		m, answer := openQuestion(t, data)
		view := strings.Join(m.questionLines(60, 13), "\n")
		for _, shown := range []string{"1. Yes  Recommended", "2. Yes<U+200B>", "3. Bold"} {
			assertContains(t, view, shown)
		}
		if strings.Contains(view, "\x1b[1mBold") || strings.Contains(view, "\u200b") {
			t.Fatalf("view kept a hidden difference raw: %q", view)
		}
		m, _ = drive(t, m, press(key, 0, string(key)))
		drive(t, m, press(tea.KeyEnter, 0, ""))
		got := (<-answer).value.(rpc.ToolRequestUserInputResponse).Answers["q1"].Answers
		if len(got) != 1 || got[0] != want {
			t.Fatalf("row %c sent %q, want the offered label %q", key, got, want)
		}
	}
}

func TestAPIKeyIsSentExactlyAndABlankKeyIsRefused(t *testing.T) {
	t.Parallel()
	caller := new(fakeCaller)
	m := openLoginPanel(t, caller, "openai")
	m, _ = drive(t, m, press(tea.KeyEnter, 0, ""))
	m = typeText(t, m, "\u3000 ")
	m, command := drive(t, m, press(tea.KeyEnter, 0, ""))
	if command != nil || m.login.err != "Enter the API key." {
		t.Fatalf("blank key gave command %v and error %q", command != nil, m.login.err)
	}
	m = typeText(t, m, "sk-1 ")
	m, command = drive(t, m, press(tea.KeyEnter, 0, ""))
	runPickCommand(t, m, command)
	last := caller.calls[len(caller.calls)-1]
	params, ok := last.params.(rpc.AccountLoginStartParams)
	if last.method != "account/login/start" || !ok || params.APIKey != "\u3000 sk-1 " {
		t.Fatalf("last call = %s %#v, want the key exactly as typed", last.method, last.params)
	}
}

func TestHistoryRowCutsAtAClusterAndShowsControls(t *testing.T) {
	t.Parallel()
	long := strings.Repeat("a", historyLabelBytes-1) + "e\u0301z"
	if got := historyLabel(long); got != strings.Repeat("a", historyLabelBytes-1) {
		t.Fatalf("history row of %d bytes ends with %q, want the cut before the cluster", len(got), got[len(got)-3:])
	}
	if got, want := historyLabel(" a\x1b]0;x\x07b\u202e \u3000 c\n"), "ab<U+202E> c"; got != want {
		t.Fatalf("history row = %q, want %q", got, want)
	}
}

type questionLoop struct {
	handler *programHandler
	answers chan requestAnswer
}

func (questionLoop) Notification(string, jsontext.Value) {}

func (l questionLoop) Request(ctx context.Context, _ string, _ jsontext.Value) (any, error) {
	return l.handler.answerQuestion(ctx, l.answers)
}

func answerOf(text string) requestAnswer {
	return requestAnswer{value: rpc.ToolRequestUserInputResponse{Answers: map[string]rpc.ToolRequestUserInputAnswer{"q1": {Answers: []string{text}}}}}
}

func closeConns(t *testing.T, conns ...*rpc.Conn) {
	t.Helper()
	for _, c := range conns {
		if err := c.Close(); err != nil {
			t.Errorf("close: %v", err)
		}
	}
}

func refuseBad(checked *[]string) rpc.ResponseCheck {
	return func(result jsontext.Value) error {
		var got rpc.ToolRequestUserInputResponse
		if err := json.Unmarshal(result, &got); err != nil {
			return err
		}
		*checked = append(*checked, got.Answers["q1"].Answers[0])
		if got.Answers["q1"].Answers[0] == "bad" {
			return errors.New("refused")
		}
		return nil
	}
}

func TestQuestionAnswerLoopSendsACorrectionOnTheSameRequest(t *testing.T) {
	t.Parallel()
	hostEnd, clientEnd := net.Pipe()
	loop := questionLoop{handler: &programHandler{done: make(chan struct{})}, answers: make(chan requestAnswer, 1)}
	client := rpc.Dial(t.Context(), clientEnd, loop)
	host := rpc.NewConn(t.Context(), hostEnd, nil)
	t.Cleanup(func() { closeConns(t, client, host) })
	var checked []string
	check := refuseBad(&checked)
	done := make(chan error, 1)
	var result rpc.ToolRequestUserInputResponse
	go func() {
		call := rpc.CheckedCall{Method: "item/tool/requestUserInput", Params: map[string]any{}, Check: check}
		done <- host.CallIDChecked(t.Context(), "req-1", call, &result)
	}()
	loop.answers <- answerOf("bad")
	loop.answers <- answerOf("good")
	select {
	case err := <-done:
		if err != nil || !reflect.DeepEqual(checked, []string{"bad", "good"}) || result.Answers["q1"].Answers[0] != "good" {
			t.Fatalf("call = %v after checks %q with result %+v, want the correction accepted", err, checked, result)
		}
	case <-time.After(5 * time.Second):
		t.Fatalf("the correction never reached the host; checks %q", checked)
	}
}

func TestQuestionRefusesAnOfferedLabelThatFailsTheTextCheck(t *testing.T) {
	t.Parallel()
	data := `{"threadId":"t1","turnId":"turn-1","itemId":"call-q","isBlocking":true,"questions":[` +
		`{"id":"q1","header":"Pick","question":"Which?","isOther":false,"isSecret":false,"multiSelect":false,` +
		`"options":[{"label":"a\u0000b","description":""},{"label":"No","description":""}]}]}`
	m, answer := openQuestion(t, data)
	m, _ = drive(t, m, press('1', 0, "1"))
	m, _ = drive(t, m, press(tea.KeyEnter, 0, ""))
	card := m.approvals[0].card
	if len(answer) != 0 || card.sent || card.attempts != 0 || m.notice != "Answer not sent: invalid text: answer: disallowed_control" {
		t.Fatalf("NUL label: %d sent, card sent %v after %d attempts, notice %q", len(answer), card.sent, card.attempts, m.notice)
	}
}
