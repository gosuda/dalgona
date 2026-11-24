package rpcserver

import (
	"bytes"
	json "encoding/json/v2"
)

const transcriptTail = 64 << 10

type transcriptScreen struct {
	columns  int
	rows     int
	consumed int
	tail     []byte
	answered int
	replies  []byte
}

type transcriptState struct {
	Columns  int    `json:"columns"`
	Rows     int    `json:"rows"`
	Consumed int    `json:"consumed"`
	Tail     []byte `json:"tail"`
}

func newTranscriptScreen(columns, rows int) TerminalScreen {
	return &transcriptScreen{columns: columns, rows: rows}
}

var transcriptEngine = TerminalEngine{Version: 1, EngineVersion: "transcript-test", New: newTranscriptScreen}

func (s *transcriptScreen) Write(p []byte) (int, error) {
	s.consumed += len(p)
	s.tail = append(s.tail, p...)
	if excess := len(s.tail) - transcriptTail; excess > 0 {
		s.tail = s.tail[excess:]
	}
	for asked := bytes.Count(s.tail, []byte("\x1b[c")); s.answered < asked; s.answered++ {
		s.replies = append(s.replies, "\x1b[?1;2c"...)
	}
	return len(p), nil
}

func (s *transcriptScreen) Resize(columns, rows int) { s.columns, s.rows = columns, rows }

func (s *transcriptScreen) Checkpoint() ([]byte, error) {
	if bytes.Contains(s.tail, []byte("HUGE")) {
		return make([]byte, checkpointBodyLimit+1), nil
	}
	return json.Marshal(transcriptState{Columns: s.columns, Rows: s.rows, Consumed: s.consumed, Tail: s.tail})
}

func (s *transcriptScreen) TakeReplies() []byte {
	replies := s.replies
	s.replies = nil
	return replies
}

type terminalStatusWire struct {
	State              string `json:"state"`
	InputState         string `json:"inputState"`
	ExitCode           *int   `json:"exitCode"`
	FinalOffset        string `json:"finalOffset"`
	FinalResizeOrdinal string `json:"finalResizeOrdinal"`
	OutputTruncated    *bool  `json:"outputTruncated"`
	OutputExpiresAt    string `json:"outputExpiresAt"`
}

type terminalCheckpointWire struct {
	CheckpointID  string `json:"checkpointId"`
	Version       int    `json:"version"`
	EngineVersion string `json:"engineVersion"`
	NextOffset    string `json:"nextOffset"`
	ResizeOrdinal string `json:"resizeOrdinal"`
	Columns       int    `json:"columns"`
	Rows          int    `json:"rows"`
	Total         string `json:"total"`
	Digest        string `json:"digest"`
}

type terminalAttachedWire struct {
	Identity struct {
		TerminalID       string `json:"terminalId"`
		DaemonInstanceID string `json:"daemonInstanceId"`
		ThreadID         string `json:"threadId"`
	} `json:"identity"`
	Attachment struct {
		AttachmentID     string `json:"attachmentId"`
		WriterLease      string `json:"writerLease"`
		AcceptedInputSeq string `json:"acceptedInputSeq"`
	} `json:"attachment"`
	Checkpoint terminalCheckpointWire `json:"checkpoint"`
	Status     terminalStatusWire     `json:"status"`
}

type terminalResizeWire struct {
	Ordinal string `json:"ordinal"`
	Offset  string `json:"offset"`
	Columns int    `json:"columns"`
	Rows    int    `json:"rows"`
}

type terminalPageWire struct {
	Offset        string               `json:"offset"`
	Next          string               `json:"next"`
	DataBase64    string               `json:"dataBase64"`
	Resizes       []terminalResizeWire `json:"resizes"`
	ResizeOrdinal string               `json:"resizeOrdinal"`
	Status        terminalStatusWire   `json:"status"`
}
