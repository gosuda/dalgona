package mcp

import (
	"bufio"
	"bytes"
	"context"
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"io"
	"mime"
	"net/http"
	"strings"
	"time"
)

var errSessionExpired = errors.New("MCP session expired")

const maxRedirects = 10

type httpTransport struct {
	name            string
	url             string
	client          *http.Client
	auth            TokenSource
	sessionID       string
	protocolVersion string
}

func newHTTPTransport(name, url string, auth TokenSource) *httpTransport {
	client := &http.Client{Timeout: 5 * time.Minute, CheckRedirect: dropForeignAuthorization}
	return &httpTransport{name: name, url: url, client: client, auth: auth}
}

func dropForeignAuthorization(request *http.Request, via []*http.Request) error {
	if len(via) >= maxRedirects {
		return fmt.Errorf("stopped after %d redirects", maxRedirects)
	}
	if request.URL.Scheme != via[0].URL.Scheme || request.URL.Host != via[0].URL.Host {
		request.Header.Del("Authorization")
	}
	return nil
}

func (t *httpTransport) request(ctx context.Context, id uint64, method string, params jsontext.Value) (jsontext.Value, error) {
	message, err := encodeMessage(&id, method, params)
	if err != nil {
		return nil, err
	}
	return t.post(ctx, message, &id)
}

func (t *httpTransport) notify(ctx context.Context, method string) error {
	message, err := encodeMessage(nil, method, nil)
	if err != nil {
		return err
	}
	_, err = t.post(ctx, message, nil)
	return err
}

func (t *httpTransport) post(ctx context.Context, message []byte, expectedID *uint64) (jsontext.Value, error) {
	token, err := t.token(ctx)
	if err != nil {
		return nil, err
	}
	response, err := t.send(ctx, message, token)
	if err != nil {
		return nil, err
	}
	if response.StatusCode == http.StatusUnauthorized && t.auth != nil {
		response, err = t.retryUnauthorized(ctx, message, response)
		if err != nil {
			return nil, err
		}
	}
	defer func() { _ = response.Body.Close() }()
	return t.receive(response, expectedID)
}

func (t *httpTransport) send(ctx context.Context, message []byte, token string) (*http.Response, error) {
	request, err := http.NewRequestWithContext(ctx, http.MethodPost, t.url, bytes.NewReader(message))
	if err != nil {
		return nil, fmt.Errorf("create HTTP request: %w", err)
	}
	request.Header.Set("Content-Type", "application/json")
	request.Header.Set("Accept", "application/json, text/event-stream")
	setAuthorization(request, token)
	t.setHeaders(request)
	response, err := t.client.Do(request)
	if err != nil {
		return nil, fmt.Errorf("send HTTP request: %w", err)
	}
	return response, nil
}

func setAuthorization(request *http.Request, token string) {
	if token != "" {
		request.Header.Set("Authorization", "Bearer "+token)
	}
}

func (t *httpTransport) token(ctx context.Context) (string, error) {
	if t.auth == nil {
		return "", nil
	}
	token, err := t.auth.Token(ctx)
	if errors.Is(err, errNotLoggedIn) {
		return "", &LoginRequiredError{Name: t.name, Err: err}
	}
	if err != nil {
		return "", fmt.Errorf("MCP server %s token: %w", t.name, err)
	}
	return token, nil
}

func (t *httpTransport) retryUnauthorized(ctx context.Context, message []byte, rejected *http.Response) (*http.Response, error) {
	discardBody(rejected)
	token, err := t.auth.Refresh(ctx)
	if err != nil {
		return nil, t.refreshFailure(ctx, err)
	}
	response, err := t.send(ctx, message, token)
	if err != nil {
		return nil, err
	}
	if response.StatusCode == http.StatusUnauthorized {
		discardBody(response)
		return nil, &LoginRequiredError{Name: t.name}
	}
	return response, nil
}

func (t *httpTransport) refreshFailure(ctx context.Context, err error) error {
	if ctxErr := ctx.Err(); ctxErr != nil {
		return fmt.Errorf("refresh MCP server %s token: %w", t.name, ctxErr)
	}
	if errors.Is(err, errRefreshUnsupported) {
		return err
	}
	return &LoginRequiredError{Name: t.name, Err: err}
}

func discardBody(response *http.Response) {
	_, _ = io.Copy(io.Discard, io.LimitReader(response.Body, 8192))
	_ = response.Body.Close()
}

func (t *httpTransport) receive(response *http.Response, expectedID *uint64) (jsontext.Value, error) {
	if err := t.captureSession(response); err != nil {
		return nil, err
	}
	if response.StatusCode == http.StatusNotFound && t.sessionID != "" {
		return nil, errSessionExpired
	}
	if response.StatusCode < 200 || response.StatusCode >= 300 {
		return nil, readHTTPStatusError(response)
	}
	if expectedID == nil {
		return nil, nil
	}
	return readResponsePayload(response, *expectedID)
}

