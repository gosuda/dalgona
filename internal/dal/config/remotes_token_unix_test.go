//go:build unix

package config_test

import (
	"os"
	"path/filepath"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/config"
	"golang.org/x/sys/unix"
)

func TestReadTokenFileRefusesSymlinkAndFIFO(t *testing.T) {
	t.Parallel()
	home := t.TempDir()
	writeConfigFile(t, home, "target", "relay-token\n")
	link := filepath.Join(home, "link")
	if err := os.Symlink(filepath.Join(home, "target"), link); err != nil {
		t.Fatal(err)
	}
	pipe := filepath.Join(home, "pipe")
	if err := unix.Mkfifo(pipe, 0o600); err != nil {
		t.Fatal(err)
	}
	for _, path := range []string{link, pipe} {
		got, err := config.ReadTokenFile(path)
		if want := "token_file " + path + ": not a regular file"; got != "" || err == nil || err.Error() != want {
			t.Fatalf("ReadTokenFile(%q) returned %q, %v; want no token and %q", path, got, err, want)
		}
	}
}
