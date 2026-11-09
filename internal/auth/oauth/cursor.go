package oauth

import (
	"context"
	"crypto/rand"
	"errors"
	"fmt"
	"net/http"
	"net/url"
	"time"
)

func CursorPoll(ctx context.Context, provider Provider, open func(string) error, _ func(context.Context) (string, error)) (Token, error) {
	verifier, challenge, err := verifierPair()
	if err != nil {
		return Token{}, fmt.Errorf("oauth Cursor verifier: %w", err)
	}
	identifier, err := cursorUUID()
	if err != nil {
		return Token{}, fmt.Errorf("oauth Cursor UUID: %w", err)
	}
	deepLink, err := url.Parse(provider.AuthorizeURL)
	if err != nil {
		return Token{}, fmt.Errorf("parse Cursor deep link: %w", err)
	}
	query := deepLink.Query()
	query.Set("challenge", challenge)
	query.Set("uuid", identifier)
	query.Set("mode", "login")
	query.Set("redirectTarget", "cli")
	deepLink.RawQuery = query.Encode()
	if openErr := open(deepLink.String()); openErr != nil {
		return Token{}, fmt.Errorf("open Cursor deep link: %w", openErr)
	}
	return pollCursor(ctx, provider, identifier, verifier)
}

func cursorUUID() (string, error) {
	buffer := make([]byte, 16)
	if _, err := rand.Read(buffer); err != nil {
		return "", fmt.Errorf("read Cursor UUID bytes: %w", err)
	}
	buffer[6] = buffer[6]&0x0f | 0x40
	buffer[8] = buffer[8]&0x3f | 0x80
	return fmt.Sprintf("%08x-%04x-%04x-%04x-%012x",
		buffer[0:4], buffer[4:6], buffer[6:8], buffer[8:10], buffer[10:16]), nil
}

func pollCursor(ctx context.Context, provider Provider, identifier, verifier string) (Token, error) {
	delay := time.Second
	consecutiveErrors := 0
	var lastErr error
	for range 150 {
		if waitErr := wait(ctx, delay); waitErr != nil {
			return Token{}, waitErr
		}
		token, pending, err := cursorPollAttempt(ctx, provider, identifier, verifier)
		if err == nil && !pending {
			return token, nil
		}
		if pending {
			consecutiveErrors = 0
			delay = min(delay*6/5, 10*time.Second)
			continue
		}
		consecutiveErrors++
		lastErr = err
		if consecutiveErrors >= 3 {
			return Token{}, fmt.Errorf("too many consecutive errors during Cursor auth polling: %w", lastErr)
		}
	}
	return Token{}, errors.New("cursor authentication polling timeout")
}

func cursorPollAttempt(ctx context.Context, provider Provider, identifier, verifier string) (Token, bool, error) {
	endpoint, err := url.Parse(provider.PollURL)
	if err != nil {
		return Token{}, false, fmt.Errorf("parse Cursor poll URL: %w", err)
	}
	query := endpoint.Query()
	query.Set("uuid", identifier)
	query.Set("verifier", verifier)
	endpoint.RawQuery = query.Encode()
	response, err := get(ctx, endpoint.String())
	if err != nil {
		return Token{}, false, err
	}
	if response.status == http.StatusNotFound {
		return Token{}, true, nil
	}
	if statusErr := response.requireSuccess(); statusErr != nil {
		return Token{}, false, statusErr
	}
	decoded, err := response.object()
	if err != nil {
		return Token{}, false, err
	}
	token, err := tokenFromObject(decoded, "")
	if err != nil {
		return Token{}, false, err
	}
	return token, false, nil
}
