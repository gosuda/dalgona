package webfetch

import (
	"bytes"
	"cmp"
	"context"
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"io"
	"mime"
	"net"
	"net/http"
	"net/netip"
	"net/url"
	"strconv"
	"strings"
	"time"

	"github.com/gosuda/dalgona/internal/runner"
	"github.com/gosuda/dalgona/internal/tool"
	"golang.org/x/net/html"
	"golang.org/x/net/html/atom"
)

const defaultMaxBytes int64 = 1048576

var errFetchTimeout = errors.New("remote fetch timed out")

type Tool struct {
	client *http.Client
	allow  func(net.IP) bool
}

type arguments struct {
	url      string
	maxBytes int64
}

type processResult struct {
	data   []byte
	exit   *runner.Exit
	path   string
	offset int64
}

func New() tool.Tool { return &Tool{client: newHTTPClient(), allow: publicAddrs} }

func (t *Tool) Spec() tool.Spec {
	return tool.Spec{
		Name:        "web_fetch",
		Description: "Fetch an HTTP or HTTPS URL. HTML is converted to text. Other textual content is returned unchanged. The response body is capped by max_bytes.",
		Schema:      jsontext.Value(`{"type":"object","properties":{"url":{"type":"string"},"max_bytes":{"type":"integer","minimum":1,"maximum":16777216}},"required":["url"],"additionalProperties":false}`),
		Keep:        tool.KeepHead,
	}
}

func (t *Tool) Class() tool.Class { return tool.ClassRead }

func (t *Tool) Run(ctx context.Context, call tool.Call, env tool.Env) (tool.Result, error) {
	args, err := parseArgs(call.Args, t.allow)
	if err != nil {
		return tool.Result{}, err
	}
	if env.Runner == nil {
		return execute(ctx, args, t.client)
	}
	if tool.IsLocalRunner(env.Runner) {
		return execute(ctx, args, t.client)
	}
	return executeRemote(ctx, call.ID, args, env)
}

func execute(ctx context.Context, args arguments, client *http.Client) (tool.Result, error) {
	request, err := http.NewRequestWithContext(ctx, http.MethodGet, args.url, nil)
	if err != nil {
		return tool.Result{}, fmt.Errorf("web_fetch: create request: %w", err)
	}
	response, err := client.Do(request)
	if err != nil {
		text := fmt.Sprintf("web_fetch: fetch %s: %v", args.url, err)
		return tool.Result{Output: []byte(text), OK: false}, nil
	}
	defer func() { _ = response.Body.Close() }()
	if response.StatusCode < 200 || response.StatusCode >= 300 {
		text := "web_fetch: HTTP " + response.Status
		return tool.Result{Output: []byte(text), OK: false}, nil
	}
	body, truncated, err := readCapped(response.Body, args.maxBytes)
	if err != nil {
		text := fmt.Sprintf("web_fetch: read %s: %v", args.url, err)
		return tool.Result{Output: []byte(text), OK: false}, nil
	}
	output, err := renderBody(body, response.Header.Get("Content-Type"))
	if err != nil {
		text := fmt.Sprintf("web_fetch: render %s: %v", args.url, err)
		return tool.Result{Output: []byte(text), OK: false}, nil
	}
	if truncated {
		output = append(output, "\n[content truncated at "+strconv.FormatInt(args.maxBytes, 10)+" bytes]"...)
	}
	return tool.Result{Output: output, OK: true}, nil
}

func newHTTPClient() *http.Client {
	return newClient(publicAddrs)
}

func newLoopbackClient() *http.Client {
	return newClient(loopbackAddrs)
}

func newClient(allow func(net.IP) bool) *http.Client {
	transport := http.DefaultTransport.(*http.Transport).Clone()
	transport.Proxy = nil
	transport.DialContext = dialGuard(allow)
	transport.TLSHandshakeTimeout = 10 * time.Second
	transport.ResponseHeaderTimeout = 15 * time.Second
	client := &http.Client{Transport: transport, Timeout: 30 * time.Second}
	client.CheckRedirect = checkRedirect(allow)
	return client
}

