package fake

import (
	"context"
	"fmt"
	"io"
	"net"
	"strconv"
	"sync"

	"github.com/gosuda/dalgona/internal/sandboxsvc"
)

type Handler func(net.Conn)

type Service struct {
	mu        sync.Mutex
	nextID    uint64
	boxes     map[string]sandboxsvc.Spec
	created   int
	destroyed int
	handler   Handler
}

func New(handlers ...Handler) *Service {
	var handler Handler
	if len(handlers) > 0 {
		handler = handlers[0]
	}
	return &Service{boxes: make(map[string]sandboxsvc.Spec), handler: handler}
}

func (s *Service) Create(_ context.Context, spec sandboxsvc.Spec) (string, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.nextID++
	id := spec.Name
	if id == "" {
		id = "fake-" + strconv.FormatUint(s.nextID, 10)
	}
	s.boxes[id] = spec
	s.created++
	return id, nil
}

func (s *Service) Attach(_ context.Context, id string) (string, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if _, ok := s.boxes[id]; !ok {
		return "", fmt.Errorf("fake: unknown box %s", id)
	}
	return id, nil
}

func (s *Service) Destroy(_ context.Context, id string) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	if _, ok := s.boxes[id]; !ok {
		return fmt.Errorf("fake: unknown box %s: %w", id, sandboxsvc.ErrGone)
	}
	delete(s.boxes, id)
	s.destroyed++
	return nil
}

func (s *Service) Connect(_ context.Context, id string, _ sandboxsvc.Endpoint) (io.ReadWriteCloser, error) {
	s.mu.Lock()
	_, ok := s.boxes[id]
	handler := s.handler
	s.mu.Unlock()
	if !ok {
		return nil, fmt.Errorf("fake: unknown box %s", id)
	}
	left, right := net.Pipe()
	if handler != nil {
		go func() {
			handler(right)
			_ = right.Close()
		}()
	} else {
		_ = right.Close()
	}
	return left, nil
}

func (s *Service) CreateCount() int {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.created
}

func (s *Service) DestroyCount() int {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.destroyed
}
