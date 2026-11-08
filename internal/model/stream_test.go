package model

import (
	"bytes"
	"errors"
	"io"
	"net/http"
	"strings"
	"testing"
	"testing/synctest"
	"time"
)

func TestReadSSE(t *testing.T) {
	t.Parallel()
	input := ": comment\r\nevent: one\r\ndata: first\r\ndata: second\n\n" +
		"data: {\"value\":1}\n\n{\"raw\":true}\n"
	var events []string
	var values []string
	err := ReadSSE(t.Context(), strings.NewReader(input), func(event, data []byte) error {
		events = append(events, string(event))
		values = append(values, string(data))
		return nil
	})
	if err != nil {
		t.Fatalf("ReadSSE() error = %v", err)
	}
	wantEvents := []string{"one", "", ""}
	wantValues := []string{"first\nsecond", `{"value":1}`, `{"raw":true}`}
	for i := range wantValues {
		if events[i] != wantEvents[i] || values[i] != wantValues[i] {
			t.Fatalf("event %d = %q %q, want %q %q", i, events[i], values[i], wantEvents[i], wantValues[i])
		}
	}
}

func TestReadSSEWithTimeoutsStartDeadlineEndsAtFirstEvent(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		body := io.NopCloser(strings.NewReader("data: first\n\ndata: second\n\n"))
		var values []string
		start := time.Now()
		err := ReadSSEWithTimeouts(t.Context(), body, SSEOptions{StartTimeout: 20 * time.Millisecond}, func(_, data []byte) error {
			values = append(values, string(data))
			if len(values) == 1 {
				time.Sleep(40 * time.Millisecond)
			}
			return nil
		})
		if err != nil || len(values) != 2 {
			t.Fatalf("ReadSSEWithTimeouts() = %v, values %v", err, values)
		}
		if time.Since(start) < 40*time.Millisecond {
			t.Fatal("test did not cross the start deadline")
		}
	})
}

func TestReadSSEWithTimeoutsStartDeadline(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		reader, writer := io.Pipe()
		t.Cleanup(func() { _ = reader.Close(); _ = writer.Close() })
		start := time.Now()
		err := ReadSSEWithTimeouts(t.Context(), reader, SSEOptions{StartTimeout: 20 * time.Millisecond}, func(_, _ []byte) error { return nil })
		if !errors.Is(err, ErrStreamStartTimeout) {
			t.Fatalf("error = %v, want ErrStreamStartTimeout", err)
		}
		if time.Since(start) > time.Second {
			t.Fatal("start timeout did not fire promptly")
		}
	})
}

func TestReadSSEWithTimeoutsIdleDeadlineAndPause(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		reader, writer := io.Pipe()
		t.Cleanup(func() { _ = reader.Close(); _ = writer.Close() })
		control := NewStreamTimers()
		control.PauseIdle()
		go func() {
			_, _ = io.WriteString(writer, "data: first\n\n")
			time.Sleep(40 * time.Millisecond)
			control.ResumeIdle()
		}()
		err := ReadSSEWithTimeouts(t.Context(), reader, SSEOptions{IdleTimeout: 20 * time.Millisecond, Control: control}, func(_, _ []byte) error { return nil })
		if !errors.Is(err, ErrStreamIdleTimeout) {
			t.Fatalf("error = %v, want ErrStreamIdleTimeout", err)
		}
	})
}

func TestRetryDelay(t *testing.T) {
	t.Parallel()
	if got := RetryDelay(0, 0, func() float64 { return 0 }); got != 800*time.Millisecond {
		t.Fatalf("minimum first delay = %v", got)
	}
	if got := RetryDelay(2, 0, func() float64 { return 1 }); got != 4800*time.Millisecond {
		t.Fatalf("maximum third delay = %v", got)
	}
	if got := RetryDelay(0, 5*time.Second, nil); got != 5*time.Second {
		t.Fatalf("Retry-After delay = %v", got)
	}
}

func TestRetryAfter(t *testing.T) {
	t.Parallel()
	if got := RetryAfter(" 3 "); got != 3*time.Second {
		t.Fatalf("RetryAfter(seconds) = %v", got)
	}
	date := time.Now().Add(4 * time.Second).UTC().Format(http.TimeFormat)
	if got := RetryAfter(date); got < 2*time.Second || got > 5*time.Second {
		t.Fatalf("RetryAfter(date) = %v", got)
	}
}

func TestReadSSEHandlerError(t *testing.T) {
	t.Parallel()
	want := errors.New("invalid event")
	err := ReadSSE(t.Context(), bytes.NewBufferString("data: x\n\n"), func(_, _ []byte) error { return want })
	if !errors.Is(err, want) {
		t.Fatalf("error = %v, want %v", err, want)
	}
}
