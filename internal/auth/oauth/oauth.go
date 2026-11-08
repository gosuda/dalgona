package oauth

import (
	"bytes"
	"cmp"
	"context"
	"crypto/rand"
	"crypto/sha256"
	"crypto/subtle"
	"encoding/base64"
	json "encoding/json/jsontext"
	"errors"
	"fmt"
	"io"
	"maps"
	"net"
	"net/http"
	"net/url"
	"slices"
	"strconv"
	"strings"
	"time"
)

type Provider struct {
	ID                    string
	AuthorizeURL          string
	TokenURL              string
	ClientID              string
	ClientSecret          string
	Scopes                []string
	RedirectHost          string
	BindHost              string
	RedirectPort          int
	FallbackPort          int
	AllowPortFallback     bool
	RedirectPath          string
	DeviceURL             string
	DeviceTokenURL        string
	DeviceVerificationURL string
	DeviceRedirectURL     string
	PollURL               string
	RefreshURL            string
	ExtraAuthParams       map[string]string
}

type Token struct {
	Access  string
	Refresh string
	Expires time.Time
	Account string
}

const (
	maxResponseBytes     = 1 << 20
	chatGPTPublicID      = "app_EMoamEEZ73f0CkXaXp7hrann"
	anthropicPublicID    = "9d1c250a-e61b-44d9-88ed-594dd1962f5e"
	antigravityPublicID  = "1071006060591-tmhssin2h21lcre235vtdollojh4g403ep.apps.googleusercontent.com"
	antigravityPublicKey = "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"
	cursorPublicID       = "KbZUR41cY7W6zRSdpSUJ7I7mLYBKOCmB"
	openAIEndpoint       = "https://auth.openai.com/oauth/token"
	claudeEndpoint       = "https://platform.claude.com/v1/oauth/token"
	googleEndpoint       = "https://oauth2.googleapis.com/token"
	devinEndpoint        = "https://api.devin.ai/auth/cli/token"
	openAIDeviceEndpoint = "https://auth.openai.com/api/accounts/deviceauth/token"
)

var Providers = map[string]Provider{
	// source: .references/codex/codex-rs/login/src/server.rs
	"chatgpt": {
		ID: "chatgpt", AuthorizeURL: "https://auth.openai.com/oauth/authorize", TokenURL: openAIEndpoint,
		ClientID: chatGPTPublicID, Scopes: []string{"openid", "profile", "email", "offline_access", "api.connectors.read", "api.connectors.invoke"},
		RedirectHost: "127.0.0.1", RedirectPort: 1455, FallbackPort: 1457, RedirectPath: "/auth/callback",
		DeviceURL: "https://auth.openai.com/api/accounts/deviceauth/usercode", DeviceTokenURL: openAIDeviceEndpoint,
		DeviceVerificationURL: "https://auth.openai.com/codex/device", DeviceRedirectURL: "https://auth.openai.com/deviceauth/callback",
		ExtraAuthParams: map[string]string{"id_token_add_organizations": "true", "codex_cli_simplified_flow": "true", "originator": "codex_cli_rs"},
	},
	// source: .references/pi/packages/ai/src/auth/oauth/anthropic.ts
	"anthropic": {
		ID: "anthropic", AuthorizeURL: "https://claude.ai/oauth/authorize", TokenURL: claudeEndpoint,
		ClientID: anthropicPublicID, Scopes: []string{"org:create_api_key", "user:profile", "user:inference", "user:sessions:claude_code", "user:mcp_servers", "user:file_upload"},
		RedirectHost: "localhost", BindHost: "127.0.0.1", RedirectPort: 53692, AllowPortFallback: true, RedirectPath: "/callback",
		ExtraAuthParams: map[string]string{"code": "true"},
	},
	// source: .references/oh-my-pi/packages/catalog/src/compat/rules/auth/google-antigravity.kdl
	"antigravity": {
		ID: "antigravity", AuthorizeURL: "https://accounts.google.com/o/oauth2/v2/auth", TokenURL: googleEndpoint,
		ClientID: antigravityPublicID, ClientSecret: antigravityPublicKey,
		Scopes:       []string{"https://www.googleapis.com/auth/cloud-platform", "https://www.googleapis.com/auth/userinfo.email", "https://www.googleapis.com/auth/userinfo.profile", "https://www.googleapis.com/auth/cclog", "https://www.googleapis.com/auth/experimentsandconfigs", "https://www.googleapis.com/auth/aicode"},
		RedirectHost: "127.0.0.1", RedirectPort: 51121, RedirectPath: "/oauth-callback",
		ExtraAuthParams: map[string]string{"access_type": "offline", "prompt": "consent"},
	},
	// source: .references/oh-my-pi/packages/catalog/src/compat/rules/auth/devin.kdl
	"devin": {
		ID: "devin", AuthorizeURL: "https://app.devin.ai/auth/cli/continue", TokenURL: devinEndpoint,
		RedirectHost: "127.0.0.1", RedirectPort: 59653, RedirectPath: "/callback", ExtraAuthParams: map[string]string{"prompt": "select_account"},
	},
	// source: .references/oh-my-pi/packages/ai/src/registry/oauth/cursor.ts
	"cursor": {
		ID: "cursor", AuthorizeURL: "https://cursor.com/loginDeepControl", ClientID: cursorPublicID,
		PollURL: "https://api2.cursor.sh/auth/poll", RefreshURL: "https://api2.cursor.sh/oauth/token", ExtraAuthParams: map[string]string{},
	},
}

