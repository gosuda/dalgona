//go:build unix

package patch

import (
	"errors"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func heldDirPin(t *testing.T) (*dirPin, string) {
	t.Helper()
	env, cwd := editTestEnv(t)
	writeFixture(t, cwd, "sub/f.txt", "one\n")
	pin, err := pinVerifiedComponents(env, "sub/f.txt", filepath.Join(cwd, "sub", "f.txt"))
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(pin.close)
	return pin, cwd
}

func TestDirPinSyncDirReportsAClosedHandle(t *testing.T) {
	t.Run("IO-05/closed handle", func(t *testing.T) {
		pin, cwd := heldDirPin(t)
		if err := pin.file.Close(); err != nil {
			t.Fatal(err)
		}
		err := pin.syncDir()
		if !errors.Is(err, os.ErrClosed) {
			t.Fatalf("syncDir of a closed handle = %v, want a wrapped %v", err, os.ErrClosed)
		}
		if want := "patch: sync directory " + filepath.Join(cwd, "sub"); !strings.Contains(err.Error(), want) {
			t.Fatalf("syncDir error %q does not name the directory with %q", err, want)
		}
	})
}

func TestDirPinSyncDirSyncsAHeldDirectory(t *testing.T) {
	t.Run("IO-05/open handle", func(t *testing.T) {
		pin, _ := heldDirPin(t)
		if err := pin.syncDir(); err != nil {
			t.Fatalf("syncDir of a held directory = %v, want nil", err)
		}
	})
}

func TestNilDirPinSyncDirIsANoOp(t *testing.T) {
	t.Run("IO-05/no pin", func(t *testing.T) {
		var pin *dirPin
		if err := pin.syncDir(); err != nil {
			t.Fatalf("syncDir of a nil pin = %v, want nil", err)
		}
	})
}
