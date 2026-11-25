package terminal

import (
	"bytes"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"testing"

	"github.com/gosuda/dalgona/internal/dalgona/vtstate"
)

func checkpointInfo(t *testing.T, term *vtstate.Terminal, nextOffset, resizeOrdinal uint64) (CheckpointInfo, []byte) {
	t.Helper()
	body, err := term.Checkpoint()
	if err != nil {
		t.Fatalf("Checkpoint: %v", err)
	}
	sum := sha256.Sum256(body)
	columns, rows := term.Size()
	return CheckpointInfo{
		Version:       vtstate.Version,
		EngineVersion: vtstate.EngineVersion(),
		NextOffset:    nextOffset,
		ResizeOrdinal: resizeOrdinal,
		Columns:       columns,
		Rows:          rows,
		Total:         len(body),
		Digest:        hex.EncodeToString(sum[:]),
	}, body
}

func replicaOf(t *testing.T, columns, rows int, stream string) *Replica {
	t.Helper()
	daemon := vtstate.New(columns, rows)
	daemon.Feed([]byte(stream))
	info, body := checkpointInfo(t, daemon, uint64(len(stream)), 0)
	replica, err := NewReplica(info, body)
	if err != nil {
		t.Fatalf("NewReplica: %v", err)
	}
	return replica
}

type outputLog struct {
	data    []byte
	resizes []Resize
}

func (l *outputLog) read(offset, ordinal uint64, byteLimit, resizeLimit int) Read {
	var resizes []Resize
	end := min(offset+uint64(max(byteLimit, 0)), uint64(len(l.data)))
	for _, r := range l.resizes {
		if r.Ordinal <= ordinal {
			continue
		}
		if len(resizes) == resizeLimit || r.Offset > end {
			end = min(end, r.Offset)
			break
		}
		resizes = append(resizes, r)
	}
	last := ordinal
	if len(resizes) > 0 {
		last = resizes[len(resizes)-1].Ordinal
	}
	return Read{Offset: offset, Next: end, Data: l.data[offset:end], Resizes: resizes, ResizeOrdinal: last}
}

type step struct {
	output  string
	columns int
	rows    int
}

func TestReplicaFollowsTheAuthoritativeTerminalThroughOrderedResizes(t *testing.T) {
	steps := []step{
		{output: "\x1b[?1049h\x1b[2J\x1b[Hfull screen app 漢字 with a line longer than thirty cells"},
		{columns: 30, rows: 8},
		{columns: 40, rows: 10},
		{columns: 25, rows: 6},
		{output: "\x1b[5;1Hbottom row\x1b[2;4r\x1b[3;1Hregion\r\n\r\n\r\nscrolled\x1b[r\x1b[?1049l"},
		{output: "\x1b]8;;https://example.com\x1b\\link\x1b]8;;\x1b\\ then a line that wraps at twenty-five but not at fifty\x1b[12;1Hrow twelve"},
		{columns: 50, rows: 12},
		{output: "after resize\r\n"},
	}
	daemon := vtstate.New(40, 10)
	info, body := checkpointInfo(t, daemon, 0, 0)
	var log outputLog
	for _, s := range steps {
		applyAuthoritativeStep(daemon, &log, s)
	}
	want, _ := daemon.Checkpoint()
	for _, limits := range [][2]int{{1, 1}, {3, 2}, {7, 64}, {1 << 14, 64}} {
		replica, err := NewReplica(info, body)
		if err != nil {
			t.Fatal(err)
		}
		driveReplicaToTip(t, replica, log, limits)
		checkReplicaMatches(t, replica, want, limits)
	}
}

func applyAuthoritativeStep(daemon *vtstate.Terminal, log *outputLog, s step) {
	if s.output != "" {
		daemon.Feed([]byte(s.output))
		log.data = append(log.data, s.output...)
		return
	}
	daemon.Resize(s.columns, s.rows)
	log.resizes = append(log.resizes, Resize{Ordinal: uint64(len(log.resizes) + 1), Offset: uint64(len(log.data)), Columns: s.columns, Rows: s.rows})
}

