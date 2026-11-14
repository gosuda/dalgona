package oauth

import (
	"bytes"
	"cmp"
	"context"
	"encoding/base64"
	json "encoding/json/jsontext"
	"errors"
	"fmt"
	"io"
	"maps"
	"net/http"
	"net/url"
	"slices"
	"strconv"
	"strings"
	"time"
)

type wireResponse struct {
	status int
	body   []byte
}

type object map[string]json.Value

func formTokenRequest(ctx context.Context, provider Provider, endpoint string, values url.Values) (object, error) {
	response, err := postForm(ctx, endpoint, values)
	return decodeTokenResponse(provider, response, err)
}

func jsonTokenRequest(ctx context.Context, provider Provider, endpoint string, values url.Values) (object, error) {
	response, err := postJSON(ctx, endpoint, values)
	return decodeTokenResponse(provider, response, err)
}

func decodeTokenResponse(provider Provider, response wireResponse, requestErr error) (object, error) {
	if requestErr != nil {
		return nil, fmt.Errorf("%s token request: %w", provider.ID, requestErr)
	}
	if statusErr := response.requireSuccess(); statusErr != nil {
		return nil, statusErr
	}
	decoded, decodeErr := response.object()
	if decodeErr != nil {
		return nil, decodeErr
	}
	return decoded, nil
}

func postForm(ctx context.Context, endpoint string, values url.Values) (wireResponse, error) {
	return post(ctx, endpoint, "application/x-www-form-urlencoded", []byte(values.Encode()))
}

func postJSON(ctx context.Context, endpoint string, values url.Values) (wireResponse, error) {
	body, err := encodeObject(values)
	if err != nil {
		return wireResponse{}, fmt.Errorf("encode oauth request: %w", err)
	}
	return post(ctx, endpoint, "application/json", body)
}

func post(ctx context.Context, endpoint, contentType string, body []byte) (wireResponse, error) {
	request, err := http.NewRequestWithContext(ctx, http.MethodPost, endpoint, bytes.NewReader(body))
	if err != nil {
		return wireResponse{}, fmt.Errorf("create oauth request: %w", err)
	}
	request.Header.Set("Content-Type", contentType)
	request.Header.Set("Accept", "application/json")
	return do(request)
}

func get(ctx context.Context, endpoint string) (wireResponse, error) {
	request, err := http.NewRequestWithContext(ctx, http.MethodGet, endpoint, nil)
	if err != nil {
		return wireResponse{}, fmt.Errorf("create oauth request: %w", err)
	}
	request.Header.Set("Accept", "application/json")
	return do(request)
}

func do(request *http.Request) (wireResponse, error) {
	client := http.Client{Timeout: 30 * time.Second}
	response, err := client.Do(request)
	if err != nil {
		return wireResponse{}, fmt.Errorf("send oauth request: %w", redactRequestQuery(err))
	}
	body, readErr := io.ReadAll(io.LimitReader(response.Body, maxResponseBytes+1))
	closeErr := response.Body.Close()
	if readErr != nil {
		return wireResponse{}, fmt.Errorf("read oauth response: %w", readErr)
	}
	if closeErr != nil {
		return wireResponse{}, fmt.Errorf("close oauth response: %w", closeErr)
	}
	if len(body) > maxResponseBytes {
		return wireResponse{}, errors.New("oauth response exceeds 1 MiB")
	}
	return wireResponse{status: response.StatusCode, body: body}, nil
}

func redactRequestQuery(err error) error {
	urlError, ok := errors.AsType[*url.Error](err)
	if !ok || urlError.URL == "" {
		return err
	}
	parsed, parseErr := url.Parse(urlError.URL)
	if parseErr != nil || parsed.RawQuery == "" {
		return err
	}
	redacted := *urlError
	parsed.RawQuery = "REDACTED"
	redacted.URL = parsed.String()
	return &redacted
}

func (response wireResponse) requireSuccess() error {
	if response.status < http.StatusOK || response.status >= http.StatusMultipleChoices {
		return fmt.Errorf("oauth endpoint returned HTTP %d", response.status)
	}
	return nil
}

func (response wireResponse) object() (object, error) {
	decoded, err := decodeObject(response.body)
	if err != nil {
		return nil, fmt.Errorf("decode oauth response: %w", err)
	}
	return decoded, nil
}

func encodeObject(values url.Values) ([]byte, error) {
	var buffer bytes.Buffer
	encoder := json.NewEncoder(&buffer)
	if err := encoder.WriteToken(json.BeginObject); err != nil {
		return nil, fmt.Errorf("write object start: %w", err)
	}
	for _, key := range slices.Sorted(maps.Keys(values)) {
		if err := encoder.WriteToken(json.String(key)); err != nil {
			return nil, fmt.Errorf("write object key: %w", err)
		}
		if err := encoder.WriteToken(json.String(values.Get(key))); err != nil {
			return nil, fmt.Errorf("write object value: %w", err)
		}
	}
	if err := encoder.WriteToken(json.EndObject); err != nil {
		return nil, fmt.Errorf("write object end: %w", err)
	}
	return buffer.Bytes(), nil
}

