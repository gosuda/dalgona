package rpc

import (
	"os"
	"path/filepath"
	"runtime"
	"slices"
	"strconv"
	"strings"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/config"
)

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

type execObserver struct {
	threadID  string
	turnID    string
	deltas    []string
	command   Item
	completed bool
}

func (o *execObserver) observe(t *testing.T, m message) {
	t.Helper()
	switch {
	case m.Method == outputDeltaMethod:
		o.observeOutputDelta(t, m)
	case m.Method == methodItemCompleted && itemOf(t, m).ID == "call-x":
		o.completed = true
		o.command = itemOf(t, m)
	}
}

func (o *execObserver) observeOutputDelta(t *testing.T, m message) {
	t.Helper()
	expect(t, !o.completed, "outputDelta after item/completed", string(m.Params))
	validateLine(t, "v2/CommandExecutionOutputDeltaNotification.json", m.Params)
	expect(t, fieldOf(t, m.Params, "threadId") == o.threadID && fieldOf(t, m.Params, "turnId") == o.turnID &&
		fieldOf(t, m.Params, "itemId") == "call-x", "outputDelta ids", string(m.Params))
	o.deltas = append(o.deltas, fieldOf(t, m.Params, "delta"))
}

func (o *execObserver) observeBeforeFirstOutputDelta(t *testing.T, m message) {
	t.Helper()
	if (m.Method == methodItemCompleted && itemOf(t, m).ID == "call-x") || m.Method == "turn/completed" {
		t.Fatalf("%s arrived before the first outputDelta", m.Method)
	}
	o.observe(t, m)
}

func (o *execObserver) lastDelta() string {
	if len(o.deltas) == 0 {
		return ""
	}
	return o.deltas[len(o.deltas)-1]
}

func awaitFirstOutputDelta(t *testing.T, notes <-chan message, obs *execObserver) string {
	t.Helper()
	for obs.lastDelta() == "" {
		select {
		case m := <-notes:
			obs.observeBeforeFirstOutputDelta(t, m)
		case <-time.After(waitLimit):
			t.Fatal("no outputDelta while the command waited on the gate")
		}
	}
	return obs.lastDelta()
}

func runExec(t *testing.T, command string) execRun {
	t.Helper()
	r := newRig(t, config.ApprovalYolo)
	r.llm.push(llmReply{body: sseCall("call-x", "exec", `{"command":`+strconv.Quote(command)+`}`)}, llmReply{body: sseText("done")})
	c := r.connect()
	run := execRun{threadID: startThread(t, r, c, nil)}
	run.turnID = startTurn(t, c, run.threadID, "stream it")
	obs := &execObserver{threadID: run.threadID, turnID: run.turnID}
	for _, m := range c.collect() {
		obs.observe(t, m)
	}
	run.deltas, run.command = obs.deltas, obs.command
	expect(t, obs.completed && run.command.Status == statusCompleted, "command item", run.command)
	return run
}

func TestServeExecStreamsOutputDeltaBeforeCompletion(t *testing.T) {
	if runtime.GOOS == "windows" {
		t.Skip("the gate command needs a POSIX shell")
	}
	gate := filepath.Join(t.TempDir(), "release")
	release := func() { _ = os.WriteFile(gate, []byte("release\n"), 0o600) }
	t.Cleanup(release)

	r := newRig(t, config.ApprovalYolo)
	command := "printf 'first\\n'; while [ ! -e '" + gate + "' ]; do sleep 0.02; done; printf 'second\\n'"
	r.llm.push(llmReply{body: sseCall("call-x", "exec", `{"command":`+strconv.Quote(command)+`}`)}, llmReply{body: sseText("done")})
	c := r.connect()
	threadID := startThread(t, r, c, nil)
	turnID := startTurn(t, c, threadID, "stream it")

	obs := &execObserver{threadID: threadID, turnID: turnID}
	firstDelta := awaitFirstOutputDelta(t, c.notes, obs)
	expect(t, slices.Equal(strings.Fields(firstDelta), []string{"first"}), "first delta is the first line", firstDelta)

	release()

	seen := c.collect()
	for _, m := range seen {
		obs.observe(t, m)
	}
	expect(t, obs.completed && obs.command.Status == statusCompleted, "command item", obs.command)
	expect(t, fieldOf(t, seen[len(seen)-1].Params, "turn", "status") == statusCompleted, "turn/completed status", string(seen[len(seen)-1].Params))
	expect(t, len(obs.deltas) >= 2, "deltas streamed around the gate", obs.deltas)
	expect(t, strings.Join(obs.deltas, "") == obs.command.Output, "deltas equal aggregatedOutput", obs.deltas)
	expect(t, slices.Equal(strings.Fields(obs.command.Output), []string{"first", "second"}), "streamed output", obs.command.Output)
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
