package agent

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"slices"

	"github.com/gosuda/dalgona/internal/dal/agent/event"
	"github.com/gosuda/dalgona/internal/dal/journal"
	"github.com/gosuda/dalgona/internal/dal/tool"
	asktool "github.com/gosuda/dalgona/internal/dal/tool/ask"
)

var errCeilingUnavailable = errors.New("agent: child tool ceiling unavailable")

func (t *Thread) childTools(ctx context.Context) ([]string, error) {
	if err := t.ensureTools(ctx); err != nil {
		return nil, err
	}
	return ceilingOf(t.currentTools())
}

func ceilingOf(set *toolset) ([]string, error) {
	if set == nil {
		return nil, errCeilingUnavailable
	}
	if set.duplicate != "" {
		return nil, fmt.Errorf("agent: duplicate tool name %q", set.duplicate)
	}
	names := []string{tool.YieldName}
	for name, item := range set.byName {
		if name != asktool.Name && name != taskName && name != tool.YieldName && item.Class() != tool.ClassAgent {
			names = append(names, name)
		}
	}
	slices.Sort(names)
	return names, nil
}

func (t *Thread) confine(ctx context.Context, tools []tool.Tool) ([]tool.Tool, error) {
	if t.created.Parent == "" || t.created.Advisor != "" {
		return tools, nil
	}
	allowed, err := t.ceiling(ctx)
	if err != nil {
		return nil, err
	}
	kept := make([]tool.Tool, 0, len(allowed))
	for _, item := range tools {
		if _, found := slices.BinarySearch(allowed, item.Spec().Name); found {
			kept = append(kept, item)
		}
	}
	return kept, nil
}

func (t *Thread) ceiling(ctx context.Context) ([]string, error) {
	recorded, err := t.host.recordedCeiling(t.created.Parent, t.id)
	if err != nil {
		return nil, err
	}
	if len(recorded) > 0 {
		return recorded, nil
	}
	return nil, errCeilingUnavailable
}

func (h *Host) recordedCeiling(parent, child string) ([]string, error) {
	var names []string
	needle := []byte(child)
	err := h.journal.Replay(parent, func(_ uint64, raw []byte) error {
		if !bytes.Contains(raw, needle) {
			return nil
		}
		decoded, err := event.Decode(raw)
		if err != nil {
			return fmt.Errorf("decode: %w", err)
		}
		if spawned, ok := decoded.(event.ChildSpawned); ok && spawned.ChildID == child {
			names = spawned.Tools
		}
		return nil
	})
	if _, damaged := errors.AsType[*journal.DamageError](err); damaged {
		err = nil
	}
	if err != nil {
		return nil, fmt.Errorf("agent: child tool ceiling of %s: %w", child, err)
	}
	return names, nil
}
