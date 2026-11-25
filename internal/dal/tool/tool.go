package tool

import (
	"context"
	"encoding/json/jsontext"
	"errors"

	"github.com/gosuda/dalgona/internal/dal/runner"
	"github.com/gosuda/dalgona/internal/dal/tool/snapshot"
)

type Class int

const (
	ClassRead Class = iota + 1
	ClassEdit
	ClassExec
	ClassAgent
	ClassScript
	ClassRemote
)

const YieldName = "yield"

type Keep int

const (
	KeepHead Keep = iota
	KeepTail
)

type Spec struct {
	Name        string
	Description string
	Schema      jsontext.Value
	Freeform    bool
	Keep        Keep
	Serial      bool
}

type Call struct {
	ID   string
	Name string
	Args jsontext.Value
}

type Masker interface {
	Mask(text string) string
	Unmask(text string) string
	UnmaskStrict(text string) (string, error)
}
type Env struct {
	Runner    runner.Runner
	Cwd       string
	Home      string
	Sandbox   runner.Sandbox
	Masker    Masker
	Snapshots *snapshot.Store
	Evidence  snapshot.Evidence
}

func IsLocalRunner(run runner.Runner) bool {
	_, ok := run.(*runner.Local)
	return ok
}

type FailureKind string

const (
	FailureNone     FailureKind = ""
	FailureInternal FailureKind = "internal"
)

func MachineryFailure(ctx context.Context, err error) FailureKind {
	if ctx.Err() != nil || errors.Is(err, context.Canceled) || errors.Is(err, context.DeadlineExceeded) {
		return FailureNone
	}
	return FailureInternal
}

type OrdinaryError struct{ Err error }

func (e *OrdinaryError) Error() string { return e.Err.Error() }

func (e *OrdinaryError) Unwrap() error { return e.Err }

func Ordinary(err error) error {
	if err == nil {
		return nil
	}
	return &OrdinaryError{Err: err}
}

func IsOrdinary(err error) bool {
	var ordinary *OrdinaryError
	return errors.As(err, &ordinary)
}

type Result struct {
	Output              []byte
	OK                  bool
	Meta                jsontext.Value
	ExitCode            *int
	Path                string
	Failure             FailureKind
	ObservedOutputBytes int64 `json:"-"`
	OutputTruncated     bool  `json:"-"`
}

type Tool interface {
	Spec() Spec
	Class() Class
	Run(ctx context.Context, call Call, env Env) (Result, error)
}

type FileChange struct {
	Path    string `json:"path"`
	PreSHA  string `json:"pre_sha"`
	PostSHA string `json:"post_sha"`
	PreBlob string `json:"pre_blob,omitempty"`
	Pre     []byte `json:"-"`
	Post    []byte `json:"-"`
}

type Plan struct {
	Files          []FileChange
	RequestedPaths []string
	Apply          func(ctx context.Context) (Result, error)
	Discard        func()
}

type Editor interface {
	Tool
	Plan(ctx context.Context, call Call, env Env) (Plan, error)
}
