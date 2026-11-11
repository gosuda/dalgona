package exec

import (
	"fmt"
	"os"
	"testing"

	"github.com/gosuda/dalgona/internal/runner"
)

func TestMain(m *testing.M) {
	if len(os.Args) > 1 && os.Args[1] == "__shim" {
		os.Exit(runner.ShimMain(os.Args[2:]))
	}
	if len(os.Args) > 1 && os.Args[1] == "__self-test" {
		_, _ = fmt.Fprint(os.Stdout, os.Args[0])
		os.Exit(0)
	}
	if len(os.Args) > 1 && os.Args[1] == "__print-cwd" {
		os.Exit(printCwd())
	}
	os.Exit(m.Run())
}

func printCwd() int {
	dir, err := os.Getwd()
	if err != nil {
		_, _ = fmt.Fprint(os.Stderr, err)
		return 1
	}
	_, _ = fmt.Fprint(os.Stdout, dir)
	return 0
}
