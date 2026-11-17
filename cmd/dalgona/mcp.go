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

	"github.com/gosuda/dalgona/internal/auth"
	"github.com/gosuda/dalgona/internal/auth/oauth"
	"github.com/gosuda/dalgona/internal/config"
	"github.com/gosuda/dalgona/internal/mcp"
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

func mcpLogin(ctx context.Context, cfg config.Config, name string, std stdio) error {
	spec, ok := cfg.MCPServers[name]
	if !ok {
		return fmt.Errorf("mcp server %s is not configured", name)
	}
	if !spec.OAuth {
		return fmt.Errorf("mcp server %s has no oauth value", name)
	}
	client := &http.Client{Timeout: 30 * time.Second}
	resolved, err := mcpResolve(ctx, client, spec)
	if err != nil {
		return err
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
	token, err := oauth.PKCE(ctx, provider, func(url string) error {
		_, writeErr := fmt.Fprintf(std.out, "Open this URL to log in to %s:\n%s\n", name, url)
		return writeErr
	}, pasteCode(ctx, bufio.NewReader(std.in), std.out))
	if err != nil {
		return err
	}
	registration := mcp.Registration{
		Resource:              resolved.resource.URL,
		Issuer:                resolved.meta.Issuer,
		AuthorizationEndpoint: resolved.meta.AuthorizationEndpoint,
		TokenEndpoint:         resolved.meta.TokenEndpoint,
		ClientID:              resolved.clientID,
		Scopes:                resolved.scopes,
	}
	if writeErr := mcp.WriteRegistration(cfg.Home, name, registration); writeErr != nil {
		return writeErr
	}
	store, err := auth.NewWithPatterns(cfg.Home, cfg.Secrets.Patterns, auth.WithBuiltinPatterns(cfg.Secrets.Builtin))
	if err != nil {
		return err
	}
	if credErr := store.SetCredential(mcp.CredentialKey(name), mcp.TokenCredential(resolved.resource.URL, token)); credErr != nil {
		return credErr
	}
	fmt.Fprintf(std.out, "logged in to MCP server %s\n", name)
	return nil
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
