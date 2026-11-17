package github

import (
	"bytes"
	"context"
	json "encoding/json/v2"
	"errors"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/url"
	"strings"
	"sync"
	"time"
)

const (
	policyBot      = "bot"
	policyReporter = "reporter"
)

const (
	botTimeout      = 30 * time.Second
	botBodyLimit    = 8 << 20
	reporterTimeout = 10 * time.Second
	reporterLimit   = 1 << 20
	maxReadRetries  = 3
	mutationSpacing = time.Second
	apiVersion      = "2026-03-10"
)

type ClientConfig struct {
	BaseURL      string
	Repositories []Repository
	Policy       string
}

type Client struct {
	baseURL      *url.URL
	repositories map[Repository]bool
	policy       string
	credential   Credential
	http         *http.Client
	reqMu        sync.Mutex
	mu           sync.Mutex
	rate         rateState
	sleeper      func(context.Context, time.Duration) error
	now          func() time.Time
	lastMutation time.Time
}

type APIError struct {
	Operation string
	Status    int
	RequestID string
	Message   string
	RetryAt   time.Time
}

func (e *APIError) Error() string {
	return fmt.Sprintf("github: %s: status %d", e.Operation, e.Status)
}

type AmbiguousWriteError struct {
	Operation string
	RequestID string
}

func (e *AmbiguousWriteError) Error() string {
	return fmt.Sprintf("github: %s: publication outcome unknown", e.Operation)
}

func NewClient(cfg ClientConfig, credential Credential, transport *http.Client) (*Client, error) {
	if credential == nil {
		return nil, errors.New("github: credential is required")
	}
	if cfg.Policy != policyBot && cfg.Policy != policyReporter {
		return nil, fmt.Errorf("github: unknown policy %q", cfg.Policy)
	}
	parsed, err := parseBaseURL(cfg.BaseURL)
	if err != nil {
		return nil, err
	}
	repositories, err := parseClientRepositories(cfg.Repositories)
	if err != nil {
		return nil, err
	}
	client := &Client{
		baseURL:      parsed,
		repositories: repositories,
		policy:       cfg.Policy,
		credential:   credential,
		sleeper:      sleepContext,
		now:          time.Now,
	}
	client.http = client.buildHTTPClient(transport)
	return client, nil
}

func parseBaseURL(raw string) (*url.URL, error) {
	base := raw
	if base == "" {
		base = "https://api.github.com"
	}
	parsed, err := url.Parse(base)
	if err != nil || parsed.Host == "" {
		return nil, errors.New("github: invalid base url")
	}
	if parsed.Scheme != "https" && (parsed.Scheme != "http" || !isLoopbackHost(parsed.Hostname())) {
		return nil, errors.New("github: base url must use https")
	}
	return parsed, nil
}

func parseClientRepositories(repositories []Repository) (map[Repository]bool, error) {
	if len(repositories) == 0 {
		return nil, errors.New("github: at least one repository is required")
	}
	parsed := make(map[Repository]bool, len(repositories))
	for _, repo := range repositories {
		if repo.Owner == "" || repo.Name == "" {
			return nil, errors.New("github: invalid repository")
		}
		parsed[repo] = true
	}
	return parsed, nil
}

func isLoopbackHost(host string) bool {
	if host == "localhost" {
		return true
	}
	ip := net.ParseIP(host)
	return ip != nil && ip.IsLoopback()
}

func (c *Client) buildHTTPClient(transport *http.Client) *http.Client {
	rt := http.RoundTripper(http.DefaultTransport)
	if transport != nil && transport.Transport != nil {
		rt = transport.Transport
	}
	if c.policy == policyReporter {
		return &http.Client{
			Transport: rt,
			Timeout:   reporterTimeout,
			CheckRedirect: func(req *http.Request, via []*http.Request) error {
				return http.ErrUseLastResponse
			},
		}
	}
	return &http.Client{
		Transport: rt,
		Timeout:   botTimeout,
		CheckRedirect: func(req *http.Request, via []*http.Request) error {
			if len(via) >= 3 {
				return errors.New("github: too many redirects")
			}
			if len(via) > 0 && !sameOrigin(req.URL, via[0].URL) {
				return errors.New("github: cross-origin redirect refused")
			}
			return nil
		},
	}
}

