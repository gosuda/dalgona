package tool

import (
	"context"
	"encoding/json/jsontext"

	"github.com/gosuda/dalgona/internal/runner"
	"github.com/gosuda/dalgona/internal/tool/snapshot"
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
}

func IsLocalRunner(run runner.Runner) bool {
	_, ok := run.(*runner.Local)
	return ok
}

type Result struct {
	Output   []byte
	OK       bool
	Meta     jsontext.Value
	ExitCode *int
	Path     string
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
	Files   []FileChange
	Apply   func(ctx context.Context) (Result, error)
	Discard func()
}

type Editor interface {
	Tool
	Plan(ctx context.Context, call Call, env Env) (Plan, error)
}
