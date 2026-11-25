package platform

import (
	"errors"
	"fmt"
	"net/url"
	"strings"
)

var (
	ErrNotLocalFile      = errors.New("the selection is not a local file; choose a file on this computer")
	ErrPortalUnavailable = errors.New("no XDG desktop portal answered; install xdg-desktop-portal with a desktop backend such as xdg-desktop-portal-gnome, then try again")
)

type DialogKind uint8

const (
	DialogOpenFiles DialogKind = iota + 1
	DialogChooseFolder
	DialogSaveFile
)

type FileFilter struct {
	Name       string
	Extensions []string
}

type DialogRequest struct {
	Kind          DialogKind
	Title         string
	Filters       []FileFilter
	SuggestedName string
}

type DialogResult struct {
	Cancelled bool
	Files     []string
}

func (r DialogRequest) Check() error {
	switch {
	case r.Kind < DialogOpenFiles || r.Kind > DialogSaveFile:
		return errors.New("dialog: the kind is not open files, choose folder, or save file")
	case r.Title == "":
		return errors.New("dialog: name the action in the title")
	case r.Kind == DialogChooseFolder && len(r.Filters) > 0:
		return errors.New("dialog: a folder choice takes no file filters")
	case (r.Kind == DialogSaveFile) != (r.SuggestedName != ""):
		return errors.New("dialog: only a save dialog suggests a file name, and it always does")
	case strings.ContainsAny(r.SuggestedName, `/\`+"\x00"):
		return errors.New("dialog: the suggested name contains a path separator or NUL")
	}
	return checkFilters(r.Filters)
}

func checkFilters(filters []FileFilter) error {
	for _, f := range filters {
		if f.Name == "" || len(f.Extensions) == 0 {
			return errors.New("dialog: each filter needs a name and at least one extension")
		}
		for _, ext := range f.Extensions {
			if ext == "" || strings.ContainsAny(ext, `./\*`+"\x00") {
				return fmt.Errorf("dialog: extension %q must be a bare suffix such as png", ext)
			}
		}
	}
	return nil
}

func LocalFile(uri string) (string, error) {
	u, err := url.Parse(uri)
	if err != nil || !localAuthority(u) || !plainURL(u) || !unambiguousPath(u) {
		return "", ErrNotLocalFile
	}
	return localPath(u.Path)
}

func localAuthority(u *url.URL) bool {
	host := u.Host == "" || strings.EqualFold(u.Host, "localhost")
	return strings.EqualFold(u.Scheme, "file") && u.User == nil && host
}

func plainURL(u *url.URL) bool {
	return u.Opaque == "" && u.RawQuery == "" && !u.ForceQuery && u.Fragment == ""
}

func unambiguousPath(u *url.URL) bool {
	escapedSlash := strings.Contains(strings.ToLower(u.EscapedPath()), "%2f")
	return strings.HasPrefix(u.Path, "/") && !escapedSlash && !strings.ContainsRune(u.Path, 0)
}

func ParseURIList(list string) ([]string, error) {
	var paths []string
	for line := range strings.Lines(list) {
		line = strings.TrimSuffix(strings.TrimSuffix(line, "\n"), "\r")
		if line == "" || strings.HasPrefix(line, "#") {
			continue
		}
		path, err := LocalFile(line)
		if err != nil {
			return nil, err
		}
		paths = append(paths, path)
	}
	if len(paths) == 0 {
		return nil, ErrNotLocalFile
	}
	return paths, nil
}
