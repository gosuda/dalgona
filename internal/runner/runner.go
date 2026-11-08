package runner

import (
	"context"
	"io/fs"
	"time"
)

type SandboxMode string

const (
	SandboxWorkspaceWrite SandboxMode = "workspace-write"
	SandboxOff            SandboxMode = "off"
)

type Sandbox struct {
	Mode          SandboxMode
	WritableRoots []string
}

type ProcessSpec struct {
	ID      string
	Argv    []string
	Dir     string
	Env     []string
	Sandbox Sandbox
}

type Exit struct {
	Code    int       `json:"code"`
	Signal  string    `json:"signal"`
	EndedAt time.Time `json:"ended_at"`
}

type Output struct {
	Data  []byte
	Next  int64
	Exit  *Exit
	Alive bool
	Path  string
}

type Signal int

const (
	SignalTerm Signal = iota + 1
	SignalKill
)

type FileInfo struct {
	Name    string
	Size    int64
	Mode    fs.FileMode
	ModTime time.Time
}

type Processes interface {
	Start(ctx context.Context, spec ProcessSpec) error
	Read(ctx context.Context, id string, offset int64) (Output, error)
	Signal(ctx context.Context, id string, s Signal) error
}

type Files interface {
	ReadFile(ctx context.Context, path string) ([]byte, error)
	WriteFile(ctx context.Context, path string, data []byte, perm fs.FileMode) error
	Stat(ctx context.Context, path string) (FileInfo, error)
	ReadDir(ctx context.Context, path string) ([]FileInfo, error)
	Remove(ctx context.Context, path string) error
}

type Runner interface {
	Processes
	Files
	Close() error
}
