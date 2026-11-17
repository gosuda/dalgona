package oauth

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"net/http"
	"strings"

	json "encoding/json/v2"
)

func RegisterClient(ctx context.Context, client *http.Client, endpoint, redirectURI string, scopes []string) (string, error) {
	body, err := json.Marshal(map[string]any{
		"client_name":                "dalgona",
		"redirect_uris":              []string{redirectURI},
		"grant_types":                []string{"authorization_code", "refresh_token"},
		"response_types":             []string{"code"},
		"token_endpoint_auth_method": "none",
		"scope":                      strings.Join(scopes, " "),
	})
	if err != nil {
		return "", fmt.Errorf("encode registration request: %w", err)
	}
	request, err := http.NewRequestWithContext(ctx, http.MethodPost, endpoint, bytes.NewReader(body))
	if err != nil {
		return "", fmt.Errorf("create registration request: %w", err)
	}
	request.Header.Set("Content-Type", "application/json")
	request.Header.Set("Accept", "application/json")
	response, err := doWith(client, request)
	if err != nil {
		return "", err
	}
	if requireErr := response.requireSuccess(); requireErr != nil {
		return "", fmt.Errorf("register client: %w", requireErr)
	}
	decoded, err := response.object()
	if err != nil {
		return "", err
	}
	clientID, err := decoded.string("client_id")
	if err != nil || clientID == "" {
		return "", errors.New("registration response holds no client_id")
	}
	return clientID, nil
}
