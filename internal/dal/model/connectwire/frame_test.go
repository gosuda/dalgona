package connectwire_test

import (
	"bytes"
	"encoding/binary"
	"errors"
	"fmt"
	"io"
	"math"
	"math/rand"
	"testing"
	"testing/iotest"

	"github.com/gosuda/dalgona/internal/dal/model/connectwire"
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

func frameBytes(size int) []byte {
	payload := make([]byte, size)
	for i := range payload {
		payload[i] = byte((i*31 + size + 7) & 0xff)
	}
	return payload
}

func mustFrameLen(t *testing.T, size int) uint32 {
	t.Helper()
	if size >= 0 && size <= math.MaxUint32 {
		return uint32(size)
	}
	t.Fatalf("frame size %d exceeds uint32 range", size)
	panic("unreachable")
}

func TestFrameRoundTripProperty(t *testing.T) {
	t.Parallel()
	const limit = 1024
	r := rand.New(rand.NewSource(1))
	sizes := []int{0, 1, 2, 7, 127, 255, 256, limit - 1, limit}
	for i := 0; i < 24; i++ {
		sizes = append(sizes, r.Intn(limit+1))
	}
	flags := []byte{0, connectwire.Compressed, connectwire.EndStream, connectwire.Compressed | connectwire.EndStream}
	wraps := []func(io.Reader) io.Reader{iotest.OneByteReader, iotest.HalfReader}
	for _, size := range sizes {
		checkFrameRoundTrip(t, size, flags, wraps)
	}
}

func checkFrameRoundTrip(t *testing.T, size int, flags []byte, wraps []func(io.Reader) io.Reader) {
	t.Helper()
	payload := frameBytes(size)
	for _, flag := range flags {
		checkFrameFlag(t, flag, payload, size, wraps)
	}
}

type frameCase struct {
	frame   []byte
	flag    byte
	payload []byte
	size    int
	length  uint32
	wraps   []func(io.Reader) io.Reader
}

func checkFrameFlag(t *testing.T, flag byte, payload []byte, size int, wraps []func(io.Reader) io.Reader) {
	t.Helper()
	frame := connectwire.AppendFrame(nil, flag, payload)
	length := mustFrameLen(t, size)
	if frame[0] != flag || binary.BigEndian.Uint32(frame[1:5]) != length || !bytes.Equal(frame[5:], payload) {
		t.Fatalf("frame header mismatch size %d flag %d", size, flag)
	}
	checkFrameWraps(t, frameCase{frame: frame, flag: flag, payload: payload, size: size, length: length, wraps: wraps})
	if size > 0 {
		checkFrameOverCap(t, frame, flag, size)
	}
}

func checkFrameWraps(t *testing.T, c frameCase) {
	t.Helper()
	for ri, wrap := range c.wraps {
		got, err := connectwire.ReadFrame(wrap(bytes.NewReader(c.frame)), nil, c.length)
		if err != nil {
			t.Fatalf("size %d flag %d reader %d: %v", c.size, c.flag, ri, err)
		}
		if got.Flags != c.flag || !bytes.Equal(got.Payload, c.payload) {
			t.Fatalf("round trip mismatch size %d flag %d reader %d", c.size, c.flag, ri)
		}
	}
}

func checkFrameOverCap(t *testing.T, frame []byte, flag byte, size int) {
	t.Helper()
	over := mustFrameLen(t, size-1)
	want := fmt.Sprintf("connect frame length %d exceeds %d", size, size-1)
	if _, err := connectwire.ReadFrame(bytes.NewReader(frame), nil, over); err == nil || err.Error() != want {
		t.Fatalf("over cap size %d flag %d error = %v, want %q", size, flag, err, want)
	}
}

func TestFrameCapRoundTripAndRefusal(t *testing.T) {
	t.Parallel()
	const frameCap = 16 << 20
	payload := make([]byte, frameCap)
	for i := range payload {
		payload[i] = byte((i * 7) & 0xff)
	}
	for _, flag := range []byte{0, connectwire.Compressed, connectwire.EndStream, connectwire.Compressed | connectwire.EndStream} {
		checkCapFlag(t, flag, payload, frameCap)
	}
	checkBigFrame(t, frameCap)
}

func checkCapFlag(t *testing.T, flag byte, payload []byte, frameCap int) {
	t.Helper()
	frame := connectwire.AppendFrame(nil, flag, payload)
	if binary.BigEndian.Uint32(frame[1:5]) != mustFrameLen(t, frameCap) {
		t.Fatalf("cap header = %x", frame[:5])
	}
	got, err := connectwire.ReadFrame(iotest.HalfReader(bytes.NewReader(frame)), nil, mustFrameLen(t, frameCap))
	if err != nil {
		t.Fatal(err)
	}
	if got.Flags != flag || !bytes.Equal(got.Payload, payload) {
		t.Fatalf("cap round trip flag %d mismatch", flag)
	}
	over := mustFrameLen(t, frameCap-1)
	want := fmt.Sprintf("connect frame length %d exceeds %d", frameCap, frameCap-1)
	if _, err := connectwire.ReadFrame(bytes.NewReader(frame), nil, over); err == nil || err.Error() != want {
		t.Fatalf("cap over limit error = %v, want %q", err, want)
	}
}

func checkBigFrame(t *testing.T, frameCap int) {
	t.Helper()
	big := connectwire.AppendFrame(nil, 0, make([]byte, frameCap+1))
	want := fmt.Sprintf("connect frame length %d exceeds %d", frameCap+1, frameCap)
	if _, err := connectwire.ReadFrame(bytes.NewReader(big), nil, mustFrameLen(t, frameCap)); err == nil || err.Error() != want {
		t.Fatalf("big frame error = %v, want %q", err, want)
	}
}
