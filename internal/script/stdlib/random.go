package stdlib

import (
	"errors"
	"math/rand/v2"

	"go.starlark.net/starlark"
	"go.starlark.net/starlarkstruct"
)

type randomSource struct {
	rng *rand.Rand
}

func randomModule(seed int64) starlark.Value {
	//nolint:gosec // Replay requires a deterministic seeded PRNG, not cryptographic randomness.
	source := &randomSource{rng: rand.New(rand.NewPCG(unsignedInt64(seed), unsignedInt64(seed)^0x9e3779b97f4a7c15))}
	return &starlarkstruct.Module{Name: "random", Members: starlark.StringDict{
		"random":  starlark.NewBuiltin("random.random", source.random),
		"randint": starlark.NewBuiltin("random.randint", source.randint),
		"uniform": starlark.NewBuiltin("random.uniform", source.uniform),
		"choice":  starlark.NewBuiltin("random.choice", source.choice),
		"shuffle": starlark.NewBuiltin("random.shuffle", source.shuffle),
	}}
}

func (s *randomSource) random(_ *starlark.Thread, b *starlark.Builtin, args starlark.Tuple, kwargs []starlark.Tuple) (starlark.Value, error) {
	if err := starlark.UnpackPositionalArgs(b.Name(), args, kwargs, 0); err != nil {
		return nil, err
	}
	return starlark.Float(s.rng.Float64()), nil
}

func (s *randomSource) randint(_ *starlark.Thread, b *starlark.Builtin, args starlark.Tuple, kwargs []starlark.Tuple) (starlark.Value, error) {
	var low, high int64
	if err := starlark.UnpackPositionalArgs(b.Name(), args, kwargs, 2, &low, &high); err != nil {
		return nil, err
	}
	if high < low {
		return nil, errors.New("empty range for randint")
	}
	span := unsignedInt64(high) - unsignedInt64(low) + 1
	var offset uint64
	if span == 0 {
		offset = s.rng.Uint64()
	} else {
		offset = s.rng.Uint64N(span)
	}
	return starlark.MakeInt64(signedInt64(unsignedInt64(low) + offset)), nil
}

func unsignedInt64(value int64) uint64 {
	if value >= 0 {
		return uint64(value)
	}
	//nolint:gosec // Negative values retain their two's-complement bit pattern.
	return ^uint64(-(value + 1))
}

func signedInt64(value uint64) int64 {
	if value <= uint64(1<<63-1) {
		return int64(value)
	}
	//nolint:gosec // The branch bounds the converted magnitude to MaxInt64.
	return -int64(^value) - 1
}

func (s *randomSource) uniform(_ *starlark.Thread, b *starlark.Builtin, args starlark.Tuple, kwargs []starlark.Tuple) (starlark.Value, error) {
	var lowValue, highValue starlark.Value
	if err := starlark.UnpackPositionalArgs(b.Name(), args, kwargs, 2, &lowValue, &highValue); err != nil {
		return nil, err
	}
	low, ok := starlark.AsFloat(lowValue)
	if !ok {
		return nil, errors.New("random.uniform expects numeric bounds")
	}
	high, ok := starlark.AsFloat(highValue)
	if !ok {
		return nil, errors.New("random.uniform expects numeric bounds")
	}
	return starlark.Float(low + s.rng.Float64()*(high-low)), nil
}

func (s *randomSource) choice(_ *starlark.Thread, b *starlark.Builtin, args starlark.Tuple, kwargs []starlark.Tuple) (starlark.Value, error) {
	var value starlark.Value
	if err := starlark.UnpackPositionalArgs(b.Name(), args, kwargs, 1, &value); err != nil {
		return nil, err
	}
	if text, ok := starlark.AsString(value); ok {
		return chooseString(s.rng, text)
	}
	sequence, ok := value.(starlark.Indexable)
	if !ok {
		return nil, errors.New("random.choice expects a sequence")
	}
	if sequence.Len() == 0 {
		return nil, errors.New("cannot choose from an empty sequence")
	}
	return sequence.Index(s.rng.IntN(sequence.Len())), nil
}

func chooseString(rng *rand.Rand, text string) (starlark.Value, error) {
	runes := []rune(text)
	if len(runes) == 0 {
		return nil, errors.New("cannot choose from an empty sequence")
	}
	return starlark.String(string(runes[rng.IntN(len(runes))])), nil
}

func (s *randomSource) shuffle(_ *starlark.Thread, b *starlark.Builtin, args starlark.Tuple, kwargs []starlark.Tuple) (starlark.Value, error) {
	var list *starlark.List
	if err := starlark.UnpackPositionalArgs(b.Name(), args, kwargs, 1, &list); err != nil {
		return nil, err
	}
	values := make([]starlark.Value, list.Len())
	for i := range values {
		values[i] = list.Index(i)
	}
	s.rng.Shuffle(len(values), func(i, j int) {
		values[i], values[j] = values[j], values[i]
	})
	for i, value := range values {
		if err := list.SetIndex(i, value); err != nil {
			return nil, err
		}
	}
	return starlark.None, nil
}
