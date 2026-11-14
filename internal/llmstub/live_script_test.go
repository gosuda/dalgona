package llmstub

import (
	"context"
	"errors"
	"os"
	"slices"
	"testing"

	jsontext "encoding/json/jsontext"
	json "encoding/json/v2"

	"github.com/gosuda/dalgona/internal/decide"
	"github.com/gosuda/dalgona/internal/model"
)

const liveScript = "testdata/live.json"

func loadLiveScript(t *testing.T) Script {
	t.Helper()
	raw, err := os.ReadFile(liveScript)
	if err != nil {
		t.Fatal(err)
	}
	script, err := Parse(liveScript, raw)
	if err != nil {
		t.Fatal(err)
	}
	return script
}

var errRecorded = errors.New("llmstub test: request recorded")

type recordingProvider struct {
	request model.Request
}

func (p *recordingProvider) Stream(_ context.Context, request model.Request) (model.Stream, error) {
	p.request = request
	return nil, errRecorded
}

func (p *recordingProvider) Models(context.Context) ([]model.Model, error) {
	return nil, nil
}

type wireMessage struct {
	Role    string `json:"role"`
	Content string `json:"content"`
}

type wireBody struct {
	Messages []wireMessage `json:"messages"`
}

func noulQuestion(instructions, yes, no string) decide.Question {
	return decide.Question{
		Type:         decide.TypeNoul,
		Instructions: instructions,
		Criteria:     map[string]string{"true": yes, "false": no},
	}
}

func judgeBody(t *testing.T, state string, questions map[string]decide.Question) []byte {
	t.Helper()
	provider := &recordingProvider{}
	_, err := decide.NewLLM(provider, "qwen-test").Ask(t.Context(), jsontext.Value(state), questions)
	if !errors.Is(err, errRecorded) {
		t.Fatalf("Ask error = %v, want the recorded request", err)
	}
	raw, err := json.Marshal(wireBody{Messages: []wireMessage{
		{Role: "system", Content: provider.request.System},
		{Role: "user", Content: provider.request.Messages[0].Text},
	}})
	if err != nil {
		t.Fatal(err)
	}
	return raw
}

func firstMatch(turns []Turn, view requestView) string {
	for _, turn := range turns {
		if matches(turn.When, view) {
			return turn.ID
		}
	}
	return ""
}

func TestLiveScriptRoutesScriptedRequests(t *testing.T) {
	t.Parallel()
	script := loadLiveScript(t)
	if len(script.Models) != 2 || len(script.Turns) == 0 {
		t.Fatalf("models = %d, turns = %d", len(script.Models), len(script.Turns))
	}
	turns := script.Turns
	editState := `{"task":"edit notes","tool":"apply_patch","files":[{"path":"notes.md","kind":"text","change":"update","added":1,"removed":1,"diff":"@@\n-old line\n+new line\n"}]}`
	turnState := `{"task":"edit notes","files":[{"path":"notes.md","kind":"text","change":"update","added":1,"removed":1,"diff":"@@\n-old line\n+new line\n"}]}`
	editQuestions := map[string]decide.Question{
		"sound":       noulQuestion("Do the added lines serve the task?", "They serve it.", "They do not."),
		"lost_code":   noulQuestion("Is code removed?", "Yes.", "No."),
		"placeholder": noulQuestion("Is a placeholder added?", "Yes.", "No."),
		"off_task":    noulQuestion("Is off-task code changed?", "Yes.", "No."),
	}
	turnQuestions := map[string]decide.Question{
		"sound":      noulQuestion("Do the added lines serve the task?", "They serve it.", "They do not."),
		"incomplete": noulQuestion("Is a named change missing?", "Yes.", "No."),
	}
	cases := []struct {
		name string
		body func(t *testing.T) []byte
		want string
	}{
		{"edit judge request", func(t *testing.T) []byte { return judgeBody(t, editState, editQuestions) }, "t-judge"},
		{"turn judge request", func(t *testing.T) []byte { return judgeBody(t, turnState, turnQuestions) }, "t-judge-turn"},
		{"working model prompt", func(*testing.T) []byte { return []byte(`{"messages":[{"role":"user","content":"edit notes"}]}`) }, "t-patch"},
		{"compaction summarizer request", func(*testing.T) []byte {
			return []byte(`{"messages":[{"role":"system","content":"Write a detailed summary of the conversation."},{"role":"user","content":"<conversation>\n[user] read a.txt\n[user] take your time\n</conversation>\n"}]}`)
		}, "t-compact"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			view, err := decodeRequest(tc.body(t))
			if err != nil {
				t.Fatal(err)
			}
			if got := firstMatch(turns, view); got != tc.want {
				t.Fatalf("first matching turn = %q, want %q", got, tc.want)
			}
		})
	}
}

func TestLiveSubagentTurnYields(t *testing.T) {
	t.Parallel()
	turns := loadLiveScript(t).Turns
	view, err := decodeRequest([]byte(`{"messages":[{"role":"user","content":"helper task: count files"}]}`))
	if err != nil {
		t.Fatal(err)
	}
	id := firstMatch(turns, view)
	index := slices.IndexFunc(turns, func(turn Turn) bool { return turn.ID == id })
	if index < 0 {
		t.Fatal("no scripted turn answers the subagent request")
	}
	yields := slices.ContainsFunc(turns[index].Steps, func(step Step) bool {
		return step.ToolCall != nil && step.ToolCall.Name == "yield"
	})
	if !yields {
		t.Fatalf("subagent turn %s never calls yield, so the child ends as subagent ended without yield", id)
	}
}