func sameOrigin(a, b *url.URL) bool {
	return a.Scheme == b.Scheme && a.Host == b.Host
}

func sleepContext(ctx context.Context, d time.Duration) error {
	if d <= 0 {
		return nil
	}
	timer := time.NewTimer(d)
	defer timer.Stop()
	select {
	case <-ctx.Done():
		return ctx.Err()
	case <-timer.C:
		return nil
	}
}

func (c *Client) bodyLimit() int64 {
	if c.policy == policyReporter {
		return reporterLimit
	}
	return botBodyLimit
}

func (c *Client) apiURL(path string) string {
	return strings.TrimRight(c.baseURL.String(), "/") + path
}

func (c *Client) APIBase() string {
	return c.apiURL("")
}

func (c *Client) resolveURL(nextURL, fallback string) (string, error) {
	if nextURL == "" {
		return fallback, nil
	}
	parsed, err := url.Parse(nextURL)
	if err != nil || parsed.Host == "" {
		return "", errors.New("github: invalid next url")
	}
	if !sameOrigin(parsed, c.baseURL) {
		return "", errors.New("github: next url is not allowlisted")
	}
	return nextURL, nil
}

func (c *Client) checkRepository(repo Repository) error {
	if !c.repositories[repo] {
		return errors.New("github: repository is not allowlisted")
	}
	return nil
}

func (c *Client) spaceMutation(ctx context.Context) error {
	if c.policy != policyBot {
		return nil
	}
	c.mu.Lock()
	wait := time.Duration(0)
	if !c.lastMutation.IsZero() {
		wait = mutationSpacing - c.now().Sub(c.lastMutation)
		if wait < 0 {
			wait = 0
		}
	}
	c.lastMutation = c.now().Add(wait)
	c.mu.Unlock()
	if wait > 0 {
		return c.sleeper(ctx, wait)
	}
	return nil
}

func (c *Client) acquireToken(ctx context.Context, repo Repository, workflow bool) (string, error) {
	token, err := c.credential.Token(ctx, CredentialRequest{Repository: repo, Workflow: workflow})
	if err != nil {
		return "", err
	}
	if token.Value == "" {
		return "", errors.New("github: credential returned an empty token")
	}
	return token.Value, nil
}

func (c *Client) setClockForTest(now func() time.Time, sleeper func(context.Context, time.Duration) error) {
	c.now = now
	c.sleeper = sleeper
}

type requestSpec struct {
	method    string
	url       string
	body      []byte
	mutate    bool
	workflow  bool
	repo      Repository
	headers   map[string]string
	operation string
}

type roundResult struct {
	data      []byte
	meta      ResponseMeta
	requestID string
}

type botProgress struct {
	refreshed         bool
	rateRetried       bool
	transportAttempts int
	rateAttempts      int
}

func (c *Client) do(ctx context.Context, spec requestSpec) (roundResult, error) {
	if spec.operation == "" {
		spec.operation = strings.ToLower(spec.method) + " request"
	}
	c.reqMu.Lock()
	defer c.reqMu.Unlock()
	if c.policy == policyReporter {
		return c.doReporter(ctx, spec)
	}
	return c.doBot(ctx, spec)
}

func (c *Client) newRequest(ctx context.Context, spec requestSpec, token string) (*http.Request, error) {
	var reader io.Reader
	if spec.body != nil {
		reader = bytes.NewReader(spec.body)
	}
	req, err := http.NewRequestWithContext(ctx, spec.method, spec.url, reader)
	if err != nil {
		return nil, fmt.Errorf("github: build request: %w", err)
	}
	if spec.method != http.MethodGet && spec.method != http.MethodHead {
		req.Header.Set("Content-Type", "application/json")
	}
	req.Header.Set("Accept", "application/vnd.github+json")
	req.Header.Set("X-GitHub-Api-Version", apiVersion)
	req.Header.Set("Authorization", "Bearer "+token)
	for k, v := range spec.headers {
		req.Header.Set(k, v)
	}
	return req, nil
}

