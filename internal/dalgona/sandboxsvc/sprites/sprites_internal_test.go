package sprites

import (
	"io"
	"testing"

	flysprites "github.com/superfly/sprites-go"
)

func TestStreamReadEOFBeforeWait(t *testing.T) {
	t.Parallel()
	stdin, stdinWriter := io.Pipe()
	stdout, stdoutWriter := io.Pipe()
	stream := &stream{
		input:        stdinWriter,
		output:       stdout,
		outputWriter: stdoutWriter,
		done:         make(chan struct{}),
	}
	t.Cleanup(func() {
		_ = stdin.Close()
		_ = stream.Close()
	})
	wait := func() error {
		if _, writeErr := stdoutWriter.Write([]byte("hi\n")); writeErr != nil {
			return writeErr
		}
		return &flysprites.ExitError{Code: 3}
	}
	go stream.await(wait, func() int { return 3 })
	output, readErr := io.ReadAll(stream)
	if readErr != nil {
		t.Fatalf("read output: %v", readErr)
	}
	if string(output) != "hi\n" {
		t.Fatalf("output %q, want %q", output, "hi\n")
	}
	code, waitErr := stream.Wait()
	if waitErr != nil {
		t.Fatalf("wait: %v", waitErr)
	}
	if code != 3 {
		t.Fatalf("exit code %d, want 3", code)
	}
	secondCode, secondErr := stream.Wait()
	if secondErr != nil || secondCode != code {
		t.Fatalf("second Wait() = (%d, %v), want (%d, nil)", secondCode, secondErr, code)
	}
}
