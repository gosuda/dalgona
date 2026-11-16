package chatgpt

import (
	"bytes"
	"cmp"
	"context"
	"errors"
	"fmt"
	"io"
	"math"
	"net/http"
	"net/url"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"time"

	json "encoding/json/v2"

	"github.com/gosuda/dalgona/internal/dal/auth/oauth"
	"github.com/gosuda/dalgona/internal/dal/model"
)

const (
	catalogFile   = "codex-models.json"
	modelsTimeout = 5 * time.Second
	modelsLimit   = 1 << 20
	refusalLimit  = 512
	logInterval   = time.Hour
	maxWhole      = 1<<31 - 1
)

var (
	errModelsTimeout = errors.New("models request timed out")
	errTooLarge      = errors.New("response exceeds the size limit")
	errNoCache       = errors.New("cache directory is not configured")
	knownEfforts     = []string{"low", "medium", "high", "xhigh", "max", "ultra"}
)

type identity struct {
	endpoint string
	account  string
}

type catalogModel struct {
	ID        string   `json:"id"`
	Context   int      `json:"context"`
	MaxOutput int      `json:"max_output"`
	Efforts   []string `json:"efforts"`
	Images    bool     `json:"images"`
}

type memoryCatalog struct {
	id     identity
	models []catalogModel
}

type catalogDoc struct {
	Endpoint  string         `json:"endpoint"`
	Account   string         `json:"account"`
	FetchedAt time.Time      `json:"fetched_at"`
	Models    []catalogModel `json:"models"`
}

type degradable struct{ err error }

func (d *degradable) Error() string { return d.err.Error() }

func (d *degradable) Unwrap() error { return d.err }

func degraded(format string, args ...any) error {
	return &degradable{err: fmt.Errorf(format, args...)}
}

type wireLevel struct {
	Effort string `json:"effort"`
}

type wireRow struct {
	Slug             string      `json:"slug"`
	Visibility       string      `json:"visibility"`
	Priority         *float64    `json:"priority"`
	ContextWindow    float64     `json:"context_window"`
	MaxContextWindow float64     `json:"max_context_window"`
	EffectivePercent float64     `json:"effective_context_window_percent"`
	MaxOutputTokens  float64     `json:"max_output_tokens"`
	Levels           []wireLevel `json:"supported_reasoning_levels"`
	Modalities       *[]string   `json:"input_modalities"`
}

type wireCatalog struct {
	Models *[]wireRow `json:"models"`
}

func (p *Provider) Models(ctx context.Context) ([]model.Model, error) {
	if err := ctx.Err(); err != nil {
		return nil, fmt.Errorf("chatgpt: models: %w", err)
	}
	token, err := loadToken(ctx, p.token)
	if err != nil {
		return nil, err
	}
	id := identity{endpoint: p.endpoint, account: token.Account}
	p.switchTo(id)
	version, err := p.clientVersion(ctx)
	if err != nil {
		return nil, err
	}
	catalog, err := p.fetch(ctx, token, version)
	if err != nil {
		return p.degrade(ctx, id, err)
	}
	p.remember(id, catalog)
	return p.models(catalog), nil
}

func (p *Provider) switchTo(id identity) {
	p.mu.Lock()
	defer p.mu.Unlock()
	if p.memory != nil && p.memory.id != id {
		p.memory = nil
	}
}

func (p *Provider) remember(id identity, catalog []catalogModel) {
	p.mu.Lock()
	p.memory = &memoryCatalog{id: id, models: catalog}
	p.mu.Unlock()
	doc := catalogDoc{Endpoint: id.endpoint, Account: id.account, FetchedAt: p.now(), Models: catalog}
	if err := p.writeCache(catalogFile, doc); err != nil {
		p.log.Debug("chatgpt: model catalog cache not written", "error", err)
	}
}

func (p *Provider) degrade(ctx context.Context, id identity, err error) ([]model.Model, error) {
	soft, isSoft := errors.AsType[*degradable](err)
	if ctx.Err() != nil || !isSoft {
		return nil, fmt.Errorf("chatgpt: %w", err)
	}
	catalog, source := p.fallback(id)
	if p.logDue(id) {
		p.log.Warn("chatgpt: live model list unavailable: " + soft.Error() + "; using " + source)
	}
	return p.models(catalog), nil
}

func (p *Provider) fallback(id identity) ([]catalogModel, string) {
	p.mu.Lock()
	memory := p.memory
	p.mu.Unlock()
	if memory != nil && memory.id == id {
		return memory.models, "cache"
	}
	if stored := p.storedCatalog(id); stored != nil {
		return stored, "cache"
	}
	return staticCatalog(), "static list"
}

