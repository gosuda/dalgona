package wire

import (
	"github.com/gosuda/dalgona/internal/model/connectwire"
	"google.golang.org/protobuf/encoding/protowire"
)

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
		return connectwire.AppendVariant(dst, protowire.Number(m.Kind), AppendTextMessage(nil, m.DataView))
	case ShellStreamExit:
		return connectwire.AppendMessage(dst, protowire.Number(m.Kind), m.Exit, appendShellExit)
	case ShellStreamStart:
		return connectwire.AppendVariant(dst, protowire.Number(m.Kind), nil)
	}
	return dst
}

func appendShellExit(dst []byte, m *ShellExit) []byte {
	dst = connectwire.AppendUint32(dst, 1, m.Code)
	dst = connectwire.AppendRaw(dst, 2, m.CwdView)
	return connectwire.AppendBool(dst, 4, m.Aborted)
}

func ConsumeShellStreamEvent(b []byte, m *ShellStreamEvent) error {
	return connectwire.ConsumeFields(b, "cursor shell stream", func(f connectwire.Field) error {
		if !connectwire.IsVariant(f, ShellStreamStdout, ShellStreamStart) {
			return nil
		}
		*m = ShellStreamEvent{Kind: f.Number}
		switch f.Number {
		case ShellStreamStdout, ShellStreamStderr:
			data, err := ConsumeTextMessage(f.Data)
			m.DataView = data
			return err
		case ShellStreamExit:
			return connectwire.ConsumeNested(f, "cursor shell stream", &m.Exit, consumeShellExit)
		}
		return nil
	})
}

func consumeShellExit(b []byte, m *ShellExit) error {
	return connectwire.ConsumeFields(b, "cursor shell exit", func(f connectwire.Field) error {
		switch f.Number {
		case 1:
			return connectwire.SetUint32(f, "cursor shell exit", &m.Code)
		case 2:
			return connectwire.SetBytes(f, "cursor shell exit", &m.CwdView)
		case 4:
			return connectwire.SetBool(f, "cursor shell exit", &m.Aborted)
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
	dst = connectwire.AppendRaw(dst, 1, m.CommandView)
	dst = connectwire.AppendRaw(dst, 2, m.WorkingDirectoryView)
	dst = connectwire.AppendInt32(dst, 3, m.ExitCode)
	dst = connectwire.AppendRaw(dst, 4, m.SignalView)
	dst = connectwire.AppendRaw(dst, 5, m.StdoutView)
	dst = connectwire.AppendRaw(dst, 6, m.StderrView)
	return connectwire.AppendInt32(dst, 7, m.ExecutionTime)
}

func ConsumeShellOutcome(b []byte, m *ShellOutcome) error {
	return connectwire.ConsumeFields(b, "cursor shell outcome", func(f connectwire.Field) error {
		switch f.Number {
		case 1:
			return connectwire.SetBytes(f, "cursor shell outcome", &m.CommandView)
		case 2:
			return connectwire.SetBytes(f, "cursor shell outcome", &m.WorkingDirectoryView)
		case 3:
			return connectwire.SetInt32(f, "cursor shell outcome", &m.ExitCode)
		case 4:
			return connectwire.SetBytes(f, "cursor shell outcome", &m.SignalView)
		case 5:
			return connectwire.SetBytes(f, "cursor shell outcome", &m.StdoutView)
		case 6:
			return connectwire.SetBytes(f, "cursor shell outcome", &m.StderrView)
		case 7:
			return connectwire.SetInt32(f, "cursor shell outcome", &m.ExecutionTime)
		}
		return nil
	})
}

func AppendShellResult(dst []byte, m *ShellResult) []byte {
	if m.Success != nil {
		return connectwire.AppendMessage(dst, 1, m.Success, AppendShellOutcome)
	}
	return connectwire.AppendOptionalMessage(dst, 2, m.Failure, AppendShellOutcome)
}

func ConsumeShellResult(b []byte, m *ShellResult) error {
	return connectwire.ConsumeFields(b, "cursor shell result", func(f connectwire.Field) error {
		switch f.Number {
		case 1:
			m.Failure = nil
			return connectwire.ConsumeNested(f, "cursor shell result", &m.Success, ConsumeShellOutcome)
		case 2:
			m.Success = nil
			return connectwire.ConsumeNested(f, "cursor shell result", &m.Failure, ConsumeShellOutcome)
		}
		return nil
	})
}
