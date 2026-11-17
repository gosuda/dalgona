package oauth

import (
	"bytes"
	"context"
	json "encoding/json/jsontext"
	"errors"
	"fmt"
	"net"
	"net/http"
	"net/url"
	"regexp"
	"slices"
	"strings"
	"time"
)

const discoveryTimeout = 30 * time.Second

var resourceMetadataParam = regexp.MustCompile(`(?i)(?:^|[\s,])resource_metadata\s*=\s*(?:"((?:[^"\\]|\\.)*)"|([^\s,"]+))`)

var quotedPairReplacer = strings.NewReplacer(`\"`, `"`, `\\`, `\`)

type Resource struct {
	URL                 string
	AuthorizationServer string
	Scopes              []string
}

type ServerMetadata struct {
	Issuer                string
	AuthorizationEndpoint string
	TokenEndpoint         string
	RegistrationEndpoint  string
}

func CanonicalResource(raw string) (string, error) {
	parsed, err := url.Parse(raw)
	if err != nil || parsed.Host == "" || parsed.User != nil || (parsed.Scheme != "https" && parsed.Scheme != "http") {
		return "", fmt.Errorf("oauth: invalid resource URL %q", raw)
	}
	return canonicalURL(parsed), nil
}

func canonicalURL(parsed *url.URL) string {
	port := parsed.Port()
	if port == defaultPort(parsed.Scheme) {
		port = ""
	}
	host := strings.ToLower(parsed.Hostname())
	if port != "" {
		host = net.JoinHostPort(host, port)
	} else if strings.Contains(host, ":") {
		host = "[" + host + "]"
	}
	canonical := url.URL{Scheme: parsed.Scheme, Host: host, Path: parsed.Path, RawPath: parsed.RawPath, RawQuery: parsed.RawQuery}
	if canonical.Path == "/" {
		canonical.Path, canonical.RawPath = "", ""
	}
	return canonical.String()
}

func defaultPort(scheme string) string {
	if scheme == "https" {
		return "443"
	}
	return "80"
}

func SecureURL(raw string) bool {
	parsed, err := url.Parse(raw)
	if err != nil || parsed.Host == "" {
		return false
	}
	if parsed.Scheme == "https" {
		return true
	}
	return parsed.Scheme == "http" && loopbackHost(parsed.Hostname())
}

func loopbackHost(host string) bool {
	if strings.EqualFold(host, "localhost") {
		return true
	}
	ip := net.ParseIP(host)
	return ip != nil && ip.IsLoopback()
}

func DiscoverResource(ctx context.Context, client *http.Client, mcpURL, wwwAuthenticate string) (Resource, error) {
	canonical, err := CanonicalResource(mcpURL)
	if err != nil {
		return Resource{}, err
	}
	candidates, err := resourceMetadataURLs(canonical, wwwAuthenticate)
	if err != nil {
		return Resource{}, err
	}
	decoded, err := firstMetadata(ctx, client, candidates)
	if err != nil {
		return Resource{}, fmt.Errorf("oauth: protected resource metadata for %s: %w", canonical, err)
	}
	return resourceFromMetadata(decoded, canonical)
}

func resourceMetadataURLs(canonical, wwwAuthenticate string) ([]string, error) {
	if match := resourceMetadataParam.FindStringSubmatch(wwwAuthenticate); match != nil {
		header := match[2]
		if match[1] != "" {
			header = quotedPairReplacer.Replace(match[1])
		}
		if !SecureURL(header) {
			return nil, fmt.Errorf("oauth: protected resource metadata URL %s is not https", header)
		}
		return []string{header}, nil
	}
	parsed, err := url.Parse(canonical)
	if err != nil {
		return nil, fmt.Errorf("oauth: invalid resource URL %q", canonical)
	}
	origin := parsed.Scheme + "://" + parsed.Host
	root := origin + "/.well-known/oauth-protected-resource"
	if parsed.Path == "" {
		return []string{root}, nil
	}
	return []string{root + parsed.EscapedPath(), root}, nil
}

func resourceFromMetadata(decoded object, canonical string) (Resource, error) {
	member, err := decoded.string("resource")
	if err != nil {
		return Resource{}, fmt.Errorf("oauth: protected resource metadata: %w", err)
	}
	named, canonicalErr := CanonicalResource(member)
	if canonicalErr != nil || named != canonical {
		return Resource{}, fmt.Errorf("oauth: protected resource metadata names %s, want %s", member, canonical)
	}
	servers, err := decoded.strings("authorization_servers")
	if err != nil {
		return Resource{}, fmt.Errorf("oauth: protected resource metadata: %w", err)
	}
	if len(servers) == 0 {
		return Resource{}, fmt.Errorf("oauth: protected resource metadata for %s lists no authorization server", canonical)
	}
	if !SecureURL(servers[0]) {
		return Resource{}, fmt.Errorf("oauth: authorization server %s is not https", servers[0])
	}
	scopes, err := decoded.strings("scopes_supported")
	if err != nil {
		return Resource{}, fmt.Errorf("oauth: protected resource metadata: %w", err)
	}
	return Resource{URL: canonical, AuthorizationServer: servers[0], Scopes: scopes}, nil
}

func DiscoverServer(ctx context.Context, client *http.Client, issuer string) (ServerMetadata, error) {
	if !SecureURL(issuer) {
		return ServerMetadata{}, fmt.Errorf("oauth: authorization server %s is not https", issuer)
	}
	parsed, err := url.Parse(issuer)
	if err != nil {
		return ServerMetadata{}, fmt.Errorf("oauth: invalid authorization server %q", issuer)
	}
	decoded, err := firstMetadata(ctx, client, serverMetadataURLs(parsed))
	if err != nil {
		return ServerMetadata{}, fmt.Errorf("oauth: server %s metadata: %w", issuer, err)
	}
	return serverFromMetadata(decoded, issuer)
}

func serverMetadataURLs(issuer *url.URL) []string {
	origin := issuer.Scheme + "://" + issuer.Host
	path := strings.TrimRight(issuer.EscapedPath(), "/")
	const (
		oauthWellKnown = "/.well-known/oauth-authorization-server"
		oidcWellKnown  = "/.well-known/openid-configuration"
	)
	if path == "" {
		return []string{origin + oauthWellKnown, origin + oidcWellKnown}
	}
	return []string{origin + oauthWellKnown + path, origin + oidcWellKnown + path, origin + path + oidcWellKnown}
}

func serverFromMetadata(decoded object, issuer string) (ServerMetadata, error) {
	named, err := decoded.string("issuer")
	if err != nil {
		return ServerMetadata{}, fmt.Errorf("oauth: server %s metadata: %w", issuer, err)
	}
	if named != issuer {
		return ServerMetadata{}, fmt.Errorf("oauth: server metadata names issuer %s, want %s", named, issuer)
	}
	methods, err := decoded.strings("code_challenge_methods_supported")
	if err != nil {
		return ServerMetadata{}, fmt.Errorf("oauth: server %s metadata: %w", issuer, err)
	}
	if !slices.Contains(methods, "S256") {
		return ServerMetadata{}, fmt.Errorf("oauth: server %s does not support PKCE S256", issuer)
	}
	meta := ServerMetadata{Issuer: issuer}
	if meta.AuthorizationEndpoint, err = requiredEndpoint(decoded, issuer, "authorization_endpoint"); err != nil {
		return ServerMetadata{}, err
	}
	if meta.TokenEndpoint, err = requiredEndpoint(decoded, issuer, "token_endpoint"); err != nil {
		return ServerMetadata{}, err
	}
	if meta.RegistrationEndpoint, err = optionalEndpoint(decoded, issuer, "registration_endpoint"); err != nil {
		return ServerMetadata{}, err
	}
	return meta, nil
}

func requiredEndpoint(decoded object, issuer, key string) (string, error) {
	value, err := optionalEndpoint(decoded, issuer, key)
	if err != nil {
		return "", err
	}
	if value == "" {
		return "", fmt.Errorf("oauth: server %s has no %s", issuer, key)
	}
	return value, nil
}

func optionalEndpoint(decoded object, issuer, key string) (string, error) {
	value, err := decoded.string(key)
	if err != nil {
		return "", fmt.Errorf("oauth: server %s metadata: %w", issuer, err)
	}
	if value != "" && !SecureURL(value) {
		return "", fmt.Errorf("oauth: server %s %s %s is not https", issuer, key, value)
	}
	return value, nil
}

func Register(ctx context.Context, client *http.Client, meta ServerMetadata, redirect string, scopes []string) (string, error) {
	if meta.RegistrationEndpoint == "" {
		return "", errors.New("oauth: server has no registration endpoint; set oauth_client_id")
	}
	body, err := registrationBody(redirect, scopes)
	if err != nil {
		return "", err
	}
	ctx, cancel := context.WithTimeout(ctx, discoveryTimeout)
	defer cancel()
	request, err := http.NewRequestWithContext(ctx, http.MethodPost, meta.RegistrationEndpoint, bytes.NewReader(body))
	if err != nil {
		return "", fmt.Errorf("create oauth registration request: %w", err)
	}
	request.Header.Set("Content-Type", "application/json")
	request.Header.Set("Accept", "application/json")
	response, err := doWith(client, request)
	if err != nil {
		return "", fmt.Errorf("oauth: register client: %w", err)
	}
	return registeredClientID(response)
}

func registeredClientID(response wireResponse) (string, error) {
	if response.status != http.StatusOK && response.status != http.StatusCreated {
		return "", fmt.Errorf("oauth: registration endpoint returned HTTP %d", response.status)
	}
	decoded, err := response.object()
	if err != nil {
		return "", fmt.Errorf("oauth: register client: %w", err)
	}
	clientID, err := decoded.string("client_id")
	if err != nil {
		return "", fmt.Errorf("oauth: register client: %w", err)
	}
	if clientID == "" {
		return "", errors.New("oauth: registration response has no client_id")
	}
	return clientID, nil
}

func registrationBody(redirect string, scopes []string) ([]byte, error) {
	var buffer bytes.Buffer
	encoder := json.NewEncoder(&buffer)
	tokens := []json.Token{
		json.BeginObject,
		json.String("client_name"), json.String("dalgona"),
		json.String("redirect_uris"), json.BeginArray, json.String(redirect), json.EndArray,
		json.String("grant_types"), json.BeginArray, json.String("authorization_code"), json.String("refresh_token"), json.EndArray,
		json.String("response_types"), json.BeginArray, json.String("code"), json.EndArray,
		json.String("token_endpoint_auth_method"), json.String("none"),
	}
	if len(scopes) > 0 {
		tokens = append(tokens, json.String("scope"), json.String(strings.Join(scopes, " ")))
	}
	tokens = append(tokens, json.EndObject)
	for _, token := range tokens {
		if err := encoder.WriteToken(token); err != nil {
			return nil, fmt.Errorf("encode oauth registration: %w", err)
		}
	}
	return buffer.Bytes(), nil
}

func firstMetadata(ctx context.Context, client *http.Client, candidates []string) (object, error) {
	failures := make([]error, 0, len(candidates))
	for _, candidate := range candidates {
		decoded, err := fetchMetadata(ctx, client, candidate)
		if err == nil {
			return decoded, nil
		}
		if ctxErr := ctx.Err(); ctxErr != nil {
			return nil, fmt.Errorf("oauth discovery: %w", ctxErr)
		}
		failures = append(failures, fmt.Errorf("%s: %w", candidate, err))
	}
	return nil, errors.Join(failures...)
}

func fetchMetadata(ctx context.Context, client *http.Client, endpoint string) (object, error) {
	ctx, cancel := context.WithTimeout(ctx, discoveryTimeout)
	defer cancel()
	request, err := http.NewRequestWithContext(ctx, http.MethodGet, endpoint, nil)
	if err != nil {
		return nil, fmt.Errorf("create oauth request: %w", err)
	}
	request.Header.Set("Accept", "application/json")
	response, err := doWith(client, request)
	if err != nil {
		return nil, err
	}
	if statusErr := response.requireSuccess(); statusErr != nil {
		return nil, statusErr
	}
	return response.object()
}

func (decoded object) strings(key string) ([]string, error) {
	value, ok := decoded[key]
	if !ok || value.Kind() == 'n' {
		return nil, nil
	}
	if value.Kind() != '[' {
		return nil, fmt.Errorf("oauth field %s is not an array", key)
	}
	decoder := json.NewDecoder(bytes.NewReader(value))
	if _, err := decoder.ReadToken(); err != nil {
		return nil, fmt.Errorf("decode oauth field %s: %w", key, err)
	}
	var items []string
	for decoder.PeekKind() != ']' {
		token, err := decoder.ReadToken()
		if err != nil {
			return nil, fmt.Errorf("decode oauth field %s: %w", key, err)
		}
		if token.Kind() != '"' {
			return nil, fmt.Errorf("oauth field %s holds a value that is not a string", key)
		}
		items = append(items, token.String())
	}
	return items, nil
}
