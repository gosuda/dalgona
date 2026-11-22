package tui

import (
	"context"
	"encoding/base64"
	"errors"
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"

	"github.com/gosuda/dalgona/internal/dal/rpc"
)

type refusingCaller struct {
	fakeCaller
	method string
}

func (r *refusingCaller) Call(ctx context.Context, method string, params, result any) error {
	if method != r.method {
		return r.fakeCaller.Call(ctx, method, params, result)
	}
	r.calls = append(r.calls, callRecord{method: method, params: params})
	return errors.New("refused")
}

func lastPromptText(t *testing.T, calls []callRecord) (string, string) {
	t.Helper()
	if len(calls) == 0 {
		t.Fatal("no request was made")
	}
	last := calls[len(calls)-1]
	switch params := last.params.(type) {
	case rpc.TurnStartParams:
		return last.method, params.Input[0].Text
	case rpc.TurnSteerParams:
		return last.method, params.Input[0].Text
	}
	t.Fatalf("last call = %s %#v, want a turn request", last.method, last.params)
	return "", ""
}

func requireLastPrompt(t *testing.T, calls []callRecord, count int, method, want string) {
	t.Helper()
	if len(calls) != count {
		t.Fatalf("made %d requests, want %d: %#v", len(calls), count, calls)
	}
	if got, text := lastPromptText(t, calls); got != method || text != want {
		t.Fatalf("last call = %s %q, want %s with the pasted text unchanged %q", got, text, method, want)
	}
}

func TestQueuedChipDraftRecalledIsSentAsPastedTextNotCommand(t *testing.T) {
	t.Parallel()
	for _, paste := range slashPastes {
		for _, recall := range historyRecalls {
			t.Run(paste.name+"/"+recall.name, func(t *testing.T) {
				t.Parallel()
				caller := new(fakeCaller)
				m := pastedDraftModel(t, caller, paste.text)
				m, command := drive(t, m, press(tea.KeyEnter, tea.ModAlt, ""))
				m = runCommand(t, m, command, "alt+enter did not start the follow-up")
				requireLastPrompt(t, caller.calls, 1, "turn/start", paste.text)
				m = recall.run(t, m)
				m, command = drive(t, m, press(tea.KeyEnter, 0, ""))
				runCommand(t, m, command, "Enter did not send the recalled follow-up; notice: "+m.notice)
				requireLastPrompt(t, caller.calls, 2, "turn/steer", paste.text)
			})
		}
	}
}

func TestDequeuedChipDraftIsSentAsPastedTextNotCommand(t *testing.T) {
	t.Parallel()
	for _, paste := range slashPastes {
		t.Run(paste.name, func(t *testing.T) {
			t.Parallel()
			caller := new(fakeCaller)
			m := pastedDraftModel(t, caller, paste.text)
			m.thread.state = stateRunning
			m.thread.turnID = "turn-1"
			m, _ = drive(t, m, press(tea.KeyEnter, tea.ModAlt, ""))
			if len(m.queue) != 1 || m.input.composer.Value() != "" {
				t.Fatalf("queue = %v and composer %q, want one queued follow-up and an empty composer", m.queue, m.input.composer.Value())
			}
			m, _ = drive(t, m, press(tea.KeyUp, tea.ModAlt, ""))
			if got := m.input.composer.Expanded(); got != paste.text {
				t.Fatalf("alt+up put %q in the composer, want the pasted text", got)
			}
			m, command := drive(t, m, press(tea.KeyEnter, 0, ""))
			runCommand(t, m, command, "Enter did not send the dequeued follow-up; notice: "+m.notice)
			requireLastPrompt(t, caller.calls, 1, "turn/steer", paste.text)
		})
	}
}

var refusedSends = []struct {
	name   string
	state  string
	method string
}{
	{name: "start", state: stateIdle, method: "turn/start"},
	{name: "steer", state: stateRunning, method: "turn/steer"},
}

