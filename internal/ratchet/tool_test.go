package ratchet

import (
	"context"
	"encoding/json/jsontext"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/runner"
	"github.com/gosuda/dalgona/internal/tool"
)

const initJSON = `{"action":"init","flow":"x","cases":["cases.txt"],"harness":["run.sh"],"change":["p.md"],"off_limits":["notes.md"]}`

type fakeAsker struct {
	answer   Answer
	err      error
	calls    int
	question Question
}

func (a *fakeAsker) Ask(_ context.Context, _ string, q Question) (Answer, error) {
	a.calls++
	a.question = q
	return a.answer, a.err
}

func initFiles() map[string]string {
	return map[string]string{"cases.txt": "c\n", "run.sh": "r\n", "p.md": "one\n", "notes.md": "n\n"}
}

func makeWorkspace(t *testing.T, files map[string]string) string {
	t.Helper()
	dir := t.TempDir()
	for name, body := range files {
		p := filepath.Join(dir, filepath.FromSlash(name))
		if err := os.MkdirAll(filepath.Dir(p), 0o750); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(p, []byte(body), 0o600); err != nil {
			t.Fatal(err)
		}
	}
	return dir
}

func toolEnv(t *testing.T, ws string) tool.Env {
	t.Helper()
	local, err := runner.NewLocal(filepath.Join(t.TempDir(), "runs"))
	if err != nil {
		t.Fatal(err)
	}
	return tool.Env{Runner: local, Cwd: ws}
}

func callTool(t *testing.T, tl Tool, env tool.Env, callID, args string) (tool.Result, error) {
	t.Helper()
	return tl.Run(t.Context(), tool.Call{ID: callID, Name: Name, Args: jsontext.Value(args)}, env)
}

func mustCall(t *testing.T, tl Tool, env tool.Env, callID, args string) string {
	t.Helper()
	res, err := callTool(t, tl, env, callID, args)
	if err != nil {
		t.Fatalf("%s: %v", args, err)
	}
	return string(res.Output)
}

func openTestFlow(t *testing.T, ws string) (*flow, *state) {
	t.Helper()
	f, err := openFlow(ws, "x")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = f.close() })
	s, err := f.load()
	if err != nil {
		t.Fatal(err)
	}
	return f, s
}

func expectedDigest(t *testing.T, ws, stage string) string {
	t.Helper()
	f, s := openTestFlow(t, ws)
	sha, err := f.digest(s, stage)
	if err != nil {
		t.Fatal(err)
	}
	return sha
}

func stageStatus(t *testing.T, ws, stage string) string {
	t.Helper()
	f, s := openTestFlow(t, ws)
	status, err := f.approvals(s)
	if err != nil {
		t.Fatal(err)
	}
	return status[stage]
}

func writeVariant(t *testing.T, ws, variant string, lines []string) {
	t.Helper()
	dir := filepath.Join(ws, ".dalgona", "ratchet", "x", variant)
	if err := os.MkdirAll(dir, 0o750); err != nil {
		t.Fatal(err)
	}
	body := strings.Join(lines, "\n") + "\n"
	if err := os.WriteFile(filepath.Join(dir, "results.jsonl"), []byte(body), 0o600); err != nil {
		t.Fatal(err)
	}
}