func (p *Provider) storedCatalog(id identity) []catalogModel {
	var doc catalogDoc
	if err := p.readCache(catalogFile, &doc); err != nil {
		return nil
	}
	if doc.Endpoint != id.endpoint || doc.Account != id.account || len(doc.Models) == 0 {
		return nil
	}
	if slices.ContainsFunc(doc.Models, func(item catalogModel) bool { return item.ID == "" }) {
		return nil
	}
	return doc.Models
}

func (p *Provider) logDue(id identity) bool {
	p.mu.Lock()
	defer p.mu.Unlock()
	now := p.now()
	if last, logged := p.logged[id]; logged && now.Sub(last) < logInterval {
		return false
	}
	p.logged[id] = now
	return true
}

func staticCatalog() []catalogModel {
	catalog := make([]catalogModel, len(staticModels))
	for index, item := range staticModels {
		catalog[index] = catalogModel{ID: item.id, Context: item.context, Efforts: item.efforts, Images: item.images}
	}
	return catalog
}

func (p *Provider) models(catalog []catalogModel) []model.Model {
	models := make([]model.Model, len(catalog))
	for index, item := range catalog {
		models[index] = model.Model{
			ID:         item.ID,
			Context:    model.ContextWindow(item.ID, item.Context, p.contextWindows),
			MaxOutput:  item.MaxOutput,
			Efforts:    slices.Clone(item.Efforts),
			ToolChoice: true,
			Images:     item.Images,
		}
	}
	return models
}

func (p *Provider) fetch(ctx context.Context, token oauth.Token, version string) ([]catalogModel, error) {
	ctx, cancel := context.WithTimeoutCause(ctx, modelsTimeout, errModelsTimeout)
	defer cancel()
	target := strings.TrimRight(p.baseURL, "/") + "/models?client_version=" + url.QueryEscape(version)
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, target, nil)
	if err != nil {
		return nil, fmt.Errorf("create models request: %w", err)
	}
	resp, err := p.accountClient(token).Do(req)
	if err != nil {
		return nil, degraded("models request: %w", timeoutCause(ctx, err, errModelsTimeout))
	}
	defer resp.Body.Close()
	return readCatalog(resp)
}

func timeoutCause(ctx context.Context, err, timeout error) error {
	if errors.Is(context.Cause(ctx), timeout) {
		return timeout
	}
	return err
}

func (p *Provider) accountClient(token oauth.Token) *http.Client {
	client := *p.plain
	client.Transport = accountTransport{
		next:  cmp.Or(p.plain.Transport, http.DefaultTransport),
		token: func(context.Context) (oauth.Token, error) { return token, nil },
	}
	client.CheckRedirect = func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse }
	return &client
}

func readCatalog(resp *http.Response) ([]catalogModel, error) {
	if resp.StatusCode >= http.StatusInternalServerError {
		return nil, degraded("models status %d", resp.StatusCode)
	}
	if resp.StatusCode < http.StatusOK || resp.StatusCode >= http.StatusMultipleChoices {
		return nil, refusal(resp)
	}
	data, err := readLimited(resp.Body, modelsLimit)
	if err != nil {
		return nil, degraded("read models response: %w", err)
	}
	return parseCatalog(data)
}

func refusal(resp *http.Response) error {
	body, err := io.ReadAll(io.LimitReader(resp.Body, refusalLimit))
	detail := string(bytes.TrimSpace(body))
	if err != nil {
		detail = "response body unreadable: " + err.Error()
	}
	return &model.HTTPError{
		Status:     resp.StatusCode,
		RetryAfter: model.RetryAfter(resp.Header.Get("Retry-After")),
		Err:        fmt.Errorf("models http %d: %s", resp.StatusCode, detail),
	}
}

func parseCatalog(data []byte) ([]catalogModel, error) {
	var doc wireCatalog
	if err := json.Unmarshal(data, &doc); err != nil {
		return nil, degraded("decode models response: %w", err)
	}
	if doc.Models == nil {
		return nil, degraded("models response has no models member")
	}
	rows := visibleRows(*doc.Models)
	if len(rows) == 0 {
		return nil, degraded("models response lists no listable model")
	}
	catalog := make([]catalogModel, len(rows))
	for index, row := range rows {
		catalog[index] = row.catalog()
	}
	return catalog, nil
}

