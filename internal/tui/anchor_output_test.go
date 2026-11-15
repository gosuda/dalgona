package tui

import (
	"bytes"
	"errors"
	"fmt"
	"io"
	"testing"
	"testing/synctest"

	"github.com/charmbracelet/x/ansi"
)

func TestDockOutputStripsSplitMarkers(t *testing.T) {
	t.Parallel()
	payload := "frame" + dockWriteMarker(7) + "tail"
	for split := range len(payload) + 1 {
		t.Run(fmt.Sprint(split), func(t *testing.T) {
			checkDockSplit(t, payload, split)
		})
	}
}

func checkDockSplit(t *testing.T, payload string, split int) {
	synctest.Test(t, func(t *testing.T) {
		var wire bytes.Buffer
		written := make(chan dockWrittenMsg, 1)
		output := newDockOutput(&wire, nil, func(msg dockWrittenMsg) { written <- msg })
		for _, part := range []string{payload[:split], payload[split:]} {
			if n, err := io.WriteString(output, part); n != len(part) || err != nil {
				t.Fatalf("Write = (%d, %v), want (%d, nil)", n, err, len(part))
			}
		}
		if message := <-written; message.revision != 7 {
			t.Fatalf("write receipt = %v", message)
		}
		if wire.String() != "frametail" {
			t.Fatalf("wire = %q, want frametail", wire.String())
		}
	})
}

func TestDockOutputAcknowledgesAfterWireWrite(t *testing.T) {
	t.Parallel()
	synctest.Test(t, func(t *testing.T) {
		reader, writer := io.Pipe()
		defer reader.Close()
		defer writer.Close()
		written := make(chan dockWrittenMsg, 1)
		output := newDockOutput(writer, nil, func(msg dockWrittenMsg) { written <- msg })
		done := make(chan error, 1)
		go func() {
			_, err := io.WriteString(output, "frame"+dockWriteMarker(7)+"tail")
			done <- err
		}()
		frame := make([]byte, 5)
		if _, err := io.ReadFull(reader, frame); err != nil {
			t.Fatal(err)
		}
		select {
		case message := <-written:
			t.Fatalf("premature wire receipt %v", message)
		default:
		}
		tail := make([]byte, 4)
		if _, err := io.ReadFull(reader, tail); err != nil {
			t.Fatal(err)
		}
		if err := <-done; err != nil {
			t.Fatal(err)
		}
		if string(frame)+string(tail) != "frametail" {
			t.Fatalf("wire contains marker bytes: %q %q", frame, tail)
		}
		if message := <-written; message.revision != 7 {
			t.Fatalf("wire receipt = %v", message)
		}
	})
}

type limitedDockWriter struct {
	remaining int
}

func (writer *limitedDockWriter) Write(p []byte) (int, error) {
	n := min(len(p), writer.remaining)
	writer.remaining -= n
	return n, nil
}

func TestDockOutputReturnsOriginalByteCountsOnShortWrite(t *testing.T) {
	t.Parallel()
	marker := dockWriteMarker(7)
	for _, limit := range []int{2, 7} {
		written := make(chan dockWrittenMsg, 1)
		output := newDockOutput(&limitedDockWriter{remaining: limit}, nil, func(msg dockWrittenMsg) { written <- msg })
		n, err := io.WriteString(output, "frame"+marker+"tail")
		want := limit
		if limit > len("frame") {
			want += len(marker)
		}
		if n != want || !errors.Is(err, io.ErrShortWrite) {
			t.Fatalf("limit %d: Write = (%d, %v), want (%d, short write)", limit, n, err, want)
		}
	}
}

func TestDockOutputQueriesOnlyEnabledInput(t *testing.T) {
	t.Parallel()
	for _, input := range []io.Reader{nil, bytes.NewReader(nil)} {
		var wire bytes.Buffer
		written := make(chan dockWrittenMsg, 1)
		output := newDockOutput(&wire, input, func(msg dockWrittenMsg) { written <- msg })
		if _, err := io.WriteString(output, dockQueryMarker); err != nil {
			t.Fatal(err)
		}
		want := ""
		if input != nil {
			want = ansi.RequestCursorPositionReport
		}
		if wire.String() != want {
			t.Fatalf("query wire = %q, want %q", wire.String(), want)
		}
	}
}
