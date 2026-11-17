package github

import (
	"context"
	"errors"
)

type UserCredential struct {
	resolve func(context.Context) (string, error)
}

func NewUserCredential(resolve func(context.Context) (string, error)) *UserCredential {
	return &UserCredential{resolve: resolve}
}

func (c *UserCredential) Token(ctx context.Context, request CredentialRequest) (Token, error) {
	if c == nil || c.resolve == nil {
		return Token{}, errors.New("github: user credential resolver is not configured")
	}
	value, err := c.resolve(ctx)
	if err != nil {
		return Token{}, err
	}
	if value == "" {
		return Token{}, errors.New("github: user credential resolved an empty token")
	}
	return Token{Value: value}, nil
}

func (c *UserCredential) Invalidate(ctx context.Context, request CredentialRequest) error {
	return nil
}
