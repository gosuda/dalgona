package client

import (
	"fmt"
	"os"
	"path/filepath"
	"testing"
	"time"
)

func TestMain(m *testing.M) {
	if os.Getenv("DALGONA_TEST_NEVER_LISTEN") != "" {
		os.Exit(0)
	}
	if handled, code := dispatchDialProbe(); handled {
		os.Exit(code)
	}
	os.Exit(m.Run())
}

func TestEnsureTimeout(t *testing.T) {
	old := ensureTimeout
	ensureTimeout = 20 * time.Millisecond
	defer func() { ensureTimeout = old }()
	home := shortHome(t)
	exe, err := os.Executable()
	if err != nil {
		t.Fatal(err)
	}
	t.Setenv("DALGONA_TEST_NEVER_LISTEN", "1")
	_, err = Ensure(t.Context(), home, exe)
	if err == nil || err.Error() != fmt.Sprintf("daemon did not start: see %s", filepath.Join(home, "daemon.log")) {
		t.Fatalf("err=%v", err)
	}
}
