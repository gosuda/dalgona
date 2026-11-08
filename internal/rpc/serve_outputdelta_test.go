package rpc

import (
	"strconv"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/config"
)

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
	run := runExec(t, "printf a; sleep 0.2; printf b")
	expect(t, len(run.deltas) > 0, "outputDelta count", run.deltas)
	expect(t, strings.Join(run.deltas, "") == run.command.Output && run.command.Output == "ab", "deltas equal aggregatedOutput", run)
}

func TestServeExecStreamsCompletedLinesWhileRunning(t *testing.T) {
	run := runExec(t, `printf 'a\n'; sleep 0.3; printf b`)
	expect(t, len(run.deltas) >= 2 && run.deltas[0] == "a\n", "first line streamed before the command ended", run.deltas)
	expect(t, strings.Join(run.deltas, "") == run.command.Output && run.command.Output == "a\nb", "deltas equal aggregatedOutput", run)
}

func TestServeExecDeltasMaskSecretSplitAcrossReads(t *testing.T) {
	probe := "dlt-probe-7Qx2Lm9Vw4"
	t.Setenv("DELTA_PROBE_TOKEN", probe)
	run := runExec(t, `s=$DELTA_PROBE_TOKEN; printf 'key=%s' "${s%"${s#??????}"}"; sleep 0.3; printf '%s\nend\n' "${s#??????}"`)
	joined := strings.Join(run.deltas, "")
	expect(t, joined == run.command.Output, "deltas equal aggregatedOutput", run)
	expect(t, strings.Contains(joined, "key={{secret:DELTA_PROBE_TOKEN}}\n"), "masked placeholder streamed", run.deltas)
	for _, delta := range run.deltas {
		expect(t, !strings.Contains(delta, probe[:6]) && !strings.Contains(delta, probe[6:]), "masked value fragment in delta", delta)
	}
}
