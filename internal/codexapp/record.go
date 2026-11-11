package codexapp

import (
	"bytes"
	"context"
	"encoding/json/jsontext"
	"errors"
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"
	"sync"
	"time"

	"github.com/gosuda/dalgona/internal/auth"
)

const (
	recordPrefix      = "dalgona codex record: "
	recordChunkBuffer = 32 << 10
	directionToServer = "c2s"
	directionToClient = "s2c"
)

type censusLog struct {
	file *os.File
	mu   sync.Mutex
	err  error
}

type lineObserver struct {
	census  *censusLog
	dir     string
	pending []byte
	reader  bytes.Reader
	decoder *jsontext.Decoder
	encoder *jsontext.Encoder
	record  bytes.Buffer
	name    []byte
	method  []byte
	id      []byte
}

func RecordMain(ctx context.Context, args []string, stdio io.ReadWriter, stderr io.Writer, home string) int {
	realPath, realArgs, err := parseRecordArgs(args)
	if err != nil {
		fmt.Fprintf(stderr, "%s%v\n", recordPrefix, err)
		return 2
	}
	census, err := openCensus(home, time.Now())
	if err != nil {
		fmt.Fprintf(stderr, "%s%v\n", recordPrefix, err)
		return 1
	}
	fmt.Fprintf(stderr, "%scensus %s\n", recordPrefix, census.file.Name())
	code, runErr := runRecorded(ctx, exec.CommandContext(ctx, realPath, realArgs...), stdio, stderr, census)
	err = errors.Join(runErr, census.close())
	if err != nil {
		fmt.Fprintf(stderr, "%s%v\n", recordPrefix, err)
		return max(code, 1)
	}
	return code
}

func parseRecordArgs(args []string) (string, []string, error) {
	realArgs := make([]string, 0, len(args))
	realPath := ""
	for i := 0; i < len(args); i++ {
		if value, ok := strings.CutPrefix(args[i], "--real="); ok {
			realPath = value
			continue
		}
		if args[i] != "--real" {
			realArgs = append(realArgs, args[i])
			continue
		}
		if i+1 >= len(args) {
			return "", nil, errors.New("--real requires a value")
		}
		i++
		realPath = args[i]
	}
	if realPath == "" {
		return "", nil, errors.New("--real <path-to-codex> is required")
	}
	if len(realArgs) == 0 {
		realArgs = append(realArgs, "app-server")
	}
	return realPath, realArgs, nil
}

func openCensus(home string, now time.Time) (*censusLog, error) {
	dir := filepath.Join(home, "census")
	if homeErr := auth.ProtectHomeDir(home); homeErr != nil {
		return nil, fmt.Errorf("create census directory: %w", homeErr)
	}
	if err := os.MkdirAll(dir, 0o700); err != nil {
		return nil, fmt.Errorf("create census directory: %w", err)
	}
	path := filepath.Join(dir, strconv.FormatInt(now.Unix(), 10)+".jsonl")
	file, err := os.OpenFile(path, os.O_WRONLY|os.O_CREATE|os.O_APPEND, 0o600)
	if err != nil {
		return nil, fmt.Errorf("open census file: %w", err)
	}
	return &censusLog{file: file}, nil
}

func (c *censusLog) close() error {
	closeErr := c.file.Close()
	if closeErr != nil {
		closeErr = fmt.Errorf("close census file: %w", closeErr)
	}
	c.mu.Lock()
	defer c.mu.Unlock()
	return errors.Join(c.err, closeErr)
}

func (c *censusLog) fail(err error) {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.err == nil {
		c.err = err
	}
}

func (o *lineObserver) appendRecord() {
	o.record.Reset()
	o.encoder.Reset(&o.record)
	if err := encodeCensusRecord(o.encoder, o.dir, o.method, o.id); err != nil {
		o.census.fail(fmt.Errorf("encode census record: %w", err))
		return
	}
	if _, err := o.census.file.Write(o.record.Bytes()); err != nil {
		o.census.fail(fmt.Errorf("write census record: %w", err))
	}
}

func encodeCensusRecord(encoder *jsontext.Encoder, dir string, method, id jsontext.Value) error {
	err := errors.Join(
		encoder.WriteToken(jsontext.BeginObject),
		encoder.WriteToken(jsontext.String("dir")),
		encoder.WriteToken(jsontext.String(dir)),
	)
	if len(method) > 0 {
		err = errors.Join(err, encoder.WriteToken(jsontext.String("method")), encoder.WriteValue(method))
	}
	if len(id) > 0 {
		err = errors.Join(err, encoder.WriteToken(jsontext.String("id")), encoder.WriteValue(id))
	}
	return errors.Join(err, encoder.WriteToken(jsontext.EndObject))
}

