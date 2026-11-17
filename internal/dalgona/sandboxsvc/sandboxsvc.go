package sandboxsvc

import (
	"context"
	"errors"
	"io"
)

type Endpoint int

const (
	ExecServer Endpoint = iota + 1
	Bridge
)

func (e Endpoint) Argv() []string {
	if e == Bridge {
		return []string{"sh", "-c", `exec "$HOME/.dalgona/bin/dalgona" bridge`}
	}
	return []string{"sh", "-c", `exec "$HOME/.dalgona/bin/dalgona" exec-server --listen stdio:// --root /workspace`}
}

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

var ErrGone = errors.New("sandboxsvc: box is gone")

func IsGone(err error) bool {
	return errors.Is(err, ErrGone)
}
