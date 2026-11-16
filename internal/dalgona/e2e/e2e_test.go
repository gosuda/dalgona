package e2e

import (
	"bytes"
	"context"
	json "encoding/json/v2"
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"regexp"
	"slices"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/runner"
	"github.com/gosuda/dalgona/internal/dal/tool"
	"github.com/gosuda/dalgona/internal/dalgona/ratchet"
	"github.com/gosuda/dalgona/internal/llmstub"
)

const (
	stubKey       = "sk-live-test"
	fixtureScript = "testdata/live.json"
	fixtureHome   = "testdata/workspace"
	liveDoc       = "../../../docs/dal/spec/live-debug.md"
)

func loadFixture(t *testing.T) llmstub.Script {
	t.Helper()
	raw, err := os.ReadFile(fixtureScript)
	if err != nil {
		t.Fatal(err)
	}
	script, err := llmstub.Parse(fixtureScript, raw)
	if err != nil {
		t.Fatal(err)
	}
	return script
}

func newStub(t *testing.T, script llmstub.Script, record *bytes.Buffer) *httptest.Server {
	t.Helper()
	server := httptest.NewServer(llmstub.NewHandler(script, llmstub.Options{Key: stubKey, Record: record}))
	t.Cleanup(server.Close)
	return server
}

func postChat(t *testing.T, server *httptest.Server, body string) (int, string) {
	t.Helper()
	request, err := http.NewRequest("POST", server.URL+"/v1/chat/completions", strings.NewReader(body))
	if err != nil {
		t.Fatal(err)
	}
	request.Header.Set("Content-Type", "application/json")
	request.Header.Set("Authorization", "Bearer "+stubKey)
	response, err := http.DefaultClient.Do(request)
	if err != nil {
		t.Fatal(err)
	}
	defer response.Body.Close()
	data, err := io.ReadAll(response.Body)
	if err != nil {
		t.Fatal(err)
	}
	return response.StatusCode, string(data)
}

func recordedTurns(t *testing.T, record *bytes.Buffer) []string {
	t.Helper()
	var turns []string
	for _, line := range bytes.Split(record.Bytes(), []byte("\n")) {
		line = bytes.TrimSpace(line)
		if len(line) == 0 {
			continue
		}
		var entry struct {
			Turn string `json:"turn"`
		}
		if err := json.Unmarshal(line, &entry); err != nil {
			t.Fatalf("decode record %s: %v", line, err)
		}
		turns = append(turns, entry.Turn)
	}
	return turns
}

func lastRecordedTurn(t *testing.T, record *bytes.Buffer) string {
	t.Helper()
	turns := recordedTurns(t, record)
	if len(turns) == 0 {
		t.Fatal("no scripted request was recorded")
	}
	return turns[len(turns)-1]
}

func TestLiveScriptParses(t *testing.T) {
	t.Parallel()
	script := loadFixture(t)
	got := []string{}
	for _, model := range script.Models {
		got = append(got, model.ID)
	}
	if !slices.Equal(got, []string{"qwen-test", "qwen-small"}) {
		t.Fatalf("models = %v, want the two scripted models", got)
	}
	if len(script.Turns) != 43 {
		t.Fatalf("turns = %d, want the full battery script", len(script.Turns))
	}
	seen := map[string]bool{}
	for _, turn := range script.Turns {
		if turn.ID == "" || seen[turn.ID] {
			t.Fatalf("turn id %q is empty or duplicated", turn.ID)
		}
		seen[turn.ID] = true
	}
}

func TestLiveScriptRoutesScriptedRequests(t *testing.T) {
	t.Parallel()
	script := loadFixture(t)
	cases := []struct {
		name string
		body string
		want string
		code int
	}{
		{"edit judge request", `{"messages":[{"role":"system","content":"judge it"},{"role":"user","content":"check {\"off_task\":0}"}]}`, "t-judge", 200},
		{"turn judge request", `{"messages":[{"role":"system","content":"judge it"},{"role":"user","content":"check {\"incomplete\":0}"}]}`, "t-judge-turn", 200},
		{"working model prompt", `{"messages":[{"role":"user","content":"edit notes"}]}`, "t-patch", 200},
		{"history prompt", `{"messages":[{"role":"user","content":"hello"}]}`, "t-hello", 200},
		{"longer hello prompt", `{"messages":[{"role":"user","content":"say hello"}]}`, "", 400},
		{"unmatched prompt", `{"messages":[{"role":"user","content":"unrelated unscripted prompt"}]}`, "", 400},
		{"compaction summarizer request", `{"messages":[{"role":"system","content":"Write a detailed summary of the conversation."},{"role":"user","content":"<conversation>\n[user] read a.txt\n[user] take your time\n</conversation>\n"}]}`, "t-compact", 200},
		{"history read prompt", `{"messages":[{"role":"user","content":"read a.txt"}]}`, "t-read", 200},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			var record bytes.Buffer
			server := newStub(t, script, &record)
			code, _ := postChat(t, server, tc.body)
			if code != tc.code {
				t.Fatalf("status = %d, want %d", code, tc.code)
			}
			if tc.want == "" {
				if got := lastRecordedTurn(t, &record); got != "unmatched" {
					t.Fatalf("unmatched request recorded turn %q, want unmatched", got)
				}
				return
			}
			if got := lastRecordedTurn(t, &record); got != tc.want {
				t.Fatalf("routed turn = %q, want %q", got, tc.want)
			}
		})
	}
}

