package wire

import "google.golang.org/protobuf/encoding/protowire"

type McpArgument struct {
	NameView  []byte
	ValueView []byte
}

func AppendMcpArgument(dst []byte, m *McpArgument) []byte {
	dst = appendRaw(dst, 1, m.NameView)
	return appendRaw(dst, 2, m.ValueView)
}

func ConsumeMcpArgument(b []byte, m *McpArgument) error {
	return consumeFields(b, "cursor MCP argument", func(f field) error {
		switch f.number {
		case 1:
			return setBytes(f, "cursor MCP argument", &m.NameView)
		case 2:
			return setBytes(f, "cursor MCP argument", &m.ValueView)
		default:
			return nil
		}
	})
}

type McpArgs struct {
	NameView               []byte
	Args                   []McpArgument
	ToolCallIDView         []byte
	ProviderIdentifierView []byte
	ToolNameView           []byte
	SmartModeApprovalView  []byte
	ApprovalOnly           bool
	SkipApproval           bool
	ServerIdentifierView   []byte
}

func AppendMcpArgs(dst []byte, m *McpArgs) []byte {
	dst = appendRaw(dst, 1, m.NameView)
	for i := range m.Args {
		dst = appendMessage(dst, 2, &m.Args[i], AppendMcpArgument)
	}
	dst = appendRaw(dst, 3, m.ToolCallIDView)
	dst = appendRaw(dst, 4, m.ProviderIdentifierView)
	dst = appendRaw(dst, 5, m.ToolNameView)
	dst = appendRaw(dst, 6, m.SmartModeApprovalView)
	dst = appendBool(dst, 7, m.ApprovalOnly)
	dst = appendBool(dst, 8, m.SkipApproval)
	return appendRaw(dst, 9, m.ServerIdentifierView)
}

func ConsumeMcpArgs(b []byte, m *McpArgs) error {
	return consumeFields(b, "cursor MCP args", func(f field) error {
		if f.number <= 5 {
			return consumeMcpArgsBase(f, m)
		}
		return consumeMcpArgsOptions(f, m)
	})
}

func consumeMcpArgsBase(f field, m *McpArgs) error {
	switch f.number {
	case 1:
		return setBytes(f, "cursor MCP args", &m.NameView)
	case 2:
		return consumeMcpArgument(f, m)
	case 3:
		return setBytes(f, "cursor MCP args", &m.ToolCallIDView)
	case 4:
		return setBytes(f, "cursor MCP args", &m.ProviderIdentifierView)
	case 5:
		return setBytes(f, "cursor MCP args", &m.ToolNameView)
	default:
		return nil
	}
}

func consumeMcpArgsOptions(f field, m *McpArgs) error {
	switch f.number {
	case 6:
		return setBytes(f, "cursor MCP args", &m.SmartModeApprovalView)
	case 7:
		return setBool(f, "cursor MCP args", &m.ApprovalOnly)
	case 8:
		return setBool(f, "cursor MCP args", &m.SkipApproval)
	case 9:
		return setBytes(f, "cursor MCP args", &m.ServerIdentifierView)
	default:
		return nil
	}
}

func consumeMcpArgument(f field, m *McpArgs) error {
	if err := requireKind(f, protowire.BytesType, "cursor MCP args"); err != nil {
		return err
	}
	var arg McpArgument
	if err := ConsumeMcpArgument(bytesValue(f), &arg); err != nil {
		return err
	}
	m.Args = append(m.Args, arg)
	return nil
}

type McpContentItem struct {
	ContentNumber int32
	ContentView   []byte
}

type McpSuccess struct {
	Content []McpContentItem
	IsError bool
}

func AppendMcpContentItem(dst []byte, m *McpContentItem) []byte {
	if m.ContentNumber != 0 {
		return appendVariant(dst, protowire.Number(m.ContentNumber), m.ContentView)
	}
	return dst
}

func ConsumeMcpContentItem(b []byte, item *McpContentItem) error {
	return consumeFields(b, "cursor MCP content", func(f field) error {
		if f.number != 1 && f.number != 2 {
			return nil
		}
		item.ContentNumber = f.number
		return setBytes(f, "cursor MCP content", &item.ContentView)
	})
}

