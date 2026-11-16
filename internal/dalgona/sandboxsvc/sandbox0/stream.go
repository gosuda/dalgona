package sandbox0

import (
	"errors"
	"fmt"
	"io"
	"log/slog"
	"sync"

	sdk "github.com/sandbox0-ai/sdk-go"
)

type ContextStream interface {
	SendInput(data, requestID string) error
	Recv() (sdk.StreamOutput, error)
	SendSignal(signal string) error
	Close() error
}

type Stream struct {
	stream    ContextStream
	log       *slog.Logger
	pending   string
	closeOnce sync.Once
	closeErr  error
}

func NewStream(stream ContextStream, logger *slog.Logger) *Stream {
	if logger == nil {
		logger = slog.Default()
	}
	return &Stream{stream: stream, log: logger}
}

func (s *Stream) Write(p []byte) (int, error) {
	if err := s.stream.SendInput(string(p), ""); err != nil {
		return 0, fmt.Errorf("sandbox0: send input: %w", err)
	}
	return len(p), nil
}

func (s *Stream) Read(p []byte) (int, error) {
	if len(p) == 0 {
		return 0, nil
	}
	if s.pending != "" {
		n := copy(p, s.pending)
		s.pending = s.pending[n:]
		return n, nil
	}
	output, err := s.nextOutput()
	if err != nil {
		return 0, err
	}
	n := copy(p, output.Data)
	s.pending = output.Data[n:]
	return n, nil
}

func (s *Stream) nextOutput() (sdk.StreamOutput, error) {
	for {
		output, err := s.stream.Recv()
		if errors.Is(err, io.EOF) {
			return sdk.StreamOutput{}, io.EOF
		}
		if err != nil {
			return sdk.StreamOutput{}, fmt.Errorf("sandbox0: receive output: %w", err)
		}
		switch output.Source {
		case "stderr":
			s.log.Debug("sandbox0 process stderr", "data", output.Data)
		case "done":
			return sdk.StreamOutput{}, io.EOF
		case "stdout":
			if output.Data != "" {
				return output, nil
			}
		}
	}
}

func (s *Stream) Close() error {
	s.closeOnce.Do(func() {
		var signalErr error
		if err := s.stream.SendSignal("TERM"); err != nil {
			signalErr = fmt.Errorf("sandbox0: send TERM: %w", err)
		}
		var closeErr error
		if err := s.stream.Close(); err != nil {
			closeErr = fmt.Errorf("sandbox0: close stream: %w", err)
		}
		s.closeErr = errors.Join(signalErr, closeErr)
	})
	return s.closeErr
}