func decodeObject(data []byte) (object, error) {
	decoder := json.NewDecoder(bytes.NewReader(data))
	opening, err := decoder.ReadToken()
	if err != nil {
		return nil, fmt.Errorf("read object start: %w", err)
	}
	if opening.Kind() != '{' {
		return nil, errors.New("oauth response is not a JSON object")
	}
	decoded := make(object)
	for decoder.PeekKind() != '}' {
		name, nameErr := decoder.ReadToken()
		if nameErr != nil {
			return nil, fmt.Errorf("read object key: %w", nameErr)
		}
		key := name.String()
		value, valueErr := decoder.ReadValue()
		if valueErr != nil {
			return nil, fmt.Errorf("read object value: %w", valueErr)
		}
		decoded[key] = value.Clone()
	}
	if _, closeErr := decoder.ReadToken(); closeErr != nil {
		return nil, fmt.Errorf("read object end: %w", closeErr)
	}
	if _, trailingErr := decoder.ReadValue(); !errors.Is(trailingErr, io.EOF) {
		return nil, errors.New("oauth response has trailing JSON")
	}
	return decoded, nil
}

func (decoded object) string(key string) (string, error) {
	value, ok := decoded[key]
	if !ok {
		return "", nil
	}
	if value.Kind() != '"' {
		return "", fmt.Errorf("oauth field %s is not a string", key)
	}
	decoder := json.NewDecoder(bytes.NewReader(value))
	token, err := decoder.ReadToken()
	if err != nil {
		return "", fmt.Errorf("decode oauth field %s: %w", key, err)
	}
	return token.String(), nil
}

func (decoded object) firstString(keys ...string) (string, error) {
	for _, key := range keys {
		value, err := decoded.string(key)
		if err != nil {
			return "", err
		}
		if value != "" {
			return value, nil
		}
	}
	return "", nil
}

func (decoded object) integer(key string) (int64, error) {
	value, ok := decoded[key]
	if !ok || value.Kind() == 'n' {
		return 0, nil
	}
	if value.Kind() == '"' {
		text, err := decoded.string(key)
		if err != nil {
			return 0, err
		}
		number, parseErr := strconv.ParseInt(text, 10, 64)
		if parseErr != nil {
			return 0, fmt.Errorf("decode oauth field %s: %w", key, parseErr)
		}
		return number, nil
	}
	decoder := json.NewDecoder(bytes.NewReader(value))
	token, err := decoder.ReadToken()
	if err != nil {
		return 0, fmt.Errorf("decode oauth field %s: %w", key, err)
	}
	number, numberErr := token.Int()
	if numberErr != nil {
		return 0, fmt.Errorf("decode oauth field %s: %w", key, numberErr)
	}
	return number, nil
}

func (decoded object) boolean(key string) (bool, error) {
	value, ok := decoded[key]
	if !ok {
		return false, nil
	}
	if value.Kind() != 't' && value.Kind() != 'f' {
		return false, fmt.Errorf("oauth field %s is not a boolean", key)
	}
	return value.Kind() == 't', nil
}

type tokenFields struct {
	access    string
	refresh   string
	token     string
	idToken   string
	expiresIn int64
}

func readTokenFields(decoded object) (tokenFields, error) {
	access, err := decoded.firstString("access_token", "accessToken", "token")
	if err != nil {
		return tokenFields{}, fmt.Errorf("decode oauth access token: %w", err)
	}
	refresh, err := decoded.firstString("refresh_token", "refreshToken")
	if err != nil {
		return tokenFields{}, fmt.Errorf("decode oauth refresh token: %w", err)
	}
	tokenValue, err := decoded.string("token")
	if err != nil {
		return tokenFields{}, fmt.Errorf("decode oauth token: %w", err)
	}
	idToken, err := decoded.string("id_token")
	if err != nil {
		return tokenFields{}, fmt.Errorf("decode oauth id token: %w", err)
	}
	expiresIn, err := decoded.integer("expires_in")
	if err != nil {
		return tokenFields{}, fmt.Errorf("decode oauth expiry: %w", err)
	}
	return tokenFields{access: access, refresh: refresh, token: tokenValue, idToken: idToken, expiresIn: expiresIn}, nil
}

func tokenFromObject(decoded object, previousRefresh string) (Token, error) {
	fields, err := readTokenFields(decoded)
	if err != nil {
		return Token{}, err
	}
	if fields.access == "" {
		return Token{}, errors.New("oauth response has no access token")
	}
	token := Token{
		Access: fields.access, Refresh: cmp.Or(fields.refresh, previousRefresh, fields.token),
		Account: jwtAccount(fields.idToken),
	}
	if fields.expiresIn > 0 {
		token.Expires = time.Now().Add(time.Duration(fields.expiresIn) * time.Second)
	} else {
		token.Expires = jwtExpiry(fields.access)
	}
	return token, nil
}

func jwtAccount(raw string) string {
	claims := jwtClaims(raw)
	if len(claims) == 0 {
		return ""
	}
	if authValue, ok := claims["https://api.openai.com/auth"]; ok {
		authClaims, err := decodeObject(authValue)
		if err == nil {
			account, _ := authClaims.string("chatgpt_account_id")
			if account != "" {
				return account
			}
		}
	}
	account, _ := claims.firstString("email", "sub")
	return account
}

func jwtExpiry(raw string) time.Time {
	claims := jwtClaims(raw)
	if len(claims) == 0 {
		return time.Time{}
	}
	expires, err := claims.integer("exp")
	if err != nil || expires <= 0 {
		return time.Time{}
	}
	return time.Unix(expires, 0)
}

func jwtClaims(raw string) object {
	parts := strings.Split(raw, ".")
	if len(parts) != 3 {
		return nil
	}
	payload, err := base64.RawURLEncoding.DecodeString(parts[1])
	if err != nil {
		return nil
	}
	claims, decodeErr := decodeObject(payload)
	if decodeErr != nil {
		return nil
	}
	return claims
}