func readResponsePayload(response *http.Response, expectedID uint64) (jsontext.Value, error) {
	mediaType, _, err := mime.ParseMediaType(response.Header.Get("Content-Type"))
	if err != nil {
		return nil, fmt.Errorf("parse response Content-Type: %w", err)
	}
	switch mediaType {
	case "application/json":
		return readJSONResponse(response.Body, expectedID)
	case "text/event-stream":
		return readSSEResponse(response.Body, expectedID)
	default:
		return nil, fmt.Errorf("unsupported response Content-Type %q", mediaType)
	}
}

func (t *httpTransport) setHeaders(request *http.Request) {
	if t.sessionID != "" {
		request.Header.Set("Mcp-Session-Id", t.sessionID)
	}
	if t.protocolVersion != "" {
		request.Header.Set("MCP-Protocol-Version", t.protocolVersion)
	}
}

func (t *httpTransport) captureSession(response *http.Response) error {
	sessionID := response.Header.Get("Mcp-Session-Id")
	if sessionID == "" {
		return nil
	}
	for _, char := range []byte(sessionID) {
		if char < 0x21 || char > 0x7e {
			return errors.New("mcp: server returned an invalid session id")
		}
	}
	t.sessionID = sessionID
	return nil
}

func readHTTPStatusError(response *http.Response) error {
	body, err := io.ReadAll(io.LimitReader(response.Body, 8192))
	if err != nil {
		return fmt.Errorf("mcp: HTTP %s: read error body: %w", response.Status, err)
	}
	detail := strings.TrimSpace(string(body))
	if detail == "" {
		return fmt.Errorf("mcp: HTTP %s", response.Status)
	}
	return fmt.Errorf("mcp: HTTP %s: %s", response.Status, detail)
}

func readJSONResponse(body io.Reader, expectedID uint64) (jsontext.Value, error) {
	raw, err := io.ReadAll(io.LimitReader(body, 16<<20+1))
	if err != nil {
		return nil, fmt.Errorf("read JSON response: %w", err)
	}
	if len(raw) > 16<<20 {
		return nil, errors.New("mcp: JSON response exceeds 16 MiB")
	}
	matched, result, err := matchResponse(raw, expectedID)
	if err != nil {
		return nil, fmt.Errorf("decode JSON response: %w", err)
	}
	if !matched {
		return nil, errors.New("mcp: JSON response id does not match the request")
	}
	return result, nil
}

func readSSEResponse(body io.Reader, expectedID uint64) (jsontext.Value, error) {
	scanner := bufio.NewScanner(body)
	scanner.Buffer(make([]byte, 64*1024), 16<<20)
	var data []byte
	for done := false; ; {
		var line []byte
		if scanner.Scan() {
			line = scanner.Bytes()
		} else {
			done = true
		}
		if len(line) > 0 {
			data = appendSSEData(data, line)
			continue
		}
		matched, result, err := matchSSEData(data, expectedID)
		if err != nil || matched {
			return result, err
		}
		data = data[:0]
		if done {
			if err := scanner.Err(); err != nil {
				return nil, fmt.Errorf("read SSE response: %w", err)
			}
			return nil, errors.New("mcp: SSE stream ended without a matching response")
		}
	}
}

func appendSSEData(data, line []byte) []byte {
	if len(line) == 0 || line[0] == ':' {
		return data
	}
	field, value, found := bytes.Cut(line, []byte{':'})
	if !found {
		value = nil
	}
	value = bytes.TrimPrefix(value, []byte{' '})
	if !bytes.Equal(field, []byte("data")) {
		return data
	}
	data = append(data, value...)
	return append(data, '\n')
}

func matchSSEData(data []byte, expectedID uint64) (bool, jsontext.Value, error) {
	if len(data) == 0 {
		return false, nil, nil
	}
	data = bytes.TrimSuffix(data, []byte{'\n'})
	if len(data) == 0 {
		return false, nil, nil
	}
	matched, result, err := matchResponse(data, expectedID)
	if err != nil {
		return false, nil, fmt.Errorf("decode SSE response: %w", err)
	}
	return matched, result, err
}

func (t *httpTransport) setProtocolVersion(version string) {
	t.protocolVersion = version
}

func (t *httpTransport) resetSession() {
	t.sessionID = ""
	t.protocolVersion = ""
}

func (t *httpTransport) Close() error {
	if t.sessionID != "" {
		t.deleteSession()
		t.sessionID = ""
	}
	t.client.CloseIdleConnections()
	return nil
}

func (t *httpTransport) deleteSession() {
	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()
	request, err := http.NewRequestWithContext(ctx, http.MethodDelete, t.url, nil)
	if err != nil {
		return
	}
	request.Header.Set("Mcp-Session-Id", t.sessionID)
	if t.protocolVersion != "" {
		request.Header.Set("MCP-Protocol-Version", t.protocolVersion)
	}
	if token, tokenErr := t.token(ctx); tokenErr == nil {
		setAuthorization(request, token)
	}
	response, err := t.client.Do(request)
	if err != nil {
		return
	}
	_ = response.Body.Close()
}
