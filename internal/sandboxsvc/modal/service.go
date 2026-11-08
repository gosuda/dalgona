package modal

import (
	"cmp"
	"context"
	"errors"
	"fmt"
	"io"
	"strings"
	"sync"
	"time"

	"github.com/gosuda/dalgona/internal/runner"
	"github.com/gosuda/dalgona/internal/sandboxsvc"
)

const defaultImage = "docker.io/library/buildpack-deps:stable"

type Process interface {
	io.ReadWriteCloser
	Wait() (int, error)
}

type Box interface {
	Exec(ctx context.Context, argv []string) (Process, error)
	Poll(ctx context.Context) (*int, error)
	Terminate(ctx context.Context) error
}

type CreateRequest struct {
	Name        string
	Sequence    int
	Volume      string
	Image       string
	Timeout     time.Duration
	IdleTimeout time.Duration
}

type API interface {
	Create(ctx context.Context, request CreateRequest) (Box, string, error)
	FromID(ctx context.Context, id string) (Box, error)
	DeleteVolume(ctx context.Context, name string) error
}

type Service struct {
	api       API
	bootstrap runner.BootstrapFunc
	image     string
	images    sync.Map
}

func New(api API, bootstrap runner.BootstrapFunc) *Service {
	return NewWithImage(api, defaultImage, bootstrap)
}

func NewWithImage(api API, image string, bootstrap runner.BootstrapFunc) *Service {
	return &Service{api: api, bootstrap: bootstrap, image: cmp.Or(image, defaultImage)}
}

func (s *Service) Create(ctx context.Context, spec sandboxsvc.Spec) (string, error) {
	if spec.Name == "" {
		return "", errors.New("modal: box name is required")
	}
	_, id, err := s.createBox(ctx, spec.Name, cmp.Or(spec.Image, s.image))
	return id, err
}

func (s *Service) createBox(ctx context.Context, volume, image string) (Box, string, error) {
	image = cmp.Or(image, defaultImage)
	request := CreateRequest{
		Name:        volume,
		Sequence:    1,
		Volume:      volume,
		Image:       image,
		Timeout:     24 * time.Hour,
		IdleTimeout: time.Hour,
	}
	box, sandboxID, err := s.api.Create(ctx, request)
	if err != nil {
		return nil, "", fmt.Errorf("modal: create sandbox: %w", err)
	}
	if box == nil || sandboxID == "" {
		return nil, "", errors.New("modal: create sandbox returned no id")
	}
	s.images.Store(volume, image)
	return box, sandboxID + "/" + volume, nil
}

func (s *Service) Attach(ctx context.Context, id string) (string, error) {
	sandboxID, volume, err := splitID(id)
	if err != nil {
		return "", err
	}
	box, err := s.api.FromID(ctx, sandboxID)
	if err != nil {
		return "", fmt.Errorf("modal: attach %s: %w", id, err)
	}
	exit, err := box.Poll(ctx)
	if err != nil {
		return "", fmt.Errorf("modal: poll %s: %w", id, err)
	}
	if exit != nil {
		return s.replace(ctx, id, volume)
	}
	if bootstrapErr := s.runBootstrap(ctx, id, box); bootstrapErr != nil {
		return "", bootstrapErr
	}
	return id, nil
}

func (s *Service) replace(ctx context.Context, id, volume string) (string, error) {
	image := s.image
	if stored, ok := s.images.Load(volume); ok {
		image, _ = stored.(string)
	}
	box, current, err := s.createBox(ctx, volume, image)
	if err != nil {
		return "", fmt.Errorf("modal: replace %s: %w", id, err)
	}
	bootstrapErr := s.runBootstrap(ctx, current, box)
	if bootstrapErr == nil {
		return current, nil
	}
	cleanupCtx, cancel := context.WithTimeout(context.WithoutCancel(ctx), time.Minute)
	defer cancel()
	if terminateErr := box.Terminate(cleanupCtx); terminateErr != nil {
		return "", errors.Join(bootstrapErr, fmt.Errorf("modal: terminate failed replacement %s: %w", current, terminateErr))
	}
	return "", bootstrapErr
}

func (s *Service) runBootstrap(ctx context.Context, id string, box Box) error {
	if s.bootstrap == nil {
		return nil
	}
	run := func(runCtx context.Context, argv []string) (runner.Stream, error) {
		return s.start(runCtx, box, argv)
	}
	if err := s.bootstrap(ctx, id, run); err != nil {
		return fmt.Errorf("modal: bootstrap %s: %w", id, err)
	}
	return nil
}

func (s *Service) Destroy(ctx context.Context, id string) error {
	sandboxID, volume, err := splitID(id)
	if err != nil {
		return err
	}
	box, err := s.api.FromID(ctx, sandboxID)
	if err != nil {
		return fmt.Errorf("modal: attach %s: %w", id, err)
	}
	if err := box.Terminate(ctx); err != nil {
		return fmt.Errorf("modal: terminate %s: %w", id, err)
	}
	if err := s.api.DeleteVolume(ctx, volume); err != nil {
		return fmt.Errorf("modal: delete volume %s: %w", volume, err)
	}
	s.images.Delete(volume)
	return nil
}

func (s *Service) Connect(ctx context.Context, id string, endpoint sandboxsvc.Endpoint) (io.ReadWriteCloser, error) {
	sandboxID, _, err := splitID(id)
	if err != nil {
		return nil, err
	}
	box, err := s.api.FromID(ctx, sandboxID)
	if err != nil {
		return nil, fmt.Errorf("modal: attach %s: %w", id, err)
	}
	argv := []string{"sh", "-c", `exec "$HOME/.dalgona/bin/dalgona" exec-server --listen stdio:// --root /workspace`}
	if endpoint == sandboxsvc.Bridge {
		argv = []string{"sh", "-c", `exec "$HOME/.dalgona/bin/dalgona" bridge`}
	}
	return s.start(ctx, box, argv)
}

func (s *Service) start(ctx context.Context, box Box, argv []string) (runner.Stream, error) {
	if len(argv) == 0 {
		return nil, errors.New("modal: empty command")
	}
	stream, err := box.Exec(ctx, argv)
	if err != nil {
		return nil, fmt.Errorf("modal: start process: %w", err)
	}
	return stream, nil
}

func splitID(id string) (string, string, error) {
	sandboxID, volume, found := strings.Cut(id, "/")
	if !found || sandboxID == "" || volume == "" || strings.Contains(volume, "/") {
		return "", "", fmt.Errorf("modal: invalid box id %s", id)
	}
	return sandboxID, volume, nil
}

var _ sandboxsvc.Service = (*Service)(nil)
