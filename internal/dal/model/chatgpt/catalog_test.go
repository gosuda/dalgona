package chatgpt

import (
	"bytes"
	"context"
	"errors"
	"net/http"
	"os"
	"path/filepath"
	"reflect"
	"runtime"
	"slices"
	"strconv"
	"strings"
	"testing"
	"time"

	json "encoding/json/v2"

	"github.com/gosuda/dalgona/internal/dal/auth/oauth"
	"github.com/gosuda/dalgona/internal/dal/model"
)

type catalogOnDisk struct {
	Endpoint string `json:"endpoint"`
	Account  string `json:"account"`
	Models   []struct {
		ID      string `json:"id"`
		Context int    `json:"context"`
	} `json:"models"`
}

func readCatalogFile(t *testing.T, dir string) (catalogOnDisk, []byte) {
	t.Helper()
	raw, err := os.ReadFile(filepath.Join(dir, "codex-models.json"))
	if err != nil {
		t.Fatal(err)
	}
	var doc catalogOnDisk
	err = json.Unmarshal(raw, &doc)
	if err != nil {
		t.Fatalf("decode catalog file: %v: %s", err, raw)
	}
	return doc, raw
}

func checkDirectory(t *testing.T, dir string, want []string) {
	t.Helper()
	entries, err := os.ReadDir(dir)
	if err != nil {
		t.Fatal(err)
	}
	var names []string
	for _, entry := range entries {
		names = append(names, entry.Name())
		if runtime.GOOS == "windows" {
			continue
		}
		info, infoErr := entry.Info()
		if infoErr != nil {
			t.Fatal(infoErr)
		}
		if info.Mode().Perm() != 0o600 {
			t.Errorf("%s has mode %v, want 0600", entry.Name(), info.Mode().Perm())
		}
	}
	if !slices.Equal(names, want) {
		t.Fatalf("cache directory holds %v, want %v", names, want)
	}
}

func TestChatGPTModelsLive(t *testing.T) {
	registry, models := okRegistry("0.153.0"), okModels()
	s := newSite(t, registry, models)
	dir := t.TempDir()
	account := "acct-A"
	got, err := New(s.config(dir, &account)).Models(t.Context())
	if err != nil {
		t.Fatal(err)
	}
	want := []model.Model{
		{ID: "gpt-live-a", Context: 200000, MaxOutput: 32000, Efforts: []string{"medium", "ultra"}, ToolChoice: true, Images: true},
		{ID: "gpt-live-b", Context: 190000, Efforts: []string{"low", "high"}, ToolChoice: true},
	}
	if !reflect.DeepEqual(got, want) {
		t.Fatalf("models = %+v, want %+v", got, want)
	}
	npm, list := registry.view(), models.view()
	checkLiveRequestCounts(t, npm, list)
	checkCatalogFileContents(t, dir, s.url)
}

func checkLiveRequestCounts(t *testing.T, npm, list seen) {
	t.Helper()
	if npm.hits != 1 || npm.uri != "/registry/@openai%2Fcodex/latest" || npm.auth != "" {
		t.Fatalf("registry saw %+v, want one credential-free request for the scoped package", npm)
	}
	if list.hits != 1 || list.uri != "/backend-api/codex/models?client_version=0.153.0" {
		t.Fatalf("model list saw %+v, want one request that carries the npm version", list)
	}
	if list.auth != "Bearer access-acct-A" || list.account != "acct-A" {
		t.Fatalf("model list saw %+v, want the account headers", list)
	}
}

func checkCatalogFileContents(t *testing.T, dir, endpoint string) {
	t.Helper()
	doc, raw := readCatalogFile(t, dir)
	if doc.Endpoint != endpoint || doc.Account != "acct-A" || len(doc.Models) != 2 || doc.Models[0].ID != "gpt-live-a" || doc.Models[0].Context != 200000 {
		t.Fatalf("catalog file = %+v, want the endpoint, the account and the live rows", doc)
	}
	if bytes.Contains(raw, []byte("access-acct-A")) {
		t.Fatalf("catalog file holds the token: %s", raw)
	}
	checkDirectory(t, dir, []string{"codex-client-version.json", "codex-models.json"})
}

