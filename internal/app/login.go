package app

import (
	"bufio"
	"context"
	"errors"
	"fmt"
	"io"
	"slices"
	"strings"

	"github.com/gosuda/dalgona/internal/auth"
	"github.com/gosuda/dalgona/internal/auth/oauth"
)

var (
	apiKeyProviders = []string{"anthropic", "google", "openai", "openrouter"}
	oauthProviders  = []string{"antigravity", "chatgpt", "cursor", "devin"}
)

func Providers() []string {
	return slices.Sorted(slices.Values(slices.Concat(apiKeyProviders, oauthProviders)))
}

func Login(ctx context.Context, store *auth.Store, provider string, in io.Reader, out io.Writer) error {
	lines := bufio.NewReader(in)
	switch {
	case slices.Contains(oauthProviders, provider):
		token, err := oauthLogin(ctx, provider, lines, out)
		if err != nil {
			return fmt.Errorf("login %s: %w", provider, err)
		}
		return store.SetCredential(provider, credentialFrom(token))
	case slices.Contains(apiKeyProviders, provider):
		fmt.Fprintf(out, "Paste the %s API key: ", provider)
		key, err := lines.ReadString('\n')
		key = strings.TrimSpace(key)
		if key == "" {
			return errors.Join(fmt.Errorf("login %s: no API key was entered", provider), ignoreEOF(err))
		}
		return store.SetAPIKey(provider, key)
	default:
		return fmt.Errorf("unknown provider %q; known providers: %s", provider, strings.Join(Providers(), ", "))
	}
}

func ignoreEOF(err error) error {
	if errors.Is(err, io.EOF) {
		return nil
	}
	return err
}

func oauthLogin(ctx context.Context, provider string, lines *bufio.Reader, out io.Writer) (oauth.Token, error) {
	definition, ok := oauth.ProviderByID(provider)
	if !ok {
		return oauth.Token{}, fmt.Errorf("no OAuth flow for %s", provider)
	}
	open := func(url string) error {
		_, err := fmt.Fprintf(out, "Open this URL to log in to %s:\n%s\n", provider, url)
		return err
	}
	if provider == "cursor" {
		return oauth.CursorPoll(ctx, definition, open, nil)
	}
	paste := func(ctx context.Context) (string, error) {
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
	return oauth.PKCE(ctx, definition, open, paste)
}