func AppendMcpSuccess(dst []byte, m *McpSuccess) []byte {
	for i := range m.Content {
		dst = appendMessage(dst, 1, &m.Content[i], AppendMcpContentItem)
	}
	return appendBool(dst, 2, m.IsError)
}

func ConsumeMcpSuccess(b []byte, m *McpSuccess) error {
	return consumeFields(b, "cursor MCP success", func(f field) error {
		switch f.number {
		case 1:
			if err := requireKind(f, protowire.BytesType, "cursor MCP success"); err != nil {
				return err
			}
			item, err := consumeMcpContent(bytesValue(f))
			if err != nil {
				return err
			}
			m.Content = append(m.Content, item)
		case 2:
			return setBool(f, "cursor MCP success", &m.IsError)
		}
		return nil
	})
}

func consumeMcpContent(b []byte) (McpContentItem, error) {
	var item McpContentItem
	err := ConsumeMcpContentItem(b, &item)
	return item, err
}

type McpResult struct {
	Success      *McpSuccess
	ResultNumber int32
	ResultView   []byte
}

func AppendMcpResult(dst []byte, m *McpResult) []byte {
	if m.Success != nil {
		return appendMessage(dst, 1, m.Success, AppendMcpSuccess)
	}
	if m.ResultNumber != 0 {
		return appendVariant(dst, protowire.Number(m.ResultNumber), m.ResultView)
	}
	return dst
}

func ConsumeMcpResult(b []byte, m *McpResult) error {
	return consumeFields(b, "cursor MCP result", func(f field) error {
		if !isVariant(f, 1, 7) {
			return nil
		}
		*m = McpResult{}
		if f.number == 1 {
			return consumeNested(f, "cursor MCP result", &m.Success, ConsumeMcpSuccess)
		}
		m.ResultNumber = f.number
		return setBytes(f, "cursor MCP result", &m.ResultView)
	})
}

type ExecClientMessage struct {
	ID                    uint32
	ExecIDView            []byte
	RequestContextResult  *RequestContextResult
	McpResult             *McpResult
	ResultNumber          int32
	ResultView            []byte
	LocalExecutionTimeMS  int32
	HookAdditionalContext [][]byte
}

func (m *ExecClientMessage) resetResult() {
	m.RequestContextResult = nil
	m.McpResult = nil
	m.ResultNumber = 0
	m.ResultView = nil
}

func AppendExecClientMessage(dst []byte, m *ExecClientMessage) []byte {
	dst = appendUint32(dst, 1, m.ID)
	dst = appendRaw(dst, 15, m.ExecIDView)
	dst = appendExecResult(dst, m)
	dst = appendInt32(dst, 39, m.LocalExecutionTimeMS)
	return appendRepeatedBytes(dst, 45, m.HookAdditionalContext)
}

func appendExecResult(dst []byte, m *ExecClientMessage) []byte {
	switch {
	case m.RequestContextResult != nil:
		return appendMessage(dst, 10, m.RequestContextResult, AppendRequestContextResult)
	case m.McpResult != nil:
		return appendMessage(dst, 11, m.McpResult, AppendMcpResult)
	case m.ResultNumber != 0:
		return appendVariant(dst, protowire.Number(m.ResultNumber), m.ResultView)
	}
	return dst
}

func ConsumeExecClientMessage(b []byte, m *ExecClientMessage) error {
	return consumeFields(b, "cursor exec client", func(f field) error {
		switch f.number {
		case 1:
			return setUint32(f, "cursor exec client", &m.ID)
		case 15:
			return setBytes(f, "cursor exec client", &m.ExecIDView)
		case 10:
			m.resetResult()
			return consumeNested(f, "cursor exec client", &m.RequestContextResult, ConsumeRequestContextResult)
		case 11:
			m.resetResult()
			return consumeNested(f, "cursor exec client", &m.McpResult, ConsumeMcpResult)
		case 39:
			return setInt32(f, "cursor exec client", &m.LocalExecutionTimeMS)
		case 45:
			return appendBytesField(f, "cursor exec client", &m.HookAdditionalContext)
		default:
			return consumeExecResultVariant(f, m)
		}
	})
}

