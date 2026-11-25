//go:build windows

package platform

import "path/filepath"

func localPath(path string) (string, error) {
	if len(path) < 4 || path[0] != '/' || path[2] != ':' || path[3] != '/' || !isDriveLetter(path[1]) {
		return "", ErrNotLocalFile
	}
	return filepath.FromSlash(path[1:]), nil
}

func isDriveLetter(b byte) bool {
	return 'a' <= b && b <= 'z' || 'A' <= b && b <= 'Z'
}
