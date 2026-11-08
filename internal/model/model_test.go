package model

import "testing"

func TestClassify(t *testing.T) {
	t.Parallel()
	cases := []struct {
		name   string
		status int
		body   string
		want   Verdict
	}{
		{name: "rate limit", status: 429, want: Retry},
		{name: "request timeout", status: 408, want: Retry},
		{name: "server", status: 503, want: Retry},
		{name: "transport", status: 0, body: "unexpected EOF", want: Retry},
		{name: "body rate limit", status: 200, body: `{"error":{"code":429}}`, want: Retry},
		{name: "body string server", status: 200, body: `{"error":{"status":"503"}}`, want: Retry},
		{name: "overflow", status: 400, body: `{"error":{"code":"context_length_exceeded"}}`, want: Overflow},
		{name: "anthropic overflow", status: 400, body: "prompt is too long", want: Overflow},
		{name: "google overflow", status: 400, body: "exceeds the maximum number of tokens", want: Overflow},
		{name: "unauthorized", status: 401, want: Auth},
		{name: "forbidden", status: 403, want: Auth},
		{name: "bad request", status: 400, body: "bad", want: Terminal},
		{name: "usage limit", status: 200, body: `{"error":{"code":"usage_limit_reached"}}`, want: Terminal},
	}
	for _, test := range cases {
		t.Run(test.name, func(t *testing.T) {
			t.Parallel()
			if got := Classify(test.status, []byte(test.body)); got != test.want {
				t.Fatalf("Classify(%d, %q) = %v, want %v", test.status, test.body, got, test.want)
			}
		})
	}
}

func TestContextWindow(t *testing.T) {
	t.Parallel()
	overrides := map[string]int{"configured": 64000}
	cases := []struct {
		name     string
		modelID  string
		reported int
		want     int
	}{
		{name: "configured overrides report", modelID: "configured", reported: 200000, want: 64000},
		{name: "reported fallback", modelID: "reported", reported: 200000, want: 200000},
		{name: "default fallback", modelID: "unknown", want: 128000},
	}
	for _, test := range cases {
		t.Run(test.name, func(t *testing.T) {
			t.Parallel()
			if got := ContextWindow(test.modelID, test.reported, overrides); got != test.want {
				t.Fatalf("ContextWindow() = %d, want %d", got, test.want)
			}
		})
	}
}

func TestClampEffort(t *testing.T) {
	t.Parallel()
	cases := []struct {
		name      string
		request   string
		supported []string
		want      string
	}{
		{name: "maximum clamps down", request: "max", supported: []string{"low", "high"}, want: "high"},
		{name: "highest below request", request: "xhigh", supported: []string{"high", "low", "medium"}, want: "high"},
		{name: "minimum clamps up", request: "low", supported: []string{"medium", "high"}, want: "medium"},
		{name: "nil omits effort", request: "medium", want: ""},
		{name: "default omits effort", request: "default", supported: []string{"low"}, want: ""},
	}
	for _, test := range cases {
		t.Run(test.name, func(t *testing.T) {
			t.Parallel()
			if got := ClampEffort(test.request, test.supported); got != test.want {
				t.Fatalf("ClampEffort(%q, %v) = %q, want %q", test.request, test.supported, got, test.want)
			}
		})
	}
}

func TestResolveRole(t *testing.T) {
	t.Parallel()
	cases := []struct {
		name     string
		role     string
		thread   string
		offered  []string
		want     string
		wantLine string
	}{
		{name: "static preset", role: "smol", thread: "anthropic/claude-opus-5-5", offered: []string{"claude-haiku-5-5"}, want: "anthropic/claude-haiku-5-5"},
		{name: "missing preset", role: "smol", thread: "anthropic/claude-opus-5-5", want: "anthropic/claude-opus-5-5", wantLine: "roles.smol preset anthropic/claude-haiku-5-5 is not offered; using anthropic/claude-opus-5-5"},
		{name: "slow preset", role: "slow", thread: "openai/gpt-6.1-sol", offered: []string{"gpt-6-astra"}, want: "openai/gpt-6-astra"},
		{name: "dynamic preset", role: "smol", thread: "chatcompat/first", offered: []string{"cheap", "large"}, want: "chatcompat/cheap"},
		{name: "default role", role: "default", thread: "google/gemini-3.8-flash", want: "google/gemini-3.8-flash"},
		{name: "explicit model", role: "openai/other", thread: "openai/gpt-6.1-sol", want: "openai/other"},
	}
	for _, test := range cases {
		t.Run(test.name, func(t *testing.T) {
			t.Parallel()
			got, line := ResolveRole(test.role, test.thread, test.offered)
			if got != test.want || line != test.wantLine {
				t.Fatalf("ResolveRole() = %q, %q; want %q, %q", got, line, test.want, test.wantLine)
			}
		})
	}
}
