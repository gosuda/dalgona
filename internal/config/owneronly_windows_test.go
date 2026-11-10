//go:build windows

package config_test

import (
	"path/filepath"
	"testing"

	"github.com/gosuda/dalgona/internal/auth"
)

func assertOwnerOnly(t *testing.T, path string) {
	t.Helper()
	if err := auth.CheckHomeDir(filepath.Dir(path)); err != nil {
		t.Fatalf("home: %v", err)
	}
}