func consumeExecResultVariant(f field, m *ExecClientMessage) error {
	if f.kind != protowire.BytesType || f.number < 2 || f.number > 55 {
		return nil
	}
	m.resetResult()
	m.ResultNumber = f.number
	return setBytes(f, "cursor exec client", &m.ResultView)
}

type ShellArgs struct {
	CommandView                 []byte
	WorkingDirectoryView        []byte
	Timeout                     int32
	ToolCallIDView              []byte
	SimpleCommands              [][]byte
	InputRedirect               bool
	OutputRedirect              bool
	ParsingResultView           []byte
	RequestedSandboxPolicyView  []byte
	FileOutputThresholdBytes    uint64
	Background                  bool
	SkipApproval                bool
	TimeoutBehavior             int32
	HardTimeout                 int32
	DescriptionView             []byte
	ClassifierResultView        []byte
	CloseStdin                  bool
	OutputNotificationView      []byte
	SmartModeApprovalView       []byte
	HookApprovalRequirementView []byte
	ConversationIDView          []byte
}

func AppendShellArgs(dst []byte, m *ShellArgs) []byte {
	dst = appendRaw(dst, 1, m.CommandView)
	dst = appendRaw(dst, 2, m.WorkingDirectoryView)
	dst = appendInt32(dst, 3, m.Timeout)
	dst = appendRaw(dst, 4, m.ToolCallIDView)
	dst = appendRepeatedBytes(dst, 5, m.SimpleCommands)
	dst = appendBool(dst, 6, m.InputRedirect)
	dst = appendBool(dst, 7, m.OutputRedirect)
	dst = appendRaw(dst, 8, m.ParsingResultView)
	dst = appendRaw(dst, 9, m.RequestedSandboxPolicyView)
	dst = appendUint64(dst, 10, m.FileOutputThresholdBytes)
	dst = appendBool(dst, 11, m.Background)
	dst = appendBool(dst, 12, m.SkipApproval)
	dst = appendInt32(dst, 13, m.TimeoutBehavior)
	dst = appendInt32(dst, 14, m.HardTimeout)
	dst = appendRaw(dst, 15, m.DescriptionView)
	dst = appendRaw(dst, 16, m.ClassifierResultView)
	dst = appendBool(dst, 17, m.CloseStdin)
	dst = appendRaw(dst, 18, m.OutputNotificationView)
	dst = appendRaw(dst, 19, m.SmartModeApprovalView)
	dst = appendRaw(dst, 20, m.HookApprovalRequirementView)
	return appendRaw(dst, 21, m.ConversationIDView)
}

func ConsumeShellArgs(b []byte, m *ShellArgs) error {
	return consumeFields(b, "cursor shell args", func(f field) error {
		switch {
		case f.number <= 7:
			return consumeShellBase(f, m)
		case f.number <= 14:
			return consumeShellExecution(f, m)
		default:
			return consumeShellOptions(f, m)
		}
	})
}

func consumeShellBase(f field, m *ShellArgs) error {
	switch f.number {
	case 1:
		return setBytes(f, "cursor shell args", &m.CommandView)
	case 2:
		return setBytes(f, "cursor shell args", &m.WorkingDirectoryView)
	case 3:
		return setInt32(f, "cursor shell args", &m.Timeout)
	case 4:
		return setBytes(f, "cursor shell args", &m.ToolCallIDView)
	case 5:
		if err := requireKind(f, protowire.BytesType, "cursor shell args"); err != nil {
			return err
		}
		m.SimpleCommands = append(m.SimpleCommands, bytesValue(f))
	case 6:
		return setBool(f, "cursor shell args", &m.InputRedirect)
	case 7:
		return setBool(f, "cursor shell args", &m.OutputRedirect)
	}
	return nil
}

func consumeShellExecution(f field, m *ShellArgs) error {
	switch f.number {
	case 8:
		return setBytes(f, "cursor shell args", &m.ParsingResultView)
	case 9:
		return setBytes(f, "cursor shell args", &m.RequestedSandboxPolicyView)
	case 10:
		return setUint64(f, "cursor shell args", &m.FileOutputThresholdBytes)
	case 11:
		return setBool(f, "cursor shell args", &m.Background)
	case 12:
		return setBool(f, "cursor shell args", &m.SkipApproval)
	case 13:
		return setInt32(f, "cursor shell args", &m.TimeoutBehavior)
	case 14:
		return setInt32(f, "cursor shell args", &m.HardTimeout)
	}
	return nil
}

