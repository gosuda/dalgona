package main

import (
	"bufio"
	"context"
	"errors"
	"fmt"
	"io"
	"net/http"
	"strings"
	"time"

	"github.com/gosuda/dalgona/internal/dal/auth"
	"github.com/gosuda/dalgona/internal/dal/auth/oauth"
	"github.com/gosuda/dalgona/internal/dal/config"
	"github.com/gosuda/dalgona/internal/dalgona/mcp"
)

const mcpCallbackPath = "/callback"

func runMCP(ctx context.Context, args []string, std stdio) int {
	if len(args) != 2 || args[0] != "login" {
		return usage(std, "mcp login <name>")
	}
	cfg, err := loadConfig()
	if err != nil {
		return fail(std, 1, err)
	}
	if err := mcpLogin(ctx, cfg, args[1], std); err != nil {
		return fail(std, 1, err)
	}
	return 0
}

type oauthPrompt struct {
	open  func(string) error
	paste func(context.Context) (string, error)
}

func mcpLogin(ctx context.Context, cfg config.Config, name string, std stdio) error {
	spec, err := mcpOAuthSpec(cfg, name)
	if err != nil {
		return err
	}
	prompt := oauthPrompt{
		open: func(url string) error {
			_, writeErr := fmt.Fprintf(std.out, "Open this URL to log in to %s:\n%s\n", name, url)
			return writeErr
		},
		paste: pasteCode(ctx, bufio.NewReader(std.in), std.out),
	}
	registration, token, err := mcpAuthorize(ctx, name, spec, prompt)
	if err != nil {
		return err
	}
	store, err := auth.NewWithPatterns(cfg.Home, cfg.Secrets.Patterns, auth.WithBuiltinPatterns(cfg.Secrets.Builtin))
	if err != nil {
		return err
	}
	if err := mcpCommit(cfg.Home, store, name, registration, token); err != nil {
		return err
	}
	fmt.Fprintf(std.out, "logged in to MCP server %s\n", name)
	return nil
}

func mcpOAuthSpec(cfg config.Config, name string) (config.MCPServerSpec, error) {
	spec, ok := cfg.MCPServers[name]
	if !ok {
		return spec, fmt.Errorf("mcp server %s is not configured", name)
	}
	if !spec.OAuth {
		return spec, fmt.Errorf("mcp server %s has no oauth value", name)
	}
	return spec, nil
}

func mcpAuthorize(ctx context.Context, name string, spec config.MCPServerSpec, prompt oauthPrompt) (mcp.Registration, oauth.Token, error) {
	client := &http.Client{Timeout: 30 * time.Second}
	resolved, err := mcpResolve(ctx, client, spec)
	if err != nil {
		return mcp.Registration{}, oauth.Token{}, err
	}
	provider := oauth.Provider{
		ID:           "mcp-" + name,
		AuthorizeURL: resolved.meta.AuthorizationEndpoint,
		TokenURL:     resolved.meta.TokenEndpoint,
		ClientID:     resolved.clientID,
		Scopes:       resolved.scopes,
		RedirectHost: "127.0.0.1",
		RedirectPort: spec.OAuthCallbackPort,
		RedirectPath: mcpCallbackPath,
		Resource:     resolved.resource.URL,
	}
	token, err := oauth.PKCE(ctx, provider, prompt.open, prompt.paste)
	if err != nil {
		return mcp.Registration{}, oauth.Token{}, err
	}
	registration := mcp.Registration{
		Resource:              resolved.resource.URL,
		Issuer:                resolved.meta.Issuer,
		AuthorizationEndpoint: resolved.meta.AuthorizationEndpoint,
		TokenEndpoint:         resolved.meta.TokenEndpoint,
		ClientID:              resolved.clientID,
		Scopes:                resolved.scopes,
	}
	return registration, token, nil
}

func mcpCommit(home string, store *auth.Store, name string, registration mcp.Registration, token oauth.Token) error {
	if err := mcp.WriteRegistration(home, name, registration); err != nil {
		return err
	}
	return store.SetCredential(mcp.CredentialKey(name), mcp.TokenCredential(registration.Resource, token))
}

type mcpResolved struct {
	resource oauth.Resource
	meta     oauth.ServerMetadata
	clientID string
	scopes   []string
}

func mcpResolve(ctx context.Context, client *http.Client, spec config.MCPServerSpec) (mcpResolved, error) {
	resolved := mcpResolved{}
	resource, err := oauth.DiscoverResource(ctx, client, spec.URL, wwwAuthenticate(ctx, client, spec.URL))
	if err != nil {
		return resolved, err
	}
	resolved.resource = resource
	meta, err := oauth.DiscoverServer(ctx, client, resource.AuthorizationServer)
	if err != nil {
		return resolved, err
	}
	resolved.meta = meta
	resolved.scopes = spec.OAuthScopes
	if len(resolved.scopes) == 0 {
		resolved.scopes = resource.Scopes
	}
	resolved.clientID = spec.OAuthClientID
	if resolved.clientID != "" {
		return resolved, nil
	}
	redirectURI := fmt.Sprintf("http://127.0.0.1:%d%s", spec.OAuthCallbackPort, mcpCallbackPath)
	resolved.clientID, err = oauth.Register(ctx, client, meta, redirectURI, resolved.scopes)
	if err != nil {
		return resolved, err
	}
	return resolved, nil
}

func wwwAuthenticate(ctx context.Context, client *http.Client, rawURL string) string {
	request, err := http.NewRequestWithContext(ctx, http.MethodGet, rawURL, nil)
	if err != nil {
		return ""
	}
	response, err := client.Do(request)
	if err != nil {
		return ""
	}
	header := ""
	if response.StatusCode == http.StatusUnauthorized {
		header = response.Header.Get("WWW-Authenticate")
	}
	_ = response.Body.Close()
	return header
}

func pasteCode(ctx context.Context, lines *bufio.Reader, out io.Writer) func(context.Context) (string, error) {
	return func(ctx context.Context) (string, error) {
		fmt.Fprint(out, "Or paste the redirect URL or code here: ")
		line, err := lines.ReadString('\n')
		if line = strings.TrimSpace(line); line != "" {
			return line, nil
		}
		if errors.Is(err, io.EOF) {
			<-ctx.Done()
			return "", ctx.Err()
		}
		return "", err
	}
}