func publicAddrs(ip net.IP) bool {
	return !blockedIP(ip)
}

func loopbackAddrs(ip net.IP) bool {
	return ip.IsLoopback() || !blockedIP(ip)
}

var blockedPrefixes = []netip.Prefix{
	netip.MustParsePrefix("0.0.0.0/8"),
	netip.MustParsePrefix("10.0.0.0/8"),
	netip.MustParsePrefix("100.64.0.0/10"),
	netip.MustParsePrefix("127.0.0.0/8"),
	netip.MustParsePrefix("169.254.0.0/16"),
	netip.MustParsePrefix("172.16.0.0/12"),
	netip.MustParsePrefix("192.0.0.0/24"),
	netip.MustParsePrefix("192.0.2.0/24"),
	netip.MustParsePrefix("192.168.0.0/16"),
	netip.MustParsePrefix("198.18.0.0/15"),
	netip.MustParsePrefix("198.51.100.0/24"),
	netip.MustParsePrefix("203.0.113.0/24"),
	netip.MustParsePrefix("224.0.0.0/4"),
	netip.MustParsePrefix("240.0.0.0/4"),
	netip.MustParsePrefix("255.255.255.255/32"),
	netip.MustParsePrefix("::/128"),
	netip.MustParsePrefix("::1/128"),
	netip.MustParsePrefix("fc00::/7"),
	netip.MustParsePrefix("fe80::/10"),
	netip.MustParsePrefix("ff00::/8"),
	netip.MustParsePrefix("64:ff9b::/96"),
	netip.MustParsePrefix("2001:db8::/32"),
}

var nat64Prefix = netip.MustParsePrefix("64:ff9b::/96")

func blockedIP(ip net.IP) bool {
	if ip == nil {
		return true
	}
	addr, ok := netip.AddrFromSlice(ip)
	if !ok {
		return true
	}
	addr = addr.Unmap()
	if !addr.IsValid() {
		return true
	}
	for _, prefix := range blockedPrefixes {
		if prefix.Contains(addr) {
			return true
		}
	}
	if embedded, ok := embeddedIPv4(addr); ok {
		for _, prefix := range blockedPrefixes {
			if prefix.Contains(embedded) {
				return true
			}
		}
	}
	return false
}

func embeddedIPv4(addr netip.Addr) (netip.Addr, bool) {
	if !addr.Is6() || !nat64Prefix.Contains(addr) {
		return netip.Addr{}, false
	}
	raw := addr.As16()
	embedded := netip.AddrFrom4([4]byte{raw[12], raw[13], raw[14], raw[15]})
	if !embedded.IsValid() {
		return netip.Addr{}, false
	}
	return embedded, true
}

func hostIP(host string) net.IP {
	bare, _, _ := strings.Cut(host, "%")
	return net.ParseIP(bare)
}

func dialGuard(allow func(net.IP) bool) func(context.Context, string, string) (net.Conn, error) {
	dialer := &net.Dialer{Timeout: 10 * time.Second, KeepAlive: 30 * time.Second}
	return func(ctx context.Context, network, address string) (net.Conn, error) {
		host, port, err := net.SplitHostPort(address)
		if err != nil {
			return nil, fmt.Errorf("web_fetch: invalid dial address: %w", err)
		}
		if ip := hostIP(host); ip != nil {
			if !allow(ip) {
				return nil, fmt.Errorf("web_fetch: blocked address %q", host)
			}
			return dialer.DialContext(ctx, network, address)
		}
		resolved, err := net.DefaultResolver.LookupIPAddr(ctx, host)
		if err != nil {
			return nil, fmt.Errorf("web_fetch: resolve %q: %w", host, err)
		}
		plan := dialPlan{dialer: dialer, network: network, host: host, port: port, addrs: resolved, allow: allow}
		return plan.connect(ctx)
	}
}

type dialPlan struct {
	dialer  *net.Dialer
	network string
	host    string
	port    string
	addrs   []net.IPAddr
	allow   func(net.IP) bool
}

