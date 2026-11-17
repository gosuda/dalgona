package github

import (
	"net/http"
	"strconv"
	"strings"
	"time"
)

type rateState struct {
	secondaryWaits int
}

func requestID(resp *http.Response) string {
	return resp.Header.Get("X-GitHub-Request-Id")
}

func parseResponseMeta(resp *http.Response, now time.Time) ResponseMeta {
	meta := ResponseMeta{
		Status:      resp.StatusCode,
		ETag:        resp.Header.Get("ETag"),
		NextURL:     nextLink(resp.Header.Values("Link")),
		NotModified: resp.StatusCode == http.StatusNotModified,
	}
	header := resp.Header
	if limit, ok := headerInt(header, "X-RateLimit-Limit"); ok {
		meta.Limit = limit
	}
	if remaining, ok := headerInt(header, "X-RateLimit-Remaining"); ok {
		meta.Remaining = remaining
	}
	if used, ok := headerInt(header, "X-RateLimit-Used"); ok {
		meta.Used = used
	}
	if reset, ok := headerInt64(header, "X-RateLimit-Reset"); ok && reset > 0 {
		meta.Reset = time.Unix(reset, 0).UTC()
	}
	meta.Resource = header.Get("X-RateLimit-Resource")
	if raw := header.Get("Retry-After"); raw != "" {
		meta.RetryAfter = parseRetryAfter(raw, now)
	}
	meta.PollInterval = parsePollInterval(header.Get("X-Poll-Interval"))
	return meta
}

func headerInt(header http.Header, key string) (int, bool) {
	value, err := strconv.Atoi(header.Get(key))
	if err != nil {
		return 0, false
	}
	return value, true
}

func headerInt64(header http.Header, key string) (int64, bool) {
	value, err := strconv.ParseInt(header.Get(key), 10, 64)
	if err != nil {
		return 0, false
	}
	return value, true
}

func parsePollInterval(raw string) time.Duration {
	if raw == "" {
		return 0
	}
	seconds, err := strconv.ParseFloat(raw, 64)
	if err != nil || seconds < 0 {
		return 0
	}
	return time.Duration(seconds * float64(time.Second))
}

func parseRetryAfter(raw string, now time.Time) time.Time {
	raw = strings.TrimSpace(raw)
	if seconds, err := strconv.ParseInt(raw, 10, 64); err == nil {
		return now.Add(time.Duration(seconds) * time.Second)
	}
	if t, err := http.ParseTime(raw); err == nil {
		return t
	}
	return time.Time{}
}

func nextLink(links []string) string {
	for _, header := range links {
		for _, part := range strings.Split(header, ",") {
			part = strings.TrimSpace(part)
			if !strings.Contains(part, `rel="next"`) {
				continue
			}
			start := strings.Index(part, "<")
			end := strings.Index(part, ">")
			if start >= 0 && end > start {
				return part[start+1 : end]
			}
		}
	}
	return ""
}

func isRateLimited(status int, meta ResponseMeta, body []byte, now time.Time) bool {
	if status != http.StatusForbidden && status != http.StatusTooManyRequests {
		return false
	}
	if !meta.RetryAfter.IsZero() && meta.RetryAfter.After(now) {
		return true
	}
	if status == http.StatusTooManyRequests {
		return true
	}
	if meta.Limit > 0 && meta.Remaining == 0 {
		return true
	}
	text := strings.ToLower(string(body))
	return isSecondaryLimitText(text)
}

func isSecondaryLimitText(text string) bool {
	return strings.Contains(text, "secondary rate limit") ||
		strings.Contains(text, "rate limit exceeded") ||
		strings.Contains(text, "abuse detection")
}

func rateWait(meta ResponseMeta, state *rateState, now time.Time) time.Duration {
	wait := time.Duration(0)
	if !meta.RetryAfter.IsZero() {
		if candidate := meta.RetryAfter.Sub(now); candidate > wait {
			wait = candidate
		}
	}
	if !meta.Reset.IsZero() {
		if candidate := meta.Reset.Add(time.Second).Sub(now); candidate > wait {
			wait = candidate
		}
	}
	if wait > 0 {
		return wait
	}
	if state.secondaryWaits < 3 {
		state.secondaryWaits++
	}
	return [...]time.Duration{60 * time.Second, 120 * time.Second, 240 * time.Second}[min(state.secondaryWaits, 3)-1]
}
