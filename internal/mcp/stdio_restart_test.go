package mcp_test

import (
	"context"
	"encoding/json/jsontext"
	"errors"
	"os"
	"path/filepath"
	"runtime"
	"strconv"
	"syscall"
	"testing"

	"github.com/gosuda/dalgona/internal/mcp"
	"github.com/gosuda/dalgona/internal/tool"
)

func restartFakeScript(countFile, pidFile string) string {
	return `#!/bin/bash
count_file="` + countFile + `"
pid_file="` + pidFile + `"
n=0
[ -f "$count_file" ] && n=$(cat "$count_file")
n=$((n+1))
echo "$n" > "$count_file"
if [ "$n" = "1" ]; then
  read -r line
  id=$(printf '%s' "$line" | grep -o '"id":[0-9]*' | tr -d -c 0-9)
  printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2025-11-25","capabilities":{"tools":{}},"serverInfo":{"name":"t","version":"0"}}}\n' "$id"
  read -r line
  read -r line
  id=$(printf '%s' "$line" | grep -o '"id":[0-9]*' | tr -d -c 0-9)
  printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"t__echo","description":"d","inputSchema":{"type":"object"}}],"nextCursor":""}}\n' "$id"
  exit 0
fi
echo $$ > "$pid_file"
read -r line
id=$(printf '%s' "$line" | grep -o '"id":[0-9]*' | tr -d -c 0-9)
printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"1999-01-01","capabilities":{},"serverInfo":{"name":"t","version":"0"}}}\n' "$id"
exec cat
`
}

func requireProcessGone(t *testing.T, pidFile string) {
	t.Helper()
	raw, err := os.ReadFile(pidFile)
	if err != nil {
		t.Fatalf("restarted server never launched: %v", err)
	}
	pid, err := strconv.Atoi(string(raw[:len(raw)-1]))
	if err != nil {
		t.Fatalf("parse pid %q: %v", raw, err)
	}
	probe, err := os.FindProcess(pid)
	if err != nil {
		t.Fatalf("find restarted server process: %v", err)
	}
	if err := probe.Signal(syscall.Signal(0)); err == nil {
		t.Fatal("restarted server survived the failed re-initialize")
	} else if !errors.Is(err, os.ErrProcessDone) {
		t.Fatalf("probe restarted server: %v", err)
	}
}

func TestStdioRestartClosesTransportWhenInitializeFails(t *testing.T) {
	if runtime.GOOS == "windows" {
		t.Skip("bash test server needs a POSIX shell")
	}
	dir := t.TempDir()
	script := filepath.Join(dir, "fake-mcp.sh")
	countFile := filepath.Join(dir, "count")
	pidFile := filepath.Join(dir, "pid")
	if err := os.WriteFile(script, []byte(restartFakeScript(countFile, pidFile)), 0o755); err != nil {
		t.Fatal(err)
	}
	client, err := mcp.New(context.Background(), mcp.Config{Name: "fake", Command: script})
	if err != nil {
		t.Fatalf("start MCP client: %v", err)
	}
	t.Cleanup(func() { _ = client.Close() })
	tools, err := client.Tools(context.Background())
	if err != nil {
		t.Fatalf("list tools: %v", err)
	}
	_, err = tools[0].Run(context.Background(), tool.Call{Args: jsontext.Value(`{"text":"x"}`)}, tool.Env{})
	if err == nil {
		t.Fatal("call against a server that fails re-initialize succeeded")
	}
	requireProcessGone(t, pidFile)
}
