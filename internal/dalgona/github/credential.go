package github

import "context"

type Credential interface {
	Token(ctx context.Context, request CredentialRequest) (Token, error)
}

type CredentialInvalidator interface {
	Invalidate(ctx context.Context, request CredentialRequest) error
}
