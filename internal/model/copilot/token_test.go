package copilot

import (
	"context"
	"errors"
	"fmt"
	"io"
	"net/http"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"testing/synctest"
	"time"
)

type exchangeTransport struct {
	calls   atomic.Int32
	release chan struct{}
	last    atomic.Pointer[http.Request]
}

func (e *exchangeTransport) RoundTrip(req *http.Request) (*http.Response, error) {
	number := e.calls.Add(1)
	e.last.Store(req)
	if e.release != nil {
		select {
		case <-e.release:
		case <-req.Context().Done():
			return nil, req.Context().Err()
		}
	}
	body := fmt.Sprintf(`{"token":"cop-%d","expires_at":%d}`, number, time.Now().Add(time.Hour).Unix())
	return &http.Response{StatusCode: http.StatusOK, Header: http.Header{}, Body: io.NopCloser(strings.NewReader(body)), Request: req}, nil
}

func fixedGitHub(value string) func(context.Context) (string, error) {
	return func(context.Context) (string, error) { return value, nil }
}

func newSource(github func(context.Context) (string, error), transport http.RoundTripper) *tokenSource {
	return newTokenSource(github, &http.Client{Transport: transport}, "https://token.test/exchange")
}

func bearerIs(t *testing.T, source *tokenSource, want string) {
	t.Helper()
	got, err := source.bearer(t.Context())
	if err != nil || got != want {
		t.Fatalf("bearer = %q, %v; want %q", got, err, want)
	}
}

func TestCopilotExchangeCaches(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		transport := &exchangeTransport{}
		source := newSource(fixedGitHub("gh-1"), transport)
		bearerIs(t, source, "cop-1")
		bearerIs(t, source, "cop-1")
		if transport.calls.Load() != 1 {
			t.Fatalf("exchanges = %d, want 1 for two requests", transport.calls.Load())
		}
		time.Sleep(time.Hour - 61*time.Second)
		bearerIs(t, source, "cop-1")
		time.Sleep(time.Second)
		bearerIs(t, source, "cop-2")
		if transport.calls.Load() != 2 {
			t.Fatalf("exchanges = %d, want 2 once the clock reaches expires_at minus 60 s", transport.calls.Load())
		}
		request := transport.last.Load()
		if request.Method != http.MethodGet || request.Header.Get("Authorization") != "Token gh-1" || request.Header.Get("Accept") != "application/json" {
			t.Fatalf("exchange request = %s %v", request.Method, request.Header)
		}
	})
}

func TestCopilotExchangeFollowsTheGitHubToken(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		transport := &exchangeTransport{}
		var current atomic.Value
		current.Store("gh-1")
		source := newSource(func(context.Context) (string, error) { return current.Load().(string), nil }, transport)
		bearerIs(t, source, "cop-1")
		current.Store("gh-2")
		bearerIs(t, source, "cop-2")
		bearerIs(t, source, "cop-2")
		if got := transport.last.Load().Header.Get("Authorization"); transport.calls.Load() != 2 || got != "Token gh-2" {
			t.Fatalf("exchanges = %d, last authorization = %q; want 2 and the new token", transport.calls.Load(), got)
		}
	})
}

func TestCopilotExchangeSharesOneRefresh(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		transport := &exchangeTransport{release: make(chan struct{})}
		source := newSource(fixedGitHub("gh-1"), transport)
		const callers = 8
		tokens := make([]string, callers)
		errs := make([]error, callers)
		var group sync.WaitGroup
		for index := range callers {
			group.Go(func() { tokens[index], errs[index] = source.bearer(t.Context()) })
		}
		synctest.Wait()
		close(transport.release)
		group.Wait()
		for index := range callers {
			if errs[index] != nil || tokens[index] != "cop-1" {
				t.Fatalf("caller %d: token = %q, error = %v; want cop-1", index, tokens[index], errs[index])
			}
		}
		if transport.calls.Load() != 1 {
			t.Fatalf("exchanges = %d, want 1 shared by %d callers", transport.calls.Load(), callers)
		}
	})
}

func TestCopilotExchangeStopsAfterThirtySeconds(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		source := newSource(fixedGitHub("gh-1"), &exchangeTransport{release: make(chan struct{})})
		start := time.Now()
		_, err := source.bearer(t.Context())
		if !errors.Is(err, context.DeadlineExceeded) || time.Since(start) != 30*time.Second {
			t.Fatalf("error = %v after %v, want a deadline error after 30 s", err, time.Since(start))
		}
	})
}

func TestCopilotWaitingCallerHonoursItsContext(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		transport := &exchangeTransport{release: make(chan struct{})}
		source := newSource(fixedGitHub("gh-1"), transport)
		first := make(chan error, 1)
		go func() {
			_, err := source.bearer(t.Context())
			first <- err
		}()
		synctest.Wait()
		waiter, cancel := context.WithCancel(t.Context())
		result := make(chan error, 1)
		go func() {
			_, err := source.bearer(waiter)
			result <- err
		}()
		synctest.Wait()
		cancel()
		if err := <-result; !errors.Is(err, context.Canceled) {
			t.Fatalf("waiting caller error = %v, want context.Canceled", err)
		}
		close(transport.release)
		if err := <-first; err != nil {
			t.Fatalf("first caller error = %v", err)
		}
		if transport.calls.Load() != 1 {
			t.Fatalf("exchanges = %d, want 1", transport.calls.Load())
		}
	})
}
