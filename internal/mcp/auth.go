package mcp

import (
	"cmp"
	"context"
	"encoding/json/v2"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"runtime"
	"slices"
	"sync"
	"time"

	"github.com/gosuda/dalgona/internal/auth"
	"github.com/gosuda/dalgona/internal/auth/oauth"
)

const (
	RegistrationFile = "mcp-oauth.json"
	credentialPrefix = "mcp-"
	oauthType        = "oauth"
)

const (
	AuthUnsupported = "unsupported"
	AuthBearerToken = "bearerToken"
	AuthOAuth       = "oAuth"
	AuthNotLoggedIn = "notLoggedIn"
)

var (
	errNotLoggedIn        = errors.New("not logged in")
	errRefreshUnsupported = errors.New("token has no refresh grant")
)

type TokenSource interface {
	Token(ctx context.Context) (string, error)
	Refresh(ctx context.Context) (string, error)
}

type LoginRequiredError struct {
	Name string
	Err  error
}

func (e *LoginRequiredError) Error() string {
	return "MCP server " + e.Name + " needs login: run dalgona mcp login " + e.Name
}

func (e *LoginRequiredError) Unwrap() error { return e.Err }

type Registration struct {
	Resource              string   `json:"resource"`
	Issuer                string   `json:"issuer"`
	AuthorizationEndpoint string   `json:"authorization_endpoint"`
	TokenEndpoint         string   `json:"token_endpoint"`
	ClientID              string   `json:"client_id"`
	Scopes                []string `json:"scopes,omitempty"`
}

type AuthOptions struct {
	Name           string
	URL            string
	BearerTokenEnv string
	OAuth          bool
	OAuthClientID  string
	Home           string
	Store          *auth.Store
}

func CredentialKey(name string) string { return credentialPrefix + name }

func TokenCredential(resource string, token oauth.Token) auth.Credential {
	credential := auth.Credential{Type: oauthType, Access: token.Access, Refresh: token.Refresh, Account: resource}
	if !token.Expires.IsZero() {
		credential.Expires = token.Expires.UTC().Format(time.RFC3339)
	}
	return credential
}

func NewTokenSource(opts AuthOptions) (TokenSource, error) {
	if opts.BearerTokenEnv != "" {
		return bearerSource{name: opts.Name, env: opts.BearerTokenEnv}, nil
	}
	if opts.OAuth {
		source, err := newOAuthSource(opts)
		if err != nil {
			return nil, err
		}
		return source, nil
	}
	return nil, nil
}

func AuthStatus(opts AuthOptions) string {
	switch {
	case opts.URL == "":
		return AuthUnsupported
	case opts.BearerTokenEnv != "":
		return bearerStatus(opts.BearerTokenEnv)
	case opts.OAuth:
		return oauthStatus(opts)
	default:
		return AuthUnsupported
	}
}

func bearerStatus(env string) string {
	if os.Getenv(env) == "" {
		return AuthNotLoggedIn
	}
	return AuthBearerToken
}

func oauthStatus(opts AuthOptions) string {
	source, err := newOAuthSource(opts)
	if err != nil || source.ready() != nil {
		return AuthNotLoggedIn
	}
	return AuthOAuth
}

type bearerSource struct {
	name string
	env  string
}

func (b bearerSource) Token(context.Context) (string, error) {
	value := os.Getenv(b.env)
	if value == "" {
		return "", fmt.Errorf("bearer_token_env %s is not set", b.env)
	}
	return value, nil
}

func (b bearerSource) Refresh(context.Context) (string, error) {
	return "", fmt.Errorf("MCP server %s rejected the token in bearer_token_env %s: %w", b.name, b.env, errRefreshUnsupported)
}

type oauthSource struct {
	name     string
	resource string
	clientID string
	home     string
	store    *auth.Store
	mu       sync.Mutex
	entry    *sourceEntry
}

type sourceEntry struct {
	stored       auth.Credential
	saving       auth.Credential
	registration Registration
	source       *oauth.Source
}

func newOAuthSource(opts AuthOptions) (*oauthSource, error) {
	resource, err := oauth.CanonicalResource(opts.URL)
	if err != nil {
		return nil, fmt.Errorf("mcp: server %s: %w", opts.Name, err)
	}
	return &oauthSource{name: opts.Name, resource: resource, clientID: opts.OAuthClientID, home: opts.Home, store: opts.Store}, nil
}

func (s *oauthSource) Token(ctx context.Context) (string, error) {
	entry, err := s.current()
	if err != nil {
		return "", err
	}
	token, err := entry.source.Token(ctx)
	return token.Access, err
}

func (s *oauthSource) Refresh(ctx context.Context) (string, error) {
	entry, err := s.current()
	if err != nil {
		return "", err
	}
	token, err := entry.source.Refresh(ctx)
	return token.Access, err
}

func (s *oauthSource) ready() error {
	_, err := s.current()
	return err
}

