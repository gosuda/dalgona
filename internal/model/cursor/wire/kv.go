package wire

const (
	ServerCheckpointNumber int32 = 3
	ServerKvNumber         int32 = 4
	ClientKvNumber         int32 = 3
)

type GetBlobArgs struct {
	BlobIDView []byte
}

type SetBlobArgs struct {
	BlobIDView   []byte
	BlobDataView []byte
}

type KvServerMessage struct {
	ID  uint32
	Get *GetBlobArgs
	Set *SetBlobArgs
}

type GetBlobResult struct {
	BlobDataView []byte
}

type SetBlobResult struct {
	ErrorView []byte
}

type KvClientMessage struct {
	ID  uint32
	Get *GetBlobResult
	Set *SetBlobResult
}

func AppendGetBlobArgs(dst []byte, m *GetBlobArgs) []byte {
	return appendRaw(dst, 1, m.BlobIDView)
}

func ConsumeGetBlobArgs(b []byte, m *GetBlobArgs) error {
	return consumeFields(b, "cursor get blob", func(f field) error {
		if f.number != 1 {
			return nil
		}
		return setBytes(f, "cursor get blob", &m.BlobIDView)
	})
}

func AppendSetBlobArgs(dst []byte, m *SetBlobArgs) []byte {
	dst = appendRaw(dst, 1, m.BlobIDView)
	return appendRaw(dst, 2, m.BlobDataView)
}

func ConsumeSetBlobArgs(b []byte, m *SetBlobArgs) error {
	return consumeFields(b, "cursor set blob", func(f field) error {
		switch f.number {
		case 1:
			return setBytes(f, "cursor set blob", &m.BlobIDView)
		case 2:
			return setBytes(f, "cursor set blob", &m.BlobDataView)
		}
		return nil
	})
}

func AppendKvServerMessage(dst []byte, m *KvServerMessage) []byte {
	dst = appendUint32(dst, 1, m.ID)
	if m.Get != nil {
		return appendMessage(dst, 2, m.Get, AppendGetBlobArgs)
	}
	return appendOptionalMessage(dst, 3, m.Set, AppendSetBlobArgs)
}

func ConsumeKvServerMessage(b []byte, m *KvServerMessage) error {
	return consumeFields(b, "cursor kv server", func(f field) error {
		switch f.number {
		case 1:
			return setUint32(f, "cursor kv server", &m.ID)
		case 2:
			m.Set = nil
			return consumeNested(f, "cursor kv server", &m.Get, ConsumeGetBlobArgs)
		case 3:
			m.Get = nil
			return consumeNested(f, "cursor kv server", &m.Set, ConsumeSetBlobArgs)
		}
		return nil
	})
}

func AppendGetBlobResult(dst []byte, m *GetBlobResult) []byte {
	return appendRaw(dst, 1, m.BlobDataView)
}

func ConsumeGetBlobResult(b []byte, m *GetBlobResult) error {
	return consumeFields(b, "cursor get blob result", func(f field) error {
		if f.number != 1 {
			return nil
		}
		return setBytes(f, "cursor get blob result", &m.BlobDataView)
	})
}

func AppendSetBlobResult(dst []byte, m *SetBlobResult) []byte {
	return appendRaw(dst, 1, m.ErrorView)
}

func ConsumeSetBlobResult(b []byte, m *SetBlobResult) error {
	return consumeFields(b, "cursor set blob result", func(f field) error {
		if f.number != 1 {
			return nil
		}
		return setBytes(f, "cursor set blob result", &m.ErrorView)
	})
}

func AppendKvClientMessage(dst []byte, m *KvClientMessage) []byte {
	dst = appendUint32(dst, 1, m.ID)
	if m.Get != nil {
		return appendMessage(dst, 2, m.Get, AppendGetBlobResult)
	}
	return appendOptionalMessage(dst, 3, m.Set, AppendSetBlobResult)
}

func ConsumeKvClientMessage(b []byte, m *KvClientMessage) error {
	return consumeFields(b, "cursor kv client", func(f field) error {
		switch f.number {
		case 1:
			return setUint32(f, "cursor kv client", &m.ID)
		case 2:
			m.Set = nil
			return consumeNested(f, "cursor kv client", &m.Get, ConsumeGetBlobResult)
		case 3:
			m.Get = nil
			return consumeNested(f, "cursor kv client", &m.Set, ConsumeSetBlobResult)
		}
		return nil
	})
}
