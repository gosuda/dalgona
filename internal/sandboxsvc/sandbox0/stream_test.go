package sandbox0

import (
	"bytes"
	"errors"
	"io"
	"log/slog"
	"reflect"
	"testing"

	sdk "github.com/sandbox0-ai/sdk-go"
)

type fakeContextStream struct {
	inputs       []string
	outputs      []sdk.StreamOutput
	signals      []string
	events       []string
	recvCount    int
	recvErr      error
	sendInputErr error
	signalErr    error
	closeErr     error
}

func (f *fakeContextStream) SendInput(data, requestID string) error {
	f.inputs = append(f.inputs, data+":"+requestID)
	return f.sendInputErr
}

func (f *fakeContextStream) Recv() (sdk.StreamOutput, error) {
	f.recvCount++
	if len(f.outputs) == 0 {
		if f.recvErr != nil {
			return sdk.StreamOutput{}, f.recvErr
		}
		return sdk.StreamOutput{}, io.EOF
	}
	output := f.outputs[0]
	f.outputs = f.outputs[1:]
	return output, nil
}

func (f *fakeContextStream) SendSignal(signal string) error {
	f.signals = append(f.signals, signal)
	f.events = append(f.events, "signal:"+signal)
	return f.signalErr
}

func (f *fakeContextStream) Close() error {
	f.events = append(f.events, "close")
	return f.closeErr
}

func TestStreamWriteSendsEachBufferOnce(t *testing.T) {
	remote := &fakeContextStream{}
	stream := NewStream(remote, nil)
	for _, input := range []string{"first\n", "second\n"} {
		n, err := stream.Write([]byte(input))
		if err != nil {
			t.Fatalf("Write(%q): %v", input, err)
		}
		if n != len(input) {
			t.Fatalf("Write(%q) wrote %d bytes", input, n)
		}
	}
	want := []string{"first\n:", "second\n:"}
	if !reflect.DeepEqual(remote.inputs, want) {
		t.Fatalf("inputs = %#v, want %#v", remote.inputs, want)
	}
}

func TestStreamReadFiltersAndBuffersStdout(t *testing.T) {
	remote := &fakeContextStream{outputs: []sdk.StreamOutput{
		{Source: "stdout", Data: "hel"},
		{Source: "stderr", Data: "diagnostic"},
		{Source: "stdout", Data: "lo"},
		{Source: "done"},
	}}
	var logs bytes.Buffer
	logger := slog.New(slog.NewTextHandler(&logs, &slog.HandlerOptions{Level: slog.LevelDebug}))
	stream := NewStream(remote, logger)
	prefix := make([]byte, 2)
	n, err := stream.Read(prefix)
	if err != nil || string(prefix[:n]) != "he" {
		t.Fatalf("first Read = %q, %v", prefix[:n], err)
	}
	rest, err := io.ReadAll(stream)
	if err != nil {
		t.Fatalf("ReadAll: %v", err)
	}
	if got := string(prefix[:n]) + string(rest); got != "hello" {
		t.Fatalf("output = %q, want hello", got)
	}
	if bytes.Contains(rest, []byte("diagnostic")) {
		t.Fatal("stderr reached stdout reader")
	}
	if !bytes.Contains(logs.Bytes(), []byte("diagnostic")) {
		t.Fatal("stderr was not logged")
	}
}

func TestStreamReadEmptyBufferDoesNotReceive(t *testing.T) {
	remote := &fakeContextStream{recvErr: errors.New("unexpected receive")}
	stream := NewStream(remote, nil)
	n, err := stream.Read(nil)
	if n != 0 || err != nil {
		t.Fatalf("Read(nil) = %d, %v", n, err)
	}
	if remote.recvCount != 0 {
		t.Fatal("Read(nil) called Recv")
	}
}

func TestStreamCloseSignalsBeforeClosingOnce(t *testing.T) {
	remote := &fakeContextStream{}
	stream := NewStream(remote, nil)
	if err := stream.Close(); err != nil {
		t.Fatalf("Close: %v", err)
	}
	if err := stream.Close(); err != nil {
		t.Fatalf("second Close: %v", err)
	}
	want := []string{"signal:TERM", "close"}
	if !reflect.DeepEqual(remote.events, want) {
		t.Fatalf("events = %#v, want %#v", remote.events, want)
	}
}

func TestStreamCloseAfterSignalFailure(t *testing.T) {
	signalErr := errors.New("signal failed")
	remote := &fakeContextStream{signalErr: signalErr}
	stream := NewStream(remote, nil)
	err := stream.Close()
	if !errors.Is(err, signalErr) {
		t.Fatalf("Close error = %v, want signal failure", err)
	}
	want := []string{"signal:TERM", "close"}
	if !reflect.DeepEqual(remote.events, want) {
		t.Fatalf("events = %#v, want %#v", remote.events, want)
	}
}