func (c *Client) send(ctx context.Context, spec requestSpec, token string) (roundResult, error) {
	req, err := c.newRequest(ctx, spec, token)
	if err != nil {
		return roundResult{}, err
	}
	resp, err := c.http.Do(req)
	if err != nil {
		return roundResult{}, err
	}
	defer func() { _ = resp.Body.Close() }()
	limit := c.bodyLimit()
	data, err := io.ReadAll(io.LimitReader(resp.Body, limit+1))
	if err != nil {
		return roundResult{}, fmt.Errorf("github: read response: %w", err)
	}
	_, _ = io.Copy(io.Discard, resp.Body)
	if int64(len(data)) > limit {
		return roundResult{}, fmt.Errorf("github: response exceeds %d bytes", limit)
	}
	return roundResult{data: data, meta: parseResponseMeta(resp, c.now()), requestID: requestID(resp)}, nil
}

func (c *Client) doReporter(ctx context.Context, spec requestSpec) (roundResult, error) {
	token, err := c.acquireToken(ctx, spec.repo, spec.workflow)
	if err != nil {
		return roundResult{}, fmt.Errorf("github: %s: %w", spec.operation, err)
	}
	result, sendErr := c.send(ctx, spec, token)
	if sendErr != nil {
		if spec.mutate {
			return result, &AmbiguousWriteError{Operation: spec.operation, RequestID: result.requestID}
		}
		return result, fmt.Errorf("github: %s: %w", spec.operation, sendErr)
	}
	if result.meta.Status == http.StatusNotModified {
		return roundResult{meta: result.meta, requestID: result.requestID}, nil
	}
	if result.meta.Status >= 300 {
		if spec.mutate && result.meta.Status >= 500 {
			return result, &AmbiguousWriteError{Operation: spec.operation, RequestID: result.requestID}
		}
		return result, reporterAPIError(spec.operation, result, result.data)
	}
	return result, nil
}

func (c *Client) doBot(ctx context.Context, spec requestSpec) (roundResult, error) {
	if spec.mutate {
		if err := c.spaceMutation(ctx); err != nil {
			return roundResult{}, err
		}
	}
	var progress botProgress
	for {
		result, retry, err := c.botRound(ctx, spec, &progress)
		if err != nil {
			return result, err
		}
		if !retry {
			return result, nil
		}
	}
}

func (c *Client) botRound(ctx context.Context, spec requestSpec, progress *botProgress) (roundResult, bool, error) {
	token, err := c.acquireToken(ctx, spec.repo, spec.workflow)
	if err != nil {
		return roundResult{}, false, fmt.Errorf("github: %s: %w", spec.operation, err)
	}
	result, sendErr := c.send(ctx, spec, token)
	if sendErr != nil {
		return c.botSendError(ctx, spec, progress, result, sendErr)
	}
	return c.botStatus(ctx, spec, progress, result)
}

func (c *Client) botSendError(ctx context.Context, spec requestSpec, progress *botProgress, result roundResult, sendErr error) (roundResult, bool, error) {
	if spec.mutate {
		return result, false, &AmbiguousWriteError{Operation: spec.operation, RequestID: result.requestID}
	}
	if progress.transportAttempts >= maxReadRetries {
		return result, false, fmt.Errorf("github: %s: %w", spec.operation, sendErr)
	}
	if err := c.sleeper(ctx, retryDelay(progress.transportAttempts)); err != nil {
		return result, false, err
	}
	progress.transportAttempts++
	return roundResult{}, true, nil
}

