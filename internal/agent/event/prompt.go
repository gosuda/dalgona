package event

import (
	"errors"
	"fmt"
	"maps"
	"strings"
	"unicode/utf8"
)

const (
	TypePromptBase    = "prompt.base"
	TypeMemoryUpdated = "memory.updated"
	TypePromptBound   = "prompt.bound"
)

const (
	MaxPromptBaseBytes = 1 << 20
	MaxMemoryBytes     = 256 << 10
)

type PromptBase struct {
	Generation uint64
	Content    string
}

func (PromptBase) Type() string { return TypePromptBase }

func (e PromptBase) Validate() error {
	if e.Generation == 0 {
		return errors.New("prompt.base generation must be positive")
	}
	if e.Content == "" {
		return errors.New("prompt.base content must not be empty")
	}
	if len(e.Content) > MaxPromptBaseBytes {
		return fmt.Errorf("prompt.base content exceeds %d bytes", MaxPromptBaseBytes)
	}
	if !utf8.ValidString(e.Content) {
		return errors.New("prompt.base content must be valid UTF-8")
	}
	return nil
}

type MemoryUpdated struct {
	Generation uint64
	Content    string
}

func (MemoryUpdated) Type() string { return TypeMemoryUpdated }

func (e MemoryUpdated) Validate() error {
	if e.Generation == 0 {
		return errors.New("memory.updated generation must be positive")
	}
	if len(e.Content) > MaxMemoryBytes {
		return fmt.Errorf("memory.updated content exceeds %d bytes", MaxMemoryBytes)
	}
	if !utf8.ValidString(e.Content) {
		return errors.New("memory.updated content must be valid UTF-8")
	}
	if strings.TrimSpace(e.Content) != e.Content {
		return errors.New("memory.updated content must not start or end with white space")
	}
	return nil
}

type PromptBound struct {
	TurnID           string
	BaseGeneration   uint64
	MemoryGeneration uint64
}

func (PromptBound) Type() string { return TypePromptBound }

func (e PromptBound) Validate() error {
	if err := required(map[string]string{"turn_id": e.TurnID}); err != nil {
		return err
	}
	if e.BaseGeneration == 0 {
		return errors.New("prompt.bound base_generation must be positive")
	}
	return nil
}

func promptMembers(e Event) ([]member, error) {
	switch value := e.(type) {
	case PromptBase:
		return []member{field("generation", uint64Value(value.Generation)), field("content", stringValue(value.Content))}, nil
	case MemoryUpdated:
		return []member{field("generation", uint64Value(value.Generation)), field("content", stringValue(value.Content))}, nil
	case PromptBound:
		return []member{
			field("turn_id", stringValue(value.TurnID)), field("base_generation", uint64Value(value.BaseGeneration)),
			field("memory_generation", uint64Value(value.MemoryGeneration)),
		}, nil
	default:
		return nil, fmt.Errorf("event: %T does not match type %s", e, e.Type())
	}
}

func decodePromptEvent(kind string, r *objectReader) (Event, error) {
	switch kind {
	case TypePromptBase:
		r.require("generation")
		r.require("content")
		return PromptBase{Generation: r.uint64("generation"), Content: r.str("content")}, nil
	case TypeMemoryUpdated:
		r.require("generation")
		r.require("content")
		return MemoryUpdated{Generation: r.uint64("generation"), Content: r.str("content")}, nil
	case TypePromptBound:
		r.require("turn_id")
		r.require("base_generation")
		r.require("memory_generation")
		return PromptBound{
			TurnID: r.str("turn_id"), BaseGeneration: r.uint64("base_generation"), MemoryGeneration: r.uint64("memory_generation"),
		}, nil
	default:
		return nil, fmt.Errorf("unsupported prompt event %q", kind)
	}
}

func foldPromptEvent(s *State, e Event) error {
	switch value := e.(type) {
	case PromptBase:
		return foldPromptBase(s, value)
	case MemoryUpdated:
		return foldMemoryUpdated(s, value)
	case PromptBound:
		return foldPromptBound(s, value)
	default:
		return fmt.Errorf("event: fold: %T does not match %s", e, e.Type())
	}
}

func foldPromptBase(s *State, e PromptBase) error {
	if want := s.CurrentBaseGeneration + 1; e.Generation != want {
		return fmt.Errorf("event: fold: prompt.base generation %d, want %d", e.Generation, want)
	}
	if s.PromptBases == nil {
		s.PromptBases = make(map[uint64]string)
	}
	s.PromptBases[e.Generation] = e.Content
	s.CurrentBaseGeneration = e.Generation
	s.pruneVersions()
	return nil
}

func foldMemoryUpdated(s *State, e MemoryUpdated) error {
	if want := s.CurrentMemoryGeneration + 1; e.Generation != want {
		return fmt.Errorf("event: fold: memory.updated generation %d, want %d", e.Generation, want)
	}
	if s.MemoryVersions == nil {
		s.MemoryVersions = make(map[uint64]string)
	}
	s.MemoryVersions[e.Generation] = e.Content
	s.CurrentMemoryGeneration = e.Generation
	s.pruneVersions()
	return nil
}

func foldPromptBound(s *State, e PromptBound) error {
	if err := requireTurn(s, e.TurnID); err != nil {
		return err
	}
	if prior, bound := s.PromptBindings[e.TurnID]; bound {
		if prior != e {
			return fmt.Errorf("event: fold: prompt.bound for turn %s differs from its first binding", e.TurnID)
		}
		return nil
	}
	if _, ok := s.PromptBases[e.BaseGeneration]; !ok {
		return fmt.Errorf("event: fold: prompt.bound names unknown base generation %d", e.BaseGeneration)
	}
	if _, ok := s.MemoryVersions[e.MemoryGeneration]; !ok && e.MemoryGeneration != 0 {
		return fmt.Errorf("event: fold: prompt.bound names unknown memory generation %d", e.MemoryGeneration)
	}
	if s.PromptBindings == nil {
		s.PromptBindings = make(map[string]PromptBound)
	}
	s.PromptBindings[e.TurnID] = e
	return nil
}

func (s *State) pruneVersions() {
	maps.DeleteFunc(s.PromptBases, func(generation uint64, _ string) bool {
		return generation != s.CurrentBaseGeneration && !s.baseBound(generation)
	})
	maps.DeleteFunc(s.MemoryVersions, func(generation uint64, _ string) bool {
		return generation != s.CurrentMemoryGeneration && !s.memoryBound(generation)
	})
}

func (s *State) baseBound(generation uint64) bool {
	for _, bound := range s.PromptBindings {
		if bound.BaseGeneration == generation {
			return true
		}
	}
	return false
}

func (s *State) memoryBound(generation uint64) bool {
	for _, bound := range s.PromptBindings {
		if bound.MemoryGeneration == generation {
			return true
		}
	}
	return false
}

func (s *State) releaseBinding(turnID string) {
	delete(s.PromptBindings, turnID)
	s.pruneVersions()
}
