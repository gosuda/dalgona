package ratchet

import (
	"bytes"
	"context"
	"encoding/json/jsontext"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/runner"
	"github.com/gosuda/dalgona/internal/dal/tool"
)

const initJSON = `{"action":"init","flow":"x","cases":["cases.txt"],"harness":["run.sh"],"change":["p.md"],"off_limits":["notes.md"]}`

type fakeAsker struct {
	answer    Answer
	err       error
	calls     int
	question  Question
	roomChars int
	roomLines int
}

func (a *fakeAsker) PreviewRoom() (chars, lines int) {
	chars, lines = a.roomChars, a.roomLines
	if chars == 0 {
		chars = 2000
	}
	if lines == 0 {
		lines = 40
	}
	return chars, lines
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

func TestApprovePreviewJoinsHostBlockBlankLineAndModelPreview(t *testing.T) {
	t.Parallel()
	cases := []struct {
		name    string
		preview string
	}{
		{"empty model preview keeps the blank line", ""},
		{"model preview follows the blank line", "case a\ncase b"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			ws := makeWorkspace(t, initFiles())
			env := toolEnv(t, ws)
			asker := &fakeAsker{answer: Answer{Label: "Abort"}}
			tl := New(asker)
			mustCall(t, tl, env, "c1", initJSON)
			mustCall(t, tl, env, "c2", fmt.Sprintf(`{"action":"approve","flow":"x","stage":"inputs","question":"Approve the cases?","preview":%q}`, tc.preview))
			want := "Stage: inputs\nDigest: " + expectedDigest(t, ws, stageInputs) + "\nFlow: x\nFiles:\n- cases.txt\n\n" + tc.preview
			if got := asker.question.Options[0].Preview; got != want {
				t.Fatalf("Approve preview = %q, want %q", got, want)
			}
		})
	}
}

func casesInit(n int) (map[string]string, string) {
	files := map[string]string{"run.sh": "r\n", "p.md": "one\n", "notes.md": "n\n"}
	quoted := make([]string, 0, n)
	for i := 1; i <= n; i++ {
		name := fmt.Sprintf("c%02d.txt", i)
		files[name] = "c\n"
		quoted = append(quoted, fmt.Sprintf("%q", name))
	}
	return files, `{"action":"init","flow":"x","cases":[` + strings.Join(quoted, ",") + `],"harness":["run.sh"],"change":["p.md"],"off_limits":["notes.md"]}`
}

func TestApproveCountsTheJoinWhenPreviewIsEmpty(t *testing.T) {
	t.Parallel()
	cases := []struct {
		name    string
		paths   int
		wantErr string
	}{
		{"facts leave no line for the join", 36, "ratchet approve: preview is 1 lines and 0 characters; the room after the host facts is 0 lines and 1537 characters"},
		{"facts leave one line for the join", 35, "ratchet approve: preview is 1 lines and 0 characters; the room after the host facts is 0 lines and 1547 characters"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			files, initArgs := casesInit(tc.paths)
			ws := makeWorkspace(t, files)
			env := toolEnv(t, ws)
			asker := &fakeAsker{}
			tl := New(asker)
			mustCall(t, tl, env, "c1", initArgs)
			before := stateJSON(t, ws)
			_, err := callTool(t, tl, env, "c2", `{"action":"approve","flow":"x","stage":"inputs","question":"Approve the cases?"}`)
			if err == nil || err.Error() != tc.wantErr {
				t.Fatalf("error = %v, want %q", err, tc.wantErr)
			}
			if asker.calls != 0 {
				t.Fatalf("asker calls = %d, want 0", asker.calls)
			}
			if after := stateJSON(t, ws); !bytes.Equal(before, after) {
				t.Fatal("refused approve changed state.json")
			}
		})
	}
}

func stateJSON(t *testing.T, ws string) []byte {
	t.Helper()
	body, err := os.ReadFile(filepath.Join(ws, ".dalgona", "ratchet", "x", "state.json"))
	if err != nil {
		t.Fatal(err)
	}
	return body
}

