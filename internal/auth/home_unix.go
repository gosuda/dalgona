//go:build !windows

package auth

import "os"

func ProtectHomeTree(dir string) error {
	return os.MkdirAll(dir, 0o700)
}

func ProtectHomeDir(dir string) error {
	return os.MkdirAll(dir, 0o700)
}

func CheckHomeDir(string) error {
	return nil
}
