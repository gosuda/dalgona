package runner

import (
	"fmt"
	"os"
	"testing"
)

func TestMain(m *testing.M) {
	if len(os.Args) > 1 && os.Args[1] == "__shim" {
		os.Exit(ShimMain(os.Args[2:]))
	}
	if len(os.Args) > 1 && os.Args[1] == "__self-test" {
		_, _ = fmt.Fprint(os.Stdout, os.Args[0])
		os.Exit(0)
	}
	os.Exit(m.Run())
}
