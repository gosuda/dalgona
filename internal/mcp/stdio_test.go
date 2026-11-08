package mcp_test

import (
	"bytes"
	"context"
	"encoding/json/jsontext"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/mcp"
	"github.com/gosuda/dalgona/internal/tool"
)

func TestStdioTools(t *testing.T) {
	t.Parallel()
	client := newStdioClient(t, nil)
	tools, err := client.Tools(t.Context())
	if err != nil {
		t.Fatalf("list tools: %v", err)
	}
	if len(tools) != 2 {
		t.Fatalf("got %d tools, want 2", len(tools))
	}
	if tools[0].Spec().Name != "test__echo" || tools[1].Spec().Name != "test__other" {
		t.Fatalf("unexpected pagination order: %s, %s", tools[0].Spec().Name, tools[1].Spec().Name)
	}
	echo := tools[0]
	if echo.Class() != tool.ClassRemote {
		t.Fatalf("class = %v, want ClassRemote", echo.Class())
	}
	wantSchema := `{"type":"object","properties":{"text":{"type":"string"}},"required":["text"],"x-dalgona-passthrough":true}`
	if string(echo.Spec().Schema) != wantSchema {
		t.Fatalf("schema changed:\n got %s\nwant %s", echo.Spec().Schema, wantSchema)
	}
	assertCall(t, echo, `{"text":"hello"}`, true, "hello")
	assertCall(t, echo, `{"text":"fail"}`, false, "echo failed")
	assertCall(t, echo, `{"text":"blocks"}`, true, "hello\n[image image/png]\n[resource main.rs: file:///project/src/main.rs]")
}

func TestStdioRestartsOncePerCall(t *testing.T) {
	t.Parallel()
	dir := t.TempDir()
	startLog := filepath.Join(dir, "starts.log")
	client := newStdioClient(t, []string{
		"START_LOG=" + startLog,
		"CRASH_FILE=" + filepath.Join(dir, "crash.once"),
	})
	tools, err := client.Tools(t.Context())
	if err != nil {
		t.Fatalf("list tools: %v", err)
	}
	echo := tools[0]
	assertCall(t, echo, `{"text":"crash"}`, true, "recovered")
	_, err = echo.Run(t.Context(), tool.Call{Args: jsontext.Value(`{"text":"always"}`)}, tool.Env{})
	if err == nil {
		t.Fatal("call with an always-crashing server succeeded")
	}
	assertStartCount(t, startLog, 3)
}

func TestStdioProtocolErrorPropagates(t *testing.T) {
	t.Parallel()
	client := newStdioClient(t, nil)
	tools, err := client.Tools(t.Context())
	if err != nil {
		t.Fatalf("list tools: %v", err)
	}
	_, err = tools[0].Run(t.Context(), tool.Call{Args: jsontext.Value(`{"text":"error"}`)}, tool.Env{})
	if err == nil {
		t.Fatal("call with a server-side protocol error succeeded")
	}
	if !strings.Contains(err.Error(), "server error -32602: Unknown tool: error") {
		t.Fatalf("protocol error = %v", err)
	}
	assertCall(t, tools[0], `{"text":"hello"}`, true, "hello")
}

func newStdioClient(t *testing.T, extraEnv []string) *mcp.Client {
	t.Helper()
	binary := buildServer(t)
	client, err := mcp.New(context.Background(), mcp.Config{
		Name:    "test",
		Command: binary,
		Env:     extraEnv,
	})
	if err != nil {
		t.Fatalf("start MCP client: %v", err)
	}
	t.Cleanup(func() {
		if err := client.Close(); err != nil {
			t.Errorf("close MCP client: %v", err)
		}
	})
	return client
}

func buildServer(t *testing.T) string {
	t.Helper()
	name := "mcp-test-server"
	if runtime.GOOS == "windows" {
		name += ".exe"
	}
	binary := filepath.Join(t.TempDir(), name)
	command := exec.CommandContext(t.Context(), "go", "build", "-o", binary, "./testdata/server")
	output, err := command.CombinedOutput()
	if err != nil {
		t.Fatalf("build test server: %v\n%s", err, output)
	}
	return binary
}

func assertCall(t *testing.T, target tool.Tool, args string, wantOK bool, wantOutput string) {
	t.Helper()
	result, err := target.Run(t.Context(), tool.Call{Args: jsontext.Value(args)}, tool.Env{})
	if err != nil {
		t.Fatalf("call %s: %v", args, err)
	}
	if result.OK != wantOK || string(result.Output) != wantOutput {
		t.Fatalf("call %s = (%v, %q), want (%v, %q)", args, result.OK, result.Output, wantOK, wantOutput)
	}
}

func assertStartCount(t *testing.T, path string, want int) {
	t.Helper()
	raw, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("read start log: %v", err)
	}
	if got := bytes.Count(raw, []byte("start\n")); got != want {
		t.Fatalf("server started %d times, want %d", got, want)
	}
}
