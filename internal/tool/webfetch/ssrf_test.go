package webfetch

import (
	"net"
	"net/http"
	"net/http/httptest"
	"strconv"
	"strings"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/tool"
)

func testTool() tool.Tool {
	return &Tool{client: newLoopbackClient(), allow: loopbackAddrs}
}

func TestBlockedIPTable(t *testing.T) {
	t.Parallel()
	cases := []struct {
		raw     string
		blocked bool
	}{
		{"127.0.0.1", true},
		{"::1", true},
		{"10.0.0.5", true},
		{"172.16.9.9", true},
		{"192.168.1.1", true},
		{"169.254.169.254", true},
		{"fe80::1", true},
		{"0.0.0.0", true},
		{"::", true},
		{"224.0.0.1", true},
		{"100.100.100.200", true},
		{"8.8.8.8", false},
		{"1.1.1.1", false},
		{"2606:4700:4700::1111", false},
	}
	for _, tc := range cases {
		ip := net.ParseIP(tc.raw)
		if ip == nil {
			t.Fatalf("bad test IP %q", tc.raw)
		}
		if got := blockedIP(ip); got != tc.blocked {
			t.Errorf("blockedIP(%s) = %v, want %v", tc.raw, got, tc.blocked)
		}
	}
}

func TestValidateURLRejectsBlockedLiterals(t *testing.T) {
	t.Parallel()
	for _, raw := range []string{
		"http://169.254.169.254/latest/meta-data/",
		"http://10.0.0.5/internal",
		"http://192.168.1.1/",
		"http://127.0.0.1:8080/",
		"http://[::1]/",
		"http://[fe80::1]/",
	} {
		if err := validateURL(raw, publicAddrs); err == nil {
			t.Errorf("validateURL(%q) accepted a blocked literal", raw)
		} else if !strings.Contains(err.Error(), "blocked") {
			t.Errorf("validateURL(%q) error %v does not say blocked", raw, err)
		}
	}
	if err := validateURL("https://example.com/doc", publicAddrs); err != nil {
		t.Errorf("validateURL(public) failed: %v", err)
	}
}

func TestFetchRejectsLoopbackLiteral(t *testing.T) {
	t.Parallel()
	_, err := New().Run(t.Context(), tool.Call{
		Args: []byte(`{"url":"http://127.0.0.1/"}`),
	}, tool.Env{})
	if err == nil {
		t.Fatal("fetch of loopback literal succeeded, want a blocked error")
	}
	if !strings.Contains(err.Error(), "blocked") {
		t.Fatalf("fetch error %v does not say blocked", err)
	}
}

func TestFetchRejectsMetadataLiteral(t *testing.T) {
	t.Parallel()
	_, err := New().Run(t.Context(), tool.Call{
		Args: []byte(`{"url":"http://169.254.169.254/latest/meta-data/"}`),
	}, tool.Env{})
	if err == nil {
		t.Fatal("fetch of metadata address succeeded, want a blocked error")
	}
	if !strings.Contains(err.Error(), "blocked") {
		t.Fatalf("fetch error %v does not say blocked", err)
	}
}

func TestDialGuardBlocksLiteralWithoutNetwork(t *testing.T) {
	t.Parallel()
	guard := dialGuard(publicAddrs)
	for _, addr := range []string{"127.0.0.1:80", "10.0.0.1:80", "169.254.169.254:80"} {
		conn, err := guard(t.Context(), "tcp", addr)
		if err == nil {
			_ = conn.Close()
			t.Fatalf("dialGuard(%q) dialed a blocked address", addr)
		}
		if !strings.Contains(err.Error(), "blocked") {
			t.Fatalf("dialGuard(%q) error %v does not say blocked", addr, err)
		}
	}
}

func TestDialGuardDefersPublicNamesToDNS(t *testing.T) {
	t.Parallel()
	_, err := dialGuard(publicAddrs)(t.Context(), "tcp", "nonexistent.invalid:80")
	if err == nil {
		t.Fatal("dialGuard resolved an invalid name")
	}
	if strings.Contains(err.Error(), "blocked") {
		t.Fatalf("dialGuard misreported unresolvable name as blocked: %v", err)
	}
}

func TestRedirectToBlockedLiteralFails(t *testing.T) {
	t.Parallel()
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path == "/start" {
			http.Redirect(w, r, "http://169.254.169.254/final", http.StatusFound)
			return
		}
		http.NotFound(w, r)
	}))
	t.Cleanup(server.Close)
	result, err := testTool().Run(t.Context(), tool.Call{
		Args: []byte(`{"url":` + strconv.Quote(server.URL+"/start") + `}`),
	}, tool.Env{})
	if err != nil {
		t.Fatalf("redirected fetch returned error: %v", err)
	}
	if result.OK {
		t.Fatalf("redirect to metadata address succeeded: %q", result.Output)
	}
	if !strings.Contains(string(result.Output), "blocked") {
		t.Fatalf("redirect output %q does not say blocked", result.Output)
	}
}

func TestLoopbackPolicyStillBlocksOtherPrivate(t *testing.T) {
	t.Parallel()
	for _, raw := range []string{"10.0.0.1", "169.254.169.254"} {
		if loopbackAddrs(net.ParseIP(raw)) {
			t.Errorf("loopbackAddrs(%s) = true, want false", raw)
		}
	}
	if !loopbackAddrs(net.ParseIP("127.0.0.1")) {
		t.Error("loopbackAddrs(127.0.0.1) = false, want true")
	}
}

func TestDialPlanSkipsBlockedAddrs(t *testing.T) {
	t.Parallel()
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = listener.Close() })
	port := strconv.Itoa(listener.Addr().(*net.TCPAddr).Port)
	plan := dialPlan{
		dialer:  &net.Dialer{Timeout: 5 * time.Second},
		network: "tcp",
		host:    "example",
		port:    port,
		addrs:   []net.IPAddr{{IP: net.ParseIP("10.0.0.1")}, {IP: net.ParseIP("127.0.0.1")}},
		allow:   loopbackAddrs,
	}
	conn, err := plan.connect(t.Context())
	if err != nil {
		t.Fatal(err)
	}
	_ = conn.Close()
}

func TestDialPlanRejectsAllBlockedAddrs(t *testing.T) {
	t.Parallel()
	plan := dialPlan{
		dialer:  &net.Dialer{Timeout: 5 * time.Second},
		network: "tcp",
		host:    "example",
		port:    "80",
		addrs:   []net.IPAddr{{IP: net.ParseIP("10.0.0.1")}, {IP: net.ParseIP("169.254.169.254")}},
		allow:   publicAddrs,
	}
	_, err := plan.connect(t.Context())
	if err == nil {
		t.Fatal("connect through blocked addresses succeeded")
	}
	if !strings.Contains(err.Error(), "blocked") {
		t.Fatalf("connect error %v does not say blocked", err)
	}
}

func TestDialGuardBlocksLocalhostName(t *testing.T) {
	t.Parallel()
	_, err := dialGuard(publicAddrs)(t.Context(), "tcp", "localhost:80")
	if err == nil {
		t.Fatal("dial to localhost succeeded, want a blocked error")
	}
	if !strings.Contains(err.Error(), "blocked") {
		t.Fatalf("dial error %v does not say blocked", err)
	}
}