func (plan dialPlan) connect(ctx context.Context) (net.Conn, error) {
	var lastErr error
	for _, addr := range plan.addrs {
		if !plan.allow(addr.IP) {
			continue
		}
		var conn net.Conn
		conn, lastErr = plan.dialer.DialContext(ctx, plan.network, net.JoinHostPort(addr.IP.String(), plan.port))
		if lastErr == nil {
			return conn, nil
		}
	}
	return nil, cmp.Or(lastErr, fmt.Errorf("web_fetch: host %q resolves only to blocked addresses", plan.host))
}

func checkRedirect(allow func(net.IP) bool) func(*http.Request, []*http.Request) error {
	return func(req *http.Request, via []*http.Request) error {
		if len(via) >= 10 {
			return errors.New("web_fetch: stopped after 10 redirects")
		}
		return validateURL(req.URL.String(), allow)
	}
}

func readCapped(body io.Reader, limit int64) ([]byte, bool, error) {
	data, err := io.ReadAll(io.LimitReader(body, limit+1))
	if err != nil {
		return nil, false, fmt.Errorf("read response body: %w", err)
	}
	truncated := int64(len(data)) > limit
	if truncated {
		data = data[:int(limit)]
	}
	return data, truncated, nil
}

func renderBody(body []byte, contentType string) ([]byte, error) {
	mediaType := bodyMediaType(body, contentType)
	if mediaType == "text/html" || mediaType == "application/xhtml+xml" {
		text, err := htmlText(body)
		return []byte(text), err
	}
	if !textualMediaType(mediaType) {
		text := "[binary content " + mediaType + ", " + strconv.Itoa(len(body)) + " bytes]"
		return []byte(text), nil
	}
	return body, nil
}

func bodyMediaType(body []byte, contentType string) string {
	if contentType == "" {
		return strings.ToLower(strings.SplitN(http.DetectContentType(body), ";", 2)[0])
	}
	mediaType, _, err := mime.ParseMediaType(contentType)
	if err != nil {
		return strings.ToLower(strings.TrimSpace(strings.SplitN(contentType, ";", 2)[0]))
	}
	return strings.ToLower(mediaType)
}

func textualMediaType(mediaType string) bool {
	if strings.HasPrefix(mediaType, "text/") {
		return true
	}
	if strings.HasSuffix(mediaType, "+json") || strings.HasSuffix(mediaType, "+xml") {
		return true
	}
	switch mediaType {
	case "application/json", "application/xml", "application/javascript", "application/x-javascript", "application/x-www-form-urlencoded":
		return true
	default:
		return false
	}
}

func htmlText(body []byte) (string, error) {
	tokenizer := html.NewTokenizer(bytes.NewReader(body))
	var out strings.Builder
	out.Grow(len(body))
	skipped := 0
	for {
		tokenType := tokenizer.Next()
		if tokenType == html.ErrorToken {
			return finishHTML(&out, tokenizer.Err())
		}
		name, isTag := htmlTagName(tokenizer, tokenType)
		if isTag {
			skipped = updateSkipped(name, tokenType, skipped)
		}
		if skipped > 0 {
			continue
		}
		if tokenType == html.TextToken {
			out.Write(tokenizer.Text())
		} else if isTag && blockTag(name) {
			out.WriteByte(' ')
		}
	}
}

func finishHTML(out *strings.Builder, err error) (string, error) {
	if err != nil && !errors.Is(err, io.EOF) {
		return "", fmt.Errorf("tokenize HTML: %w", err)
	}
	return strings.Join(strings.Fields(out.String()), " "), nil
}

func htmlTagName(tokenizer *html.Tokenizer, tokenType html.TokenType) (atom.Atom, bool) {
	switch tokenType {
	case html.StartTagToken, html.EndTagToken, html.SelfClosingTagToken:
		name, _ := tokenizer.TagName()
		return atom.Lookup(name), true
	case html.ErrorToken, html.TextToken, html.CommentToken, html.DoctypeToken:
		return 0, false
	default:
		return 0, false
	}
}

