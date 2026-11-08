package sandboxsvc

import (
	"context"
	"io"
)

type Endpoint int

const (
	ExecServer Endpoint = iota + 1
	Bridge
)

type Spec struct {
	Name     string
	Image    string
	Template string
}

type Service interface {
	Create(ctx context.Context, spec Spec) (id string, err error)
	Attach(ctx context.Context, id string) (current string, err error)
	Destroy(ctx context.Context, id string) error
	Connect(ctx context.Context, id string, e Endpoint) (io.ReadWriteCloser, error)
}
