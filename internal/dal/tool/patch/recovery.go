package patch

import (
	"cmp"
	"errors"
	"slices"

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

func uncovered(shown []snapshot.Range, first, last int) []snapshot.Range {
	ordered := slices.SortedFunc(slices.Values(shown), func(a, b snapshot.Range) int {
		return cmp.Compare(a.Start, b.Start)
	})
	var gaps []snapshot.Range
	next := first
	for _, item := range ordered {
		if item.Start > last {
			break
		}
		if item.Start > next {
			gaps = append(gaps, snapshot.Range{Start: next, End: item.Start - 1})
		}
		next = max(next, item.End+1)
	}
	if next <= last {
		gaps = append(gaps, snapshot.Range{Start: next, End: last})
	}
	return gaps
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
	current, mapped := currentUncovered(store, cause)
	if !mapped {
		return unknownRead(cause.path)
	}
	ranges, remaining := splitRecovery(current)
	return &tool.Recovery{Kind: "read", Path: cause.path, Ranges: ranges, Mapping: "known", Remaining: remaining}
}

func currentUncovered(store *snapshot.Store, cause *coverageError) ([]tool.LineRange, bool) {
	shown, err := store.Shown(cause.ref)
	if err != nil {
		return nil, false
	}
	gaps := uncovered(shown, cause.first, cause.last)
	targets := make([]snapshot.Span, len(gaps))
	for i, gap := range gaps {
		targets[i] = snapshot.Span{Start: gap.Start - 1, End: gap.End}
	}
	spans, err := store.ResolveSpans(cause.ref, cause.path, cause.before, targets)
	if err != nil {
		return nil, false
	}
	current := make([]tool.LineRange, 0, len(spans))
	for _, span := range spans {
		if span.End <= span.Start {
			return nil, false
		}
		current = append(current, tool.LineRange{First: span.Start + 1, Last: span.End})
	}
	return current, len(current) > 0
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
