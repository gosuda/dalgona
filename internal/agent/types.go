package agent

import (
	"context"
	"encoding/json/jsontext"
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
)

type RetryFunc func(attempt int, verdict model.Verdict, status int, delay time.Duration)

type ProviderFactory func(onRetry RetryFunc) (*model.Registry, error)

type ToolSet func(modelID string) ([]tool.Tool, error)

type RunnerFactory func(ctx context.Context, threadID, runnerName string) (runner.Runner, error)

type HookEvent struct {
	Name   string
	Thread string
	Turn   string
	Tool   string
	Class  tool.Class
	Args   jsontext.Value
	Status string
}

type HookVerdict struct {
	Decision string
	Reason   string
	By       string
}

type Hook interface {
	Handle(ctx context.Context, e HookEvent) (HookVerdict, error)
}

type HostConfig struct {
	Journal   *journal.Store
	Config    config.Config
	Auth      *auth.Store
	Providers ProviderFactory
	Tools     ToolSet
	Runner    RunnerFactory
	Decider   Decider
	Hooks     []Hook
	Product   config.Product
	Clock     func() time.Time
	Logger    *slog.Logger
	Skills    []prompt.Skill
	ToolNotes []string
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
	Kind InputKind
	Text string
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
)

type ApprovalPrompt struct {
	RequestID string
	CallID    string
	Tool      string
	Class     tool.Class
	Summary   string
	Commands  []string
	Paths     []string
}

type Update struct {
	Kind     UpdateKind
	Thread   string
	Seq      uint64
	Event    event.Event
	ItemID   string
	Delta    string
	Approval ApprovalPrompt
}

type ThreadInfo struct {
	ID      string
	Parent  string
	ForkOf  string
	Advisor string
	Cwd     string
	Model   string
	Head    uint64
	Damaged bool
}