func updateSkipped(name atom.Atom, tokenType html.TokenType, skipped int) int {
	if name != atom.Script && name != atom.Style && name != atom.Noscript {
		return skipped
	}
	switch tokenType {
	case html.StartTagToken:
		return skipped + 1
	case html.EndTagToken:
		return max(0, skipped-1)
	case html.SelfClosingTagToken, html.ErrorToken, html.TextToken, html.CommentToken, html.DoctypeToken:
		return skipped
	default:
		return skipped
	}
}

var blockTags = map[atom.Atom]struct{}{
	atom.Address: {}, atom.Article: {}, atom.Aside: {}, atom.Blockquote: {}, atom.Br: {}, atom.Dd: {},
	atom.Div: {}, atom.Dl: {}, atom.Dt: {}, atom.Fieldset: {}, atom.Figcaption: {}, atom.Figure: {},
	atom.Footer: {}, atom.Form: {}, atom.H1: {}, atom.H2: {}, atom.H3: {}, atom.H4: {}, atom.H5: {},
	atom.H6: {}, atom.Header: {}, atom.Hr: {}, atom.Li: {}, atom.Main: {}, atom.Nav: {}, atom.Ol: {},
	atom.P: {}, atom.Pre: {}, atom.Section: {}, atom.Table: {}, atom.Tbody: {}, atom.Td: {},
	atom.Tfoot: {}, atom.Th: {}, atom.Thead: {}, atom.Tr: {}, atom.Ul: {},
}

func blockTag(name atom.Atom) bool {
	_, ok := blockTags[name]
	return ok
}

func executeRemote(ctx context.Context, id string, args arguments, env tool.Env) (tool.Result, error) {
	if id == "" {
		return tool.Result{}, errors.New("web_fetch: call id is required for remote fetch")
	}
	remoteCtx, cancel := context.WithTimeoutCause(ctx, 35*time.Second, errFetchTimeout)
	defer cancel()
	spec := runner.ProcessSpec{
		ID:      id,
		Argv:    []string{"@self", "__fetch", args.url, strconv.FormatInt(args.maxBytes, 10)},
		Dir:     env.Cwd,
		Sandbox: env.Sandbox,
	}
	if err := env.Runner.Start(remoteCtx, spec); err != nil {
		return tool.Result{}, fmt.Errorf("web_fetch: start subprocess: %w", err)
	}
	result, err := collectRemote(remoteCtx, env.Runner, id)
	if err != nil {
		return tool.Result{}, errors.Join(err, killRemote(remoteCtx, env.Runner, id))
	}
	code := result.exit.Code
	return tool.Result{Output: result.data, OK: code == 0, ExitCode: &code, Path: result.path}, nil
}

func collectRemote(ctx context.Context, run runner.Runner, id string) (processResult, error) {
	var result processResult
	for {
		previous := result.offset
		out, err := run.Read(ctx, id, previous)
		if err != nil {
			return processResult{}, fmt.Errorf("web_fetch: read subprocess: %w", err)
		}
		result.data = append(result.data, out.Data...)
		result.offset = out.Next
		result.path = out.Path
		if out.Exit != nil {
			result.exit = out.Exit
		}
		if result.exit != nil {
			if result.offset == previous {
				return result, nil
			}
			continue
		}
		if !out.Alive {
			return processResult{}, errors.New("web_fetch: subprocess stopped without an exit record")
		}
		if err := waitRemote(ctx); err != nil {
			return processResult{}, err
		}
	}
}

func killRemote(ctx context.Context, run runner.Runner, id string) error {
	killCtx, cancel := context.WithTimeout(context.WithoutCancel(ctx), time.Second)
	defer cancel()
	if err := run.Signal(killCtx, id, runner.SignalKill); err != nil {
		return fmt.Errorf("web_fetch: kill subprocess: %w", err)
	}
	return nil
}

func waitRemote(ctx context.Context) error {
	timer := time.NewTimer(5 * time.Millisecond)
	defer timer.Stop()
	select {
	case <-ctx.Done():
		return fmt.Errorf("web_fetch: wait for subprocess: %w", context.Cause(ctx))
	case <-timer.C:
		return nil
	}
}