func TestRefusedChipDraftIsSentAsPastedTextNotCommand(t *testing.T) {
	t.Parallel()
	for _, paste := range slashPastes {
		for _, send := range refusedSends {
			t.Run(paste.name+"/"+send.name, func(t *testing.T) {
				t.Parallel()
				caller := &refusingCaller{method: send.method}
				m := pastedDraftModel(t, caller, paste.text)
				m.thread.state = send.state
				m.thread.turnID = "turn-1"
				m, command := drive(t, m, press(tea.KeyEnter, 0, ""))
				m = runCommand(t, m, command, "Enter did not send the chip draft")
				if got := m.input.composer.Expanded(); got != paste.text {
					t.Fatalf("composer holds %q after the refusal, want the pasted text back", got)
				}
				m, command = drive(t, m, press(tea.KeyEnter, 0, ""))
				runCommand(t, m, command, "Enter did not send the restored draft; notice: "+m.notice)
				requireLastPrompt(t, caller.calls, 2, send.method, paste.text)
			})
		}
	}
}

func TestRefusedChipDraftKeepsTheTextTypedMeanwhile(t *testing.T) {
	t.Parallel()
	for _, paste := range slashPastes {
		t.Run(paste.name, func(t *testing.T) {
			t.Parallel()
			caller := &refusingCaller{method: "turn/start"}
			m := pastedDraftModel(t, caller, paste.text)
			m, command := drive(t, m, press(tea.KeyEnter, 0, ""))
			m = typeText(t, m, "more")
			m = runCommand(t, m, command, "Enter did not send the chip draft")
			want := paste.text + "\nmore"
			if got := m.input.composer.Expanded(); got != want {
				t.Fatalf("composer holds %q after the refusal, want %q", got, want)
			}
			m, command = drive(t, m, press(tea.KeyEnter, 0, ""))
			runCommand(t, m, command, "Enter did not send the restored draft; notice: "+m.notice)
			requireLastPrompt(t, caller.calls, 2, "turn/start", want)
		})
	}
}

func TestRefusedFollowupIsRequeuedAsPastedTextNotCommand(t *testing.T) {
	t.Parallel()
	for _, paste := range slashPastes {
		t.Run(paste.name, func(t *testing.T) {
			t.Parallel()
			caller := &refusingCaller{method: "turn/start"}
			m := pastedDraftModel(t, caller, paste.text)
			m, command := drive(t, m, press(tea.KeyEnter, tea.ModAlt, ""))
			m = runCommand(t, m, command, "alt+enter did not start the follow-up")
			if len(m.queue) != 1 {
				t.Fatalf("queue = %v after the refusal, want the follow-up back in it", m.queue)
			}
			m, _ = drive(t, m, press(tea.KeyUp, tea.ModAlt, ""))
			m, command = drive(t, m, press(tea.KeyEnter, 0, ""))
			runCommand(t, m, command, "Enter did not send the requeued follow-up; notice: "+m.notice)
			requireLastPrompt(t, caller.calls, 2, "turn/start", paste.text)
		})
	}
}

func TestFollowupStartedFromTheQueueIsRecalledAsPastedTextNotCommand(t *testing.T) {
	t.Parallel()
	for _, paste := range slashPastes {
		for _, recall := range historyRecalls {
			t.Run(paste.name+"/"+recall.name, func(t *testing.T) {
				t.Parallel()
				caller := new(fakeCaller)
				m := pastedDraftModel(t, caller, paste.text)
				m.thread.state = stateRunning
				m.thread.turnID = "turn-1"
				m, _ = drive(t, m, press(tea.KeyEnter, tea.ModAlt, ""))
				m.thread.state = stateIdle
				command := m.startQueuedFollowup()
				m = runCommand(t, m, command, "the queue did not start the follow-up")
				requireLastPrompt(t, caller.calls, 1, "turn/start", paste.text)
				m = recall.run(t, m)
				m, command = drive(t, m, press(tea.KeyEnter, 0, ""))
				runCommand(t, m, command, "Enter did not send the recalled follow-up; notice: "+m.notice)
				requireLastPrompt(t, caller.calls, 2, "turn/steer", paste.text)
			})
		}
	}
}

func TestSteeredChipDraftRecalledIsSentAsPastedTextNotCommand(t *testing.T) {
	t.Parallel()
	for _, paste := range slashPastes {
		for _, recall := range historyRecalls {
			t.Run(paste.name+"/"+recall.name, func(t *testing.T) {
				t.Parallel()
				caller := new(fakeCaller)
				m := pastedDraftModel(t, caller, paste.text)
				m.thread.state = stateRunning
				m.thread.turnID = "turn-1"
				m, command := drive(t, m, press(tea.KeyEnter, 0, ""))
				m = runCommand(t, m, command, "Enter did not steer with the chip draft")
				requireLastPrompt(t, caller.calls, 1, "turn/steer", paste.text)
				m = recall.run(t, m)
				m, command = drive(t, m, press(tea.KeyEnter, 0, ""))
				runCommand(t, m, command, "Enter did not send the recalled prompt; notice: "+m.notice)
				requireLastPrompt(t, caller.calls, 2, "turn/steer", paste.text)
			})
		}
	}
}

