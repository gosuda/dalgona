package stdlib

import (
	"errors"
	"fmt"
	"math"
	"time"

	"go.starlark.net/starlark"
	"go.starlark.net/starlarkstruct"
)

func timeModule(h Hooks) starlark.Value {
	if h.Now == nil {
		h.Now = systemNow
	}
	if h.Sleep == nil {
		h.Sleep = systemSleep
	}
	return &starlarkstruct.Module{Name: "time", Members: starlark.StringDict{
		"time":  starlark.NewBuiltin("time.time", timeNow(h.Now)),
		"sleep": starlark.NewBuiltin("time.sleep", timeSleep(h.Sleep)),
	}}
}

func systemNow() float64 {
	now := time.Now()
	return float64(now.Unix()) + float64(now.Nanosecond())/1e9
}

func systemSleep(d time.Duration) error {
	time.Sleep(d)
	return nil
}

func timeNow(now func() float64) builtinFunction {
	return func(_ *starlark.Thread, b *starlark.Builtin, args starlark.Tuple, kwargs []starlark.Tuple) (starlark.Value, error) {
		if err := starlark.UnpackPositionalArgs(b.Name(), args, kwargs, 0); err != nil {
			return nil, err
		}
		return starlark.Float(now()), nil
	}
}

func timeSleep(sleep func(time.Duration) error) builtinFunction {
	return func(_ *starlark.Thread, b *starlark.Builtin, args starlark.Tuple, kwargs []starlark.Tuple) (starlark.Value, error) {
		var value starlark.Value
		if err := starlark.UnpackPositionalArgs(b.Name(), args, kwargs, 1, &value); err != nil {
			return nil, err
		}
		seconds, ok := starlark.AsFloat(value)
		if !ok {
			return nil, errors.New("time.sleep expects a number")
		}
		if seconds < 0 || math.IsNaN(seconds) || math.IsInf(seconds, 0) {
			return nil, errors.New("sleep length must be non-negative and finite")
		}
		if err := sleep(time.Duration(seconds * float64(time.Second))); err != nil {
			return nil, fmt.Errorf("time.sleep: %w", err)
		}
		return starlark.None, nil
	}
}
