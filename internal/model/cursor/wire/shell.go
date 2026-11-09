package wire

import "google.golang.org/protobuf/encoding/protowire"

const (
	ExecShellResultNumber int32 = 2
	ExecShellStreamNumber int32 = 14
)

const (
	ShellStreamStdout int32 = 1
	ShellStreamStderr int32 = 2
	ShellStreamExit   int32 = 3
	ShellStreamStart  int32 = 4
)

type ShellExit struct {
	Code    uint32
	CwdView []byte
	Aborted bool
}

type ShellStreamEvent struct {
	Kind     int32
	DataView []byte
	Exit     *ShellExit
}

func AppendShellStreamEvent(dst []byte, m *ShellStreamEvent) []byte {
	switch m.Kind {
	case ShellStreamStdout, ShellStreamStderr:
		return appendVariant(dst, protowire.Number(m.Kind), AppendTextMessage(nil, m.DataView))
	case ShellStreamExit:
		return appendMessage(dst, protowire.Number(m.Kind), m.Exit, appendShellExit)
	case ShellStreamStart:
		return appendVariant(dst, protowire.Number(m.Kind), nil)
	}
	return dst
}

func appendShellExit(dst []byte, m *ShellExit) []byte {
	dst = appendUint32(dst, 1, m.Code)
	dst = appendRaw(dst, 2, m.CwdView)
	return appendBool(dst, 4, m.Aborted)
}

func ConsumeShellStreamEvent(b []byte, m *ShellStreamEvent) error {
	return consumeFields(b, "cursor shell stream", func(f field) error {
		if !isVariant(f, ShellStreamStdout, ShellStreamStart) {
			return nil
		}
		*m = ShellStreamEvent{Kind: f.number}
		switch f.number {
		case ShellStreamStdout, ShellStreamStderr:
			data, err := ConsumeTextMessage(f.data)
			m.DataView = data
			return err
		case ShellStreamExit:
			return consumeNested(f, "cursor shell stream", &m.Exit, consumeShellExit)
		}
		return nil
	})
}

func consumeShellExit(b []byte, m *ShellExit) error {
	return consumeFields(b, "cursor shell exit", func(f field) error {
		switch f.number {
		case 1:
			return setUint32(f, "cursor shell exit", &m.Code)
		case 2:
			return setBytes(f, "cursor shell exit", &m.CwdView)
		case 4:
			return setBool(f, "cursor shell exit", &m.Aborted)
		}
		return nil
	})
}

type ShellOutcome struct {
	CommandView          []byte
	WorkingDirectoryView []byte
	ExitCode             int32
	SignalView           []byte
	StdoutView           []byte
	StderrView           []byte
	ExecutionTime        int32
}

type ShellResult struct {
	Success *ShellOutcome
	Failure *ShellOutcome
}

func AppendShellOutcome(dst []byte, m *ShellOutcome) []byte {
	dst = appendRaw(dst, 1, m.CommandView)
	dst = appendRaw(dst, 2, m.WorkingDirectoryView)
	dst = appendInt32(dst, 3, m.ExitCode)
	dst = appendRaw(dst, 4, m.SignalView)
	dst = appendRaw(dst, 5, m.StdoutView)
	dst = appendRaw(dst, 6, m.StderrView)
	return appendInt32(dst, 7, m.ExecutionTime)
}

func ConsumeShellOutcome(b []byte, m *ShellOutcome) error {
	return consumeFields(b, "cursor shell outcome", func(f field) error {
		switch f.number {
		case 1:
			return setBytes(f, "cursor shell outcome", &m.CommandView)
		case 2:
			return setBytes(f, "cursor shell outcome", &m.WorkingDirectoryView)
		case 3:
			return setInt32(f, "cursor shell outcome", &m.ExitCode)
		case 4:
			return setBytes(f, "cursor shell outcome", &m.SignalView)
		case 5:
			return setBytes(f, "cursor shell outcome", &m.StdoutView)
		case 6:
			return setBytes(f, "cursor shell outcome", &m.StderrView)
		case 7:
			return setInt32(f, "cursor shell outcome", &m.ExecutionTime)
		}
		return nil
	})
}

func AppendShellResult(dst []byte, m *ShellResult) []byte {
	if m.Success != nil {
		return appendMessage(dst, 1, m.Success, AppendShellOutcome)
	}
	return appendOptionalMessage(dst, 2, m.Failure, AppendShellOutcome)
}

func ConsumeShellResult(b []byte, m *ShellResult) error {
	return consumeFields(b, "cursor shell result", func(f field) error {
		switch f.number {
		case 1:
			m.Failure = nil
			return consumeNested(f, "cursor shell result", &m.Success, ConsumeShellOutcome)
		case 2:
			m.Success = nil
			return consumeNested(f, "cursor shell result", &m.Failure, ConsumeShellOutcome)
		}
		return nil
	})
}
