package runner

import (
	"fmt"
	"path/filepath"
	"slices"
	"strings"
	"unicode"
)

var seatbeltEscaper = strings.NewReplacer(`\`, `\\`, `"`, `\"`)

func seatbeltProfile(roots, denied []string) string {
	var profile strings.Builder
	profile.WriteString("(version 1) (allow default)")
	for _, socket := range denied {
		profile.WriteString(` (deny network-outbound (remote unix-socket (path-literal "`)
		profile.WriteString(seatbeltEscaper.Replace(socket))
		profile.WriteString(`")))`)
	}
	profile.WriteString(" (deny file-write*)")
	for _, root := range roots {
		profile.WriteString(` (allow file-write* (subpath "`)
		profile.WriteString(seatbeltEscaper.Replace(root))
		profile.WriteString(`"))`)
	}
	profile.WriteString(` (allow file-write* (literal "/dev/null") (literal "/dev/zero") (regex #"^/dev/tty") (regex #"^/dev/fd/"))`)
	return profile.String()
}

func deniedSocketSpellings(sockets []string, resolve func(string) (string, error)) ([]string, error) {
	denied := make([]string, 0, len(sockets)*3)
	for _, socket := range sockets {
		if socket == "" {
			continue
		}
		spellings, err := socketSpellings(socket, resolve)
		if err != nil {
			return nil, err
		}
		denied = append(denied, spellings...)
	}
	slices.Sort(denied)
	return slices.Compact(denied), nil
}

func socketSpellings(socket string, resolve func(string) (string, error)) ([]string, error) {
	resolved, err := resolveSocket(socket, resolve)
	if err != nil {
		return nil, err
	}
	canonicalParent, err := resolve(filepath.Dir(resolved))
	if err != nil {
		return nil, fmt.Errorf("resolve denied socket parent %s: %w", filepath.Dir(resolved), err)
	}
	spellings := []string{filepath.Clean(socket), resolved, filepath.Join(canonicalParent, filepath.Base(resolved))}
	for _, spelling := range spellings {
		if strings.IndexFunc(spelling, unicode.IsControl) >= 0 {
			return nil, fmt.Errorf("denied socket path %q contains a control character", spelling)
		}
	}
	return spellings, nil
}

func resolveSocket(socket string, resolve func(string) (string, error)) (string, error) {
	resolved, err := resolve(socket)
	if err == nil {
		return resolved, nil
	}
	cleaned := filepath.Clean(socket)
	parent, err := resolve(filepath.Dir(cleaned))
	if err != nil {
		return "", fmt.Errorf("resolve denied socket %s: its parent directory does not resolve: %w", socket, err)
	}
	return filepath.Join(parent, filepath.Base(cleaned)), nil
}
