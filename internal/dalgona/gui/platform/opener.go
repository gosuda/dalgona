package platform

import (
	"errors"
	"fmt"
	"net/url"
	"path/filepath"
	"slices"
	"strings"
)

var (
	ErrOpenRefused   = errors.New("the link uses a scheme this action does not open; copy the link instead")
	ErrOpenCancelled = errors.New("the system did not open the item because the choice was cancelled")
	ErrNotAbsolute   = errors.New("the path is not absolute; open an absolute local path")
)

var linkSchemes = []string{"https", "http", "mailto"}

func checkOpenURL(rawURL string, schemes []string) (string, error) {
	if strings.ContainsFunc(rawURL, isURLControl) {
		return "", ErrOpenRefused
	}
	u, err := url.Parse(rawURL)
	if err != nil || u.Scheme == "" || strings.EqualFold(u.Scheme, "file") {
		return "", ErrOpenRefused
	}
	if !slices.ContainsFunc(schemes, func(s string) bool { return strings.EqualFold(s, u.Scheme) }) {
		return "", ErrOpenRefused
	}
	if webScheme(u.Scheme) && u.Host == "" {
		return "", ErrOpenRefused
	}
	return rawURL, nil
}

func checkOpenPath(path string) error {
	if !filepath.IsAbs(path) {
		return fmt.Errorf("%w: %q", ErrNotAbsolute, path)
	}
	if strings.IndexByte(path, 0) >= 0 {
		return fmt.Errorf("%w: the path contains NUL", ErrNotAbsolute)
	}
	return nil
}

func isURLControl(r rune) bool {
	return r < 0x20 || r == 0x7f || r == ' '
}

func webScheme(scheme string) bool {
	return strings.EqualFold(scheme, "http") || strings.EqualFold(scheme, "https")
}
