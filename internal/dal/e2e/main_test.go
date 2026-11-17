package e2e

import (
	"os"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/runner"
)

func TestMain(m *testing.M) {
	if len(os.Args) > 1 && os.Args[1] == "__shim" {
		os.Exit(runner.ShimMain(os.Args[2:]))
	}
	os.Exit(m.Run())
}