func (c *Client) botStatus(ctx context.Context, spec requestSpec, progress *botProgress, result roundResult) (roundResult, bool, error) {
	status := result.meta.Status
	if refreshed := c.maybeRefreshCredential(ctx, spec, progress, status); refreshed {
		return roundResult{}, true, nil
	}
	if status == http.StatusNotModified {
		return roundResult{meta: result.meta, requestID: result.requestID}, false, nil
	}
	if status >= 200 && status < 300 {
		return result, false, nil
	}
	if isRateLimited(status, result.meta, result.data, c.now()) {
		return c.botRateLimited(ctx, spec, progress, result)
	}
	return c.botSettledStatus(ctx, spec, progress, result)
}

func (c *Client) botSettledStatus(ctx context.Context, spec requestSpec, progress *botProgress, result roundResult) (roundResult, bool, error) {
	if spec.mutate {
		return c.botMutationError(spec, result)
	}
	if isTransient(result.meta.Status) && progress.transportAttempts < maxReadRetries {
		if err := c.sleeper(ctx, retryDelay(progress.transportAttempts)); err != nil {
			return result, false, err
		}
		progress.transportAttempts++
		return roundResult{}, true, nil
	}
	return result, false, botAPIError(spec.operation, result, result.data)
}

func (c *Client) maybeRefreshCredential(ctx context.Context, spec requestSpec, progress *botProgress, status int) bool {
	if status != http.StatusUnauthorized || spec.mutate || progress.refreshed {
		return false
	}
	progress.refreshed = true
	c.refreshCredential(ctx, spec)
	return true
}

func (c *Client) botRateLimited(ctx context.Context, spec requestSpec, progress *botProgress, result roundResult) (roundResult, bool, error) {
	c.mu.Lock()
	wait := rateWait(result.meta, &c.rate, c.now())
	c.mu.Unlock()
	switch {
	case spec.mutate && progress.rateRetried:
		return result, false, botAPIError(spec.operation, result, result.data)
	case spec.mutate:
		progress.rateRetried = true
	case progress.rateAttempts >= maxReadRetries:
		return result, false, errors.New("github: rate limit retry budget exhausted")
	default:
		progress.rateAttempts++
	}
	if err := c.sleeper(ctx, wait); err != nil {
		return result, false, err
	}
	return roundResult{}, true, nil
}

func (c *Client) botMutationError(spec requestSpec, result roundResult) (roundResult, bool, error) {
	if result.meta.Status >= 500 {
		return result, false, &AmbiguousWriteError{Operation: spec.operation, RequestID: result.requestID}
	}
	return result, false, botAPIError(spec.operation, result, result.data)
}

func (c *Client) refreshCredential(ctx context.Context, spec requestSpec) {
	if invalidator, ok := c.credential.(CredentialInvalidator); ok {
		_ = invalidator.Invalidate(ctx, CredentialRequest{Repository: spec.repo, Workflow: spec.workflow})
	}
}

func isTransient(status int) bool {
	return status == http.StatusBadGateway || status == http.StatusServiceUnavailable || status == http.StatusGatewayTimeout
}

func retryDelay(attempt int) time.Duration {
	return time.Duration(1<<attempt) * time.Second
}

func retryAt(meta ResponseMeta) time.Time {
	if !meta.RetryAfter.IsZero() {
		return meta.RetryAfter
	}
	if !meta.Reset.IsZero() {
		return meta.Reset.Add(time.Second)
	}
	return time.Time{}
}

func apiMessage(data []byte) string {
	var decoded struct {
		Message string `json:"message"`
	}
	if err := json.Unmarshal(data, &decoded); err == nil {
		return decoded.Message
	}
	return ""
}

func botAPIError(operation string, result roundResult, data []byte) error {
	return &APIError{
		Operation: operation,
		Status:    result.meta.Status,
		RequestID: result.requestID,
		Message:   apiMessage(data),
		RetryAt:   retryAt(result.meta),
	}
}

func reporterAPIError(operation string, result roundResult, data []byte) error {
	return &APIError{
		Operation: operation,
		Status:    result.meta.Status,
		RequestID: result.requestID,
		Message:   apiMessage(data),
		RetryAt:   retryAt(result.meta),
	}
}
