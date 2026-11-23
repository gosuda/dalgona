package rpcserver

import (
	"encoding/base64"
	json "encoding/json/v2"
	"errors"
	"os"
	"strconv"
	"strings"
	"syscall"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/dal/rpc"

	"github.com/gosuda/dalgona/internal/dal/config"
)

func TestCommandExecTerminate(t *testing.T) {
	r := newRig(t, config.ApprovalAuto)
	c := r.connect()
	finished := make(chan message, 1)
	go func() {
		finished <- c.call("command/exec", map[string]any{
			"command": []string{os.Args[0]}, "processId": "p2", "streamStdoutStderr": true,
			"env": map[string]any{"RPC_TEST_SLEEP": "1", "RPC_TEST_PRINT_PID": "1"},
		})
	}()
	pid := awaitShimPid(t, c)
	c.ok("command/exec/terminate", map[string]any{"processId": "p2"}, nil)
	select {
	case m := <-finished:
		expect(t, m.Error == nil, "terminated command/exec error", m.Error)
		var response rpc.CommandExecResponse
		expect(t, json.Unmarshal(m.Result, &response) == nil, "decode terminated result", string(m.Result))
		expect(t, response.ExitCode != 0, "terminated exit code", response.ExitCode)
	case <-time.After(waitLimit):
		t.Fatal("terminated command/exec did not answer")
	}
	requireProcessGone(t, pid)
}

func awaitShimPid(t *testing.T, c *wireClient) int {
	t.Helper()
	var streamed []byte
	for {
		delta := c.await("command/exec/outputDelta", nil)
		chunk, err := base64.StdEncoding.DecodeString(fieldOf(t, delta.Params, "deltaBase64"))
		if err != nil {
			t.Fatalf("decode output delta %s: %v", delta.Params, err)
		}
		streamed = append(streamed, chunk...)
		fields := strings.Fields(string(streamed))
		if len(fields) == 2 && fields[0] == "ready" {
			return parseShimPid(t, fields[1])
		}
	}
}

func parseShimPid(t *testing.T, raw string) int {
	t.Helper()
	pid, err := strconv.Atoi(raw)
	if err != nil {
		t.Fatalf("decode shim pid %q: %v", raw, err)
	}
	return pid
}

func requireProcessGone(t *testing.T, pid int) {
	t.Helper()
	for deadline := time.Now().Add(10 * time.Second); time.Now().Before(deadline); time.Sleep(20 * time.Millisecond) {
		if err := syscall.Kill(pid, 0); errors.Is(err, syscall.ESRCH) {
			return
		}
	}
	t.Fatalf("process %d outlived command/exec/terminate", pid)
}