func TestChatGPTLiveFailureUsesStatic(t *testing.T) {
	cases := []struct {
		name   string
		change func(*backend)
		cause  string
	}{
		{"server error", func(b *backend) { b.status, b.body = http.StatusServiceUnavailable, "busy" }, "models status 503"},
		{"connection dropped", func(b *backend) { b.drop = true }, "models request"},
		{"undecodable body", func(b *backend) { b.body = "<html>portal</html>" }, "decode models response"},
		{"no models member", func(b *backend) { b.body = `{"data":[]}` }, "no models member"},
		{"nothing listable", func(b *backend) { b.body = `{"models":[{"slug":"x","visibility":"hide"}]}` }, "lists no listable model"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			models := okModels()
			models.update(tc.change)
			s := newSite(t, okRegistry("0.153.0"), models)
			book := &logbook{}
			account := "acct-A"
			cfg := s.config(t.TempDir(), &account)
			cfg.Logger = book.logger()
			expectIDs(t, New(cfg), "outage", staticIDs())
			if book.count("chatgpt: live model list unavailable: ") != 1 || book.count(tc.cause) != 1 || book.count("; using static list") != 1 {
				t.Fatalf("log = %q, want one line with cause %q and the static list", book.buf.String(), tc.cause)
			}
		})
	}
	t.Run("healthy backend is not replaced", func(t *testing.T) {
		s := newSite(t, okRegistry("0.153.0"), okModels())
		book := &logbook{}
		account := "acct-A"
		cfg := s.config(t.TempDir(), &account)
		cfg.Logger = book.logger()
		expectIDs(t, New(cfg), "live", liveIDs())
		if book.count("live model list unavailable") != 0 {
			t.Fatalf("log = %q, want no outage line", book.buf.String())
		}
	})
}

func TestStaticFallbackKeepsDonorValues(t *testing.T) {
	models := okModels()
	models.update(func(b *backend) { b.status = http.StatusServiceUnavailable })
	s := newSite(t, okRegistry("0.153.0"), models)
	account := "acct-A"
	cfg := s.config(t.TempDir(), &account)
	cfg.ContextWindows = map[string]int{"gpt-6-luna": 64000}
	got, err := New(cfg).Models(t.Context())
	if err != nil {
		t.Fatal(err)
	}
	byID := make(map[string]model.Model, len(got))
	for _, item := range got {
		if !item.ToolChoice || !item.Images {
			t.Fatalf("%s: tool choice %v, images %v, want both", item.ID, item.ToolChoice, item.Images)
		}
		byID[item.ID] = item
	}
	full := []string{"low", "medium", "high", "xhigh", "max", "ultra"}
	if !slices.Equal(byID["gpt-6.1-sol"].Efforts, full) {
		t.Fatalf("sol = %+v", byID["gpt-6.1-sol"])
	}
	checkStaticRow(t, byID["gpt-6.1-terra"], full, 500000)
	checkStaticRow(t, byID["gpt-6-luna"], full[:5], 64000)
	checkStaticRow(t, byID["gpt-5.5"], full[:4], 272000)
}

func checkStaticRow(t *testing.T, item model.Model, efforts []string, context int) {
	t.Helper()
	if item.Context != context || !slices.Equal(item.Efforts, efforts) {
		t.Fatalf("%s = %+v, want window %d and efforts %v", item.ID, item, context, efforts)
	}
}

func TestContextOverrideBeatsLiveWindow(t *testing.T) {
	s := newSite(t, okRegistry("0.153.0"), okModels())
	dir := t.TempDir()
	account := "acct-A"
	cfg := s.config(dir, &account)
	cfg.ContextWindows = map[string]int{"gpt-live-a": 64000}
	got, err := New(cfg).Models(t.Context())
	if err != nil || len(got) != 2 {
		t.Fatalf("models = %+v, err = %v", got, err)
	}
	if got[0].Context != 64000 || got[1].Context != 190000 {
		t.Fatalf("windows = %d and %d, want 64000 and 190000", got[0].Context, got[1].Context)
	}
	if doc, _ := readCatalogFile(t, dir); doc.Models[0].Context != 200000 {
		t.Fatalf("cached window = %d, want the reported 200000 so a later override change still applies", doc.Models[0].Context)
	}
}

