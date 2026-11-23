package client

import (
	"fmt"
	"os"
	"path/filepath"

	"github.com/gosuda/dalgona/internal/dal/auth"
)

func SocketPath(home string) string { return filepath.Join(home, "dalgona.sock") }

func TightenHome(home string) error {
	return protectAndChmod(home, auth.ProtectHomeTree)
}

func tightenHomeRoot(home string) error {
	return protectAndChmod(home, auth.ProtectHomeDir)
}

func protectAndChmod(home string, protect func(string) error) error {
	if homeErr := protect(home); homeErr != nil {
		return fmt.Errorf("daemon home: %w", homeErr)
	}
	if chmodErr := os.Chmod(home, 0o700); chmodErr != nil { //nolint:gosec // 0700 is a directory permission, not a file
		return fmt.Errorf("daemon home permissions: %w", chmodErr)
	}
	return nil
}
