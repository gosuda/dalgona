package oauth

import (
	"context"
	"errors"
	"fmt"
	"time"
)

const refreshMargin = time.Minute

type Source struct {
	provider Provider
	load     func(context.Context) (Token, error)
	save     func(context.Context, Token) error
	turn     chan struct{}
	cached   Token
	loaded   bool
	unsaved  bool
}

func NewSource(provider Provider, load func(context.Context) (Token, error), save func(context.Context, Token) error) *Source {
	return &Source{provider: provider, load: load, save: save, turn: make(chan struct{}, 1)}
}

func (s *Source) Token(ctx context.Context) (Token, error) {
	select {
	case s.turn <- struct{}{}:
	case <-ctx.Done():
		return Token{}, fmt.Errorf("%s token: %w", s.provider.ID, ctx.Err())
	}
	defer func() { <-s.turn }()
	if err := s.ensureLoaded(ctx); err != nil {
		return Token{}, err
	}
	if s.unsaved {
		return s.persist(ctx)
	}
	if !expiring(s.cached) {
		return s.cached, nil
	}
	return s.refresh(ctx)
}

func (s *Source) Access(ctx context.Context) (string, error) {
	token, err := s.Token(ctx)
	return token.Access, err
}

func (s *Source) ensureLoaded(ctx context.Context) error {
	if s.loaded {
		return nil
	}
	token, err := s.load(ctx)
	if err != nil {
		return fmt.Errorf("load %s token: %w", s.provider.ID, err)
	}
	s.cached, s.loaded = token, true
	return nil
}

func expiring(token Token) bool {
	return !token.Expires.IsZero() && time.Until(token.Expires) <= refreshMargin
}

func (s *Source) refresh(ctx context.Context) (Token, error) {
	if s.cached.Refresh == "" {
		return Token{}, errors.New(s.provider.ID + " token expired and has no refresh token; log in again")
	}
	fresh, err := Refresh(ctx, s.provider, s.cached.Refresh)
	if err != nil {
		return Token{}, fmt.Errorf("refresh %s token: %w", s.provider.ID, err)
	}
	if fresh.Account == "" {
		fresh.Account = s.cached.Account
	}
	s.cached, s.unsaved = fresh, s.save != nil
	return s.persist(ctx)
}

func (s *Source) persist(ctx context.Context) (Token, error) {
	if !s.unsaved {
		return s.cached, nil
	}
	if err := s.save(ctx, s.cached); err != nil {
		return Token{}, fmt.Errorf("save %s token: %w", s.provider.ID, err)
	}
	s.unsaved = false
	return s.cached, nil
}