func runRecorded(ctx context.Context, cmd *exec.Cmd, stdio io.ReadWriter, stderr io.Writer, census *censusLog) (int, error) {
	cmd.Stderr = stderr
	childIn, err := cmd.StdinPipe()
	if err != nil {
		return 1, fmt.Errorf("open stdin of %s: %w", cmd.Path, err)
	}
	childOut, err := cmd.StdoutPipe()
	if err != nil {
		return 1, fmt.Errorf("open stdout of %s: %w", cmd.Path, err)
	}
	if err := cmd.Start(); err != nil {
		return 1, fmt.Errorf("start %s: %w", cmd.Path, err)
	}
	go func() {
		_ = teeLines(childIn, stdio, newLineObserver(census, directionToServer))
		_ = childIn.Close()
	}()
	relayErr := teeLines(stdio, childOut, newLineObserver(census, directionToClient))
	if relayErr != nil {
		_ = cmd.Process.Kill()
	}
	return childExit(ctx, cmd.Wait(), relayErr)
}

func childExit(ctx context.Context, waitErr, relayErr error) (int, error) {
	if relayErr != nil {
		return 1, fmt.Errorf("relay output: %w", relayErr)
	}
	if waitErr == nil {
		return 0, nil
	}
	if exitErr, ok := errors.AsType[*exec.ExitError](waitErr); ok && exitErr.ExitCode() >= 0 {
		return exitErr.ExitCode(), nil
	}
	if ctx.Err() != nil {
		return 1, nil
	}
	return 1, fmt.Errorf("wait for real binary: %w", waitErr)
}

func newLineObserver(census *censusLog, dir string) *lineObserver {
	observer := &lineObserver{census: census, dir: dir}
	observer.decoder = jsontext.NewDecoder(&observer.reader)
	observer.encoder = jsontext.NewEncoder(&observer.record)
	return observer
}

func teeLines(dst io.Writer, src io.Reader, observer *lineObserver) error {
	chunk := make([]byte, recordChunkBuffer)
	for {
		n, readErr := src.Read(chunk)
		if n > 0 {
			observer.feed(chunk[:n])
			if _, err := dst.Write(chunk[:n]); err != nil {
				return fmt.Errorf("write %s: %w", observer.dir, err)
			}
		}
		if readErr == nil {
			continue
		}
		observer.finish()
		if errors.Is(readErr, io.EOF) || errors.Is(readErr, os.ErrClosed) {
			return nil
		}
		return fmt.Errorf("read %s: %w", observer.dir, readErr)
	}
}

func (o *lineObserver) feed(chunk []byte) {
	for {
		end := bytes.IndexByte(chunk, '\n')
		if end < 0 {
			o.pending = append(o.pending, chunk...)
			return
		}
		line := chunk[:end]
		if len(o.pending) > 0 {
			o.pending = append(o.pending, line...)
			line = o.pending
		}
		o.observe(line)
		o.pending = o.pending[:0]
		chunk = chunk[end+1:]
	}
}

func (o *lineObserver) finish() {
	if len(o.pending) > 0 {
		o.observe(o.pending)
		o.pending = o.pending[:0]
	}
}

func (o *lineObserver) observe(line []byte) {
	if o.parse(line) && (len(o.method) > 0 || len(o.id) > 0) {
		o.appendRecord()
	}
}

func (o *lineObserver) parse(line []byte) bool {
	o.reader.Reset(line)
	o.decoder.Reset(&o.reader)
	o.method = o.method[:0]
	o.id = o.id[:0]
	if token, err := o.decoder.ReadToken(); err != nil || token.Kind() != jsontext.KindBeginObject {
		return false
	}
	for o.decoder.PeekKind() != jsontext.KindEndObject {
		if !o.parseMember() {
			return false
		}
	}
	if _, err := o.decoder.ReadToken(); err != nil {
		return false
	}
	_, err := o.decoder.ReadToken()
	return errors.Is(err, io.EOF)
}

func (o *lineObserver) parseMember() bool {
	name, err := o.decoder.ReadValue()
	if err != nil {
		return false
	}
	o.name, err = jsontext.AppendUnquote(o.name[:0], name)
	if err != nil {
		return false
	}
	field := o.field(string(o.name))
	value, err := o.decoder.ReadValue()
	if err != nil {
		return false
	}
	if field != nil {
		*field = append((*field)[:0], value...)
	}
	return true
}

func (o *lineObserver) field(name string) *[]byte {
	switch name {
	case "method":
		return &o.method
	case "id":
		return &o.id
	default:
		return nil
	}
}
