package agent

import (
	"context"
	"slices"
	"strings"

	"github.com/gosuda/dalgona/internal/tool"
)

type WorkspaceServer struct {
	Name    string
	Command string
	Args    []string
	Env     []string
	URL     string
}

type WorkspaceApprovalRequest struct {
	Cwd     string
	Servers []WorkspaceServer
}

const workspaceApprovalTool = "mcp_workspace"

func (t *Thread) RequestWorkspaceApproval(ctx context.Context, req WorkspaceApprovalRequest) (Reply, error) {
	requestID := newID("req-")
	replies := make(chan Reply, 1)
	prompt := ApprovalPrompt{
		RequestID: requestID,
		Tool:      workspaceApprovalTool,
		Class:     tool.ClassExec,
		Summary:   FormatWorkspacePrompt(req.Cwd, req.Servers),
		Reason:    FormatWorkspacePrompt(req.Cwd, req.Servers),
		Commands:  WorkspaceCommands(req.Servers),
		Paths:     []string{req.Cwd},
	}
	offered, err := ask(t, func(a *actor) bool { return a.offerApproval(prompt, replies) })
	if err != nil {
		return ReplyDecline, err
	}
	forwarded, linked := t.forwardApproval(prompt, replies)
	if !forwarded && (!offered || linked) {
		t.discardApproval(requestID)
		return ReplyDecline, nil
	}
	select {
	case reply := <-replies:
		if forwarded {
			prompt.Resolved = true
			t.forwardApproval(prompt, nil)
		}
		return reply, nil
	case <-ctx.Done():
		t.discardApproval(requestID)
		if forwarded {
			prompt.Resolved = true
			t.forwardApproval(prompt, nil)
		}
		return ReplyCancel, ctx.Err()
	}
}

func (t *Thread) discardApproval(requestID string) {
	_, _ = ask(t, func(a *actor) struct{} {
		delete(a.approvals, requestID)
		return struct{}{}
	})
}

func FormatWorkspacePrompt(cwd string, servers []WorkspaceServer) string {
	ordered := slices.Clone(servers)
	slices.SortFunc(ordered, func(a, b WorkspaceServer) int { return strings.Compare(a.Name, b.Name) })
	lines := make([]string, 0, len(ordered)+1)
	lines = append(lines, "Trust MCP servers for workspace "+cwd+"?")
	for _, server := range ordered {
		lines = append(lines, FormatWorkspaceServer(server))
	}
	return strings.Join(lines, "\n")
}

func FormatWorkspaceServer(server WorkspaceServer) string {
	return server.Name + ": " + describeWorkspaceServer(server)
}

func WorkspaceCommands(servers []WorkspaceServer) []string {
	ordered := slices.Clone(servers)
	slices.SortFunc(ordered, func(a, b WorkspaceServer) int { return strings.Compare(a.Name, b.Name) })
	out := make([]string, 0, len(ordered))
	for _, server := range ordered {
		out = append(out, describeWorkspaceServer(server))
	}
	return out
}

func describeWorkspaceServer(server WorkspaceServer) string {
	var detail string
	switch {
	case server.URL != "":
		detail = server.URL
	case server.Command != "" || len(server.Args) > 0:
		detail = strings.TrimSpace(server.Command + " " + strings.Join(server.Args, " "))
	}
	if len(server.Env) > 0 {
		env := redactWorkspaceEnv(server.Env)
		slices.Sort(env)
		envText := "env " + strings.Join(env, " ")
		if detail == "" {
			detail = envText
		} else {
			detail += " " + envText
		}
	}
	if detail == "" {
		return server.Name
	}
	return detail
}

func redactWorkspaceEnv(env []string) []string {
	out := make([]string, 0, len(env))
	for _, entry := range env {
		key, _, ok := strings.Cut(entry, "=")
		if !ok {
			out = append(out, entry)
			continue
		}
		out = append(out, key+"=<redacted>")
	}
	return out
}

type threadContextKey struct{}

func ContextWithThread(ctx context.Context, t *Thread) context.Context {
	return context.WithValue(ctx, threadContextKey{}, t)
}

func ThreadFromContext(ctx context.Context) (*Thread, bool) {
	t, ok := ctx.Value(threadContextKey{}).(*Thread)
	return t, ok && t != nil
}
