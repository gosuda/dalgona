package report

import (
	"regexp"
	"strings"
	"unicode/utf8"
)

const (
	markerPrefix  = "dalgona-failure:v1:"
	maxTitleBytes = 240
)

var titlePattern = regexp.MustCompile(`^\[dalgona-failure:v1:([0-9a-f]{64})\] `)

func FingerprintFromTitle(title string) (string, bool) {
	if !utf8.ValidString(title) || len(title) > maxTitleBytes {
		return "", false
	}
	parts := titlePattern.FindStringSubmatch(title)
	if parts == nil || len(title) <= len(parts[0]) {
		return "", false
	}
	return parts[1], true
}

func validFingerprint(fingerprint string) bool {
	if len(fingerprint) != 64 {
		return false
	}
	for i := 0; i < len(fingerprint); i++ {
		c := fingerprint[i]
		if (c < '0' || c > '9') && (c < 'a' || c > 'f') {
			return false
		}
	}
	return true
}

func splitReportText(text string) (string, string, bool) {
	rest, ok := strings.CutPrefix(text, "Title: ")
	if !ok {
		return "", "", false
	}
	title, body, ok := strings.Cut(rest, "\n\n")
	if !ok || title == "" || body == "" {
		return "", "", false
	}
	return title, body, true
}
