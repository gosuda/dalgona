package app

import (
	"bytes"
	"encoding/json/jsontext"
	"encoding/json/v2"
	"fmt"
	"io"
	"sync"
)

const jsonlSchemaVersion = 1

type jsonlStream struct {
	mu     sync.Mutex
	out    io.Writer
	thread string
	err    error
}

type completedItem struct {
	ThreadID string         `json:"threadId"`
	TurnID   string         `json:"turnId"`
	Item     jsontext.Value `json:"item"`
}

func (s *jsonlStream) enable(out io.Writer) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.out = out
}

func (s *jsonlStream) enabled() bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.out != nil
}

func (s *jsonlStream) watch(threadID string) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.thread = threadID
}

func (s *jsonlStream) item(params jsontext.Value) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.out == nil || s.err != nil {
		return s.err
	}
	s.err = s.writeItem(params)
	return s.err
}

func (s *jsonlStream) writeItem(params jsontext.Value) error {
	var note completedItem
	if err := json.Unmarshal(params, &note); err != nil {
		return fmt.Errorf("decode item/completed: %w", err)
	}
	if s.thread == "" || note.ThreadID != s.thread {
		return nil
	}
	line, err := itemLine(note)
	if err != nil {
		return err
	}
	return s.emit(line)
}

func (s *jsonlStream) result(threadID string, result turnResult) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.err != nil {
		return s.err
	}
	line, err := resultLine(threadID, result)
	if err != nil {
		return err
	}
	s.err = s.emit(line)
	return s.err
}

func (s *jsonlStream) emit(line []byte) error {
	n, err := s.out.Write(line)
	if err == nil && n != len(line) {
		err = io.ErrShortWrite
	}
	if err != nil {
		return fmt.Errorf("write jsonl: %w", err)
	}
	return nil
}

func itemLine(note completedItem) ([]byte, error) {
	return jsonlLine("item", note.ThreadID, func(enc *jsontext.Encoder) error {
		if err := writeTokens(enc, jsontext.String("turn"), jsontext.String(note.TurnID), jsontext.String("item")); err != nil {
			return err
		}
		if err := enc.WriteValue(note.Item); err != nil {
			return fmt.Errorf("encode item line: %w", err)
		}
		return nil
	})
}

func resultLine(threadID string, result turnResult) ([]byte, error) {
	return jsonlLine("result", threadID, func(enc *jsontext.Encoder) error {
		return writeTokens(enc,
			jsontext.String("status"), jsontext.String(result.status),
			jsontext.String("stop_reason"), jsontext.String(result.stop),
		)
	})
}

func jsonlLine(kind, threadID string, members func(*jsontext.Encoder) error) ([]byte, error) {
	var buf bytes.Buffer
	enc := jsontext.NewEncoder(&buf, jsontext.PreserveRawStrings(true))
	err := writeTokens(enc,
		jsontext.BeginObject,
		jsontext.String("schemaVersion"), jsontext.Int(jsonlSchemaVersion),
		jsontext.String("type"), jsontext.String(kind),
		jsontext.String("thread"), jsontext.String(threadID),
	)
	if err != nil {
		return nil, err
	}
	err = members(enc)
	if err == nil {
		err = writeTokens(enc, jsontext.EndObject)
	}
	if err != nil {
		return nil, err
	}
	return append(bytes.TrimSuffix(buf.Bytes(), []byte{'\n'}), '\n'), nil
}

func writeTokens(enc *jsontext.Encoder, tokens ...jsontext.Token) error {
	for _, token := range tokens {
		if err := enc.WriteToken(token); err != nil {
			return fmt.Errorf("encode jsonl line: %w", err)
		}
	}
	return nil
}