func visibleRows(all []wireRow) []wireRow {
	seen := make(map[string]bool, len(all))
	rows := make([]wireRow, 0, len(all))
	for _, row := range all {
		if row.Visibility != "list" || row.Slug == "" || seen[row.Slug] {
			continue
		}
		seen[row.Slug] = true
		rows = append(rows, row)
	}
	slices.SortStableFunc(rows, func(left, right wireRow) int { return cmp.Compare(left.rank(), right.rank()) })
	return rows
}

func (r wireRow) rank() float64 {
	if r.Priority == nil {
		return math.Inf(1)
	}
	return *r.Priority
}

func (r wireRow) catalog() catalogModel {
	return catalogModel{
		ID:        r.Slug,
		Context:   r.window(),
		MaxOutput: wholeNumber(r.MaxOutputTokens),
		Efforts:   r.efforts(),
		Images:    r.Modalities == nil || slices.Contains(*r.Modalities, "image"),
	}
}

func (r wireRow) window() int {
	window := cmp.Or(wholeNumber(r.ContextWindow), wholeNumber(r.MaxContextWindow))
	percent := wholeNumber(r.EffectivePercent)
	if percent < 1 || percent > 100 {
		return window
	}
	return window/100*percent + window%100*percent/100
}

func (r wireRow) efforts() []string {
	var efforts []string
	for _, level := range r.Levels {
		if slices.Contains(knownEfforts, level.Effort) && !slices.Contains(efforts, level.Effort) {
			efforts = append(efforts, level.Effort)
		}
	}
	return efforts
}

func wholeNumber(value float64) int {
	if value < 1 || value > maxWhole {
		return 0
	}
	return int(value)
}

func canonicalEndpoint(base string) string {
	parsed, err := url.Parse(base)
	if err != nil || parsed.Host == "" {
		return strings.TrimRight(base, "/")
	}
	scheme := strings.ToLower(parsed.Scheme)
	host := strings.ToLower(parsed.Host)
	switch scheme {
	case "https":
		host = strings.TrimSuffix(host, ":443")
	case "http":
		host = strings.TrimSuffix(host, ":80")
	}
	return scheme + "://" + host + strings.TrimRight(parsed.EscapedPath(), "/")
}

func readLimited(source io.Reader, limit int64) ([]byte, error) {
	data, err := io.ReadAll(io.LimitReader(source, limit+1))
	if err != nil {
		return nil, fmt.Errorf("read: %w", err)
	}
	if int64(len(data)) > limit {
		return nil, errTooLarge
	}
	return data, nil
}

func (p *Provider) readCache(name string, into any) error {
	if p.cacheDir == "" {
		return errNoCache
	}
	file, err := os.Open(filepath.Join(p.cacheDir, name))
	if err != nil {
		return fmt.Errorf("open cache %s: %w", name, err)
	}
	defer file.Close()
	data, err := readLimited(file, modelsLimit)
	if err != nil {
		return fmt.Errorf("read cache %s: %w", name, err)
	}
	if err = json.Unmarshal(data, into); err != nil {
		return fmt.Errorf("decode cache %s: %w", name, err)
	}
	return nil
}

func (p *Provider) writeCache(name string, value any) error {
	if p.cacheDir == "" {
		return nil
	}
	data, err := json.Marshal(value)
	if err != nil {
		return fmt.Errorf("encode cache %s: %w", name, err)
	}
	if err = os.MkdirAll(p.cacheDir, 0o700); err != nil {
		return fmt.Errorf("create cache directory: %w", err)
	}
	temp, err := os.CreateTemp(p.cacheDir, ".cache-*")
	if err != nil {
		return fmt.Errorf("create cache file: %w", err)
	}
	if err = fillCache(temp, data); err != nil {
		return errors.Join(err, os.Remove(temp.Name()))
	}
	if err = os.Rename(temp.Name(), filepath.Join(p.cacheDir, name)); err != nil {
		return errors.Join(fmt.Errorf("replace cache %s: %w", name, err), os.Remove(temp.Name()))
	}
	return nil
}

func fillCache(temp *os.File, data []byte) error {
	if err := temp.Chmod(0o600); err != nil {
		return errors.Join(fmt.Errorf("secure cache file: %w", err), temp.Close())
	}
	if _, err := temp.Write(data); err != nil {
		return errors.Join(fmt.Errorf("write cache file: %w", err), temp.Close())
	}
	if err := temp.Close(); err != nil {
		return fmt.Errorf("close cache file: %w", err)
	}
	return nil
}