func TestLiveSubagentTurnYields(t *testing.T) {
	t.Parallel()
	script := loadFixture(t)
	var child *llmstub.Turn
	for i := range script.Turns {
		if strings.Contains(script.Turns[i].When.User, "helper task: count files") || strings.Contains(script.Turns[i].When.AnyText, "helper task: count files") {
			child = &script.Turns[i]
		}
	}
	if child == nil {
		t.Fatal("no scripted turn answers the subagent request")
	}
	for _, step := range child.Steps {
		if step.ToolCall != nil && step.ToolCall.Name == "yield" {
			return
		}
	}
	t.Fatalf("subagent turn %s never calls yield, so the child ends as subagent ended without yield", child.ID)
}

var turnReference = regexp.MustCompile(`(?:^|[^A-Za-z0-9_-])(t-[a-z0-9-]+(?:-[a-z0-9]+)*)`)

func namedTurns(text string) []string {
	var ids []string
	for _, match := range turnReference.FindAllStringSubmatch(text, -1) {
		if !slices.Contains(ids, match[1]) {
			ids = append(ids, match[1])
		}
	}
	return ids
}

func docSections(t *testing.T, titles ...string) string {
	t.Helper()
	raw, err := os.ReadFile(liveDoc)
	if err != nil {
		t.Fatal(err)
	}
	doc := string(raw)
	var out strings.Builder
	for _, title := range titles {
		parts := strings.SplitN(doc, "## "+title, 2)
		if len(parts) != 2 {
			t.Fatalf("%s has no section %q", liveDoc, title)
		}
		rest, _, _ := strings.Cut(parts[1], "\n## ")
		out.WriteString(rest)
	}
	return out.String()
}

func checkDrift(t *testing.T, script llmstub.Script, tables string) {
	t.Helper()
	named := namedTurns(tables)
	for _, id := range named {
		if !slices.ContainsFunc(script.Turns, func(turn llmstub.Turn) bool { return turn.ID == id }) {
			t.Errorf("turn %s is named in %s but is not in %s", id, liveDoc, fixtureScript)
		}
	}
	for _, turn := range script.Turns {
		if turn.When.ToolResult != "" || slices.Contains(named, turn.ID) {
			continue
		}
		prompt := turn.When.User
		if prompt == "" {
			prompt = turn.When.AnyText
		}
		if !strings.Contains(tables, prompt) {
			t.Errorf("turn %s in %s is driven by no session row in %s", turn.ID, fixtureScript, liveDoc)
		}
	}
}

func TestLiveSessionTurnsExist(t *testing.T) {
	t.Parallel()
	script := loadFixture(t)
	checkDrift(t, script, docSections(t, "Dal session", "Dalgona session"))
}

func TestLiveEditReviewContinuationKeepsJudgeRouting(t *testing.T) {
	t.Parallel()
	script := loadFixture(t)
	feedback := `<edit-review call=\"call-patch\" paths=\"notes.md\">`
	cases := []struct {
		body string
		want string
	}{
		{`{"messages":[{"role":"user","content":"` + feedback + `"}]}`, "t-edit-review-continue"},
		{`{"messages":[{"role":"user","content":"` + feedback + `"},{"role":"user","content":"{\"off_task\":0}"}]}`, "t-judge"},
		{`{"messages":[{"role":"user","content":"` + feedback + `"},{"role":"user","content":"{\"incomplete\":0}"}]}`, "t-judge-turn"},
	}
	for _, tc := range cases {
		var record bytes.Buffer
		server := newStub(t, script, &record)
		code, _ := postChat(t, server, tc.body)
		if code != 200 {
			t.Fatalf("routed request status = %d, want 200", code)
		}
		if got := lastRecordedTurn(t, &record); got != tc.want {
			t.Fatalf("routed turn = %q, want %q", got, tc.want)
		}
	}
	var record bytes.Buffer
	server := newStub(t, script, &record)
	code, _ := postChat(t, server, `{"messages":[{"role":"user","content":"unrelated feedback"}]}`)
	if code != 400 {
		t.Fatalf("unrelated feedback status = %d, want 400", code)
	}
	for _, id := range []string{"t-edit-review-continue", "t-judge", "t-judge-turn"} {
		i := slices.IndexFunc(script.Turns, func(turn llmstub.Turn) bool { return turn.ID == id })
		if i < 0 {
			t.Fatalf("fixture lacks turn %s", id)
		}
		steps := script.Turns[i].Steps
		if len(steps) != 2 || steps[len(steps)-1].Finish != "stop" {
			t.Fatalf("%s does not end with a normal reply: %+v", id, steps)
		}
	}
}

