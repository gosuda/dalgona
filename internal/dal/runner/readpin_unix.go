//go:build unix && !linux

package runner

import (
	"fmt"
	"os"
)

func openPinned(root, rel string) (*os.File, error) {
	dir, err := os.Open(root)
	if err != nil {
		return nil, fmt.Errorf("runner: pinned open %s: %w", rel, err)
	}
	return walkNoFollow(dir, rel)
}