func (s *oauthSource) current() (*sourceEntry, error) {
	credential, err := s.credential()
	if err != nil {
		return nil, err
	}
	registration, err := s.registration()
	if err != nil {
		return nil, err
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	if entry := s.entry; entry != nil && entry.registration.same(registration) && (entry.stored == credential || entry.saving == credential) {
		return entry, nil
	}
	entry := &sourceEntry{stored: credential, registration: registration}
	provider := oauth.Provider{
		ID: CredentialKey(s.name), AuthorizeURL: registration.AuthorizationEndpoint, TokenURL: registration.TokenEndpoint,
		ClientID: registration.ClientID, Scopes: slices.Clone(registration.Scopes), Resource: s.resource,
	}
	load := func(context.Context) (oauth.Token, error) { return tokenFrom(credential) }
	save := func(_ context.Context, token oauth.Token) error { return s.save(entry, s.credentialFrom(token)) }
	entry.source = oauth.NewSource(provider, load, save)
	s.entry = entry
	return entry, nil
}

func (s *oauthSource) save(entry *sourceEntry, credential auth.Credential) error {
	s.mu.Lock()
	expected := entry.stored
	entry.saving = credential
	s.mu.Unlock()
	written, err := s.store.SetCredentialIfUnchanged(CredentialKey(s.name), expected, credential)
	s.mu.Lock()
	if written {
		entry.stored = credential
	}
	entry.saving = auth.Credential{}
	s.mu.Unlock()
	if err != nil {
		return fmt.Errorf("save %s token: %w", CredentialKey(s.name), err)
	}
	return nil
}

func (s *oauthSource) credential() (auth.Credential, error) {
	if s.store == nil {
		return auth.Credential{}, errNotLoggedIn
	}
	key := CredentialKey(s.name)
	entries := s.store.Entries()
	index := slices.IndexFunc(entries, func(entry auth.ProviderEntry) bool { return entry.ID == key })
	if index < 0 {
		return auth.Credential{}, errNotLoggedIn
	}
	credential := entries[index].Credential
	if credential.Type != oauthType || cmp.Or(credential.Access, credential.Refresh) == "" {
		return auth.Credential{}, errNotLoggedIn
	}
	if credential.Account != s.resource {
		return auth.Credential{}, fmt.Errorf("%s token was issued for %s, not %s; run dalgona mcp login %s", key, cmp.Or(credential.Account, "no resource"), s.resource, s.name)
	}
	return credential, nil
}

func (s *oauthSource) registration() (Registration, error) {
	registration, err := ReadRegistration(s.home, s.name)
	if err != nil {
		return Registration{}, err
	}
	relogin := "; run dalgona mcp login " + s.name
	if registration.Resource != s.resource {
		return Registration{}, fmt.Errorf("%s registration is for %s, not %s%s", RegistrationFile, cmp.Or(registration.Resource, "no resource"), s.resource, relogin)
	}
	if s.clientID != "" && registration.ClientID != s.clientID {
		return Registration{}, fmt.Errorf("%s registration uses client %s, not oauth_client_id %s%s", RegistrationFile, registration.ClientID, s.clientID, relogin)
	}
	if registration.ClientID == "" || !oauth.SecureURL(registration.Issuer) || !oauth.SecureURL(registration.TokenEndpoint) || !oauth.SecureURL(registration.AuthorizationEndpoint) {
		return Registration{}, fmt.Errorf("%s registration for %s is incomplete or insecure%s", RegistrationFile, s.name, relogin)
	}
	return registration, nil
}

func (r Registration) same(other Registration) bool {
	return r.Resource == other.Resource && r.Issuer == other.Issuer && r.AuthorizationEndpoint == other.AuthorizationEndpoint &&
		r.TokenEndpoint == other.TokenEndpoint && r.ClientID == other.ClientID && slices.Equal(r.Scopes, other.Scopes)
}

func ReadRegistration(home, name string) (Registration, error) {
	data, err := readRegistrationFile(filepath.Join(home, RegistrationFile))
	if err != nil {
		return Registration{}, err
	}
	var registrations map[string]Registration
	if decodeErr := json.Unmarshal(data, &registrations); decodeErr != nil {
		return Registration{}, fmt.Errorf("decode %s: %w", RegistrationFile, decodeErr)
	}
	registration, ok := registrations[name]
	if !ok {
		return Registration{}, errNotLoggedIn
	}
	return registration, nil
}

func WriteRegistration(home, name string, registration Registration) error {
	path := filepath.Join(home, RegistrationFile)
	registrations := map[string]Registration{}
	data, err := os.ReadFile(path)
	switch {
	case errors.Is(err, os.ErrNotExist):
	case err != nil:
		return fmt.Errorf("read %s: %w", RegistrationFile, err)
	default:
		if decodeErr := json.Unmarshal(data, &registrations); decodeErr != nil {
			return fmt.Errorf("decode %s: %w", RegistrationFile, decodeErr)
		}
	}
	registrations[name] = registration
	encoded, err := json.Marshal(registrations)
	if err != nil {
		return fmt.Errorf("encode %s: %w", RegistrationFile, err)
	}
	if err := os.WriteFile(path, encoded, 0o600); err != nil {
		return fmt.Errorf("write %s: %w", RegistrationFile, err)
	}
	return nil
}

func readRegistrationFile(path string) ([]byte, error) {
	info, err := os.Lstat(path)
	if errors.Is(err, os.ErrNotExist) {
		return nil, errNotLoggedIn
	}
	if err != nil {
		return nil, fmt.Errorf("inspect %s: %w", RegistrationFile, err)
	}
	if !info.Mode().IsRegular() || (runtime.GOOS != "windows" && info.Mode().Perm()&0o077 != 0) {
		return nil, fmt.Errorf("%s must be a regular file with mode 0600", RegistrationFile)
	}
	data, err := os.ReadFile(path)
	if err != nil {
		return nil, fmt.Errorf("read %s: %w", RegistrationFile, err)
	}
	return data, nil
}

func tokenFrom(credential auth.Credential) (oauth.Token, error) {
	token := oauth.Token{Access: credential.Access, Refresh: credential.Refresh, Account: credential.Account}
	if credential.Expires == "" {
		return token, nil
	}
	expires, err := time.Parse(time.RFC3339, credential.Expires)
	if err != nil {
		return oauth.Token{}, fmt.Errorf("credential expiry: %w", err)
	}
	token.Expires = expires
	return token, nil
}

func (s *oauthSource) credentialFrom(token oauth.Token) auth.Credential {
	return TokenCredential(s.resource, token)
}