func Main(argv []string, stdout io.Writer) int {
	if len(argv) != 2 {
		_, _ = fmt.Fprint(stdout, "usage: __fetch <url> <max-bytes>")
		return 2
	}
	limit, err := positiveBytes(argv[1])
	if err != nil {
		_, _ = fmt.Fprint(stdout, err)
		return 2
	}
	args := arguments{url: argv[0], maxBytes: limit}
	if validationErr := validateURL(args.url, publicAddrs); validationErr != nil {
		_, _ = fmt.Fprint(stdout, validationErr)
		return 2
	}
	result, err := execute(context.Background(), args, newHTTPClient())
	if err != nil {
		_, _ = fmt.Fprint(stdout, err)
		return 1
	}
	if _, err := stdout.Write(result.Output); err != nil {
		return 1
	}
	if !result.OK {
		return 1
	}
	return 0
}

func parseArgs(raw jsontext.Value, allow func(net.IP) bool) (arguments, error) {
	decoder := jsontext.NewDecoder(bytes.NewReader(raw))
	start, err := decoder.ReadToken()
	if err != nil {
		return arguments{}, fmt.Errorf("web_fetch: read arguments: %w", err)
	}
	if start.Kind() != '{' {
		return arguments{}, errors.New("web_fetch: arguments must be an object")
	}
	args := arguments{maxBytes: defaultMaxBytes}
	for decoder.PeekKind() != '}' {
		name, err := decoder.ReadToken()
		if err != nil {
			return arguments{}, fmt.Errorf("web_fetch: read argument name: %w", err)
		}
		if err := parseArgument(decoder, name.String(), &args); err != nil {
			return arguments{}, err
		}
	}
	if _, err := decoder.ReadToken(); err != nil {
		return arguments{}, fmt.Errorf("web_fetch: read argument end: %w", err)
	}
	if err := validateURL(args.url, allow); err != nil {
		return arguments{}, err
	}
	return args, nil
}

func parseArgument(decoder *jsontext.Decoder, name string, args *arguments) error {
	value, err := decoder.ReadValue()
	if err != nil {
		return fmt.Errorf("web_fetch: read argument %s: %w", name, err)
	}
	switch name {
	case "url":
		text, err := jsonString(value)
		if err != nil {
			return fmt.Errorf("web_fetch: invalid url: %w", err)
		}
		args.url = text
		return nil
	case "max_bytes":
		limit, err := positiveBytes(string(bytes.TrimSpace(value)))
		if err != nil {
			return fmt.Errorf("web_fetch: invalid max_bytes: %w", err)
		}
		args.maxBytes = limit
		return nil
	default:
		return nil
	}
}

func jsonString(value []byte) (string, error) {
	decoder := jsontext.NewDecoder(bytes.NewReader(value))
	token, err := decoder.ReadToken()
	if err != nil {
		return "", fmt.Errorf("read string: %w", err)
	}
	if token.Kind() != '"' {
		return "", errors.New("expected string")
	}
	return token.String(), nil
}

func positiveBytes(raw string) (int64, error) {
	limit, err := strconv.ParseInt(raw, 10, 64)
	if err != nil {
		return 0, fmt.Errorf("web_fetch: max_bytes must be an integer: %w", err)
	}
	if limit <= 0 || limit == int64(^uint64(0)>>1) {
		return 0, errors.New("web_fetch: max_bytes must be positive and less than the maximum signed integer")
	}
	if limit > 16<<20 {
		return 0, errors.New("web_fetch: max_bytes must be at most 16777216")
	}
	return limit, nil
}

func validateURL(raw string, allow func(net.IP) bool) error {
	parsed, err := url.ParseRequestURI(raw)
	if err != nil {
		return fmt.Errorf("web_fetch: invalid URL: %w", err)
	}
	if parsed.Scheme != "http" && parsed.Scheme != "https" || parsed.Host == "" {
		return errors.New("web_fetch: URL must use http or https and include a host")
	}
	if ip := hostIP(parsed.Hostname()); ip != nil && !allow(ip) {
		return fmt.Errorf("web_fetch: URL host %q is blocked", parsed.Hostname())
	}
	return nil
}

var _ tool.Tool = (*Tool)(nil)
