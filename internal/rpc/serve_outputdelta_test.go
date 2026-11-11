package rpc

import (
	"runtime"
	"slices"
	"strconv"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/config"
)

func streamCommand(before, after string) string {
	if runtime.GOOS == "windows" {
		return "echo " + before + " & ping -n 2 127.0.0.1 >nul & echo " + after
	}
	return "echo " + before + "; sleep 0.3; echo " + after
}

func splitSecretCommand() string {
	if runtime.GOOS == "windows" {
		return `<nul set /p "=key=%DELTA_PROBE_TOKEN:~0,6%" & ping -n 2 127.0.0.1 >nul & echo %DELTA_PROBE_TOKEN:~6% & echo end`
	}
	return `s=$DELTA_PROBE_TOKEN; printf 'key=%s' "${s%"${s#??????}"}"; sleep 0.3; printf '%s\nend\n' "${s#??????}"`
}

const outputDeltaMethod = "item/commandExecution/outputDelta"

type execRun struct {
	threadID string
	turnID   string
	deltas   []string
	command  Item
}

func runExec(t *testing.T, command string) execRun {
	t.Helper()
	r := newRig(t, config.ApprovalYolo)
	r.llm.push(llmReply{body: sseCall("call-x", "exec", `{"command":`+strconv.Quote(command)+`}`)}, llmReply{body: sseText("done")})
	c := r.connect()
	run := execRun{threadID: startThread(t, r, c, nil)}
	run.turnID = startTurn(t, c, run.threadID, "stream it")
	completed := false
	for _, m := range c.collect("turn/completed") {
		switch {
		case m.Method == outputDeltaMethod:
			expect(t, !completed, "outputDelta after item/completed", string(m.Params))
			validateLine(t, "v2/CommandExecutionOutputDeltaNotification.json", m.Params)
			expect(t, fieldOf(t, m.Params, "threadId") == run.threadID && fieldOf(t, m.Params, "turnId") == run.turnID &&
				fieldOf(t, m.Params, "itemId") == "call-x", "outputDelta ids", string(m.Params))
			run.deltas = append(run.deltas, fieldOf(t, m.Params, "delta"))
		case m.Method == methodItemCompleted && itemOf(t, m).ID == "call-x":
			completed = true
			run.command = itemOf(t, m)
		}
	}
	expect(t, completed && run.command.Status == statusCompleted, "command item", run.command)
	return run
}

func TestServeExecStreamsOutputDeltasBeforeCompletion(t *testing.T) {
	run := runExec(t, streamCommand("a", "b"))
	expect(t, len(run.deltas) > 0, "outputDelta count", run.deltas)
	expect(t, strings.Join(run.deltas, "") == run.command.Output, "deltas equal aggregatedOutput", run)
	expect(t, slices.Equal(strings.Fields(run.command.Output), []string{"a", "b"}), "streamed output", run.command.Output)
}

func TestServeExecStreamsCompletedLinesWhileRunning(t *testing.T) {
	run := runExec(t, streamCommand("a", "b"))
	expect(t, len(run.deltas) >= 2, "first line streamed before the command ended", run.deltas)
	expect(t, slices.Equal(strings.Fields(run.deltas[0]), []string{"a"}), "first delta is the first line", run.deltas)
	expect(t, strings.Join(run.deltas, "") == run.command.Output, "deltas equal aggregatedOutput", run)
	expect(t, slices.Equal(strings.Fields(run.command.Output), []string{"a", "b"}), "streamed output", run.command.Output)
}

func TestServeExecDeltasMaskSecretSplitAcrossReads(t *testing.T) {
	probe := "dlt-probe-7Qx2Lm9Vw4"
	t.Setenv("DELTA_PROBE_TOKEN", probe)
	run := runExec(t, splitSecretCommand())
	joined := strings.Join(run.deltas, "")
	expect(t, joined == run.command.Output, "deltas equal aggregatedOutput", run)
	expect(t, strings.Contains(joined, "key={{secret:DELTA_PROBE_TOKEN}}"), "masked placeholder streamed", run.deltas)
	for _, delta := range run.deltas {
		expect(t, !strings.Contains(delta, probe[:6]) && !strings.Contains(delta, probe[6:]), "masked value fragment in delta", delta)
	}
}