func TestCatalogCacheIsAccountScoped(t *testing.T) {
	models := okModels()
	s := newSite(t, okRegistry("0.153.0"), models)
	dir := t.TempDir()
	account := "acct-A"
	cfg := s.config(dir, &account)
	provider := New(cfg)
	outage := func(b *backend) { b.status, b.body = http.StatusServiceUnavailable, "busy" }

	expectIDs(t, provider, "live for A", liveIDs())
	models.update(outage)
	expectIDs(t, provider, "A offline uses the memory", liveIDs())
	expectIDs(t, New(cfg), "A offline in a new process uses the file", liveIDs())

	account = "acct-B"
	expectIDs(t, provider, "B offline ignores A in memory", staticIDs())
	expectIDs(t, New(cfg), "B offline ignores A in the file", staticIDs())

	account = "acct-A"
	expectIDs(t, New(cfg), "A offline again, B wrote nothing", liveIDs())

	account = "acct-B"
	models.update(func(b *backend) {
		b.status, b.body = http.StatusOK, `{"models":[{"slug":"gpt-b-only","visibility":"list","priority":1}]}`
	})
	expectIDs(t, provider, "live for B", []string{"gpt-b-only"})
	models.update(outage)
	account = "acct-A"
	expectIDs(t, provider, "A offline after B replaced the file", staticIDs())
	account = "acct-B"
	expectIDs(t, provider, "B offline uses its own catalog", []string{"gpt-b-only"})
}

func TestCatalogCacheIgnoresForeignEndpointAndDamage(t *testing.T) {
	dir := t.TempDir()
	account := "acct-A"
	home := okModels()
	first := newSite(t, okRegistry("0.153.0"), home)
	expectIDs(t, New(first.config(dir, &account)), "live", liveIDs())

	down := okModels()
	down.update(func(b *backend) { b.status = http.StatusServiceUnavailable })
	other := newSite(t, okRegistry("0.153.0"), down)
	expectIDs(t, New(other.config(dir, &account)), "another endpoint", staticIDs())

	home.update(func(b *backend) { b.status = http.StatusServiceUnavailable })
	same := New(first.config(dir, &account))
	expectIDs(t, same, "same endpoint and account", liveIDs())

	path := filepath.Join(dir, "codex-models.json")
	damaged := map[string]string{
		"not json":     "{not json",
		"empty model":  `{"endpoint":"` + first.url + `","account":"acct-A","models":[{"id":""}]}`,
		"no models":    `{"endpoint":"` + first.url + `","account":"acct-A","models":[]}`,
		"wrong member": `{"endpoint":"` + first.url + `","account":"acct-A","models":"x"}`,
	}
	for name, content := range damaged {
		if err := os.WriteFile(path, []byte(content), 0o600); err != nil {
			t.Fatal(err)
		}
		expectIDs(t, New(first.config(dir, &account)), name, staticIDs())
	}
}

func TestCanonicalEndpoint(t *testing.T) {
	cases := []struct{ in, want string }{
		{"https://chatgpt.com/backend-api/codex", "https://chatgpt.com/backend-api/codex"},
		{"HTTPS://ChatGPT.COM:443/backend-api/codex/", "https://chatgpt.com/backend-api/codex"},
		{"https://user:secret@chatgpt.com/backend-api/codex?x=1#frag", "https://chatgpt.com/backend-api/codex"},
		{"http://127.0.0.1:8080/x", "http://127.0.0.1:8080/x"},
		{"http://127.0.0.1:80/x", "http://127.0.0.1/x"},
	}
	for _, tc := range cases {
		if got := canonicalEndpoint(tc.in); got != tc.want {
			t.Errorf("canonicalEndpoint(%q) = %q, want %q", tc.in, got, tc.want)
		}
	}
	if canonicalEndpoint("https://chatgpt.com/a") == canonicalEndpoint("https://chatgpt.com/b") {
		t.Error("different paths gave one endpoint")
	}
}

func TestRefusalIsReturnedOnceAndNeverRetried(t *testing.T) {
	for _, status := range []int{400, 401, 402, 403, 404, 429} {
		t.Run(strconv.Itoa(status), func(t *testing.T) {
			models := okModels()
			s := newSite(t, okRegistry("0.153.0"), models)
			book := &logbook{}
			account := "acct-A"
			cfg := s.config(t.TempDir(), &account)
			cfg.Logger = book.logger()
			provider := New(cfg)
			expectIDs(t, provider, "live", liveIDs())
			before := models.view().hits
			models.update(func(b *backend) { b.status, b.body = status, `{"error":{"code":"account_restricted"}}` })
			got, err := provider.Models(t.Context())
			failure, isHTTP := errors.AsType[*model.HTTPError](err)
			if err == nil || !isHTTP || failure.Status != status || got != nil {
				t.Fatalf("models = %+v, err = %v, want the %d refusal and no list", got, err, status)
			}
			if !strings.Contains(err.Error(), "account_restricted") {
				t.Fatalf("err = %v, want the provider's reason", err)
			}
			if after := models.view().hits; after != before+1 {
				t.Fatalf("model list was requested %d times, want 1", after-before)
			}
			if book.count("live model list unavailable") != 0 {
				t.Fatalf("log = %q, want no outage line for a refusal", book.buf.String())
			}
		})
	}
	t.Run("server error keeps the cache", func(t *testing.T) {
		models := okModels()
		s := newSite(t, okRegistry("0.153.0"), models)
		account := "acct-A"
		provider := New(s.config(t.TempDir(), &account))
		expectIDs(t, provider, "live", liveIDs())
		models.update(func(b *backend) { b.status = http.StatusBadGateway })
		expectIDs(t, provider, "gateway failure", liveIDs())
	})
}

