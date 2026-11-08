package mcp_test

import (
	"testing"

	"github.com/gosuda/dalgona/internal/mcp"
)

func TestNewValidatesConfig(t *testing.T) {
	t.Parallel()
	if _, err := mcp.New(t.Context(), mcp.Config{Name: "x", Command: "mcp-server", URL: "http://127.0.0.1:1"}); err == nil || err.Error() != "mcp: exactly one of command or URL is required" {
		t.Fatalf("both transports = %v", err)
	}
	if _, err := mcp.New(t.Context(), mcp.Config{URL: "http://127.0.0.1:1"}); err == nil || err.Error() != "mcp: server name is required" {
		t.Fatalf("no name = %v", err)
	}
	if _, err := mcp.New(t.Context(), mcp.Config{Name: "x"}); err == nil || err.Error() != "mcp: exactly one of command or URL is required" {
		t.Fatalf("no transport = %v", err)
	}
}