func ProviderByID(id string) (Provider, bool) {
	provider, ok := Providers[id]
	return provider, ok
}

type callbackServer struct {
	server   *http.Server
	redirect string
	result   chan callbackResult
}

type callbackResult struct {
	code  string
	state string
}

type authorization struct {
	code   string
	state  string
	pasted bool
	err    error
}

type pkceParameters struct {
	verifier  string
	challenge string
	state     string
}

func startCallback(provider Provider) (*callbackServer, error) {
	host := cmp.Or(provider.BindHost, provider.RedirectHost)
	if host == "" {
		return nil, errors.New("provider has no callback")
	}
	listener, err := listenCallback(provider, host)
	if err != nil {
		return nil, err
	}
	port := listener.Addr().(*net.TCPAddr).Port
	callback := &callbackServer{result: make(chan callbackResult, 1)}
	callback.redirect = "http://" + net.JoinHostPort(provider.RedirectHost, strconv.Itoa(port)) + provider.RedirectPath
	mux := http.NewServeMux()
	mux.HandleFunc(provider.RedirectPath, func(writer http.ResponseWriter, request *http.Request) {
		query := request.URL.Query()
		select {
		case callback.result <- callbackResult{code: query.Get("code"), state: query.Get("state")}:
		default:
		}
		writer.Header().Set("Content-Type", "text/plain; charset=utf-8")
		_, _ = io.WriteString(writer, "Login complete. You can close this tab.")
	})
	callback.server = &http.Server{
		Handler: mux, ReadHeaderTimeout: 5 * time.Second, ReadTimeout: 15 * time.Second,
		WriteTimeout: 15 * time.Second, IdleTimeout: 30 * time.Second,
	}
	go func() {
		_ = callback.server.Serve(listener)
	}()
	return callback, nil
}

func listenCallback(provider Provider, host string) (net.Listener, error) {
	ports := []int{provider.RedirectPort}
	if provider.FallbackPort > 0 || provider.AllowPortFallback {
		ports = append(ports, provider.FallbackPort)
	}
	listenErrors := make([]error, 0, len(ports))
	for _, port := range ports {
		listener, err := net.Listen("tcp", net.JoinHostPort(host, strconv.Itoa(port)))
		if err == nil {
			return listener, nil
		}
		listenErrors = append(listenErrors, err)
	}
	return nil, fmt.Errorf("oauth callback listen: %w", errors.Join(listenErrors...))
}