func TestApproveRefusesPreviewOverRoom(t *testing.T) {
	t.Parallel()
	files := map[string]string{"cases1.txt": "c1\n", "cases2.txt": "c2\n", "run.sh": "r\n", "p.md": "one\n", "notes.md": "n\n"}
	initTwoCases := `{"action":"init","flow":"x","cases":["cases1.txt","cases2.txt"],"harness":["run.sh"],"change":["p.md"],"off_limits":["notes.md"]}`
	preview := strings.Repeat(`case 12\n`, 37) + `case 12`
	t.Run("preview over the room left by the host facts", func(t *testing.T) {
		t.Parallel()
		ws := makeWorkspace(t, files)
		env := toolEnv(t, ws)
		asker := &fakeAsker{}
		tl := New(asker)
		mustCall(t, tl, env, "c1", initTwoCases)
		before := stateJSON(t, ws)
		_, err := callTool(t, tl, env, "c2", `{"action":"approve","flow":"x","stage":"inputs","question":"Approve the cases?","preview":"`+preview+`"}`)
		want := "ratchet approve: preview is 38 lines and 303 characters; the room after the host facts is 33 lines and 1871 characters"
		if err == nil || err.Error() != want {
			t.Fatalf("error = %v, want %q", err, want)
		}
		if asker.calls != 0 {
			t.Fatalf("asker calls = %d, want 0", asker.calls)
		}
		if after := stateJSON(t, ws); !bytes.Equal(before, after) {
			t.Fatal("refused approve changed state.json")
		}
	})
	t.Run("a short preview asks", func(t *testing.T) {
		t.Parallel()
		ws := makeWorkspace(t, files)
		env := toolEnv(t, ws)
		asker := &fakeAsker{answer: Answer{Label: "Abort"}}
		tl := New(asker)
		mustCall(t, tl, env, "c1", initTwoCases)
		short := strings.Repeat(`case 12\n`, 9) + `case 12`
		mustCall(t, tl, env, "c2", `{"action":"approve","flow":"x","stage":"inputs","question":"Approve the cases?","preview":"`+short+`"}`)
		if asker.calls != 1 {
			t.Fatalf("asker calls = %d, want 1", asker.calls)
		}
	})
}

func TestApproveRefusesFactsOverLimit(t *testing.T) {
	t.Parallel()
	t.Run("host facts over the room", func(t *testing.T) {
		t.Parallel()
		files, initJSON := casesInit(37)
		ws := makeWorkspace(t, files)
		env := toolEnv(t, ws)
		asker := &fakeAsker{}
		tl := New(asker)
		mustCall(t, tl, env, "c1", initJSON)
		before := stateJSON(t, ws)
		_, err := callTool(t, tl, env, "c2", `{"action":"approve","flow":"x","stage":"inputs","question":"Approve the cases?"}`)
		want := "ratchet approve: the inputs facts need 41 lines and 471 characters, and one question holds 40 lines and 2000 characters; list directories in place of single files, or shorten the command"
		if err == nil || err.Error() != want {
			t.Fatalf("error = %v, want %q", err, want)
		}
		if asker.calls != 0 {
			t.Fatalf("asker calls = %d, want 0", asker.calls)
		}
		if after := stateJSON(t, ws); !bytes.Equal(before, after) {
			t.Fatal("refused approve changed state.json")
		}
	})
	t.Run("facts that leave exactly the join ask", func(t *testing.T) {
		t.Parallel()
		files, initArgs := casesInit(34)
		ws := makeWorkspace(t, files)
		env := toolEnv(t, ws)
		asker := &fakeAsker{answer: Answer{Label: "Abort"}}
		tl := New(asker)
		mustCall(t, tl, env, "c1", initArgs)
		mustCall(t, tl, env, "c2", `{"action":"approve","flow":"x","stage":"inputs","question":"Approve the cases?"}`)
		if asker.calls != 1 {
			t.Fatalf("asker calls = %d, want 1", asker.calls)
		}
		if lines := strings.Count(asker.question.Options[0].Preview, "\n") + 1; lines != 40 {
			t.Fatalf("Approve preview holds %d lines, want the 40 of the room", lines)
		}
	})
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
	for _, want := range []string{`"inputs":"current"`, `"grader":"current"`, `"plan":"current"`, "v1 (best)", "| keep |"} {
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
