package scripthost_test

import (
	"os"
	"path/filepath"
	"slices"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/agent"
	"github.com/gosuda/dalgona/internal/agent/event"
	"github.com/gosuda/dalgona/internal/script"
	"github.com/gosuda/dalgona/internal/scripthost"
)

func TestWorkflowReplaysRecordedEffectsAfterCrash(t *testing.T) {
	t.Parallel()
	code := "ctx.tools.exec(command = \"echo one >> log.txt\")\nctx.tools.exec(command = \"echo two >> log.txt\")\n" +
		"a = ctx.judge(\"hold\", {\"ok\": {\"type\": \"noul\"}})\nctx.tools.exec(command = \"echo three >> log.txt\")\na[\"ok\"].noul\n"
	crashed, threadID, home := crashWorkflow(t, code)
	judge := newJudgeServer(t, nil)
	revived, events := reviveWorkflow(t, crashed.cwd, home, threadID, judge)
	log, err := os.ReadFile(filepath.Join(crashed.cwd, "log.txt"))
	if err != nil || !slices.Equal(logLines(log), []string{"one", "two", "three"}) {
		t.Fatalf("log = %q, %v", log, err)
	}
	if judge.requests.Load() != 1 || revived.model.requests("dalgona run flow.star") != 0 {
		t.Fatalf("judge requests = %d, model requests = %d", judge.requests.Load(), revived.model.requests("dalgona run flow.star"))
	}
	completed, turn := workflowOutcome(events)
	if len(completed) != 1 || !completed[0].OK || !strings.Contains(string(completed[0].Value), "0.9") || turn.Status != "completed" {
		t.Fatalf("script.completed = %+v, turn = %+v", completed, turn)
	}
}

func logLines(data []byte) []string {
	raw := strings.Split(string(data), "\n")
	if len(raw) > 0 && raw[len(raw)-1] == "" {
		raw = raw[:len(raw)-1]
	}
	lines := make([]string, 0, len(raw))
	for _, line := range raw {
		lines = append(lines, strings.TrimRight(line, " \r"))
	}
	return lines
}

func crashWorkflow(t *testing.T, code string) (*harness, string, string) {
	t.Helper()
	hold := make(chan struct{})
	t.Cleanup(func() { close(hold) })
	crashed := newHarness(t, withJudge(newJudgeServer(t, hold)))
	thread, err := crashed.host.Start(t.Context(), agent.ThreadSpec{})
	if err != nil {
		t.Fatal(err)
	}
	go func() {
		_, _ = scripthost.RunWorkflow(t.Context(), thread, crashed.scripts, script.Workflow{ID: thread.ID(), Entry: "flow.star", Source: []byte(code)})
	}()
	crashed.await(thread.ID(), nestedDone(thread.ID()+"/2"))
	home := t.TempDir()
	copyHome(t, crashed.home, home)
	return crashed, thread.ID(), home
}

func reviveWorkflow(t *testing.T, cwd, home, threadID string, judge *judgeServer) (*harness, []event.Seqd) {
	t.Helper()
	revived := newHarness(t, withJudge(judge), func(h *harness) { h.cwd, h.cfg.Config.Cwd = cwd, cwd })
	revived.open(home)
	if err := revived.host.Recover(t.Context()); err != nil {
		t.Fatal(err)
	}
	resumed, err := revived.host.Resume(t.Context(), threadID)
	if err != nil {
		t.Fatal(err)
	}
	return revived, revived.idle(resumed)
}

func workflowOutcome(events []event.Seqd) ([]event.ScriptCompleted, event.TurnCompleted) {
	var completed []event.ScriptCompleted
	var turn event.TurnCompleted
	for _, item := range events {
		switch value := item.Event.(type) {
		case event.ScriptCompleted:
			completed = append(completed, value)
		case event.TurnCompleted:
			turn = value
		}
	}
	return completed, turn
}
