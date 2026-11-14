//go:build !windows

package mcp_test

import (
	"context"
	"encoding/json/jsontext"
	"errors"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"syscall"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/mcp"
	"github.com/gosuda/dalgona/internal/tool"
)

func writeGroupServerScript(t *testing.T, script string) {
	t.Helper()
	scriptBody := "#!/bin/sh\n" +
		"sleep 30 &\n" +
		"echo $! > \"$GRANDCHILD_PID_FILE\"\n" +
		"read -r line\n" +
		"id=$(printf '%s' \"$line\" | grep -o '\"id\":[0-9]*' | tr -d -c 0-9)\n" +
		"printf '{\"jsonrpc\":\"2.0\",\"id\":%s,\"result\":{\"protocolVersion\":\"2025-11-25\",\"capabilities\":{\"tools\":{}},\"serverInfo\":{\"name\":\"t\",\"version\":\"0\"}}}\\n' \"$id\"\n" +
		"read -r line\n" +
		"read -r line\n" +
		"id=$(printf '%s' \"$line\" | grep -o '\"id\":[0-9]*' | tr -d -c 0-9)\n" +
		"printf '{\"jsonrpc\":\"2.0\",\"id\":%s,\"result\":{\"tools\":[{\"name\":\"t__echo\",\"description\":\"d\",\"inputSchema\":{\"type\":\"object\"}}],\"nextCursor\":\"\"}}\\n' \"$id\"\n" +
		"read -r line\n" +
		"exec sleep 30\n"
	if err := os.WriteFile(script, []byte(scriptBody), 0o755); err != nil {
		t.Fatal(err)
	}
}

func startGroupServerClient(t *testing.T, script, pidFile string) *mcp.Client {
	t.Helper()
	client, err := mcp.New(context.Background(), mcp.Config{
		Name:    "group",
		Command: script,
		Env:     []string{"GRANDCHILD_PID_FILE=" + pidFile},
	})
	if err != nil {
		t.Fatalf("start MCP client: %v", err)
	}
	t.Cleanup(func() { _ = client.Close() })
	return client
}

func readGroupGrandchildPID(t *testing.T, pidFile string) int {
	t.Helper()
	raw, err := os.ReadFile(pidFile)
	if err != nil {
		t.Fatalf("read grandchild pid: %v", err)
	}
	grandchild, err := strconv.Atoi(strings.TrimSpace(string(raw)))
	if err != nil || grandchild <= 0 {
		t.Fatalf("parse grandchild pid %q: %v", raw, err)
	}
	if err := syscall.Kill(grandchild, 0); err != nil {
		t.Fatalf("grandchild %d not running before cancel: %v", grandchild, err)
	}
	return grandchild
}

func cancelBlockedToolCall(t *testing.T, ctx context.Context, callTool tool.Tool) {
	t.Helper()
	type callOut struct {
		err error
	}
	done := make(chan callOut, 1)
	go func() {
		_, err := callTool.Run(ctx, tool.Call{Args: jsontext.Value(`{"text":"x"}`)}, tool.Env{})
		done <- callOut{err: err}
	}()
	select {
	case out := <-done:
		if out.err == nil {
			t.Fatal("blocked call succeeded, want cancellation error")
		}
	case <-time.After(15 * time.Second):
		t.Fatal("cancelled call did not return within 15s while a grandchild holds stdout")
	}
}

func closeGroupClient(t *testing.T, client *mcp.Client) {
	t.Helper()
	closeDone := make(chan error, 1)
	go func() { closeDone <- client.Close() }()
	select {
	case err := <-closeDone:
		if err != nil {
			t.Fatalf("close: %v", err)
		}
	case <-time.After(10 * time.Second):
		t.Fatal("Close did not return within 10s while a grandchild holds stdout")
	}
}

func waitGroupGrandchildGone(t *testing.T, grandchild int) {
	t.Helper()
	deadline := time.Now().Add(5 * time.Second)
	for {
		err := syscall.Kill(grandchild, 0)
		if errors.Is(err, syscall.ESRCH) || errors.Is(err, os.ErrProcessDone) {
			return
		}
		if time.Now().After(deadline) {
			t.Fatalf("grandchild %d survived Close", grandchild)
		}
		time.Sleep(50 * time.Millisecond)
	}
}

func TestStdioCloseKillsGrandchildHoldingStdout(t *testing.T) {
	dir := t.TempDir()
	script := filepath.Join(dir, "group-server.sh")
	pidFile := filepath.Join(dir, "grandchild.pid")
	writeGroupServerScript(t, script)
	client := startGroupServerClient(t, script, pidFile)
	tools, err := client.Tools(context.Background())
	if err != nil {
		t.Fatalf("list tools: %v", err)
	}
	if len(tools) != 1 {
		t.Fatalf("got %d tools, want 1", len(tools))
	}
	grandchild := readGroupGrandchildPID(t, pidFile)

	callCtx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()
	cancelBlockedToolCall(t, callCtx, tools[0])
	closeGroupClient(t, client)
	waitGroupGrandchildGone(t, grandchild)
}
