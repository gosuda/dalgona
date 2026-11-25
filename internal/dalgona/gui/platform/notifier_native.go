//go:build linux || windows || (darwin && cgo)

package platform

import "sync"

type notifierState struct {
	mu          sync.Mutex
	permission  NotificationPermission
	activations chan string
	wake        func()
}

func newNotifierState(wake func()) notifierState {
	return notifierState{activations: make(chan string, notificationActivationQueue), wake: wake}
}

func (s *notifierState) Permission() NotificationPermission {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.permission
}

func (s *notifierState) Activations() <-chan string { return s.activations }

func (s *notifierState) settle(p NotificationPermission) {
	s.mu.Lock()
	s.permission = p
	s.mu.Unlock()
}

func (s *notifierState) activate(tag string) {
	select {
	case s.activations <- tag:
	default:
		return
	}
	if s.wake != nil {
		s.wake()
	}
}
