package search

import (
	"context"
	"path/filepath"
	"slices"
	"strings"
)

func Select(ctx context.Context, cwd, path, glob string, exts []string) ([]string, error) {
	scope, err := resolveScope(cwd, path)
	if err != nil {
		return nil, err
	}
	entries, err := walkScope(cwd, scope)
	if err != nil {
		return nil, err
	}
	kept, err := parseCandidates(ctx, entries, glob, func(display string) bool {
		return slices.Contains(exts, strings.ToLower(filepath.Ext(display)))
	})
	if err != nil {
		return nil, err
	}
	paths := make([]string, 0, len(kept))
	for _, entry := range kept {
		if !selectable(entry.absolute) {
			continue
		}
		paths = append(paths, entry.display)
	}
	return paths, nil
}

func selectable(path string) bool {
	if !searchableFile(path) {
		return false
	}
	_, ok := readSearchable(path)
	return ok
}
