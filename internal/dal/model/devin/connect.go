package devin

import (
	"bytes"
	"compress/gzip"
	"errors"
	"fmt"
	"io"

	"github.com/gosuda/dalgona/internal/dal/model/connectwire"
)

const maxFramePayload = 16 << 20

func connectStatus(err error, fallback int) int {
	if status := connectwire.ErrorStatus(err); status != 0 {
		return status
	}
	return fallback
}

type inflater struct {
	reader *gzip.Reader
	out    bytes.Buffer
}

func (f *inflater) payload(frame connectwire.Frame) ([]byte, error) {
	if frame.Flags&connectwire.Compressed == 0 {
		return frame.Payload, nil
	}
	source := bytes.NewReader(frame.Payload)
	if f.reader == nil {
		reader, err := gzip.NewReader(source)
		if err != nil {
			return nil, fmt.Errorf("gunzip frame: %w", err)
		}
		f.reader = reader
	} else if err := f.reader.Reset(source); err != nil {
		return nil, fmt.Errorf("gunzip frame: %w", err)
	}
	f.out.Reset()
	written, err := f.out.ReadFrom(io.LimitReader(f.reader, maxFramePayload+1))
	if err != nil {
		return nil, fmt.Errorf("gunzip frame: %w", err)
	}
	if written > maxFramePayload {
		return nil, errors.New("gunzipped frame exceeds 16 MiB")
	}
	return f.out.Bytes(), nil
}

func gzipFrame(payload []byte) ([]byte, error) {
	var compressed bytes.Buffer
	compressed.Grow(len(payload)/2 + 64)
	writer := gzip.NewWriter(&compressed)
	if _, err := writer.Write(payload); err != nil {
		_ = writer.Close()
		return nil, fmt.Errorf("gzip request: %w", err)
	}
	if err := writer.Close(); err != nil {
		return nil, fmt.Errorf("gzip request: %w", err)
	}
	if compressed.Len() > maxFramePayload {
		return nil, errors.New("request frame exceeds 16 MiB")
	}
	return connectwire.AppendFrame(nil, connectwire.Compressed, compressed.Bytes()), nil
}