func attachedImageModel(t *testing.T, client caller) model {
	t.Helper()
	m := newModel(client, Options{}, nil)
	m.thread.id = "t1"
	m.thread.modelName = "gpt-test"
	m.thread.inputModalities = map[string][]string{"gpt-test": {"text", "image"}}
	m, _ = drive(t, m, tea.WindowSizeMsg{Width: 80, Height: 24})
	small := base64.StdEncoding.EncodeToString(pngBytes(t, 1, 1))
	m, _ = drive(t, m, imageMsg{mime: "image/png", data: small, width: 1, height: 1})
	if len(m.input.composer.images()) != 1 {
		t.Fatalf("composer holds %d image chips, want 1", len(m.input.composer.images()))
	}
	return m
}

func TestRefusedImageOnlyDraftKeepsTheCommandTypedMeanwhile(t *testing.T) {
	t.Parallel()
	for _, send := range refusedSends {
		t.Run(send.name, func(t *testing.T) {
			t.Parallel()
			caller := &refusingCaller{method: send.method}
			m := attachedImageModel(t, caller)
			m.thread.state = send.state
			m.thread.turnID = "turn-1"
			m, command := drive(t, m, press(tea.KeyEnter, 0, ""))
			m = typeText(t, m, "/new now")
			m = runCommand(t, m, command, "Enter did not send the image draft")
			m, _ = drive(t, m, press(tea.KeyEnter, 0, ""))
			ranCommand := m.confirm != nil && m.confirm.kind == confirmNew || strings.Contains(m.notice, "/new is unavailable while a turn is")
			if !ranCommand || len(caller.calls) != 1 {
				t.Fatalf("confirm = %#v and notice %q after %d requests, want the typed /new to run and no second request", m.confirm, m.notice, len(caller.calls))
			}
		})
	}
}

func TestRecalledDraftDropsItsMarkWhenAnImageIsAttachedAndRemoved(t *testing.T) {
	t.Parallel()
	for _, route := range []struct {
		name   string
		recall func(*testing.T, model) model
	}{
		{name: "history", recall: func(t *testing.T, m model) model {
			m.input.composer.recall(slashPastes[1].text, true)
			return m
		}},
		{name: "alt+up", recall: func(t *testing.T, m model) model {
			m.input.composer.SetValue("")
			m.queue = []followup{{text: slashPastes[1].text, verbatim: true}}
			m = rawUpdate(t, m, tea.KeyPressMsg{Code: tea.KeyUp, Mod: tea.ModAlt})
			if !m.input.composer.verbatim() || len(m.queue) != 0 {
				t.Fatalf("alt+up left verbatim %v and %d queued, want the follow-up back unchanged", m.input.composer.verbatim(), len(m.queue))
			}
			return m
		}},
	} {
		t.Run(route.name, func(t *testing.T) {
			t.Parallel()
			caller := new(fakeCaller)
			m := route.recall(t, attachedImageModel(t, caller))
			small := base64.StdEncoding.EncodeToString(pngBytes(t, 1, 1))
			m = rawUpdate(t, m, imageMsg{mime: "image/png", data: small, width: 1, height: 1})
			m = rawUpdate(t, m, press(tea.KeyBackspace, 0, ""))
			if got := m.input.composer.Value(); got != slashPastes[1].text || m.input.composer.verbatim() {
				t.Fatalf("composer holds %q verbatim %v, want the text back as typed text", got, m.input.composer.verbatim())
			}
			m, _ = drive(t, m, press(tea.KeyEnter, 0, ""))
			if m.confirm == nil || m.confirm.kind != confirmNew || len(caller.calls) != 0 {
				t.Fatalf("confirm = %#v after %d requests, want the new-thread confirmation of the typed /new", m.confirm, len(caller.calls))
			}
		})
	}
}
