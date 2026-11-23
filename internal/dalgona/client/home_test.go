package client

import (
	"os"
	"testing"
)

func shortHome(t *testing.T) string {
	t.Helper()
	home, err := os.MkdirTemp("", "dg")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		if err := os.RemoveAll(home); err != nil {
			t.Error(err)
		}
	})
	return home
}
