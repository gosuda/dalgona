package chatgpt

import (
	"context"
	"errors"
	"fmt"
	"net/http"
	"regexp"
	"strings"
	"time"

	json "encoding/json/v2"
)

const (
	versionURL    = "https://registry.npmjs.org/@openai%2Fcodex/latest"
	versionFile   = "codex-client-version.json"
	versionTTL    = 4 * time.Hour
	lookupTimeout = 5 * time.Second
	lookupLimit   = 1 << 20
)

var (
	releasePattern   = regexp.MustCompile(`^\d+\.\d+\.\d+$`)
	errLookupTimeout = errors.New("registry lookup timed out")
)

type versionEntry struct {
	Version   string    `json:"version"`
	ETag      string    `json:"etag"`
	CheckedAt time.Time `json:"checked_at"`
}

func releasedVersion(version string) bool {
	return releasePattern.MatchString(version) && strings.Trim(version, "0.") != ""
}

func clip(text string, limit int) string {
	if len(text) <= limit {
		return text
	}
	return text[:limit] + "..."
}

func (p *Provider) clientVersion(ctx context.Context) (string, error) {
	cached := p.cachedVersion()
	if cached != nil && p.fresh(cached.CheckedAt) {
		return cached.Version, nil
	}
	entry, err := p.lookupVersion(ctx, cached)
	switch {
	case err == nil:
		if writeErr := p.writeCache(versionFile, entry); writeErr != nil {
			p.log.Debug("chatgpt: client version cache not written", "error", writeErr)
		}
		return entry.Version, nil
	case cached != nil:
		return cached.Version, nil
	default:
		return "", fmt.Errorf("chatgpt: client version is unavailable: %w", err)
	}
}

func (p *Provider) fresh(checked time.Time) bool {
	age := p.now().Sub(checked)
	return age >= 0 && age < versionTTL
}

func (p *Provider) cachedVersion() *versionEntry {
	var entry versionEntry
	if err := p.readCache(versionFile, &entry); err != nil || !releasedVersion(entry.Version) {
		return nil
	}
	return &entry
}

func (p *Provider) lookupVersion(ctx context.Context, cached *versionEntry) (versionEntry, error) {
	ctx, cancel := context.WithTimeoutCause(ctx, lookupTimeout, errLookupTimeout)
	defer cancel()
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, versionURL, nil)
	if err != nil {
		return versionEntry{}, fmt.Errorf("create registry request: %w", err)
	}
	if cached != nil && cached.ETag != "" {
		req.Header.Set("If-None-Match", cached.ETag)
	}
	resp, err := p.plain.Do(req)
	if err != nil {
		return versionEntry{}, fmt.Errorf("registry request: %w", timeoutCause(ctx, err, errLookupTimeout))
	}
	defer resp.Body.Close()
	return p.versionFrom(resp, cached)
}

func (p *Provider) versionFrom(resp *http.Response, cached *versionEntry) (versionEntry, error) {
	switch {
	case resp.StatusCode == http.StatusNotModified && cached != nil:
		renewed := *cached
		renewed.CheckedAt = p.now()
		return renewed, nil
	case resp.StatusCode == http.StatusNotModified:
		return versionEntry{}, errors.New("registry answered not modified without a cached version")
	case resp.StatusCode != http.StatusOK:
		return versionEntry{}, fmt.Errorf("registry answered status %d", resp.StatusCode)
	}
	data, err := readLimited(resp.Body, lookupLimit)
	if err != nil {
		return versionEntry{}, fmt.Errorf("read registry response: %w", err)
	}
	var payload struct {
		Version string `json:"version"`
	}
	err = json.Unmarshal(data, &payload)
	if err != nil {
		return versionEntry{}, fmt.Errorf("decode registry response: %w", err)
	}
	if !releasedVersion(payload.Version) {
		return versionEntry{}, fmt.Errorf("registry version %q is not a released version", clip(payload.Version, 32))
	}
	return versionEntry{Version: payload.Version, ETag: resp.Header.Get("ETag"), CheckedAt: p.now()}, nil
}
