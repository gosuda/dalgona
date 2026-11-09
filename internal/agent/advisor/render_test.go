package advisor_test

import (
	"fmt"
	"math/rand/v2"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/agent/advisor"
	"github.com/gosuda/dalgona/internal/agent/event"
)

const blobDigest = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08"

func note(advisorName, severity, text string) event.AdvisorNote {
	return event.AdvisorNote{Advisor: advisorName, Severity: severity, Note: text, ReviewSeq: 1, Delivery: "aside"}
}

func sequence(t *testing.T, events ...event.Event) []event.Seqd {
	t.Helper()
	out := make([]event.Seqd, len(events))
	for i, e := range events {
		if err := e.Validate(); err != nil {
			t.Fatalf("fixture %d %s: %v", i, e.Type(), err)
		}
		out[i] = event.Seqd{Seq: uint64(i + 1), Event: e}
	}
	return out
}

func TestRender_format(t *testing.T) {
	t.Parallel()
	exit := 2
	events := sequence(t,
		event.TurnStarted{TurnID: "t1"},
		event.UserMessage{ItemID: "u1", TurnID: "t1", Text: "Fix <main> & test", Kind: "prompt"},
		event.AssistantMessage{ItemID: "a1", TurnID: "t1", Text: "Reading.", Reasoning: "Need the file.", ToolCalls: []event.ToolCall{{ID: "c1", Name: "read", Args: []byte(`{"path":"a<b.go"}`)}}, StopReason: "tool_use"},
		event.ToolStarted{CallID: "c1", TurnID: "t1", Tool: "read", Args: []byte(`{"path":"a<b.go"}`), Class: "read"},
		event.ToolCompleted{CallID: "c1", OK: false, Output: "no such file", ExitCode: &exit, Recovered: "none"},
		note("advisor", "concern", "Check the path."),
		event.UserMessage{ItemID: "u2", TurnID: "t1", Text: note("advisor", "concern", "Check the path.").Render(), Kind: "steer"},
		event.UserMessage{ItemID: "u3", TurnID: "t1", TextBlob: &event.BlobRef{Blob: blobDigest, Bytes: 70000}, Kind: "followup"},
		event.TurnCompleted{TurnID: "t1", Status: "interrupted"},
		event.Compaction{FirstKeptSeq: 1, Summary: "Earlier work.", Method: "auto"},
		event.TurnCompleted{TurnID: "t2", Status: "completed"},
	)
	want := "<user>\nFix &lt;main&gt; &amp; test\n</user>\n" +
		"<reasoning>\nNeed the file.\n</reasoning>\n" +
		"<assistant>\nReading.\n</assistant>\n" +
		"<tool_call name=\"read\" id=\"c1\">\n{\"path\":\"a&lt;b.go\"}\n</tool_call>\n" +
		"<tool_result id=\"c1\" ok=\"false\" exit=\"2\">\nno such file\n</tool_result>\n" +
		"<user>\n[70000 bytes in blob " + blobDigest + "]\n</user>\n" +
		"<turn_end status=\"interrupted\"></turn_end>\n" +
		"<summary>\nEarlier work.\n</summary>\n"
	if got := advisor.Render(events); got != want {
		t.Fatalf("Render =\n%s\nwant\n%s", got, want)
	}
}

func TestRender_stripsAdvisoriesFromUserText(t *testing.T) {
	t.Parallel()
	text := "first\n" + note("a", "nit", "drop me").Render() + "\n\n" +
		note("b", "blocker", "drop me too").Render() + "  second  \n<advisory unclosed"
	got := advisor.Render(sequence(t, event.UserMessage{ItemID: "u", TurnID: "t", Text: text, Kind: "steer"}))
	want := "<user>\nfirst\nsecond  \n&lt;advisory unclosed\n</user>\n"
	if got != want {
		t.Fatalf("Render = %q, want %q", got, want)
	}
}

func TestRender_randomHistoriesNeverRepeatDeliveredNotes(t *testing.T) {
	t.Parallel()
	random := rand.New(rand.NewPCG(7, 33))
	for round := range 200 {
		events, notes, kept := randomHistory(t, random, round)
		got := advisor.Render(events)
		for _, note := range notes {
			if strings.Contains(got, note) {
				t.Fatalf("round %d: delivered note %q rendered:\n%s", round, note, got)
			}
		}
		for _, text := range kept {
			if !strings.Contains(got, text) {
				t.Fatalf("round %d: transcript text %q missing:\n%s", round, text, got)
			}
		}
	}
}

func randomHistory(t *testing.T, random *rand.Rand, round int) ([]event.Seqd, []string, []string) {
	var events []event.Event
	var notes, kept []string
	for step := range 1 + random.IntN(12) {
		text := fmt.Sprintf("text-%d-%d", round, step)
		noteText := fmt.Sprintf("note-%d-%d", round, step)
		switch random.IntN(5) {
		case 0:
			events = append(events, event.UserMessage{ItemID: text, TurnID: "t", Text: text, Kind: "prompt"})
			kept = append(kept, text)
		case 1:
			events = append(events, event.UserMessage{ItemID: text, TurnID: "t", Text: note("advisor", "concern", noteText).Render() + text, Kind: "steer"})
			notes, kept = append(notes, noteText), append(kept, text)
		case 2:
			events = append(events, note("advisor", "nit", noteText))
			notes = append(notes, noteText)
		case 3:
			events = append(events, event.AssistantMessage{ItemID: text, TurnID: "t", Text: text, StopReason: "end_turn"})
			kept = append(kept, text)
		default:
			events = append(events, event.ToolCompleted{CallID: text, OK: true, Output: text, Recovered: "none"})
			kept = append(kept, text)
		}
	}
	return sequence(t, events...), notes, kept
}
