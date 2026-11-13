package connectwire_test

import (
	"bytes"
	"encoding/binary"
	"errors"
	"io"
	"testing"
	"testing/iotest"

	"github.com/gosuda/dalgona/internal/model/connectwire"
)

func TestEnvelopeSplitReads(t *testing.T) {
	t.Parallel()
	payload := []byte("split across every byte")
	frame := connectwire.AppendFrame(nil, connectwire.EndStream, payload)
	got, err := connectwire.ReadFrame(iotest.OneByteReader(bytes.NewReader(frame)), nil, 1024)
	if err != nil {
		t.Fatal(err)
	}
	if got.Flags != connectwire.EndStream || !bytes.Equal(got.Payload, payload) {
		t.Fatalf("frame = %#v", got)
	}
	if connectwire.EndStream != 2 || connectwire.Compressed != 1 || !bytes.Equal(frame[:5], []byte{2, 0, 0, 0, byte(len(payload) & 0xff)}) {
		t.Fatalf("header = %x", frame[:5])
	}
}

func TestEnvelopeRejectsShortAndOversizedFrames(t *testing.T) {
	t.Parallel()
	payload := []byte("split across every byte")
	frame := connectwire.AppendFrame(nil, 0, payload)
	if _, err := connectwire.ReadFrame(bytes.NewReader(frame[:len(frame)-1]), nil, 1024); !errors.Is(err, io.ErrUnexpectedEOF) {
		t.Fatalf("truncated payload error = %v", err)
	}
	if _, err := connectwire.ReadFrame(bytes.NewReader(frame[:4]), nil, 1024); !errors.Is(err, io.ErrUnexpectedEOF) {
		t.Fatalf("truncated header error = %v", err)
	}
	if _, err := connectwire.ReadFrame(bytes.NewReader(frame), nil, binary.BigEndian.Uint32(frame[1:])-1); err == nil {
		t.Fatal("oversized frame was accepted")
	}
}

func TestEnvelopeLargeFrameReusesBuffer(t *testing.T) {
	t.Parallel()
	const size = 70000
	payload := make([]byte, size)
	for i := range payload {
		payload[i] = byte(i * 7)
	}
	frame := connectwire.AppendFrame(nil, connectwire.Compressed, payload)
	want := binary.BigEndian.AppendUint32([]byte{connectwire.Compressed}, uint32(size))
	if !bytes.Equal(frame[:5], want) {
		t.Fatalf("header = %x, want %x", frame[:5], want)
	}
	buf := make([]byte, 0, size)
	got, err := connectwire.ReadFrame(iotest.HalfReader(bytes.NewReader(frame)), buf, size)
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(got.Payload, payload) || &got.Payload[0] != &buf[:1][0] {
		t.Fatal("payload differs or did not reuse the caller buffer")
	}
}

func TestTrailerError(t *testing.T) {
	t.Parallel()
	for _, tc := range []struct{ payload, code, message string }{
		{`{"error":{"code":"invalid_argument","message":"bad input","details":[{"type":"x"}]}}`, "invalid_argument", "bad input"},
		{`{"error":{"code":"resource_exhausted","message":"too large","details":[{"type":"x"}]}}`, "resource_exhausted", "too large"},
	} {
		err := connectwire.TrailerError([]byte(tc.payload))
		connectErr, ok := errors.AsType[*connectwire.ConnectError](err)
		if !ok || connectErr.Code != tc.code || connectErr.Message != tc.message {
			t.Fatalf("error = %#v", err)
		}
	}
	if err := connectwire.TrailerError([]byte(`{}`)); err != nil {
		t.Fatalf("clean trailer: %v", err)
	}
	for _, payload := range []string{`{"error":{"code":7}}`, `{"error":{"message":7}}`, `{"error":`, `{} {}`, `{}x`} {
		if err := connectwire.TrailerError([]byte(payload)); err == nil {
			t.Fatalf("%s was accepted", payload)
		}
	}
}