func TestApproveOutcomes(t *testing.T) {
	t.Parallel()
	cases := []struct {
		name     string
		answer   Answer
		err      error
		output   func(sha string) string
		ok       bool
		recorded bool
	}{
		{
			name:   "approve records the digest",
			answer: Answer{Label: "Approve"},
			output: func(sha string) string {
				return `{"approved":true,"asked":true,"sha":"` + sha + `","stage":"inputs"}`
			},
			ok:       true,
			recorded: true,
		},
		{
			name:   "revise sends feedback",
			answer: Answer{Label: "Revise", Other: "add negatives"},
			output: func(string) string {
				return `{"approved":false,"asked":true,"feedback":"add negatives","stage":"inputs"}`
			},
			ok: true,
		},
		{
			name:   "abort records nothing",
			answer: Answer{Label: "Abort"},
			output: func(string) string {
				return `{"aborted":true,"approved":false,"asked":true,"stage":"inputs"}`
			},
			ok: true,
		},
		{
			name: "no client declines",
			err:  ErrNoClient,
			output: func(string) string {
				return "declined: ratchet approvals need an interactive client"
			},
		},
		{
			name: "cancel records nothing",
			err:  ErrCancelled,
			output: func(string) string {
				return "cancelled: the question was closed; nothing was recorded"
			},
		},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			ws := makeWorkspace(t, initFiles())
			env := toolEnv(t, ws)
			asker := &fakeAsker{answer: tc.answer, err: tc.err}
			tl := New(asker)
			mustCall(t, tl, env, "c1", initJSON)
			sha := expectedDigest(t, ws, stageInputs)
			res, err := callTool(t, tl, env, "c2", `{"action":"approve","flow":"x","stage":"inputs","question":"Approve the cases?"}`)
			if err != nil {
				t.Fatal(err)
			}
			if got := string(res.Output); got != tc.output(sha) || res.OK != tc.ok {
				t.Fatalf("output = %q ok %v, want %q ok %v", got, res.OK, tc.output(sha), tc.ok)
			}
			if got := stageStatus(t, ws, stageInputs) == "current"; got != tc.recorded {
				t.Fatalf("recorded = %v, want %v", got, tc.recorded)
			}
		})
	}
}

func TestApproveAlreadyCurrentSkipsQuestion(t *testing.T) {
	t.Parallel()
	ws := makeWorkspace(t, initFiles())
	env := toolEnv(t, ws)
	asker := &fakeAsker{answer: Answer{Label: "Approve"}}
	tl := New(asker)
	mustCall(t, tl, env, "c1", initJSON)
	mustCall(t, tl, env, "c2", `{"action":"approve","flow":"x","stage":"inputs","question":"Approve the cases?"}`)
	sha := expectedDigest(t, ws, stageInputs)
	got := mustCall(t, tl, env, "c3", `{"action":"approve","flow":"x","stage":"inputs","question":"Approve the cases?"}`)
	if want := `{"approved":true,"asked":false,"sha":"` + sha + `","stage":"inputs"}`; got != want {
		t.Fatalf("second approve = %q, want %q", got, want)
	}
	if asker.calls != 1 {
		t.Fatalf("asker calls = %d, want 1", asker.calls)
	}
}

func TestApprovePlanPreview(t *testing.T) {
	t.Parallel()
	ws := makeWorkspace(t, initFiles())
	env := toolEnv(t, ws)
	asker := &fakeAsker{answer: Answer{Label: "Abort"}}
	tl := New(asker)
	mustCall(t, tl, env, "c1", initJSON)
	mustCall(t, tl, env, "c2", `{"action":"split","flow":"x","strata":{"a":"t","b":"t","c":"t","d":"t"},"test_fraction":0.25,"seed":7}`)
	mustCall(t, tl, env, "c3", `{"action":"plan","flow":"x","target":"score","direction":"higher","command":"run {variant}","reps":1}`)
	mustCall(t, tl, env, "c4", `{"action":"approve","flow":"x","stage":"plan","question":"Approve the plan?"}`)
	preview := asker.question.Options[0].Preview
	if !strings.HasPrefix(preview, "Stage: plan\nDigest: ") {
		t.Fatalf("preview starts %q, want the plan stage header", preview)
	}
	if !strings.Contains(preview, "Split: 3 train, 1 test, seed 7") {
		t.Fatalf("preview = %q, want the split line", preview)
	}
}

func TestRunRefusesForeignMembers(t *testing.T) {
	t.Parallel()
	ws := makeWorkspace(t, initFiles())
	_, err := callTool(t, New(&fakeAsker{}), toolEnv(t, ws), "c1", `{"action":"gate","flow":"x","variant":"v1","cases":["a"]}`)
	if err == nil || !strings.Contains(err.Error(), "gate does not take cases") {
		t.Fatalf("error = %v, want gate does not take cases", err)
	}
}

func TestRunNeedsLocalRunner(t *testing.T) {
	t.Parallel()
	_, err := callTool(t, New(&fakeAsker{}), tool.Env{Cwd: t.TempDir()}, "c1", initJSON)
	if err == nil || !strings.Contains(err.Error(), "ratchet needs the local runner") {
		t.Fatalf("error = %v, want ratchet needs the local runner", err)
	}
}

