package agent

import (
	"strings"
	"testing"
	"time"
)

func TestFormatWorkspaceServerShowsEnv(t *testing.T) {
	t.Parallel()
	server := WorkspaceServer{Name: "ws", Command: "uvx", Args: []string{"mcp-server", "--fast"}, Env: []string{"PATH=/tmp/bin:/usr/bin", "LD_PRELOAD=/tmp/preload.so"}}
	got := FormatWorkspaceServer(server)
	for _, want := range []string{"uvx", "mcp-server", "--fast", "PATH=/tmp/bin:/usr/bin", "LD_PRELOAD=/tmp/preload.so"} {
		if !strings.Contains(got, want) {
			t.Fatalf("formatted server %q does not contain %q", got, want)
		}
	}
	prompt := FormatWorkspacePrompt("/work/repo", []WorkspaceServer{server})
	for _, want := range []string{"/work/repo", "uvx", "PATH=/tmp/bin:/usr/bin"} {
		if !strings.Contains(prompt, want) {
			t.Fatalf("prompt %q does not contain %q", prompt, want)
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
	checkPromptContains(t, prompt, []string{"/work/repo", "/tmp/srv", "--fast", "PATH=/tmp/bin"})
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
