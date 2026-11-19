package rpc

import (
	"encoding/base64"
	json "encoding/json/v2"
	"maps"
	"os"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/dal/config"
)

func drainNotes(c *wireClient) []message {
	var out []message
	for {
		select {
		case m := <-c.notes:
			out = append(out, m)
		default:
			return out
		}
	}
}

func TestCommandExecBuffered(t *testing.T) {
	r := newRig(t, config.ApprovalAuto)
	c := r.connect()
	var response CommandExecResponse
	c.ok("command/exec", map[string]any{"command": []string{os.Args[0]}, "env": map[string]any{"RPC_TEST_PRINT": "hello"}}, &response)
	expect(t, response.ExitCode == 3 && response.Stdout == "hello" && response.Stderr == "", "command/exec", response)
}

func TestCommandExecStreams(t *testing.T) {
	r := newRig(t, config.ApprovalAuto)
	c := r.connect()
	m := c.call("command/exec", map[string]any{
		"command": []string{os.Args[0]}, "processId": "p1", "streamStdoutStderr": true,
		"env": map[string]any{"RPC_TEST_PRINT": "hello"},
	})
	expect(t, m.Error == nil, "command/exec error", m.Error)
	var response CommandExecResponse
	expect(t, json.Unmarshal(m.Result, &response) == nil, "decode command/exec", string(m.Result))
	expect(t, response.ExitCode == 3 && response.Stdout == "" && response.Stderr == "", "streamed command/exec", response)
	var streamed []byte
	for _, note := range drainNotes(c) {
		if note.Method != "command/exec/outputDelta" {
			continue
		}
		expect(t, fieldOf(t, note.Params, "processId") == "p1" && fieldOf(t, note.Params, "stream") == "stdout", "delta identity", string(note.Params))
		chunk, err := base64.StdEncoding.DecodeString(fieldOf(t, note.Params, "deltaBase64"))
		expect(t, err == nil, "delta base64", err)
		streamed = append(streamed, chunk...)
	}
	expect(t, string(streamed) == "hello", "streamed output", string(streamed))
}

func TestCommandExecTimeout(t *testing.T) {
	r := newRig(t, config.ApprovalAuto)
	c := r.connect()
	var response CommandExecResponse
	c.ok("command/exec", map[string]any{
		"command": []string{os.Args[0]}, "timeoutMs": 200, "env": map[string]any{"RPC_TEST_SLEEP": "1"},
	}, &response)
	expect(t, response.ExitCode == 124, "timed out command/exec", response)
}

func TestCommandExecRejectsTTY(t *testing.T) {
	r := newRig(t, config.ApprovalAuto)
	c := r.connect()
	c.fails("command/exec", map[string]any{"command": []string{os.Args[0]}, "tty": true}, -32602, "tty is not supported")
}

func TestCommandExecRejectsBadParams(t *testing.T) {
	r := newRig(t, config.ApprovalAuto)
	c := r.connect()
	argv := []string{os.Args[0]}
	cases := []struct {
		name   string
		params map[string]any
		want   string
	}{
		{"empty command", map[string]any{"command": []string{}}, "command must not be empty"},
		{"relative cwd", map[string]any{"command": argv, "cwd": "relative"}, "cwd must be absolute"},
		{"stream without process id", map[string]any{"command": argv, "streamStdoutStderr": true}, "streamStdoutStderr needs a processId"},
		{"size without tty", map[string]any{"command": argv, "size": map[string]any{"cols": 80, "rows": 24}}, "size needs tty"},
		{"stdin stream", map[string]any{"command": argv, "streamStdin": true}, "streamStdin is not supported"},
		{"timeout and no timeout", map[string]any{"command": argv, "timeoutMs": 1000, "disableTimeout": true}, "timeoutMs and disableTimeout exclude each other"},
		{"cap and no cap", map[string]any{"command": argv, "outputBytesCap": 10, "disableOutputCap": true}, "outputBytesCap and disableOutputCap exclude each other"},
		{"external sandbox", map[string]any{"command": argv, "sandboxPolicy": map[string]any{"type": "externalSandbox"}}, "unsupported sandboxPolicy externalSandbox"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			c.fails("command/exec", tc.params, -32602, tc.want)
		})
	}
	c.fails("command/exec/terminate", map[string]any{"processId": "nope"}, -32602, "unknown process nope")
}

func TestCommandExecLimitCountsEveryCommand(t *testing.T) {
	r := newRig(t, config.ApprovalAuto)
	c := r.connect()
	argv := []string{os.Args[0]}
	const commands = execMaxProcesses + 1
	answers := make(chan message, commands)
	for range commands {
		go func() {
			answers <- c.call("command/exec", map[string]any{
				"command": argv, "timeoutMs": 8000, "env": map[string]any{"RPC_TEST_SLEEP": "1"},
			})
		}()
	}
	refused := 0
	timer := time.NewTimer(waitLimit)
	defer timer.Stop()
	for range commands {
		timer.Reset(waitLimit)
		select {
		case m := <-answers:
			if m.Error != nil {
				expect(t, m.Error.Message == "too many running commands", "refused command/exec", m.Error.Message)
				refused++
			}
		case <-timer.C:
			t.Fatal("command/exec did not answer")
		}
	}
	expect(t, refused == 1, "commands refused by the sixteen-command limit", refused)
}

func TestCommandExecRefusesOutOfRangeLimits(t *testing.T) {
	const (
		negativeTimeout = "timeoutMs must not be negative"
		outputCapRange  = "outputBytesCap must be from 1 to 67108864"
	)
	cases := []struct {
		name   string
		limits map[string]any
		want   string
	}{
		{"timeoutMs -1 is negative", map[string]any{"timeoutMs": -1}, negativeTimeout},
		{"timeoutMs -600000 is negative", map[string]any{"timeoutMs": -600000}, negativeTimeout},
		{"timeoutMs 0 is not negative", map[string]any{"timeoutMs": 0}, ""},
		{"outputBytesCap -1 is outside 1 to 67108864", map[string]any{"outputBytesCap": -1}, outputCapRange},
		{"outputBytesCap 0 is outside 1 to 67108864", map[string]any{"outputBytesCap": 0}, outputCapRange},
		{"outputBytesCap 1 is inside 1 to 67108864", map[string]any{"outputBytesCap": 1}, ""},
		{"outputBytesCap 67108864 is inside 1 to 67108864", map[string]any{"outputBytesCap": 67108864}, ""},
		{"outputBytesCap 67108865 is outside 1 to 67108864", map[string]any{"outputBytesCap": 67108865}, outputCapRange},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			c := newRig(t, config.ApprovalAuto).connect()
			params := map[string]any{"command": []string{os.Args[0]}, "env": map[string]any{"RPC_TEST_PRINT": "hello"}}
			maps.Copy(params, tc.limits)
			if tc.want != "" {
				c.fails("command/exec", params, -32602, tc.want)
				return
			}
			var response CommandExecResponse
			c.ok("command/exec", params, &response)
			expect(t, response.ExitCode == 3, "command that passed the limits", response)
		})
	}
}