func (callback *callbackServer) Close() error {
	if err := callback.server.Close(); err != nil {
		return fmt.Errorf("close oauth callback: %w", err)
	}
	return nil
}

func PKCE(ctx context.Context, provider Provider, open func(string) error, paste func(context.Context) (string, error)) (Token, error) {
	flowContext, cancel := context.WithCancel(ctx)
	defer cancel()
	parameters, err := newPKCEParameters(provider)
	if err != nil {
		return Token{}, err
	}
	callback, err := startCallback(provider)
	if err != nil {
		return Token{}, err
	}
	defer func() { _ = callback.Close() }()
	authURL, err := authorizationURL(provider, callback.redirect, parameters.challenge, parameters.state)
	if err != nil {
		return Token{}, err
	}
	if openErr := open(authURL); openErr != nil {
		return Token{}, fmt.Errorf("open oauth authorization: %w", openErr)
	}
	grant, err := waitAuthorization(flowContext, callback, paste)
	if err != nil {
		return Token{}, err
	}
	if stateError := checkState(grant, parameters.state); stateError != nil {
		return Token{}, stateError
	}
	if grant.code == "" {
		return Token{}, errors.New("missing authorization code")
	}
	exchangeGrant := authorization{code: grant.code, state: parameters.state}
	return exchange(flowContext, provider, exchangeGrant, parameters.verifier, callback.redirect)
}

func newPKCEParameters(provider Provider) (pkceParameters, error) {
	verifier, challenge, err := verifierPair()
	if err != nil {
		return pkceParameters{}, fmt.Errorf("oauth PKCE verifier: %w", err)
	}
	state, err := randomToken()
	if err != nil {
		return pkceParameters{}, fmt.Errorf("oauth state: %w", err)
	}
	if provider.ID == "anthropic" {
		state = verifier
	}
	return pkceParameters{verifier: verifier, challenge: challenge, state: state}, nil
}

func authorizationURL(provider Provider, redirect, challenge, state string) (string, error) {
	authURL, err := url.Parse(provider.AuthorizeURL)
	if err != nil {
		return "", fmt.Errorf("parse oauth authorization URL: %w", err)
	}
	query := authURL.Query()
	query.Set("response_type", "code")
	query.Set("redirect_uri", redirect)
	query.Set("code_challenge", challenge)
	query.Set("code_challenge_method", "S256")
	query.Set("state", state)
	if provider.ClientID != "" {
		query.Set("client_id", provider.ClientID)
	}
	if len(provider.Scopes) > 0 {
		query.Set("scope", strings.Join(provider.Scopes, " "))
	}
	for key, value := range provider.ExtraAuthParams {
		query.Set(key, value)
	}
	authURL.RawQuery = query.Encode()
	return authURL.String(), nil
}

func waitAuthorization(ctx context.Context, callback *callbackServer, paste func(context.Context) (string, error)) (authorization, error) {
	var pasted chan authorization
	if paste != nil {
		pasted = make(chan authorization, 1)
		go func() {
			value, err := paste(ctx)
			if err != nil {
				pasted <- authorization{pasted: true, err: err}
				return
			}
			code, state := parseInput(value)
			pasted <- authorization{code: code, state: state, pasted: true}
		}()
	}
	timer := time.NewTimer(2 * time.Minute)
	defer timer.Stop()
	select {
	case result := <-callback.result:
		return authorization{code: result.code, state: result.state}, nil
	case result := <-pasted:
		if result.err != nil {
			return authorization{}, fmt.Errorf("oauth paste: %w", result.err)
		}
		if result.code == "" {
			return authorization{}, errors.New("oauth paste returned no authorization code")
		}
		return result, nil
	case <-timer.C:
		return authorization{}, errors.New("oauth callback timed out")
	case <-ctx.Done():
		return authorization{}, fmt.Errorf("oauth authorization: %w", ctx.Err())
	}
}

