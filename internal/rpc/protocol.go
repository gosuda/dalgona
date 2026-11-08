package rpc

import (
	"context"
	"encoding/json/jsontext"
)

type Client = Conn

type (
	ClientInfo struct {
		Name    string `json:"name"`
		Title   string `json:"title,omitzero"`
		Version string `json:"version"`
	}
	InitializeCapabilities struct {
		ExperimentalAPI                bool           `json:"experimentalApi,omitzero"`
		ExplicitGatewayOAuth           bool           `json:"explicitGatewayOauth,omitzero"`
		Extensions                     jsontext.Value `json:"extensions,omitzero"`
		MCPServerOpenAIFormElicitation bool           `json:"mcpServerOpenaiFormElicitation,omitzero"`
		OptOutNotificationMethods      []string       `json:"optOutNotificationMethods,omitzero"`
		RequestAttestation             bool           `json:"requestAttestation,omitzero"`
		RequiredHumanApproval          bool           `json:"requiredHumanApproval,omitzero"`
	}
	InitializeParams struct {
		ClientInfo   ClientInfo              `json:"clientInfo"`
		Capabilities *InitializeCapabilities `json:"capabilities,omitzero"`
	}
	InitializeResult struct {
		UserAgent      string `json:"userAgent"`
		CodexHome      string `json:"codexHome"`
		PlatformFamily string `json:"platformFamily"`
		PlatformOS     string `json:"platformOs"`
	}
	ThreadID struct {
		ID string `json:"id"`
	}
	ThreadStartParams struct {
		Model          string `json:"model,omitzero"`
		ModelProvider  string `json:"modelProvider,omitzero"`
		ApprovalPolicy string `json:"approvalPolicy,omitzero"`
		Sandbox        string `json:"sandbox,omitzero"`
		Cwd            string `json:"cwd,omitzero"`
	}
	ThreadResumeParams struct {
		ThreadID string `json:"threadId"`
	}
	ThreadReadParams struct {
		ThreadID     string `json:"threadId"`
		IncludeTurns bool   `json:"includeTurns,omitzero"`
	}
	ThreadListParams struct {
		Cursor   string `json:"cursor,omitzero"`
		Limit    int    `json:"limit,omitzero"`
		Archived bool   `json:"archived,omitzero"`
	}
	ThreadNameSetParams struct {
		ThreadID string `json:"threadId"`
		Name     string `json:"name"`
	}
	ThreadArchiveParams struct {
		ThreadID string `json:"threadId"`
	}
	ThreadRevertParams struct {
		ThreadID     string `json:"threadId"`
		BeforeTurnID string `json:"beforeTurnId"`
	}
	ThreadForkParams struct {
		ThreadID   string `json:"threadId"`
		LastTurnID string `json:"lastTurnId,omitzero"`
	}
	TurnStartParams struct {
		ThreadID       string         `json:"threadId"`
		Input          []InputItem    `json:"input"`
		Model          string         `json:"model,omitzero"`
		Effort         string         `json:"effort,omitzero"`
		ApprovalPolicy string         `json:"approvalPolicy,omitzero"`
		SandboxPolicy  *SandboxPolicy `json:"sandboxPolicy,omitzero"`
	}
	InputItem struct {
		Type string         `json:"type"`
		Text string         `json:"text,omitzero"`
		Name string         `json:"name,omitzero"`
		Data jsontext.Value `json:"data,omitzero"`
	}
	TurnSteerParams struct {
		ThreadID       string      `json:"threadId"`
		ExpectedTurnID string      `json:"expectedTurnId,omitzero"`
		Input          []InputItem `json:"input"`
	}
	TurnInterruptParams struct {
		ThreadID string `json:"threadId"`
		TurnID   string `json:"turnId"`
	}
	ConfigValueWriteParams struct {
		KeyPath         string `json:"keyPath"`
		MergeStrategy   string `json:"mergeStrategy"`
		Value           any    `json:"value"`
		FilePath        string `json:"filePath,omitzero"`
		ExpectedVersion string `json:"expectedVersion,omitzero"`
	}
	AccountLoginStartParams struct {
		Type     string `json:"type,omitzero"`
		APIKey   string `json:"apiKey,omitzero"`
		Provider string `json:"provider,omitzero"`
		Method   string `json:"method,omitzero"`
	}
	AccountLoginCancelParams struct {
		LoginID string `json:"loginId"`
	}
	AccountLogoutParams struct {
		Provider string `json:"provider,omitzero"`
	}
	SkillsListParams struct{}
	ScriptRunParams  struct {
		ThreadID string   `json:"threadId"`
		Path     string   `json:"path"`
		Args     []string `json:"args,omitzero"`
	}
	ThreadTreeParams struct {
		ThreadID string `json:"threadId"`
	}
	ThreadLeafSetParams struct {
		ThreadID string `json:"threadId"`
		TurnID   string `json:"turnId"`
	}
	ApprovalRequest struct {
		CallID   string `json:"callId"`
		ThreadID string `json:"threadId,omitzero"`
	}
	ApprovalResponse struct {
		Decision string `json:"decision"`
	}
)

func (c *Conn) Initialize(ctx context.Context, p InitializeParams, out *InitializeResult) error {
	return c.Call(ctx, "initialize", p, out)
}

func (c *Conn) ThreadStart(ctx context.Context, p ThreadStartParams, out any) error {
	return c.Call(ctx, "thread/start", p, out)
}

func (c *Conn) ThreadResume(ctx context.Context, p ThreadResumeParams, out any) error {
	return c.Call(ctx, "thread/resume", p, out)
}

func (c *Conn) ThreadList(ctx context.Context, p ThreadListParams, out any) error {
	return c.Call(ctx, "thread/list", p, out)
}

func (c *Conn) ThreadRead(ctx context.Context, p ThreadReadParams, out any) error {
	return c.Call(ctx, "thread/read", p, out)
}

func (c *Conn) TurnStart(ctx context.Context, p TurnStartParams, out any) error {
	return c.Call(ctx, "turn/start", p, out)
}

func (c *Conn) TurnSteer(ctx context.Context, p TurnSteerParams, out any) error {
	return c.Call(ctx, "turn/steer", p, out)
}

func (c *Conn) TurnInterrupt(ctx context.Context, p TurnInterruptParams, out any) error {
	return c.Call(ctx, "turn/interrupt", p, out)
}
