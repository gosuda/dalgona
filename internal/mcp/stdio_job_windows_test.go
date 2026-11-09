//go:build windows

package mcp_test

import (
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"testing"
	"time"

	"golang.org/x/sys/windows"

	"github.com/gosuda/dalgona/internal/mcp"
)

const windowsStillActive = 259

func TestStdioCloseKillsJobGrandchild(t *testing.T) {
	binary := buildServer(t)
	pidFile := filepath.Join(t.TempDir(), "grandchild.pid")
	client, err := mcp.New(t.Context(), mcp.Config{
		Name:    "group",
		Command: binary,
		Env:     []string{"GRANDCHILD_PID_FILE=" + pidFile},
	})
	if err != nil {
		t.Fatalf("start MCP client: %v", err)
	}
	if _, err := client.Tools(t.Context()); err != nil {
		_ = client.Close()
		t.Fatalf("list tools: %v", err)
	}
	pid := waitPidFile(t, pidFile)
	if !windowsProcessAlive(pid) {
		_ = client.Close()
		t.Fatalf("grandchild %d not running before close", pid)
	}
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
	deadline := time.Now().Add(5 * time.Second)
	for windowsProcessAlive(pid) {
		if time.Now().After(deadline) {
			t.Fatalf("grandchild %d survived Close", pid)
		}
		time.Sleep(50 * time.Millisecond)
	}
}

func waitPidFile(t *testing.T, path string) int {
	t.Helper()
	deadline := time.Now().Add(5 * time.Second)
	for {
		raw, err := os.ReadFile(path)
		if err == nil {
			pid, convErr := strconv.Atoi(strings.TrimSpace(string(raw)))
			if convErr == nil && pid > 0 {
				return pid
			}
			t.Fatalf("parse grandchild pid %q: %v", raw, convErr)
		}
		if time.Now().After(deadline) {
			t.Fatalf("read grandchild pid: %v", err)
		}
		time.Sleep(20 * time.Millisecond)
	}
}

func windowsProcessAlive(pid int) bool {
	handle, err := windows.OpenProcess(windows.PROCESS_QUERY_LIMITED_INFORMATION, false, uint32(pid))
	if err != nil {
		return false
	}
	defer func() { _ = windows.CloseHandle(handle) }()
	var code uint32
	if err := windows.GetExitCodeProcess(handle, &code); err != nil {
		return false
	}
	return code == windowsStillActive
}
