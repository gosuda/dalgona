package model

import (
	"context"
	"errors"
	"strings"
)

type Registry struct{ providers map[string]Provider }

func NewRegistry() *Registry { return &Registry{providers: make(map[string]Provider)} }
func (r *Registry) Register(id string, provider Provider) error {
	id = strings.ToLower(strings.TrimSpace(id))
	if id == "" {
		return errors.New("provider id is empty")
	}
	if provider == nil {
		return errors.New("provider is nil")
	}
	if r.providers == nil {
		r.providers = make(map[string]Provider)
	}
	r.providers[id] = provider
	return nil
}

func (r *Registry) Provider(id string) (Provider, bool) {
	p, ok := r.providers[strings.ToLower(id)]
	return p, ok
}

func (r *Registry) Stream(ctx context.Context, id string, req Request) (Stream, error) {
	p, ok := r.Provider(id)
	if !ok {
		return nil, errors.New("unknown provider: " + id)
	}
	return p.Stream(ctx, req)
}
