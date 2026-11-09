package agent

import (
	"strings"
	"testing"
	"time"
)

func TestFormatWorkspaceServerShowsEnv(t *testing.T) {
	t.Parallel()
	server := WorkspaceServer{Name: "ws", Command: "uvx", Args: []string{"mcp-server", "--fast"}, Env: []string{"PATH=/tmp/bin:/usr/bin", "LD_PRELOAD=/tmp/preload.so", "API_KEY=live_secret"}}
	got := FormatWorkspaceServer(server)
	requireContains(t, got, []string{"uvx", "mcp-server", "--fast", "PATH=<redacted>", "LD_PRELOAD=<redacted>", "API_KEY=<redacted>"})
	requireAbsent(t, got, []string{"/tmp/bin", "/tmp/preload.so", "live_secret"})
	prompt := FormatWorkspacePrompt("/work/repo", []WorkspaceServer{server})
	requireContains(t, prompt, []string{"/work/repo", "uvx", "PATH=<redacted>", "API_KEY=<redacted>"})
	requireAbsent(t, prompt, []string{"/tmp/bin", "live_secret"})
	for _, cmd := range WorkspaceCommands([]WorkspaceServer{server}) {
		requireAbsent(t, cmd, []string{"live_secret", "/tmp/bin"})
	}
}

func requireContains(t *testing.T, haystack string, wants []string) {
	t.Helper()
	for _, want := range wants {
		if !strings.Contains(haystack, want) {
			t.Fatalf("value %q does not contain %q", haystack, want)
		}
	}
}

func requireAbsent(t *testing.T, haystack string, absents []string) {
	t.Helper()
	for _, absent := range absents {
		if strings.Contains(haystack, absent) {
			t.Fatalf("value %q leaks raw value %q", haystack, absent)
		}
	}
}

func awaitApprovalPrompt(t *testing.T, updates <-chan Update) ApprovalPrompt {
	t.Helper()
	select {
	case update := <-updates:
		if update.Kind != UpdateApproval {
			t.Fatalf("first update kind = %d, want approval", update.Kind)
		}
		return update.Approval
	case <-time.After(10 * time.Second):
		t.Fatal("no approval prompt received within 10s")
		return ApprovalPrompt{}
	}
}

func checkPromptContains(t *testing.T, prompt ApprovalPrompt, wants []string) {
	t.Helper()
	for _, want := range wants {
		if !strings.Contains(prompt.Summary, want) && !strings.Contains(prompt.Reason, want) {
			t.Fatalf("approval prompt %q does not contain %q", prompt.Summary, want)
		}
	}
}

func checkPromptAbsent(t *testing.T, prompt ApprovalPrompt, absents []string) {
	t.Helper()
	for _, absent := range absents {
		if strings.Contains(prompt.Summary, absent) || strings.Contains(prompt.Reason, absent) {
			t.Fatalf("approval prompt %q leaks raw value %q", prompt.Summary, absent)
		}
		for _, cmd := range prompt.Commands {
			if strings.Contains(cmd, absent) {
				t.Fatalf("approval command %q leaks raw value %q", cmd, absent)
			}
		}
	}
}

func TestRequestWorkspaceApprovalReachesClient(t *testing.T) {
	t.Parallel()
	h := newHarness(t)
	thread := h.start()
	updates := thread.Subscribe()
	defer thread.Unsubscribe(updates)
	servers := []WorkspaceServer{{Name: "wsfix", Command: "/tmp/srv", Args: []string{"--fast"}, Env: []string{"PATH=/tmp/bin"}}}
	type result struct {
		reply Reply
		err   error
	}
	done := make(chan result, 1)
	go func() {
		reply, err := thread.RequestWorkspaceApproval(t.Context(), WorkspaceApprovalRequest{Cwd: "/work/repo", Servers: servers})
		done <- result{reply: reply, err: err}
	}()
	prompt := awaitApprovalPrompt(t, updates)
	checkPromptContains(t, prompt, []string{"/work/repo", "/tmp/srv", "--fast", "PATH=<redacted>"})
	checkPromptAbsent(t, prompt, []string{"/tmp/bin"})
	if prompt.Tool != "mcp_workspace" {
		t.Fatalf("approval tool = %q, want mcp_workspace", prompt.Tool)
	}
	if err := thread.Answer(prompt.RequestID, ReplyAccept); err != nil {
		t.Fatal(err)
	}
	out := <-done
	if out.err != nil || out.reply != ReplyAccept {
		t.Fatalf("reply = %q, err = %v, want accept", out.reply, out.err)
	}
}

func TestRequestWorkspaceApprovalDeclinesWithoutClient(t *testing.T) {
	t.Parallel()
	h := newHarness(t)
	thread := h.start()
	reply, err := thread.RequestWorkspaceApproval(t.Context(), WorkspaceApprovalRequest{Cwd: "/work/repo"})
	if err != nil {
		t.Fatal(err)
	}
	if reply != ReplyDecline {
		t.Fatalf("reply = %q, want decline", reply)
	}
}
