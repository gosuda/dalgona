package wire

import "google.golang.org/protobuf/encoding/protowire"

const (
	ServerExecControlNumber         int32 = 5
	ServerInteractionQueryNumber    int32 = 7
	ClientInteractionResponseNumber int32 = 6
	InteractionHeartbeatNumber      int32 = 13
)

const (
	QueryWebSearch   int32 = 2
	QueryAskQuestion int32 = 3
	QuerySwitchMode  int32 = 4
	QueryExaSearch   int32 = 5
	QueryExaFetch    int32 = 6
	QueryCreatePlan  int32 = 7
	QuerySetupVM     int32 = 8
	QueryWebFetch    int32 = 9
)

type InteractionQuery struct {
	ID          uint32
	QueryNumber int32
	QueryView   []byte
}

func AppendInteractionQuery(dst []byte, m *InteractionQuery) []byte {
	dst = appendUint32(dst, 1, m.ID)
	if m.QueryNumber < 2 {
		return dst
	}
	return appendVariant(dst, protowire.Number(m.QueryNumber), m.QueryView)
}

func ConsumeInteractionQuery(b []byte, m *InteractionQuery) error {
	*m = InteractionQuery{}
	return consumeFields(b, "cursor interaction query", func(f field) error {
		if f.number == 1 {
			return setUint32(f, "cursor interaction query", &m.ID)
		}
		if f.kind != protowire.BytesType {
			return nil
		}
		m.QueryNumber = f.number
		m.QueryView = bytesValue(f)
		return nil
	})
}

type InteractionResponse struct {
	ID           uint32
	ResultNumber int32
	ResultView   []byte
}

func AppendInteractionResponse(dst []byte, m *InteractionResponse) []byte {
	dst = appendUint32(dst, 1, m.ID)
	if m.ResultNumber < 2 {
		return dst
	}
	return appendVariant(dst, protowire.Number(m.ResultNumber), m.ResultView)
}

func ConsumeInteractionResponse(b []byte, m *InteractionResponse) error {
	*m = InteractionResponse{}
	return consumeFields(b, "cursor interaction response", func(f field) error {
		if f.number == 1 {
			return setUint32(f, "cursor interaction response", &m.ID)
		}
		if f.kind != protowire.BytesType {
			return nil
		}
		m.ResultNumber = f.number
		m.ResultView = bytesValue(f)
		return nil
	})
}

func AppendQueryRejected(dst, reason []byte) []byte {
	return appendMessage(dst, 2, &reason, appendTextField)
}

func AppendAskQuestionRejected(dst, reason []byte) []byte {
	return appendMessage(dst, 1, &reason, appendAskQuestionRejectedResult)
}

func appendAskQuestionRejectedResult(dst []byte, reason *[]byte) []byte {
	return appendMessage(dst, 3, reason, appendTextField)
}

func AppendCreatePlanError(dst, message []byte) []byte {
	return appendMessage(dst, 1, &message, appendCreatePlanErrorResult)
}

func appendCreatePlanErrorResult(dst []byte, message *[]byte) []byte {
	return appendMessage(dst, 2, message, appendTextField)
}

func appendTextField(dst []byte, text *[]byte) []byte {
	return AppendTextMessage(dst, *text)
}

type ExecServerControl struct {
	Abort   bool
	AbortID uint32
}

func AppendExecServerControl(dst []byte, m *ExecServerControl) []byte {
	if !m.Abort {
		return dst
	}
	return appendMessage(dst, 1, &m.AbortID, appendAbortID)
}

func appendAbortID(dst []byte, id *uint32) []byte {
	return appendUint32(dst, 1, *id)
}

func ConsumeExecServerControl(b []byte, m *ExecServerControl) error {
	*m = ExecServerControl{}
	return consumeFields(b, "cursor exec server control", func(f field) error {
		if f.number != 1 {
			return nil
		}
		if err := requireKind(f, protowire.BytesType, "cursor exec server control"); err != nil {
			return err
		}
		m.Abort = true
		return consumeFields(bytesValue(f), "cursor exec server abort", func(inner field) error {
			if inner.number != 1 {
				return nil
			}
			return setUint32(inner, "cursor exec server abort", &m.AbortID)
		})
	})
}
