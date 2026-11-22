package patch

import (
	"errors"

	"github.com/gosuda/dalgona/internal/dal/tool"
	"github.com/gosuda/dalgona/internal/dal/tool/snapshot"
)

const (
	recoveryMaxLines     = 40
	recoveryMaxRemaining = 64
)

type coverageError struct {
	err         error
	path, ref   string
	first, last int
	before      []byte
}

func (e *coverageError) Error() string { return e.err.Error() }

func (e *coverageError) Unwrap() error { return e.err }

type pathError struct {
	path string
	err  error
}

func (e *pathError) Error() string { return e.err.Error() }

func (e *pathError) Unwrap() error { return e.err }

func atPath(path string, err error) error {
	return &pathError{path: path, err: err}
}

func pathOf(err error) (string, bool) {
	var located *pathError
	if errors.As(err, &located) {
		return located.path, true
	}
	return "", false
}

func recoveryFor(env tool.Env, err error) *tool.Recovery {
	reason, _ := classify(err)
	if status := reasonRows[reason].status; status != tool.StatusNeedsContext && status != tool.StatusConflict {
		return nil
	}
	var coverage *coverageError
	if errors.As(err, &coverage) {
		return coverageRecovery(env.Snapshots, coverage)
	}
	path, located := pathOf(err)
	if !located {
		return nil
	}
	return unknownRead(path)
}

func unknownRead(path string) *tool.Recovery {
	return &tool.Recovery{Kind: "read", Path: path, Mapping: "unknown"}
}

func coverageRecovery(store *snapshot.Store, cause *coverageError) *tool.Recovery {
	current, mapped := currentSpan(store, cause)
	if !mapped {
		return unknownRead(cause.path)
	}
	ranges, remaining := splitRecovery(current)
	return &tool.Recovery{Kind: "read", Path: cause.path, Ranges: ranges, Mapping: "known", Remaining: remaining}
}

func currentSpan(store *snapshot.Store, cause *coverageError) ([]tool.LineRange, bool) {
	if store == nil {
		return nil, false
	}
	span, err := store.ResolveSpan(cause.ref, cause.path, cause.before, snapshot.Span{Start: cause.first - 1, End: cause.last})
	if err != nil || span.End <= span.Start {
		return nil, false
	}
	return []tool.LineRange{{First: span.Start + 1, Last: span.End}}, true
}

func splitRecovery(current []tool.LineRange) (ranges, remaining []tool.LineRange) {
	budget := recoveryMaxLines
	for _, item := range current {
		take := min(budget, item.Last-item.First+1)
		if take > 0 {
			ranges = append(ranges, tool.LineRange{First: item.First, Last: item.First + take - 1})
			budget -= take
			item.First += take
		}
		if item.First <= item.Last {
			remaining = append(remaining, item)
		}
	}
	return ranges, capRemaining(remaining)
}

func capRemaining(remaining []tool.LineRange) []tool.LineRange {
	if len(remaining) <= recoveryMaxRemaining {
		return remaining
	}
	last := remaining[len(remaining)-1].Last
	remaining = remaining[:recoveryMaxRemaining]
	remaining[recoveryMaxRemaining-1].Last = last
	return remaining
}
