package agent

import (
	"context"
	"log/slog"
	"time"

	"github.com/gosuda/dalgona/internal/agent/event"
	"github.com/gosuda/dalgona/internal/agent/prompt"
	"github.com/gosuda/dalgona/internal/auth"
	"github.com/gosuda/dalgona/internal/config"
	"github.com/gosuda/dalgona/internal/journal"
	"github.com/gosuda/dalgona/internal/model"
	"github.com/gosuda/dalgona/internal/runner"
	"github.com/gosuda/dalgona/internal/tool"
	"github.com/gosuda/dalgona/internal/tool/task"
)

type RetryFunc func(attempt int, verdict model.Verdict, status int, delay time.Duration)

type ProviderFactory func(onRetry RetryFunc) (*model.Registry, error)

type ToolSet func(ctx context.Context, modelID, cwd string) ([]tool.Tool, error)

type SkillSet func(cwd string) ([]prompt.Skill, error)

type RunnerFactory func(ctx context.Context, threadID, runnerName string) (runner.Runner, error)

type HookVerdict struct {
	Decision string
	Reason   string
	By       string
}

const (
	HookAllow = "allow"
	HookDeny  = "deny"
)

type ToolSurface interface {
	Tools(builtin []tool.Tool) []tool.Tool
	Visible(specs []tool.Spec) []tool.Spec
	Direct(name string) error
}

type ToolCallHook interface {
	ToolCall(ctx context.Context, call tool.Call) (HookVerdict, error)
}

type TurnEndHook interface {
	TurnEnd(ctx context.Context, turnID, status string) error
}

type Extension interface {
	ToolSurface
	ToolCallHook
	TurnEndHook
}

type ExtensionFactory func(t *Thread) Extension

type HostConfig struct {
	Journal        *journal.Store
	Config         config.Config
	Auth           *auth.Store
	Providers      ProviderFactory
	Tools          ToolSet
	Runner         RunnerFactory
	Decider        Decider
	Extension      ExtensionFactory
	OnChildStarted func(child *Thread, req task.Request)
	Product        config.Product
	Clock          func() time.Time
	Logger         *slog.Logger
	Skills         SkillSet
	ToolNotes      []string
	ProviderIDs    []string
}

type Overrides struct {
	Tools  []tool.Tool
	System string
}

type ThreadSpec struct {
	Cwd      string
	Model    string
	Approval config.Approval
	Sandbox  config.SandboxMode
	Effort   config.Reasoning
	Runner   string
	Parent   string
	Advisor  string
	Overrides
}

type InputKind string

const (
	InputPrompt   InputKind = "prompt"
	InputSteer    InputKind = "steer"
	InputFollowup InputKind = "followup"
)

type Input struct {
	Kind           InputKind
	Text           string
	Effort         string
	Model          string
	Approval       config.Approval
	Sandbox        config.SandboxMode
	Verbatim       bool
	ExpectedTurnID string
}

type Settings struct {
	Model    string
	Approval config.Approval
	Sandbox  config.SandboxMode
	Effort   config.Reasoning
}

type Reply string

const (
	ReplyAccept        Reply = "accept"
	ReplyAcceptSession Reply = "accept_session"
	ReplyDecline       Reply = "decline"
	ReplyCancel        Reply = "cancel"
)

type UpdateKind int

const (
	UpdateEvent UpdateKind = iota + 1
	UpdateTextDelta
	UpdateReasoningDelta
	UpdateApproval
	UpdateExecOutput
)

type ApprovalPrompt struct {
	RequestID string
	CallID    string
	Tool      string
	Class     tool.Class
	Summary   string
	Reason    string
	Command   string
	Commands  []string
	Paths     []string
	Resolved  bool
}

type Update struct {
	Kind     UpdateKind
	Thread   string
	Seq      uint64
	Event    event.Event
	ItemID   string
	CallID   string
	Delta    string
	Approval ApprovalPrompt
}

type ThreadInfo struct {
	ID       string
	Parent   string
	ForkOf   string
	Advisor  string
	Cwd      string
	Model    string
	Name     string
	Preview  string
	Archived bool
	Created  time.Time
	Updated  time.Time
	Head     uint64
	Damaged  bool
}
