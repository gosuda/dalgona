package tui

import (
	"bytes"
	"fmt"
	"io"
	"strconv"
	"sync"

	"github.com/charmbracelet/x/ansi"
	"github.com/charmbracelet/x/term"
)

const dockMarkerPrefix = "\x00dalgona-dock:"

const dockQueryMarker = dockMarkerPrefix + "query\x00"

type dockWrittenMsg struct {
	revision uint64
}

type dockOutput struct {
	writer  io.Writer
	written func(dockWrittenMsg)
	mu      sync.Mutex
	marker  [64]byte
	markerN int
	queries bool
}

type terminalDockOutput struct {
	term.File
	dock *dockOutput
}

func (output terminalDockOutput) Write(p []byte) (int, error) {
	return output.dock.Write(p)
}

func newDockOutput(writer io.Writer, input io.Reader, written func(dockWrittenMsg)) io.Writer {
	output := &dockOutput{writer: writer, written: written, queries: input != nil}
	if terminal, ok := writer.(term.File); ok {
		return terminalDockOutput{File: terminal, dock: output}
	}
	return output
}

func dockWriteMarker(revision uint64) string {
	return dockMarkerPrefix + strconv.FormatUint(revision, 10) + "\x00"
}

func (output *dockOutput) Write(p []byte) (int, error) {
	output.mu.Lock()
	n, revision, err := output.write(p)
	output.mu.Unlock()
	if revision != 0 {
		go output.written(dockWrittenMsg{revision: revision})
	}
	return n, err
}

func (output *dockOutput) write(p []byte) (consumed int, revision uint64, err error) {
	for consumed < len(p) {
		if output.markerN > 0 {
			value, markerErr := output.markerByte(p[consumed])
			consumed++
			if markerErr != nil {
				return consumed, revision, markerErr
			}
			if value != 0 {
				revision = value
			}
			continue
		}
		if p[consumed] == 0 {
			output.marker[0], output.markerN = 0, 1
			consumed++
			continue
		}
		next, writeErr := output.writeSegment(p, consumed)
		consumed = next
		if writeErr != nil {
			return consumed, revision, writeErr
		}
	}
	return consumed, revision, nil
}

func (output *dockOutput) writeSegment(p []byte, consumed int) (int, error) {
	segment, _, _ := bytes.Cut(p[consumed:], []byte{0})
	n, writeErr := output.writer.Write(segment)
	consumed += n
	if writeErr != nil {
		return consumed, writeErr
	}
	if n != len(segment) {
		return consumed, io.ErrShortWrite
	}
	return consumed, nil
}

func (output *dockOutput) markerByte(value byte) (uint64, error) {
	if output.markerN == len(output.marker) {
		return 0, fmt.Errorf("dock write marker exceeds %d bytes", len(output.marker))
	}
	output.marker[output.markerN] = value
	output.markerN++
	if value != 0 {
		return 0, nil
	}
	token, found := bytes.CutPrefix(output.marker[:output.markerN-1], []byte(dockMarkerPrefix))
	output.markerN = 0
	if !found {
		return 0, fmt.Errorf("invalid dock write marker prefix")
	}
	if bytes.Equal(token, []byte("query")) {
		return 0, output.query()
	}
	revision, err := strconv.ParseUint(string(token), 10, 64)
	if err != nil || revision == 0 {
		return 0, fmt.Errorf("invalid dock write marker revision %q", token)
	}
	return revision, nil
}

func (output *dockOutput) query() error {
	if !output.queries {
		return nil
	}
	n, err := io.WriteString(output.writer, ansi.RequestCursorPositionReport)
	if err != nil {
		return err
	}
	if n != len(ansi.RequestCursorPositionReport) {
		return io.ErrShortWrite
	}
	return nil
}
