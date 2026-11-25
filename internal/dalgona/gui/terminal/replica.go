package terminal

import (
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"fmt"
	"math"

	"github.com/gosuda/dalgona/internal/dalgona/vtstate"
)

const (
	MinColumns = 20
	MaxColumns = 240
	MinRows    = 5
	MaxRows    = 120
)

var (
	ErrCheckpointIncomplete = errors.New("terminal: checkpoint download is incomplete; finish or restart the checkpoint read")
	ErrCheckpointDigest     = errors.New("terminal: checkpoint bytes do not match their digest; request a fresh checkpoint")
	ErrReadOutOfOrder       = errors.New("terminal: read does not continue this view's byte and resize cursors; refresh through dalgona/terminal/checkpoint/open")
)

type CheckpointInfo struct {
	EngineVersion string
	Digest        string
	Version       int
	NextOffset    uint64
	ResizeOrdinal uint64
	Columns       int
	Rows          int
	Total         int
}

type Resize struct {
	Ordinal uint64
	Offset  uint64
	Columns int
	Rows    int
}

type Read struct {
	Data          []byte
	Resizes       []Resize
	Offset        uint64
	Next          uint64
	ResizeOrdinal uint64
}

type Replica struct {
	term          *vtstate.Terminal
	nextOffset    uint64
	resizeOrdinal uint64
}

func NewReplica(info CheckpointInfo, body []byte) (*Replica, error) {
	term, err := importCheckpoint(info, body)
	if err != nil {
		return nil, err
	}
	return &Replica{term: term, nextOffset: info.NextOffset, resizeOrdinal: info.ResizeOrdinal}, nil
}

func (r *Replica) Refresh(info CheckpointInfo, body []byte) error {
	term, err := importCheckpoint(info, body)
	if err != nil {
		return err
	}
	r.term, r.nextOffset, r.resizeOrdinal = term, info.NextOffset, info.ResizeOrdinal
	return nil
}

func importCheckpoint(info CheckpointInfo, body []byte) (*vtstate.Terminal, error) {
	if info.Version != vtstate.Version || info.EngineVersion != vtstate.EngineVersion() {
		return nil, fmt.Errorf("%w: checkpoint version %d engine %q, this client reads version %d engine %q; attach with a compatible backend",
			vtstate.ErrIncompatible, info.Version, info.EngineVersion, vtstate.Version, vtstate.EngineVersion())
	}
	if len(body) != info.Total {
		return nil, fmt.Errorf("%w: have %d of %d bytes", ErrCheckpointIncomplete, len(body), info.Total)
	}
	sum := sha256.Sum256(body)
	if hex.EncodeToString(sum[:]) != info.Digest {
		return nil, ErrCheckpointDigest
	}
	term, err := vtstate.Restore(body)
	if err != nil {
		return nil, err
	}
	if columns, rows := term.Size(); columns != info.Columns || rows != info.Rows {
		return nil, fmt.Errorf("%w: body holds %dx%d cells, metadata says %dx%d", vtstate.ErrInvalidCheckpoint, columns, rows, info.Columns, info.Rows)
	}
	return term, nil
}

func (r *Replica) Apply(read Read) error {
	if err := r.continues(read); err != nil {
		return err
	}
	written := 0
	for _, resize := range read.Resizes {
		span := resize.Offset - read.Offset
		if span > uint64(math.MaxInt) {
			return fmt.Errorf("%w: resize %d at offset %d lies outside %d..%d", ErrReadOutOfOrder, resize.Ordinal, resize.Offset, read.Offset, read.Next)
		}
		end := int(span)
		r.term.Feed(read.Data[written:end])
		written = end
		r.term.Resize(resize.Columns, resize.Rows)
	}
	r.term.Feed(read.Data[written:])
	r.nextOffset, r.resizeOrdinal = read.Next, read.ResizeOrdinal
	return nil
}

func (r *Replica) continues(read Read) error {
	if read.Offset != r.nextOffset || read.Next != read.Offset+uint64(len(read.Data)) {
		return fmt.Errorf("%w: read covers %d..%d with %d bytes, view expects offset %d", ErrReadOutOfOrder, read.Offset, read.Next, len(read.Data), r.nextOffset)
	}
	ordinal, offset := r.resizeOrdinal, read.Offset
	for _, resize := range read.Resizes {
		if err := resize.follows(ordinal, offset, read.Next); err != nil {
			return err
		}
		ordinal, offset = resize.Ordinal, resize.Offset
	}
	if read.ResizeOrdinal != ordinal {
		return fmt.Errorf("%w: read reports resize ordinal %d after applying %d", ErrReadOutOfOrder, read.ResizeOrdinal, ordinal)
	}
	return nil
}

func (r Resize) follows(ordinal, offset, next uint64) error {
	switch {
	case r.Ordinal != ordinal+1:
		return fmt.Errorf("%w: resize ordinal %d follows %d", ErrReadOutOfOrder, r.Ordinal, ordinal)
	case r.Offset < offset || r.Offset > next:
		return fmt.Errorf("%w: resize %d at offset %d lies outside %d..%d", ErrReadOutOfOrder, r.Ordinal, r.Offset, offset, next)
	case r.Columns < MinColumns || r.Columns > MaxColumns || r.Rows < MinRows || r.Rows > MaxRows:
		return fmt.Errorf("%w: resize %d to %dx%d is outside %d..%d columns and %d..%d rows", ErrReadOutOfOrder, r.Ordinal, r.Columns, r.Rows, MinColumns, MaxColumns, MinRows, MaxRows)
	}
	return nil
}

func (r *Replica) Cursors() (nextOffset, resizeOrdinal uint64) {
	return r.nextOffset, r.resizeOrdinal
}

func (r *Replica) Collected(finalOffset, finalResizeOrdinal uint64) bool {
	return r.nextOffset >= finalOffset && r.resizeOrdinal >= finalResizeOrdinal
}

func (r *Replica) InputModes() vtstate.InputModes {
	return r.term.InputModes()
}

func (r *Replica) Size() (columns, rows int) {
	return r.term.Size()
}

func (r *Replica) DroppedScrollbackRows() int {
	return r.term.DroppedScrollbackRows()
}