func TestModelsRedirectIsNotFollowed(t *testing.T) {
	models := okModels()
	models.update(func(b *backend) {
		b.status, b.body, b.location = http.StatusFound, "", "https://evil.invalid/collect"
	})
	s := newSite(t, okRegistry("0.153.0"), models)
	account := "acct-A"
	_, err := New(s.config(t.TempDir(), &account)).Models(t.Context())
	failure, isHTTP := errors.AsType[*model.HTTPError](err)
	if !isHTTP || failure.Status != http.StatusFound {
		t.Fatalf("err = %v, want the 302 refusal", err)
	}
	if s.blocked.Load() != 0 {
		t.Fatal("the model list request followed a redirect with the account token")
	}
}

func TestUnavailableLogIsHourly(t *testing.T) {
	models := okModels()
	models.update(func(b *backend) { b.status, b.body = http.StatusServiceUnavailable, "busy" })
	s := newSite(t, okRegistry("0.153.0"), models)
	clk, book := newClock(), &logbook{}
	account := "acct-A"
	cfg := s.config(t.TempDir(), &account)
	cfg.Logger = book.logger()
	provider := New(cfg)
	provider.now = clk.Now
	steps := []struct {
		name    string
		who     string
		advance time.Duration
		want    int
	}{
		{"first outage logs", "acct-A", 0, 1},
		{"a minute later is quiet", "acct-A", time.Minute, 1},
		{"one second short of an hour is quiet", "acct-A", 58*time.Minute + 59*time.Second, 1},
		{"after an hour it logs again", "acct-A", time.Second, 2},
		{"another account logs for itself", "acct-B", time.Second, 3},
		{"the first account is quiet again", "acct-A", time.Second, 3},
	}
	for _, step := range steps {
		clk.advance(step.advance)
		account = step.who
		expectIDs(t, provider, step.name, staticIDs())
		if got := book.count("live model list unavailable"); got != step.want {
			t.Fatalf("%s: %d outage lines, want %d", step.name, got, step.want)
		}
	}
}

func TestModelsResponseCap(t *testing.T) {
	row := `{"slug":"gpt-cap","visibility":"list","priority":1}`
	cases := []struct {
		name string
		size int
		want []string
	}{
		{"exactly the limit", modelsLimit, []string{"gpt-cap"}},
		{"one byte over", modelsLimit + 1, staticIDs()},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			models := okModels()
			models.update(func(b *backend) { b.body = padded(tc.size, row) })
			s := newSite(t, okRegistry("0.153.0"), models)
			account := "acct-A"
			expectIDs(t, New(s.config(t.TempDir(), &account)), tc.name, tc.want)
		})
	}
}

func TestCatalogRowRules(t *testing.T) {
	order, err := parseCatalog([]byte(`{"models":[
	 {"slug":"none","visibility":"list"},
	 {"slug":"c","visibility":"list","priority":3},
	 {"slug":"a","visibility":"list","priority":1},
	 {"slug":"d","visibility":"list","priority":3}
	]}`))
	if err != nil {
		t.Fatal(err)
	}
	var slugs []string
	for _, item := range order {
		slugs = append(slugs, item.ID)
	}
	if want := []string{"a", "c", "d", "none"}; !slices.Equal(slugs, want) {
		t.Fatalf("order = %v, want %v: priority ascending, ties by position, no priority last", slugs, want)
	}

	checkWindowRows(t)
	checkModalityRows(t)

	provider := New(Config{ContextWindows: map[string]int{"y": 64000}})
	got := provider.models([]catalogModel{{ID: "x"}, {ID: "y"}})
	if got[0].Context != 128000 || got[1].Context != 64000 {
		t.Fatalf("windows = %d and %d, want the 128000 default and the 64000 override", got[0].Context, got[1].Context)
	}
}