func checkState(grant authorization, expected string) error {
	if grant.pasted && grant.state == "" {
		return nil
	}
	if subtle.ConstantTimeCompare([]byte(grant.state), []byte(expected)) != 1 {
		return errors.New("OAuth state mismatch")
	}
	return nil
}

func verifierPair() (string, string, error) {
	verifier, err := randomToken()
	if err != nil {
		return "", "", err
	}
	digest := sha256.Sum256([]byte(verifier))
	return verifier, base64.RawURLEncoding.EncodeToString(digest[:]), nil
}

func randomToken() (string, error) {
	buffer := make([]byte, 32)
	if _, err := rand.Read(buffer); err != nil {
		return "", fmt.Errorf("read random bytes: %w", err)
	}
	return base64.RawURLEncoding.EncodeToString(buffer), nil
}

func parseInput(input string) (string, string) {
	value := strings.TrimSpace(input)
	parsed, parseErr := url.Parse(value)
	if parseErr == nil && parsed.Query().Get("code") != "" {
		return parsed.Query().Get("code"), parsed.Query().Get("state")
	}
	if strings.Contains(value, "#") {
		parts := strings.SplitN(value, "#", 2)
		return parts[0], parts[1]
	}
	if strings.Contains(value, "code=") {
		query, queryErr := url.ParseQuery(value)
		if queryErr == nil {
			return query.Get("code"), query.Get("state")
		}
	}
	return value, ""
}

func exchange(ctx context.Context, provider Provider, grant authorization, verifier, redirect string) (Token, error) {
	values := url.Values{"code": {grant.code}, "code_verifier": {verifier}}
	if provider.ID != "devin" {
		values.Set("grant_type", "authorization_code")
		values.Set("redirect_uri", redirect)
		if provider.ClientID != "" {
			values.Set("client_id", provider.ClientID)
		}
		if provider.ClientSecret != "" {
			values.Set("client_secret", provider.ClientSecret)
		}
	}
	if grant.state != "" && provider.ID != "chatgpt" && provider.ID != "devin" {
		values.Set("state", grant.state)
	}
	var response object
	var err error
	if jsonCodeExchange(provider.ID) {
		response, err = jsonTokenRequest(ctx, provider, provider.TokenURL, values)
	} else {
		response, err = formTokenRequest(ctx, provider, provider.TokenURL, values)
	}
	if err != nil {
		return Token{}, err
	}
	return tokenFromObject(response, "")
}

func Refresh(ctx context.Context, provider Provider, refresh string) (Token, error) {
	endpoint := cmp.Or(provider.RefreshURL, provider.TokenURL)
	values := url.Values{"grant_type": {"refresh_token"}, "refresh_token": {refresh}}
	if provider.ClientID != "" {
		values.Set("client_id", provider.ClientID)
	}
	if provider.ClientSecret != "" {
		values.Set("client_secret", provider.ClientSecret)
	}
	var response object
	var err error
	if jsonRefresh(provider.ID) {
		response, err = jsonTokenRequest(ctx, provider, endpoint, values)
	} else {
		response, err = formTokenRequest(ctx, provider, endpoint, values)
	}
	if err != nil {
		return Token{}, err
	}
	shouldLogout, boolErr := response.boolean("shouldLogout")
	if boolErr != nil {
		return Token{}, fmt.Errorf("decode oauth logout marker: %w", boolErr)
	}
	if shouldLogout {
		return Token{}, errors.New("invalid_grant: Cursor ended this session; run /login cursor again")
	}
	return tokenFromObject(response, refresh)
}

func jsonCodeExchange(id string) bool {
	return id == "anthropic" || id == "devin"
}

func jsonRefresh(id string) bool {
	return id == "chatgpt" || id == "anthropic" || id == "cursor"
}

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
		return wireResponse{}, fmt.Errorf("send oauth request: %w", err)
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
