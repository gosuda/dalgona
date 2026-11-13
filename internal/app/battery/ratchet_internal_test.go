package battery

import (
	"errors"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/agent"
	"github.com/gosuda/dalgona/internal/app"
	"github.com/gosuda/dalgona/internal/config"
	"github.com/gosuda/dalgona/internal/ratchet"
	"github.com/gosuda/dalgona/internal/tool"
	asktool "github.com/gosuda/dalgona/internal/tool/ask"
)

const testModel = "openai/gpt-test"

func ratchetHost(t *testing.T) (*agent.Host, string) {
	t.Helper()
	home, user := t.TempDir(), t.TempDir()
	t.Setenv("DALGONA_HOME", home)
	t.Setenv("HOME", user)
	t.Setenv("USERPROFILE", user)
	cwd := t.TempDir()
	host, closeHost, err := app.NewHost(t.Context(), app.Options{Product: config.ProductDalgona, Cwd: cwd, Batteries: (&Daemon{}).Load})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = closeHost() })
	return host, cwd
}

func TestRatchetToolOnlyOnTopLevelThreads(t *testing.T) {
	if got := ratchetTools(t.Context()); len(got) != 0 {
		t.Fatalf("ratchetTools without a thread = %d tools, want 0", len(got))
	}
	host, cwd := ratchetHost(t)
	top, err := host.Start(t.Context(), agent.ThreadSpec{Model: testModel, Cwd: cwd})
	if err != nil {
		t.Fatal(err)
	}
	got := ratchetTools(agent.ContextWithThread(t.Context(), top))
	if len(got) != 1 || got[0].Spec().Name != ratchet.Name {
		t.Fatalf("top-level ratchetTools = %v, want one ratchet tool", got)
	}
	child, err := host.Start(t.Context(), agent.ThreadSpec{Model: testModel, Cwd: cwd, Parent: top.ID()})
	if err != nil {
		t.Fatal(err)
	}
	if got := ratchetTools(agent.ContextWithThread(t.Context(), child)); len(got) != 0 {
		t.Fatalf("child ratchetTools = %d tools, want 0", len(got))
	}
	advisor, err := host.Start(t.Context(), agent.ThreadSpec{Model: testModel, Cwd: cwd, Parent: top.ID(), Advisor: "review", Tools: []tool.Tool{}})
	if err != nil {
		t.Fatal(err)
	}
	if got := ratchetTools(agent.ContextWithThread(t.Context(), advisor)); len(got) != 0 {
		t.Fatalf("advisor ratchetTools = %d tools, want 0", len(got))
	}
}

func TestThreadAskerMapsDecline(t *testing.T) {
	host, cwd := ratchetHost(t)
	top, err := host.Start(t.Context(), agent.ThreadSpec{Model: testModel, Cwd: cwd})
	if err != nil {
		t.Fatal(err)
	}
	asker := threadAsker{thread: top}
	question := ratchet.Question{Header: "Ratchet inputs", Text: "Approve the inputs?", Options: []ratchet.Option{{Label: "Approve"}, {Label: "Revise"}, {Label: "Abort"}}}
	_, err = asker.Ask(t.Context(), "call-1", question)
	if !errors.Is(err, ratchet.ErrNoClient) {
		t.Fatalf("Ask without a client: %v, want ErrNoClient", err)
	}
}

func TestThreadAskerRefusesOversizedQuestion(t *testing.T) {
	long := strings.Repeat("q", asktool.MaxQuestion+1)
	cases := []struct {
		name     string
		question ratchet.Question
		want     string
	}{
		{"empty text", ratchet.Question{Header: "h"}, "question text is empty"},
		{"long text", ratchet.Question{Header: "h", Text: long}, "question is longer than 500 characters"},
		{"long preview", ratchet.Question{Header: "h", Text: "t", Options: []ratchet.Option{{Label: "a", Preview: strings.Repeat("p", asktool.MaxPreview+1)}}}, "preview is longer than 2000 characters or 40 lines"},
		{"many preview lines", ratchet.Question{Header: "h", Text: "t", Options: []ratchet.Option{{Label: "a", Preview: strings.Repeat("x\n", asktool.MaxPreviewLines)}}}, "preview is longer than 2000 characters or 40 lines"},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			_, err := threadAsker{}.Ask(t.Context(), "call-1", c.question)
			if err == nil || !strings.Contains(err.Error(), c.want) {
				t.Fatalf("Ask error = %v, want it to contain %q", err, c.want)
			}
		})
	}
}

func TestAnswerFromReply(t *testing.T) {
	question := ratchet.Question{Options: []ratchet.Option{{Label: "Approve"}, {Label: "Revise"}, {Label: "Abort"}}}
	cases := []struct {
		name  string
		reply asktool.Reply
		want  ratchet.Answer
		err   error
	}{
		{"option label", asktool.Reply{Outcome: asktool.OutcomeAnswered, Answers: [][]string{{"Revise"}}}, ratchet.Answer{Label: "Revise"}, nil},
		{"typed text", asktool.Reply{Outcome: asktool.OutcomeAnswered, Answers: [][]string{{"add negatives"}}}, ratchet.Answer{Other: "add negatives"}, nil},
		{"empty list", asktool.Reply{Outcome: asktool.OutcomeAnswered, Answers: [][]string{{}}}, ratchet.Answer{}, nil},
		{"no answers", asktool.Reply{Outcome: asktool.OutcomeAnswered}, ratchet.Answer{}, nil},
		{"no client", asktool.Reply{Outcome: asktool.OutcomeNoClient}, ratchet.Answer{}, ratchet.ErrNoClient},
		{"cancelled", asktool.Reply{Outcome: asktool.OutcomeCancelled}, ratchet.Answer{}, ratchet.ErrCancelled},
		{"interrupted", asktool.Reply{Outcome: asktool.OutcomeInterrupted}, ratchet.Answer{}, ratchet.ErrCancelled},
	}
	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			got, err := answerFrom(question, c.reply)
			if c.err != nil {
				if !errors.Is(err, c.err) {
					t.Fatalf("error = %v, want %v", err, c.err)
				}
				return
			}
			if err != nil || got != c.want {
				t.Fatalf("answer = %+v, %v; want %+v", got, err, c.want)
			}
		})
	}
}