func checkWindowRows(t *testing.T) {
	t.Helper()
	windows := []struct {
		name string
		row  string
		want int
	}{
		{"effective percent applies", `"context_window":272000,"effective_context_window_percent":95`, 258400},
		{"no percent keeps the window", `"context_window":272000`, 272000},
		{"max window when the window is null", `"context_window":null,"max_context_window":872000,"effective_context_window_percent":95`, 828400},
		{"percent zero is ignored", `"context_window":200000,"effective_context_window_percent":0`, 200000},
		{"percent over 100 is ignored", `"context_window":200000,"effective_context_window_percent":101`, 200000},
		{"percent 100 keeps the window", `"context_window":200000,"effective_context_window_percent":100`, 200000},
		{"percent one rounds down", `"context_window":250,"effective_context_window_percent":1`, 2},
		{"remainder rounds down", `"context_window":1001,"effective_context_window_percent":33`, 330},
		{"float spelling", `"context_window":272000.0`, 272000},
		{"no window", `"note":1`, 0},
	}
	for _, tc := range windows {
		catalog, parseErr := parseCatalog([]byte(`{"models":[{"slug":"x","visibility":"list",` + tc.row + `}]}`))
		if parseErr != nil || len(catalog) != 1 || catalog[0].Context != tc.want {
			t.Errorf("%s: catalog = %+v, err = %v, want window %d", tc.name, catalog, parseErr, tc.want)
		}
	}
}

func checkModalityRows(t *testing.T) {
	t.Helper()
	modalities := []struct {
		name string
		row  string
		want bool
	}{
		{"absent", ``, true},
		{"null", `,"input_modalities":null`, true},
		{"text and image", `,"input_modalities":["text","image"]`, true},
		{"text only", `,"input_modalities":["text"]`, false},
		{"empty", `,"input_modalities":[]`, false},
	}
	for _, tc := range modalities {
		catalog, parseErr := parseCatalog([]byte(`{"models":[{"slug":"x","visibility":"list"` + tc.row + `}]}`))
		if parseErr != nil || len(catalog) != 1 || catalog[0].Images != tc.want {
			t.Errorf("modalities %s: catalog = %+v, err = %v, want images %v", tc.name, catalog, parseErr, tc.want)
		}
	}
}

func TestModelsWithoutLoginSendNothing(t *testing.T) {
	registry, models := okRegistry("0.153.0"), okModels()
	s := newSite(t, registry, models)
	cases := []struct {
		name  string
		token func(context.Context) (oauth.Token, error)
		want  string
	}{
		{"no token source", nil, "not logged in"},
		{"no account", func(context.Context) (oauth.Token, error) { return oauth.Token{Access: "a"}, nil }, "token has no account id"},
	}
	for _, tc := range cases {
		cfg := s.config(t.TempDir(), new(string))
		cfg.Token = tc.token
		_, err := New(cfg).Models(t.Context())
		if err == nil || !strings.Contains(err.Error(), tc.want) {
			t.Errorf("%s: err = %v, want %q", tc.name, err, tc.want)
		}
	}
	if npm, list := registry.view().hits, models.view().hits; npm != 0 || list != 0 {
		t.Fatalf("requests = %d to the registry and %d to the model list, want none", npm, list)
	}
}

func TestModelsCancelledIsNotAnOutage(t *testing.T) {
	models := okModels()
	models.update(func(b *backend) { b.hang = true })
	s := newSite(t, okRegistry("0.153.0"), models)
	book := &logbook{}
	account := "acct-A"
	cfg := s.config(t.TempDir(), &account)
	cfg.Logger = book.logger()
	ctx, cancel := context.WithTimeout(t.Context(), 200*time.Millisecond)
	defer cancel()
	got, err := New(cfg).Models(ctx)
	if !errors.Is(err, context.DeadlineExceeded) || got != nil {
		t.Fatalf("models = %+v, err = %v, want the deadline error and no list", got, err)
	}
	if book.count("live model list unavailable") != 0 {
		t.Fatalf("log = %q, want no outage line when the caller gives up", book.buf.String())
	}
}

func TestBadBaseURLIsNotAnOutage(t *testing.T) {
	s := newSite(t, okRegistry("0.153.0"), okModels())
	dir := t.TempDir()
	seedVersion(t, dir, versionOnDisk{Version: "0.153.0", CheckedAt: time.Now()})
	account := "acct-A"
	cfg := s.config(dir, &account)
	cfg.BaseURL = "http://bad host/codex"
	got, err := New(cfg).Models(t.Context())
	if err == nil || !strings.Contains(err.Error(), "create models request") || got != nil {
		t.Fatalf("models = %+v, err = %v, want a request error and no static list", got, err)
	}
}