func driveReplicaToTip(t *testing.T, replica *Replica, log outputLog, limits [2]int) {
	t.Helper()
	finalOffset, finalOrdinal := uint64(len(log.data)), uint64(len(log.resizes))
	for !replica.Collected(finalOffset, finalOrdinal) {
		offset, ordinal := replica.Cursors()
		if err := replica.Apply(log.read(offset, ordinal, limits[0], limits[1])); err != nil {
			t.Fatalf("limits %v: Apply: %v", limits, err)
		}
		if next, nextOrdinal := replica.Cursors(); next == offset && nextOrdinal == ordinal {
			t.Fatalf("limits %v: read did not advance either cursor at %d/%d", limits, offset, ordinal)
		}
	}
}

func checkReplicaMatches(t *testing.T, replica *Replica, want []byte, limits [2]int) {
	t.Helper()
	got, _ := replica.term.Checkpoint()
	if !bytes.Equal(got, want) {
		t.Fatalf("limits %v: replica state differs from the authoritative terminal", limits)
	}
}

func TestReplicaRejectsReadsThatDoNotContinueItsCursors(t *testing.T) {
	replica := replicaOf(t, 20, 5, "abc")
	cases := map[string]Read{
		"byte gap":            {Offset: 4, Next: 5, Data: []byte("x")},
		"short data":          {Offset: 3, Next: 5, Data: []byte("x")},
		"resize ordinal gap":  {Offset: 3, Next: 4, Data: []byte("x"), Resizes: []Resize{{Ordinal: 2, Offset: 3, Columns: 30, Rows: 6}}, ResizeOrdinal: 2},
		"resize beyond bytes": {Offset: 3, Next: 4, Data: []byte("x"), Resizes: []Resize{{Ordinal: 1, Offset: 5, Columns: 30, Rows: 6}}, ResizeOrdinal: 1},
		"resize too small":    {Offset: 3, Next: 4, Data: []byte("x"), Resizes: []Resize{{Ordinal: 1, Offset: 3, Columns: 10, Rows: 6}}, ResizeOrdinal: 1},
		"cursor disagreement": {Offset: 3, Next: 4, Data: []byte("x"), ResizeOrdinal: 1},
	}
	before, _ := replica.term.Checkpoint()
	for name, read := range cases {
		if err := replica.Apply(read); !errors.Is(err, ErrReadOutOfOrder) {
			t.Fatalf("%s: Apply error = %v, want ErrReadOutOfOrder", name, err)
		}
		if offset, ordinal := replica.Cursors(); offset != 3 || ordinal != 0 {
			t.Fatalf("%s: cursors moved to %d/%d", name, offset, ordinal)
		}
		if after, _ := replica.term.Checkpoint(); !bytes.Equal(after, before) {
			t.Fatalf("%s: rejected read changed the terminal", name)
		}
	}
}

func TestCheckpointImportValidatesBeforeReplacingState(t *testing.T) {
	replica := replicaOf(t, 20, 5, "kept")
	before, _ := replica.term.Checkpoint()
	info, body := checkpointInfo(t, vtstate.New(20, 5), 9, 1)
	wrongVersion := info
	wrongVersion.Version = vtstate.Version + 1
	wrongEngine := info
	wrongEngine.EngineVersion += "+other"
	wrongSize := info
	wrongSize.Columns = 21
	cases := []struct {
		name string
		info CheckpointInfo
		body []byte
		want error
	}{
		{"partial download", info, body[:len(body)-1], ErrCheckpointIncomplete},
		{"altered byte", info, append(bytes.Clone(body[:len(body)-2]), '0', '}'), ErrCheckpointDigest},
		{"schema version", wrongVersion, body, vtstate.ErrIncompatible},
		{"engine version", wrongEngine, body, vtstate.ErrIncompatible},
		{"geometry metadata", wrongSize, body, vtstate.ErrInvalidCheckpoint},
	}
	for _, c := range cases {
		if err := replica.Refresh(c.info, c.body); !errors.Is(err, c.want) {
			t.Fatalf("%s: Refresh error = %v, want %v", c.name, err, c.want)
		}
		if after, _ := replica.term.Checkpoint(); !bytes.Equal(after, before) {
			t.Fatalf("%s: failed refresh changed the terminal", c.name)
		}
		if offset, ordinal := replica.Cursors(); offset != 4 || ordinal != 0 {
			t.Fatalf("%s: failed refresh moved the cursors to %d/%d", c.name, offset, ordinal)
		}
	}
	if err := replica.Refresh(info, body); err != nil {
		t.Fatalf("valid refresh: %v", err)
	}
	if offset, ordinal := replica.Cursors(); offset != 9 || ordinal != 1 {
		t.Fatalf("refresh cursors = %d/%d, want 9/1", offset, ordinal)
	}
}
