package wire

import (
	"github.com/gosuda/dalgona/internal/model/connectwire"
	"google.golang.org/protobuf/encoding/protowire"
)

const (
	ClientExecControlNumber int32 = 5
	ClientHeartbeatNumber   int32 = 7
	ResumeActionNumber      int32 = 2
)

const (
	ExecStreamClose int32 = 1
	ExecThrow       int32 = 2
	ExecHeartbeat   int32 = 3
)

const (
	McpResultSuccessNumber  int32 = 1
	McpResultErrorNumber    int32 = 2
	McpResultRejectedNumber int32 = 3
	McpTextContentNumber    int32 = 1
)

type ExecClientControl struct {
	Kind      int32
	ID        uint32
	ErrorView []byte
}

func AppendExecClientControl(dst []byte, m *ExecClientControl) []byte {
	if m.Kind < ExecStreamClose || m.Kind > ExecHeartbeat {
		return dst
	}
	return connectwire.AppendMessage(dst, protowire.Number(m.Kind), m, appendExecControlBody)
}

func appendExecControlBody(dst []byte, m *ExecClientControl) []byte {
	dst = connectwire.AppendUint32(dst, 1, m.ID)
	if m.Kind == ExecThrow {
		dst = connectwire.AppendRaw(dst, 2, m.ErrorView)
	}
	return dst
}

func ConsumeExecClientControl(b []byte, m *ExecClientControl) error {
	return connectwire.ConsumeFields(b, "cursor exec control", func(f connectwire.Field) error {
		if !connectwire.IsVariant(f, ExecStreamClose, ExecHeartbeat) {
			return nil
		}
		*m = ExecClientControl{Kind: f.Number}
		return connectwire.ConsumeFields(f.Data, "cursor exec control", func(inner connectwire.Field) error {
			switch inner.Number {
			case 1:
				return connectwire.SetUint32(inner, "cursor exec control", &m.ID)
			case 2:
				return connectwire.SetBytes(inner, "cursor exec control", &m.ErrorView)
			}
			return nil
		})
	})
}

func AppendTextMessage(dst, text []byte) []byte {
	if text == nil {
		text = []byte{}
	}
	return connectwire.AppendRaw(dst, 1, text)
}

func ConsumeTextMessage(b []byte) ([]byte, error) {
	var text []byte
	err := connectwire.ConsumeFields(b, "cursor text message", func(f connectwire.Field) error {
		if f.Number != 1 {
			return nil
		}
		return connectwire.SetBytes(f, "cursor text message", &text)
	})
	return text, err
}
