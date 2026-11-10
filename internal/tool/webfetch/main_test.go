package webfetch

import (
	"os"
	"testing"

	"github.com/gosuda/dalgona/internal/runner"
)

func TestMain(m *testing.M) {
	if len(os.Args) > 1 && os.Args[1] == "__shim" {
		os.Exit(runner.ShimMain(os.Args[2:]))
	}
	if len(os.Args) > 1 && os.Args[1] == "__fetch" {
		os.Exit(Main(os.Args[2:], os.Stdout))
	}
	os.Exit(m.Run())
}
