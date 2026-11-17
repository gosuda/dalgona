package connectwire

import (
	"bytes"
	"encoding/binary"
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"io"
	"math"
	"net/http"
)

const (
	Compressed byte = 1
	EndStream  byte = 2
)

type Frame struct {
	Flags   byte
	Payload []byte
}

func AppendFrame(dst []byte, flags byte, payload []byte) []byte {
	if uint64(len(payload)) > math.MaxUint32 {
		panic("connect frame payload exceeds uint32")
	}
	n := uint64(len(payload))
	dst = append(dst, flags, byte((n>>24)&0xff), byte((n>>16)&0xff), byte((n>>8)&0xff), byte(n&0xff))
	return append(dst, payload...)
}

func ReadFrame(r io.Reader, buf []byte, limit uint32) (Frame, error) {
	var h [5]byte
	if _, err := io.ReadFull(r, h[:]); err != nil {
		return Frame{}, fmt.Errorf("connect frame header: %w", err)
	}
	n := binary.BigEndian.Uint32(h[1:])
	if n > limit {
		return Frame{}, fmt.Errorf("connect frame length %d exceeds %d", n, limit)
	}
	if uint64(cap(buf)) < uint64(n) {
		buf = make([]byte, n)
	}
	p := buf[:n]
	if _, err := io.ReadFull(r, p); err != nil {
		return Frame{}, fmt.Errorf("connect frame payload: %w", err)
	}
	return Frame{Flags: h[0], Payload: p}, nil
}

type ConnectError struct {
	Code    string
	Message string
}

func (e *ConnectError) Error() string {
	if e.Code == "" {
		return e.Message
	}
	if e.Message == "" {
		return e.Code
	}
	return e.Code + ": " + e.Message
}

func TrailerError(payload []byte) error {
	if len(bytes.TrimSpace(payload)) == 0 {
		return nil
	}
	out, err := readTrailer(jsontext.NewDecoder(bytes.NewReader(payload)))
	if err != nil {
		return err
	}
	if out.Code == "" && out.Message == "" {
		return nil
	}
	return &out
}

func readTrailer(d *jsontext.Decoder) (ConnectError, error) {
	var out ConnectError
	t, err := d.ReadToken()
	if err != nil {
		return out, fmt.Errorf("connect trailer: %w", err)
	}
	if t.Kind() != '{' {
		return out, errors.New("connect trailer is not an object")
	}
	if err = readTrailerFields(d, &out); err != nil {
		return out, err
	}
	return out, requireTrailerEnd(d)
}

func requireTrailerEnd(d *jsontext.Decoder) error {
	_, err := d.ReadToken()
	if errors.Is(err, io.EOF) {
		return nil
	}
	if err != nil {
		return fmt.Errorf("connect trailer end: %w", err)
	}
	return errors.New("connect trailer has trailing JSON")
}

func readTrailerFields(d *jsontext.Decoder, out *ConnectError) error {
	for d.PeekKind() != '}' {
		key, err := d.ReadToken()
		if err != nil {
			return fmt.Errorf("connect trailer key: %w", err)
		}
		if err := readTrailerValue(d, key.String(), out); err != nil {
			return err
		}
	}
	if _, err := d.ReadToken(); err != nil {
		return fmt.Errorf("connect trailer end: %w", err)
	}
	return nil
}

func readTrailerValue(d *jsontext.Decoder, key string, out *ConnectError) error {
	if key == "error" && d.PeekKind() == '{' {
		if _, err := d.ReadToken(); err != nil {
			return fmt.Errorf("connect trailer error: %w", err)
		}
		return readTrailerErrorObject(d, out)
	}
	if d.PeekKind() == '"' && (key == "code" || key == "message") {
		return readTrailerString(d, key, out)
	}
	if _, err := d.ReadValue(); err != nil {
		return fmt.Errorf("connect trailer value: %w", err)
	}
	return nil
}

func readTrailerErrorObject(d *jsontext.Decoder, out *ConnectError) error {
	for d.PeekKind() != '}' {
		key, err := d.ReadToken()
		if err != nil {
			return fmt.Errorf("connect trailer error key: %w", err)
		}
		name := key.String()
		if name != "code" && name != "message" {
			if _, err := d.ReadValue(); err != nil {
				return fmt.Errorf("connect trailer error value: %w", err)
			}
			continue
		}
		if d.PeekKind() != '"' {
			return fmt.Errorf("connect trailer %s is not a string", name)
		}
		if err := readTrailerString(d, name, out); err != nil {
			return err
		}
	}
	if _, err := d.ReadToken(); err != nil {
		return fmt.Errorf("connect trailer error end: %w", err)
	}
	return nil
}

func readTrailerString(d *jsontext.Decoder, key string, out *ConnectError) error {
	t, err := d.ReadToken()
	if err != nil {
		return fmt.Errorf("connect trailer %s: %w", key, err)
	}
	if key == "code" {
		out.Code = t.String()
	} else {
		out.Message = t.String()
	}
	return nil
}

var connectHTTPStatus = map[string]int{
	"canceled":            499,
	"unknown":             http.StatusInternalServerError,
	"invalid_argument":    http.StatusBadRequest,
	"deadline_exceeded":   http.StatusGatewayTimeout,
	"not_found":           http.StatusNotFound,
	"already_exists":      http.StatusConflict,
	"permission_denied":   http.StatusForbidden,
	"resource_exhausted":  http.StatusTooManyRequests,
	"failed_precondition": http.StatusBadRequest,
	"aborted":             http.StatusConflict,
	"out_of_range":        http.StatusBadRequest,
	"unimplemented":       http.StatusNotImplemented,
	"internal":            http.StatusInternalServerError,
	"unavailable":         http.StatusServiceUnavailable,
	"data_loss":           http.StatusInternalServerError,
	"unauthenticated":     http.StatusUnauthorized,
}

func CodeStatus(code string) int {
	return connectHTTPStatus[code]
}

func ErrorStatus(err error) int {
	if connectErr, ok := errors.AsType[*ConnectError](err); ok {
		return CodeStatus(connectErr.Code)
	}
	return 0
}
