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

func TestBlockedIPSpecialRanges(t *testing.T) {
	t.Parallel()
	blocked := []struct {
		prefix string
		addrs  []string
	}{
		{"0.0.0.0/8", []string{"0.0.0.0", "0.1.2.3", "0.255.255.255"}},
		{"10.0.0.0/8", []string{"10.0.0.0", "10.1.2.3", "10.255.255.255"}},
		{"100.64.0.0/10", []string{"100.64.0.0", "100.64.0.1", "100.127.255.255"}},
		{"127.0.0.0/8", []string{"127.0.0.0", "127.0.0.1", "127.255.255.255"}},
		{"169.254.0.0/16", []string{"169.254.0.0", "169.254.169.254", "169.254.255.255"}},
		{"172.16.0.0/12", []string{"172.16.0.0", "172.16.9.9", "172.31.255.255"}},
		{"192.0.0.0/24", []string{"192.0.0.0", "192.0.0.170"}},
		{"192.0.2.0/24", []string{"192.0.2.0", "192.0.2.1", "192.0.2.255"}},
		{"192.168.0.0/16", []string{"192.168.0.0", "192.168.1.1", "192.168.255.255"}},
		{"198.18.0.0/15", []string{"198.18.0.0", "198.18.0.1", "198.19.255.255"}},
		{"198.51.100.0/24", []string{"198.51.100.0", "198.51.100.7", "198.51.100.255"}},
		{"203.0.113.0/24", []string{"203.0.113.0", "203.0.113.9", "203.0.113.255"}},
		{"224.0.0.0/4", []string{"224.0.0.0", "224.0.0.1", "239.255.255.255"}},
		{"240.0.0.0/4", []string{"240.0.0.1", "255.255.255.254", "255.255.255.255"}},
		{"255.255.255.255/32", []string{"255.255.255.255"}},
		{"::/128", []string{"::"}},
		{"::1/128", []string{"::1"}},
		{"fc00::/7", []string{"fc00::1", "fd00::1", "fdff:ffff:ffff:ffff:ffff:ffff:ffff:ffff"}},
		{"fe80::/10", []string{"fe80::1", "febf:ffff:ffff:ffff:ffff:ffff:ffff:ffff"}},
		{"ff00::/8", []string{"ff00::1", "ff02::1", "ffff:ffff:ffff:ffff:ffff:ffff:ffff:ffff"}},
		{"64:ff9b::/96", []string{"64:ff9b::808:808", "64:ff9b::a00:1", "64:ff9b::c000:201"}},
		{"2001:db8::/32", []string{"2001:db8::1", "2001:db8:ffff:ffff:ffff:ffff:ffff:ffff"}},
		{"4-in-6 mapped", []string{"::ffff:0.1.2.3", "::ffff:10.0.0.1", "::ffff:100.64.0.1", "::ffff:198.18.0.1"}},
	}
	for _, tc := range blocked {
		for _, raw := range tc.addrs {
			ip := net.ParseIP(raw)
			if ip == nil {
				t.Fatalf("bad test IP %q in %s", raw, tc.prefix)
			}
			if !blockedIP(ip) {
				t.Errorf("blockedIP(%s) in %s = false, want true", raw, tc.prefix)
			}
		}
	}
	allowed := []string{
		"8.8.8.8",
		"1.1.1.1",
		"9.9.9.9",
		"100.128.0.1",
		"172.32.0.1",
		"192.167.0.1",
		"198.20.0.1",
		"::ffff:8.8.8.8",
		"2606:4700:4700::1111",
		"2001:4860:4860::8888",
	}
	for _, raw := range allowed {
		ip := net.ParseIP(raw)
		if ip == nil {
			t.Fatalf("bad test IP %q", raw)
		}
		if blockedIP(ip) {
			t.Errorf("blockedIP(%s) = true, want false", raw)
		}
	}
}