func consumeShellOptions(f field, m *ShellArgs) error {
	switch f.number {
	case 15:
		return setBytes(f, "cursor shell args", &m.DescriptionView)
	case 16:
		return setBytes(f, "cursor shell args", &m.ClassifierResultView)
	case 17:
		return setBool(f, "cursor shell args", &m.CloseStdin)
	case 18:
		return setBytes(f, "cursor shell args", &m.OutputNotificationView)
	case 19:
		return setBytes(f, "cursor shell args", &m.SmartModeApprovalView)
	case 20:
		return setBytes(f, "cursor shell args", &m.HookApprovalRequirementView)
	case 21:
		return setBytes(f, "cursor shell args", &m.ConversationIDView)
	}
	return nil
}

type ExecServerMessage struct {
	ID                           uint32
	ExecIDView                   []byte
	SpanContextView              []byte
	Shell                        *ShellArgs
	RequestContext               *RequestContextArgs
	Mcp                          *McpArgs
	ShellStream                  *ShellArgs
	MessageNumber                int32
	MessageView                  []byte
	AcceptHookAdditionalContexts bool
}

func (m *ExecServerMessage) resetRequest() {
	m.Shell = nil
	m.RequestContext = nil
	m.Mcp = nil
	m.ShellStream = nil
	m.MessageNumber = 0
	m.MessageView = nil
}

func AppendExecServerMessage(dst []byte, m *ExecServerMessage) []byte {
	dst = appendUint32(dst, 1, m.ID)
	dst = appendRaw(dst, 15, m.ExecIDView)
	dst = appendRaw(dst, 19, m.SpanContextView)
	dst = appendExecRequest(dst, m)
	return appendBool(dst, 55, m.AcceptHookAdditionalContexts)
}

func appendExecRequest(dst []byte, m *ExecServerMessage) []byte {
	switch {
	case m.Shell != nil:
		return appendMessage(dst, 2, m.Shell, AppendShellArgs)
	case m.RequestContext != nil:
		return appendMessage(dst, 10, m.RequestContext, AppendRequestContextArgs)
	case m.Mcp != nil:
		return appendMessage(dst, 11, m.Mcp, AppendMcpArgs)
	case m.ShellStream != nil:
		return appendMessage(dst, 14, m.ShellStream, AppendShellArgs)
	case m.MessageNumber != 0:
		return appendVariant(dst, protowire.Number(m.MessageNumber), m.MessageView)
	}
	return dst
}

func ConsumeExecServerMessage(b []byte, m *ExecServerMessage) error {
	return consumeFields(b, "cursor exec server", func(f field) error {
		switch f.number {
		case 1:
			return setUint32(f, "cursor exec server", &m.ID)
		case 15:
			return setBytes(f, "cursor exec server", &m.ExecIDView)
		case 19:
			return setBytes(f, "cursor exec server", &m.SpanContextView)
		case 55:
			return setBool(f, "cursor exec server", &m.AcceptHookAdditionalContexts)
		default:
			return consumeExecRequest(f, m)
		}
	})
}

func consumeExecRequest(f field, m *ExecServerMessage) error {
	if f.kind != protowire.BytesType || f.number < 2 || f.number > 54 {
		return nil
	}
	m.resetRequest()
	switch f.number {
	case 2:
		return consumeNested(f, "cursor exec server", &m.Shell, ConsumeShellArgs)
	case 10:
		return consumeNested(f, "cursor exec server", &m.RequestContext, ConsumeRequestContextArgs)
	case 11:
		return consumeNested(f, "cursor exec server", &m.Mcp, ConsumeMcpArgs)
	case 14:
		return consumeNested(f, "cursor exec server", &m.ShellStream, ConsumeShellArgs)
	}
	m.MessageNumber = f.number
	return setBytes(f, "cursor exec server", &m.MessageView)
}
