package wire

import "google.golang.org/protobuf/encoding/protowire"

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
	return appendMessage(dst, protowire.Number(m.Kind), m, appendExecControlBody)
}

func appendExecControlBody(dst []byte, m *ExecClientControl) []byte {
	dst = appendUint32(dst, 1, m.ID)
	if m.Kind == ExecThrow {
		dst = appendRaw(dst, 2, m.ErrorView)
	}
	return dst
}

func ConsumeExecClientControl(b []byte, m *ExecClientControl) error {
	return consumeFields(b, "cursor exec control", func(f field) error {
		if !isVariant(f, ExecStreamClose, ExecHeartbeat) {
			return nil
		}
		*m = ExecClientControl{Kind: f.number}
		return consumeFields(f.data, "cursor exec control", func(inner field) error {
			switch inner.number {
			case 1:
				return setUint32(inner, "cursor exec control", &m.ID)
			case 2:
				return setBytes(inner, "cursor exec control", &m.ErrorView)
			}
			return nil
		})
	})
}

func AppendTextMessage(dst, text []byte) []byte {
	if text == nil {
		text = []byte{}
	}
	return appendRaw(dst, 1, text)
}

func ConsumeTextMessage(b []byte) ([]byte, error) {
	var text []byte
	err := consumeFields(b, "cursor text message", func(f field) error {
		if f.number != 1 {
			return nil
		}
		return setBytes(f, "cursor text message", &text)
	})
	return text, err
}