type liveRatchetAsker struct {
	t     *testing.T
	calls int
}

func (a *liveRatchetAsker) Ask(_ context.Context, _ string, q ratchet.Question) (ratchet.Answer, error) {
	a.calls++
	if len(q.Options) != 3 || q.Options[0].Label != "Approve" || q.Options[1].Label != "Revise" || q.Options[2].Label != "Abort" {
		a.t.Fatalf("approval options = %+v", q.Options)
	}
	return ratchet.Answer{Label: "Approve"}, nil
}

func (liveRatchetAsker) PreviewRoom() (int, int) { return 2000, 40 }

func ratchetEnv(t *testing.T) tool.Env {
	t.Helper()
	local, err := runner.NewLocal(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	env := tool.Env{Cwd: t.TempDir(), Runner: local}
	if err := os.CopyFS(env.Cwd, os.DirFS(fixtureHome)); err != nil {
		t.Fatal(err)
	}
	return env
}

type ratchetRoute struct {
	body string
	want string
	call string
}

func routeRatchetTurn(t *testing.T, script llmstub.Script, tc ratchetRoute) {
	t.Helper()
	var record bytes.Buffer
	server := newStub(t, script, &record)
	code, _ := postChat(t, server, tc.body)
	if code != 200 {
		t.Fatalf("routed request status = %d, want 200", code)
	}
	if got := lastRecordedTurn(t, &record); got != tc.want {
		t.Fatalf("routed turn = %q, want %q", got, tc.want)
	}
}

func runRatchetCall(t *testing.T, script llmstub.Script, editor ratchet.Tool, env tool.Env, tc ratchetRoute) {
	t.Helper()
	i := slices.IndexFunc(script.Turns, func(turn llmstub.Turn) bool { return turn.ID == tc.want })
	if i < 0 {
		t.Fatalf("fixture lacks turn %s", tc.want)
	}
	call := script.Turns[i].Steps[0].ToolCall
	if call == nil || call.Name != "ratchet" || call.ID != tc.call {
		t.Fatalf("expected direct ratchet call %s, got %+v", tc.call, call)
	}
	result, err := editor.Run(t.Context(), tool.Call{ID: call.ID, Name: call.Name, Args: call.Arguments}, env)
	if err != nil || !result.OK {
		t.Fatalf("%s: result %+v, error %v", call.ID, result, err)
	}
	if call.ID == "call-ratchet-approve" && !bytes.Contains(result.Output, []byte(`"approved":true`)) {
		t.Fatalf("approval result = %s", result.Output)
	}
}

func driveRatchetRoute(t *testing.T, script llmstub.Script, editor ratchet.Tool, env tool.Env, tc ratchetRoute) {
	t.Helper()
	routeRatchetTurn(t, script, tc)
	if tc.call == "" {
		return
	}
	runRatchetCall(t, script, editor, env, tc)
}

func TestLiveRatchetRequestsDirectApproval(t *testing.T) {
	t.Parallel()
	script := loadFixture(t)
	env := ratchetEnv(t)
	asker := &liveRatchetAsker{t: t}
	editor := ratchet.New(asker)
	for _, tc := range []ratchetRoute{
		{`{"messages":[{"role":"user","content":"start a ratchet"}]}`, "t-ratchet", "call-ratchet"},
		{`{"messages":[{"role":"tool","content":"done","tool_call_id":"call-ratchet"}]}`, "t-ratchet-2", "call-ratchet-approve"},
		{`{"messages":[{"role":"tool","content":"done","tool_call_id":"call-ratchet-approve"}]}`, "t-ratchet-approved", ""},
	} {
		driveRatchetRoute(t, script, editor, env, tc)
	}
	if asker.calls != 1 {
		t.Fatalf("approval questions = %d, want 1", asker.calls)
	}
}

func TestLiveWorkspaceMatchesFixture(t *testing.T) {
	t.Parallel()
	for _, name := range []string{"a.txt", "fail-output.sh", "notes.md", "pixel.png"} {
		if _, err := os.Stat(filepath.Join(fixtureHome, name)); err != nil {
			t.Fatalf("workspace lacks %s: %v", name, err)
		}
	}
	raw, err := os.ReadFile(filepath.Join(fixtureHome, "a.txt"))
	if err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(string(raw), "hello from a.txt") {
		t.Fatalf("a.txt lacks the scripted read result")
	}
}
