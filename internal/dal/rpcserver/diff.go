package rpcserver

import (
	"log/slog"

	"github.com/gosuda/dalgona/internal/dal/rpc"

	"github.com/gosuda/dalgona/internal/dal/textdiff"
)

func diffChange(path, kind string, pre, post []byte) rpc.FileChange {
	diff, stats, err := textdiff.Unified("a/"+path, "b/"+path, pre, post, 3)
	if err != nil {
		slog.Default().Warn("file change diff failed", "path", path, "error", err)
		return omittedChange(path, kind)
	}
	return rpc.FileChange{Path: path, Kind: kind, Diff: diff, Additions: stats.Added, Deletions: stats.Removed}
}

func omittedChange(path, kind string) rpc.FileChange {
	return rpc.FileChange{Path: path, Kind: kind, DiffOmitted: true}
}
