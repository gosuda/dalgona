package copilot

import (
	"context"
	"errors"
	"fmt"
	"io"
	"net/http"
	"strings"
	"time"
	"unicode"

	json "encoding/json/v2"

	"github.com/gosuda/dalgona/internal/dal/secrets"
)

const (
	DefaultExchangeURL = "https://api.github.com/copilot_internal/v2/token"

	exchangeTimeout = 30 * time.Second
	refreshMargin   = time.Minute
	maxBodyBytes    = 1 << 20
	maxDetailRunes  = 200
	redacted        = "[redacted]"
)

type tokenSource struct {
	github   func(context.Context) (string, error)
	client   *http.Client
	endpoint string
	turn     chan struct{}
	cached   copilotToken
}

type copilotToken struct {
	value   string
	owner   string
	expires time.Time
}

func (t copilotToken) usable(owner string) bool {
	return t.value != "" && t.owner == owner && time.Until(t.expires) > refreshMargin
}

func newTokenSource(github func(context.Context) (string, error), client *http.Client, endpoint string) *tokenSource {
	return &tokenSource{github: github, client: client, endpoint: endpoint, turn: make(chan struct{}, 1)}
}

func (s *tokenSource) githubToken(ctx context.Context) (string, error) {
	if s.github == nil {
		return "", errors.New("copilot: not logged in; run /login copilot")
	}
	value, err := s.github(ctx)
	if err != nil {
		return "", fmt.Errorf("copilot: token: %w", err)
	}
	if value == "" {
		return "", errors.New("copilot: token is empty; run /login copilot")
	}
	return value, nil
}

func (s *tokenSource) bearer(ctx context.Context) (string, error) {
	github, err := s.githubToken(ctx)
	if err != nil {
		return "", err
	}
	select {
	case s.turn <- struct{}{}:
	case <-ctx.Done():
		return "", fmt.Errorf("copilot: token: %w", ctx.Err())
	}
	defer func() { <-s.turn }()
	if s.cached.usable(github) {
		return s.cached.value, nil
	}
	fresh, err := s.exchange(ctx, github)
	if err != nil {
		return "", err
	}
	s.cached = fresh
	return fresh.value, nil
}

func (s *tokenSource) exchange(ctx context.Context, github string) (copilotToken, error) {
	status, body, err := s.fetch(ctx, github)
	if err != nil {
		return copilotToken{}, err
	}
	if status < http.StatusOK || status >= http.StatusMultipleChoices {
		return copilotToken{}, exchangeFailure(status, body, github)
	}
	return decodeToken(body, github)
}

func (s *tokenSource) fetch(ctx context.Context, github string) (int, []byte, error) {
	ctx, cancel := context.WithTimeout(ctx, exchangeTimeout)
	defer cancel()
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, s.endpoint, nil)
	if err != nil {
		return 0, nil, fmt.Errorf("copilot: create token request: %w", err)
	}
	req.Header.Set("Authorization", "Token "+github)
	req.Header.Set("Accept", "application/json")
	resp, err := s.client.Do(req)
	if err != nil {
		return 0, nil, fmt.Errorf("copilot: token exchange: %w", err)
	}
	defer resp.Body.Close()
	body, err := io.ReadAll(io.LimitReader(resp.Body, maxBodyBytes+1))
	if err != nil {
		return 0, nil, fmt.Errorf("copilot: read token exchange response: %w", err)
	}
	if len(body) > maxBodyBytes {
		return 0, nil, errors.New("copilot: token exchange response exceeds 1 MiB")
	}
	return resp.StatusCode, body, nil
}

func exchangeFailure(status int, body []byte, github string) error {
	detail := providerDetail(body, github)
	switch status {
	case http.StatusUnauthorized:
		return fmt.Errorf("copilot: token exchange rejected the credential (HTTP %d); inspect the account before attempting login again%s", status, detail)
	case http.StatusForbidden, http.StatusNotFound:
		return fmt.Errorf("copilot: token exchange refused (HTTP %d); check account access and service availability%s", status, detail)
	}
	return fmt.Errorf("copilot: token exchange failed (HTTP %d)%s", status, detail)
}

func decodeToken(body []byte, owner string) (copilotToken, error) {
	var payload struct {
		Token     string `json:"token"`
		ExpiresAt int64  `json:"expires_at"`
	}
	if err := json.Unmarshal(body, &payload); err != nil {
		return copilotToken{}, fmt.Errorf("copilot: decode token exchange response: %w", err)
	}
	if payload.Token == "" || payload.ExpiresAt <= 0 {
		return copilotToken{}, errors.New("copilot: token exchange response has no token or expiry")
	}
	return copilotToken{value: payload.Token, owner: owner, expires: time.Unix(payload.ExpiresAt, 0)}, nil
}

func providerDetail(body []byte, hide ...string) string {
	var payload struct {
		Message string `json:"message"`
	}
	if err := json.Unmarshal(body, &payload); err != nil {
		return ""
	}
	message := cleanDetail(payload.Message, hide)
	if message == "" {
		return ""
	}
	return fmt.Sprintf("; provider message: %q", message)
}

func cleanDetail(text string, hide []string) string {
	for _, secret := range hide {
		if secret != "" {
			text = strings.ReplaceAll(text, secret, redacted)
		}
	}
	text = maskPatterns(text)
	text = strings.Join(strings.Fields(strings.Map(dropControl, text)), " ")
	return truncateRunes(text, maxDetailRunes)
}

func maskPatterns(text string) string {
	hits := secrets.Scan(text)
	if len(hits) == 0 {
		return text
	}
	var masked strings.Builder
	last := 0
	for _, hit := range hits {
		masked.WriteString(text[last:hit.Start])
		masked.WriteString(redacted)
		last = hit.End
	}
	masked.WriteString(text[last:])
	return masked.String()
}

func dropControl(r rune) rune {
	if unicode.IsControl(r) && !unicode.IsSpace(r) {
		return -1
	}
	return r
}

func truncateRunes(text string, limit int) string {
	count := 0
	for index := range text {
		if count == limit {
			return text[:index] + "…"
		}
		count++
	}
	return text
}
