package stdlib

import (
	"time"

	starlarkmath "go.starlark.net/lib/math"
	"go.starlark.net/starlark"
	"go.starlark.net/syntax"
)

type Hooks struct {
	Now   func() float64
	Sleep func(time.Duration) error
	Seed  int64
}

type builtinFunction func(*starlark.Thread, *starlark.Builtin, starlark.Tuple, []starlark.Tuple) (starlark.Value, error)

func FileOptions() syntax.FileOptions {
	return syntax.FileOptions{Set: true, While: true, TopLevelControl: true, GlobalReassign: true, Recursion: true}
}

func Predeclared(h Hooks) starlark.StringDict {
	return starlark.StringDict{
		"json":   jsonModule(),
		"math":   starlarkmath.Module,
		"time":   timeModule(h),
		"random": randomModule(h.Seed),
		"re":     regexpModule(),
	}
}
