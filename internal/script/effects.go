package script

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json/jsontext"
	"errors"
	"fmt"
)

const (
	OpTime    = "time"
	OpSleep   = "sleep"
	OpSpawn   = "spawn"
	OpJudge   = "judge"
	opToolPfx = "tool:"
)

type RunStart struct {
	ID     string
	Entry  string
	Source []byte
	Args   jsontext.Value
	Seed   int64
}

type Slot struct {
	Run     string
	N       int
	Op      string
	ArgsSHA string
}

type Recorded struct {
	Op          string
	ArgsSHA     string
	Result      jsontext.Value
	Error       string
	Interrupted bool
}

type SpawnOpts struct {
	Runner string
	Model  string
}

type JudgeRequest struct {
	State     jsontext.Value
	Questions jsontext.Value
	Model     string
}

type Journal interface {
	Started(start RunStart) (RunStart, error)
	Lookup(run string, n int) (Recorded, bool)
	Record(slot Slot, result jsontext.Value) error
	Completed(run string, outcome Outcome) error
}

type ToolCaller interface {
	CallTool(ctx context.Context, slot Slot, name string, args jsontext.Value) (jsontext.Value, error)
}

type Children interface {
	Spawn(ctx context.Context, slot Slot, prompt string, opts SpawnOpts) (string, error)
	Wait(ctx context.Context, ids []string) ([]string, error)
}

type Judger interface {
	Judge(ctx context.Context, slot Slot, req JudgeRequest) (jsontext.Value, error)
}

type Logger interface {
	Log(run, msg string)
}

type Effects interface {
	Journal
	ToolCaller
	Children
	Judger
	Logger
}

var ErrDenied = errors.New("denied")

func ToolOp(name string) string {
	return opToolPfx + name
}

func ArgsSHA(args jsontext.Value) string {
	sum := sha256.Sum256(args)
	return hex.EncodeToString(sum[:])
}

type Status string

const (
	StatusCompleted     Status = "completed"
	StatusFailed        Status = "failed"
	StatusDenied        Status = "denied"
	StatusCancelled     Status = "cancelled"
	StatusLimitExceeded Status = "limit_exceeded"
	StatusIndeterminate Status = "indeterminate"
)

type Failure struct {
	Code    string
	Message string
	Stage   string
}

type Prints struct {
	Lines     []string
	Truncated bool
}

type Outcome struct {
	Status Status
	Value  jsontext.Value
	Error  Failure
	Prints Prints
}

func complete(fx Effects, run string, outcome Outcome) (Outcome, error) {
	if err := fx.Completed(run, outcome); err != nil {
		return Outcome{}, fmt.Errorf("run %s: complete: %w", run, err)
	}
	return outcome, nil
}
