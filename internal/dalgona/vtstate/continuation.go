package vtstate

import (
	"bytes"
	"encoding/binary"
	"fmt"
	"unicode/utf8"

	"github.com/charmbracelet/x/ansi"
	"github.com/charmbracelet/x/ansi/parser"
)

const continuationVersion = 1

type sequenceTracker struct {
	sequence    []byte
	stringBytes int
	overflow    bool
}

func (s *sequenceTracker) track(b byte, before parser.State, action parser.Action, after parser.State) {
	switch {
	case after == parser.GroundState:
		s.restart()
	case startsSequence(b, before, after, action):
		s.restart()
		s.append(b)
	case before == after && action == parser.IgnoreAction:
	case action == parser.PutAction:
		s.stringBytes++
		if s.stringBytes <= parserDataBytes {
			s.append(b)
		}
	default:
		if action == parser.StartAction {
			s.stringBytes = 0
		}
		s.append(b)
	}
}

func startsSequence(b byte, before, after parser.State, action parser.Action) bool {
	switch {
	case b == ansi.ESC:
		return after == parser.EscapeState
	case b >= 0x80 && b <= 0x9f:
		return action == parser.ClearAction || action == parser.StartAction
	default:
		return after == parser.Utf8State && before != parser.Utf8State
	}
}

func (s *sequenceTracker) restart() {
	s.sequence = s.sequence[:0]
	s.stringBytes = 0
	s.overflow = false
}

func (s *sequenceTracker) append(b byte) {
	if len(s.sequence) >= maxContinuationBytes {
		s.overflow = true
		return
	}
	s.sequence = append(s.sequence, b)
}

func (t *Terminal) continuation() ([]byte, error) {
	if t.pending.overflow {
		return nil, fmt.Errorf("%w: an unfinished escape sequence holds more than %d bytes; retry the checkpoint after the sequence ends", ErrLimitExceeded, maxContinuationBytes)
	}
	data := make([]byte, 0, 1+2*binary.MaxVarintLen32+len(t.cluster)+len(t.pending.sequence))
	data = append(data, continuationVersion)
	if t.lastChar < 0 {
		return nil, invalid("repeat character %U is negative", t.lastChar)
	}
	data = binary.AppendUvarint(data, uint64(t.lastChar))     //nolint:gosec // lastChar is checked non-negative above
	data = binary.AppendUvarint(data, uint64(len(t.cluster))) //nolint:gosec // lengths are non-negative
	data = append(data, t.cluster...)
	data = append(data, t.pending.sequence...)
	if len(data) > maxContinuationBytes {
		return nil, fmt.Errorf("%w: parser continuation needs %d bytes, more than %d; retry the checkpoint after the sequence ends", ErrLimitExceeded, len(data), maxContinuationBytes)
	}
	return data, nil
}

type continuationState struct {
	cluster  []byte
	sequence []byte
	lastChar rune
}

func parseContinuation(data []byte) (continuationState, error) {
	if len(data) == 0 || len(data) > maxContinuationBytes {
		return continuationState{}, invalid("parser continuation has %d bytes, want 1 through %d", len(data), maxContinuationBytes)
	}
	if data[0] != continuationVersion {
		return continuationState{}, fmt.Errorf("%w: parser continuation format %d, this build reads format %d; attach with a compatible backend", ErrIncompatible, data[0], continuationVersion)
	}
	state, err := splitContinuation(data[1:])
	if err != nil {
		return state, err
	}
	if len(state.cluster) > 0 && !singleCluster(string(state.cluster)) {
		return state, invalid("parser continuation pending text is not one printable grapheme cluster")
	}
	return state, checkSequence(state.sequence)
}

func splitContinuation(rest []byte) (continuationState, error) {
	last, n := binary.Uvarint(rest)
	if n <= 0 || (last != 0 && (last < 0x20 || last > 0x7e)) {
		return continuationState{}, invalid("parser continuation repeat character is not printable ASCII")
	}
	rest = rest[n:]
	size, n := binary.Uvarint(rest)
	if n <= 0 || size > maxClusterBytes {
		return continuationState{}, invalid("parser continuation pending cluster length is invalid")
	}
	rest = rest[n:]
	if size > uint64(len(rest)) { //nolint:gosec // lengths are non-negative
		return continuationState{}, invalid("parser continuation pending cluster length is invalid")
	}
	return continuationState{lastChar: rune(last), cluster: rest[:size], sequence: rest[size:]}, nil //nolint:gosec // last is 0 or printable ASCII per the check above
}

func checkSequence(sequence []byte) error {
	probe := new(ansi.Parser)
	probe.SetParamsSize(parser.MaxParamsSize)
	probe.SetDataSize(1)
	var tracker sequenceTracker
	for _, b := range sequence {
		before := probe.State()
		action := probe.Advance(b)
		if !replayable(action, before, probe.State()) {
			return invalid("parser continuation contains a completed control or sequence")
		}
		tracker.track(b, before, action, probe.State())
	}
	if len(sequence) > 0 && probe.State() == parser.GroundState {
		return invalid("parser continuation ends outside an escape or UTF-8 sequence")
	}
	if !bytes.Equal(tracker.sequence, sequence) {
		return invalid("parser continuation is not the minimal unfinished sequence")
	}
	return nil
}

func replayable(action parser.Action, before, after parser.State) bool {
	switch action {
	case parser.PrintAction, parser.DispatchAction:
		return false
	case parser.ExecuteAction:
		return before == after
	}
	return true
}

func singleCluster(text string) bool {
	if text == "" || len(text) > maxClusterBytes || !utf8.ValidString(text) {
		return false
	}
	for _, r := range text {
		if r < 0x20 || (r >= 0x7f && r < 0xa0) {
			return false
		}
	}
	first, _ := ansi.FirstGraphemeCluster(text, ansi.GraphemeWidth)
	return len(first) == len(text)
}
