package agent

import (
	"slices"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/auth"
	"github.com/gosuda/dalgona/internal/config"
	asktool "github.com/gosuda/dalgona/internal/tool/ask"
)

const askFeatureArgs = `{"questions":[{"header":"Database","question":"Which database should the service use?","options":[{"label":"SQLite","description":"One file, no server to run."},{"label":"Postgres","description":"A server with replication.","preview":"services:\n  db:\n    image: postgres:17"}]},{"header":"Features","question":"Which features ship first?","multi_select":true,"options":[{"label":"Search","description":"Full-text search."},{"label":"Export","description":"CSV export."},{"label":"Sync","description":"Two-way sync."}]},{"header":"Name","question":"What should the service be called?"}]}`

func requestToolNames(request map[string]any) []string {
	var names []string
	tools, _ := request["tools"].([]any)
	for _, item := range tools {
		if object, ok := item.(map[string]any); ok {
			name, _ := object["name"].(string)
			names = append(names, name)
		}
	}
	return names
}

func TestAskToolAnswerReachesModel(t *testing.T) {
	t.Parallel()
	h := newHarness(t)
	h.model.push(calls(call{id: "q1", name: "ask", args: askFeatureArgs}), text("done"))
	thread := h.start()
	updates := thread.Subscribe()
	h.submit(thread, "ask me")
	prompt := nextQuestionUpdate(t, updates, false)
	answer := asktool.Reply{Outcome: asktool.OutcomeAnswered, Answers: [][]string{{"Postgres"}, {"Search", "Export"}, {"ledger-svc"}}}
	if err := thread.AnswerQuestion(prompt.RequestID, answer); err != nil {
		t.Fatalf("AnswerQuestion: %v", err)
	}
	h.waitIdle(thread)
	want := "User answers:\n- Database (Which database should the service use?): Postgres\n- Features (Which features ship first?): Search; Export\n- Name (What should the service be called?): \"ledger-svc\""
	if got := callOutputs(h.model.request(1))["q1"]; got != want {
		t.Fatalf("ask output = %q, want %q", got, want)
	}
}

func TestAskToolDeclinesWithoutClient(t *testing.T) {
	t.Parallel()
	h := newHarness(t)
	h.model.push(calls(call{id: "q1", name: "ask", args: askFeatureArgs}), text("done"))
	thread := h.start()
	h.submit(thread, "ask me")
	h.waitIdle(thread)
	if got := callOutputs(h.model.request(1))["q1"]; got != asktool.TextNoClient {
		t.Fatalf("ask output = %q, want %q", got, asktool.TextNoClient)
	}
}

func TestAskToolOnlyOnTopLevelThreads(t *testing.T) {
	t.Parallel()
	h := newHarness(t)
	script := newChildScript(map[string][]response{
		"child a": {calls(call{id: "qc", name: "ask", args: askFeatureArgs}), yieldData("ya", `"A"`)},
	})
	h.model.route = script.route
	h.model.push(calls(call{id: "ta", name: "task", args: `{"prompt":"child a"}`}), text("parent done"))
	h.idleThread("spawn")
	var parentTools, childTools []string
	var childOutputs map[string]string
	for i := range h.model.count() {
		request := h.model.request(i)
		prompts := userTexts(request)
		switch {
		case slices.Contains(prompts, "spawn") && parentTools == nil:
			parentTools = requestToolNames(request)
		case slices.Contains(prompts, "child a"):
			if childTools == nil {
				childTools = requestToolNames(request)
			}
			childOutputs = callOutputs(request)
		}
	}
	if !slices.Contains(parentTools, asktool.Name) {
		t.Fatalf("parent tools = %v, want ask", parentTools)
	}
	if slices.Contains(childTools, asktool.Name) {
		t.Fatalf("child tools = %v, want no ask", childTools)
	}
	if out := childOutputs["qc"]; !strings.Contains(out, "Tool ask not found") {
		t.Fatalf("child ask output = %q, want Tool ask not found", out)
	}
}

func TestAskToolInDal(t *testing.T) {
	t.Parallel()
	h := newHarness(t, func(h *harness) { h.cfg.Config.Product = config.ProductDal })
	h.idleThread("hi")
	if tools := requestToolNames(h.model.request(0)); !slices.Contains(tools, asktool.Name) {
		t.Fatalf("dal tools = %v, want ask", tools)
	}
}

func TestAskQuestionTextIsMasked(t *testing.T) {
	t.Parallel()
	const secret = "token-abcdefgh1234"
	store, err := auth.NewWithPatterns(t.TempDir(), []string{`token-[a-z0-9]{8,}`})
	if err != nil {
		t.Fatal(err)
	}
	h := newHarness(t, func(h *harness) { h.cfg.Auth = store })
	thread := h.start()
	updates := thread.Subscribe()
	questions := []asktool.Question{{
		Header: secret,
		Text:   "Which key?",
		Options: []asktool.Option{
			{Label: secret, Description: "the key"},
			{Label: "none", Description: "no key"},
		},
	}}
	results := askAsync(t.Context(), thread, "call-1", questions)
	prompt := nextQuestionUpdate(t, updates, false)
	if strings.Contains(prompt.Questions[0].Header, secret) || !strings.Contains(prompt.Questions[0].Header, "{{secret:") {
		t.Fatalf("header = %q, want only a placeholder", prompt.Questions[0].Header)
	}
	if label := prompt.Questions[0].Options[0].Label; strings.Contains(label, secret) || !strings.Contains(label, "{{secret:") {
		t.Fatalf("label = %q, want only a placeholder", label)
	}
	if err := thread.AnswerQuestion(prompt.RequestID, asktool.Reply{Outcome: asktool.OutcomeCancelled}); err != nil {
		t.Fatalf("AnswerQuestion: %v", err)
	}
	waitQuestionResult(t, results)
}