func TestSplitSeedFromCallID(t *testing.T) {
	t.Parallel()
	ws := makeWorkspace(t, initFiles())
	env := toolEnv(t, ws)
	tl := New(&fakeAsker{})
	mustCall(t, tl, env, "c1", initJSON)
	args := `{"action":"split","flow":"x","strata":{"a":"t","b":"t","c":"t","d":"t"},"test_fraction":0.5}`
	first := mustCall(t, tl, env, "c1", args)
	second := mustCall(t, tl, env, "c1", args)
	if first != second {
		t.Fatalf("split with the same call id = %q then %q, want equal results", first, second)
	}
}

func TestToolFlowEndToEnd(t *testing.T) {
	t.Parallel()
	ws := makeWorkspace(t, initFiles())
	env := toolEnv(t, ws)
	tl := New(&fakeAsker{answer: Answer{Label: "Approve"}})
	mustCall(t, tl, env, "c1", initJSON)
	mustCall(t, tl, env, "c2", `{"action":"approve","flow":"x","stage":"inputs","question":"Approve the cases?"}`)
	mustCall(t, tl, env, "c3", `{"action":"approve","flow":"x","stage":"grader","question":"Approve the grader?"}`)
	mustCall(t, tl, env, "c4", `{"action":"split","flow":"x","strata":{"a":"t","b":"t","c":"t","d":"t","e":"t","f":"t","g":"t","h":"t"},"test_fraction":0.5,"seed":7}`)
	mustCall(t, tl, env, "c5", `{"action":"plan","flow":"x","target":"score","direction":"higher","command":"run {variant}","reps":1}`)
	mustCall(t, tl, env, "c6", `{"action":"approve","flow":"x","stage":"plan","question":"Approve the plan?"}`)
	mustCall(t, tl, env, "c7", `{"action":"check","flow":"x","variant":"baseline"}`)
	writeVariant(t, ws, "baseline", []string{
		rowLine("a", 0, false, ""), rowLine("b", 0, false, ""), rowLine("c", 0, false, ""), rowLine("d", 0, false, ""),
		rowLine("e", 0, false, ""), rowLine("f", 0, false, ""), rowLine("g", 0, false, ""), rowLine("h", 0, false, ""),
	})
	if got := mustCall(t, tl, env, "c8", `{"action":"gate","flow":"x","variant":"baseline"}`); !strings.Contains(got, `"decision":"baseline"`) {
		t.Fatalf("baseline gate = %s, want decision baseline", got)
	}
	if err := os.WriteFile(filepath.Join(ws, "p.md"), []byte("two\n"), 0o600); err != nil {
		t.Fatal(err)
	}
	mustCall(t, tl, env, "c9", `{"action":"check","flow":"x","variant":"v1"}`)
	writeVariant(t, ws, "v1", []string{
		rowLine("a", 0, true, ""), rowLine("b", 0, true, ""), rowLine("c", 0, true, ""), rowLine("d", 0, true, ""),
		rowLine("e", 0, true, ""), rowLine("f", 0, true, ""), rowLine("g", 0, true, ""), rowLine("h", 0, true, ""),
	})
	if got := mustCall(t, tl, env, "c10", `{"action":"gate","flow":"x","variant":"v1","summary":"tighten p.md"}`); !strings.Contains(got, `"decision":"keep"`) {
		t.Fatalf("v1 gate = %s, want decision keep", got)
	}
	status := mustCall(t, tl, env, "c11", `{"action":"status","flow":"x"}`)
	for _, want := range []string{`"inputs":"current"`, `"grader":"current"`, `"plan":"current"`, "v1 (best)"} {
		if !strings.Contains(status, want) {
			t.Fatalf("status = %s, want %s", status, want)
		}
	}
}

func TestStatusOnInitOnlyFlow(t *testing.T) {
	t.Parallel()
	ws := makeWorkspace(t, initFiles())
	env := toolEnv(t, ws)
	tl := New(&fakeAsker{})
	mustCall(t, tl, env, "c1", initJSON)
	status := mustCall(t, tl, env, "c2", `{"action":"status","flow":"x"}`)
	for _, want := range []string{`"done":false`, `"plateau":false`} {
		if !strings.Contains(status, want) {
			t.Fatalf("status = %s, want %s", status, want)
		}
	}
}
