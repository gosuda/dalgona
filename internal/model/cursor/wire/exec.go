package wire

import (
	"github.com/gosuda/dalgona/internal/model/connectwire"
	"google.golang.org/protobuf/encoding/protowire"
)

type McpArgument struct {
	NameView  []byte
	ValueView []byte
}

func AppendMcpArgument(dst []byte, m *McpArgument) []byte {
	dst = connectwire.AppendRaw(dst, 1, m.NameView)
	return connectwire.AppendRaw(dst, 2, m.ValueView)
}

func ConsumeMcpArgument(b []byte, m *McpArgument) error {
	return connectwire.ConsumeFields(b, "cursor MCP argument", func(f connectwire.Field) error {
		switch f.Number {
		case 1:
			return connectwire.SetBytes(f, "cursor MCP argument", &m.NameView)
		case 2:
			return connectwire.SetBytes(f, "cursor MCP argument", &m.ValueView)
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
	dst = connectwire.AppendRaw(dst, 1, m.NameView)
	for i := range m.Args {
		dst = connectwire.AppendMessage(dst, 2, &m.Args[i], AppendMcpArgument)
	}
	dst = connectwire.AppendRaw(dst, 3, m.ToolCallIDView)
	dst = connectwire.AppendRaw(dst, 4, m.ProviderIdentifierView)
	dst = connectwire.AppendRaw(dst, 5, m.ToolNameView)
	dst = connectwire.AppendRaw(dst, 6, m.SmartModeApprovalView)
	dst = connectwire.AppendBool(dst, 7, m.ApprovalOnly)
	dst = connectwire.AppendBool(dst, 8, m.SkipApproval)
	return connectwire.AppendRaw(dst, 9, m.ServerIdentifierView)
}

func ConsumeMcpArgs(b []byte, m *McpArgs) error {
	return connectwire.ConsumeFields(b, "cursor MCP args", func(f connectwire.Field) error {
		if f.Number <= 5 {
			return consumeMcpArgsBase(f, m)
		}
		return consumeMcpArgsOptions(f, m)
	})
}

func consumeMcpArgsBase(f connectwire.Field, m *McpArgs) error {
	switch f.Number {
	case 1:
		return connectwire.SetBytes(f, "cursor MCP args", &m.NameView)
	case 2:
		return consumeMcpArgument(f, m)
	case 3:
		return connectwire.SetBytes(f, "cursor MCP args", &m.ToolCallIDView)
	case 4:
		return connectwire.SetBytes(f, "cursor MCP args", &m.ProviderIdentifierView)
	case 5:
		return connectwire.SetBytes(f, "cursor MCP args", &m.ToolNameView)
	default:
		return nil
	}
}

func consumeMcpArgsOptions(f connectwire.Field, m *McpArgs) error {
	switch f.Number {
	case 6:
		return connectwire.SetBytes(f, "cursor MCP args", &m.SmartModeApprovalView)
	case 7:
		return connectwire.SetBool(f, "cursor MCP args", &m.ApprovalOnly)
	case 8:
		return connectwire.SetBool(f, "cursor MCP args", &m.SkipApproval)
	case 9:
		return connectwire.SetBytes(f, "cursor MCP args", &m.ServerIdentifierView)
	default:
		return nil
	}
}

func consumeMcpArgument(f connectwire.Field, m *McpArgs) error {
	if err := connectwire.RequireKind(f, protowire.BytesType, "cursor MCP args"); err != nil {
		return err
	}
	var arg McpArgument
	if err := ConsumeMcpArgument(connectwire.BytesValue(f), &arg); err != nil {
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
		return connectwire.AppendVariant(dst, protowire.Number(m.ContentNumber), m.ContentView)
	}
	return dst
}

func ConsumeMcpContentItem(b []byte, item *McpContentItem) error {
	return connectwire.ConsumeFields(b, "cursor MCP content", func(f connectwire.Field) error {
		if f.Number != 1 && f.Number != 2 {
			return nil
		}
		item.ContentNumber = f.Number
		return connectwire.SetBytes(f, "cursor MCP content", &item.ContentView)
	})
}

func AppendMcpSuccess(dst []byte, m *McpSuccess) []byte {
	for i := range m.Content {
		dst = connectwire.AppendMessage(dst, 1, &m.Content[i], AppendMcpContentItem)
	}
	return connectwire.AppendBool(dst, 2, m.IsError)
}

func ConsumeMcpSuccess(b []byte, m *McpSuccess) error {
	return connectwire.ConsumeFields(b, "cursor MCP success", func(f connectwire.Field) error {
		switch f.Number {
		case 1:
			if err := connectwire.RequireKind(f, protowire.BytesType, "cursor MCP success"); err != nil {
				return err
			}
			item, err := consumeMcpContent(connectwire.BytesValue(f))
			if err != nil {
				return err
			}
			m.Content = append(m.Content, item)
		case 2:
			return connectwire.SetBool(f, "cursor MCP success", &m.IsError)
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
		return connectwire.AppendMessage(dst, 1, m.Success, AppendMcpSuccess)
	}
	if m.ResultNumber != 0 {
		return connectwire.AppendVariant(dst, protowire.Number(m.ResultNumber), m.ResultView)
	}
	return dst
}

func ConsumeMcpResult(b []byte, m *McpResult) error {
	return connectwire.ConsumeFields(b, "cursor MCP result", func(f connectwire.Field) error {
		if !connectwire.IsVariant(f, 1, 7) {
			return nil
		}
		*m = McpResult{}
		if f.Number == 1 {
			return connectwire.ConsumeNested(f, "cursor MCP result", &m.Success, ConsumeMcpSuccess)
		}
		m.ResultNumber = f.Number
		return connectwire.SetBytes(f, "cursor MCP result", &m.ResultView)
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
	dst = connectwire.AppendUint32(dst, 1, m.ID)
	dst = connectwire.AppendRaw(dst, 15, m.ExecIDView)
	dst = appendExecResult(dst, m)
	dst = connectwire.AppendInt32(dst, 39, m.LocalExecutionTimeMS)
	return connectwire.AppendRepeatedBytes(dst, 45, m.HookAdditionalContext)
}

func appendExecResult(dst []byte, m *ExecClientMessage) []byte {
	switch {
	case m.RequestContextResult != nil:
		return connectwire.AppendMessage(dst, 10, m.RequestContextResult, AppendRequestContextResult)
	case m.McpResult != nil:
		return connectwire.AppendMessage(dst, 11, m.McpResult, AppendMcpResult)
	case m.ResultNumber != 0:
		return connectwire.AppendVariant(dst, protowire.Number(m.ResultNumber), m.ResultView)
	}
	return dst
}

func ConsumeExecClientMessage(b []byte, m *ExecClientMessage) error {
	return connectwire.ConsumeFields(b, "cursor exec client", func(f connectwire.Field) error {
		switch f.Number {
		case 1:
			return connectwire.SetUint32(f, "cursor exec client", &m.ID)
		case 15:
			return connectwire.SetBytes(f, "cursor exec client", &m.ExecIDView)
		case 10:
			m.resetResult()
			return connectwire.ConsumeNested(f, "cursor exec client", &m.RequestContextResult, ConsumeRequestContextResult)
		case 11:
			m.resetResult()
			return connectwire.ConsumeNested(f, "cursor exec client", &m.McpResult, ConsumeMcpResult)
		case 39:
			return connectwire.SetInt32(f, "cursor exec client", &m.LocalExecutionTimeMS)
		case 45:
			return connectwire.AppendBytesField(f, "cursor exec client", &m.HookAdditionalContext)
		default:
			return consumeExecResultVariant(f, m)
		}
	})
}

func consumeExecResultVariant(f connectwire.Field, m *ExecClientMessage) error {
	if f.Kind != protowire.BytesType || f.Number < 2 || f.Number > 55 {
		return nil
	}
	m.resetResult()
	m.ResultNumber = f.Number
	return connectwire.SetBytes(f, "cursor exec client", &m.ResultView)
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
	dst = connectwire.AppendRaw(dst, 1, m.CommandView)
	dst = connectwire.AppendRaw(dst, 2, m.WorkingDirectoryView)
	dst = connectwire.AppendInt32(dst, 3, m.Timeout)
	dst = connectwire.AppendRaw(dst, 4, m.ToolCallIDView)
	dst = connectwire.AppendRepeatedBytes(dst, 5, m.SimpleCommands)
	dst = connectwire.AppendBool(dst, 6, m.InputRedirect)
	dst = connectwire.AppendBool(dst, 7, m.OutputRedirect)
	dst = connectwire.AppendRaw(dst, 8, m.ParsingResultView)
	dst = connectwire.AppendRaw(dst, 9, m.RequestedSandboxPolicyView)
	dst = connectwire.AppendUint64(dst, 10, m.FileOutputThresholdBytes)
	dst = connectwire.AppendBool(dst, 11, m.Background)
	dst = connectwire.AppendBool(dst, 12, m.SkipApproval)
	dst = connectwire.AppendInt32(dst, 13, m.TimeoutBehavior)
	dst = connectwire.AppendInt32(dst, 14, m.HardTimeout)
	dst = connectwire.AppendRaw(dst, 15, m.DescriptionView)
	dst = connectwire.AppendRaw(dst, 16, m.ClassifierResultView)
	dst = connectwire.AppendBool(dst, 17, m.CloseStdin)
	dst = connectwire.AppendRaw(dst, 18, m.OutputNotificationView)
	dst = connectwire.AppendRaw(dst, 19, m.SmartModeApprovalView)
	dst = connectwire.AppendRaw(dst, 20, m.HookApprovalRequirementView)
	return connectwire.AppendRaw(dst, 21, m.ConversationIDView)
}

func ConsumeShellArgs(b []byte, m *ShellArgs) error {
	return connectwire.ConsumeFields(b, "cursor shell args", func(f connectwire.Field) error {
		switch {
		case f.Number <= 7:
			return consumeShellBase(f, m)
		case f.Number <= 14:
			return consumeShellExecution(f, m)
		default:
			return consumeShellOptions(f, m)
		}
	})
}

func consumeShellBase(f connectwire.Field, m *ShellArgs) error {
	switch f.Number {
	case 1:
		return connectwire.SetBytes(f, "cursor shell args", &m.CommandView)
	case 2:
		return connectwire.SetBytes(f, "cursor shell args", &m.WorkingDirectoryView)
	case 3:
		return connectwire.SetInt32(f, "cursor shell args", &m.Timeout)
	case 4:
		return connectwire.SetBytes(f, "cursor shell args", &m.ToolCallIDView)
	case 5:
		if err := connectwire.RequireKind(f, protowire.BytesType, "cursor shell args"); err != nil {
			return err
		}
		m.SimpleCommands = append(m.SimpleCommands, connectwire.BytesValue(f))
	case 6:
		return connectwire.SetBool(f, "cursor shell args", &m.InputRedirect)
	case 7:
		return connectwire.SetBool(f, "cursor shell args", &m.OutputRedirect)
	}
	return nil
}

func consumeShellExecution(f connectwire.Field, m *ShellArgs) error {
	switch f.Number {
	case 8:
		return connectwire.SetBytes(f, "cursor shell args", &m.ParsingResultView)
	case 9:
		return connectwire.SetBytes(f, "cursor shell args", &m.RequestedSandboxPolicyView)
	case 10:
		return connectwire.SetUint64(f, "cursor shell args", &m.FileOutputThresholdBytes)
	case 11:
		return connectwire.SetBool(f, "cursor shell args", &m.Background)
	case 12:
		return connectwire.SetBool(f, "cursor shell args", &m.SkipApproval)
	case 13:
		return connectwire.SetInt32(f, "cursor shell args", &m.TimeoutBehavior)
	case 14:
		return connectwire.SetInt32(f, "cursor shell args", &m.HardTimeout)
	}
	return nil
}

func consumeShellOptions(f connectwire.Field, m *ShellArgs) error {
	switch f.Number {
	case 15:
		return connectwire.SetBytes(f, "cursor shell args", &m.DescriptionView)
	case 16:
		return connectwire.SetBytes(f, "cursor shell args", &m.ClassifierResultView)
	case 17:
		return connectwire.SetBool(f, "cursor shell args", &m.CloseStdin)
	case 18:
		return connectwire.SetBytes(f, "cursor shell args", &m.OutputNotificationView)
	case 19:
		return connectwire.SetBytes(f, "cursor shell args", &m.SmartModeApprovalView)
	case 20:
		return connectwire.SetBytes(f, "cursor shell args", &m.HookApprovalRequirementView)
	case 21:
		return connectwire.SetBytes(f, "cursor shell args", &m.ConversationIDView)
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
	dst = connectwire.AppendUint32(dst, 1, m.ID)
	dst = connectwire.AppendRaw(dst, 15, m.ExecIDView)
	dst = connectwire.AppendRaw(dst, 19, m.SpanContextView)
	dst = appendExecRequest(dst, m)
	return connectwire.AppendBool(dst, 55, m.AcceptHookAdditionalContexts)
}

func appendExecRequest(dst []byte, m *ExecServerMessage) []byte {
	switch {
	case m.Shell != nil:
		return connectwire.AppendMessage(dst, 2, m.Shell, AppendShellArgs)
	case m.RequestContext != nil:
		return connectwire.AppendMessage(dst, 10, m.RequestContext, AppendRequestContextArgs)
	case m.Mcp != nil:
		return connectwire.AppendMessage(dst, 11, m.Mcp, AppendMcpArgs)
	case m.ShellStream != nil:
		return connectwire.AppendMessage(dst, 14, m.ShellStream, AppendShellArgs)
	case m.MessageNumber != 0:
		return connectwire.AppendVariant(dst, protowire.Number(m.MessageNumber), m.MessageView)
	}
	return dst
}

func ConsumeExecServerMessage(b []byte, m *ExecServerMessage) error {
	return connectwire.ConsumeFields(b, "cursor exec server", func(f connectwire.Field) error {
		switch f.Number {
		case 1:
			return connectwire.SetUint32(f, "cursor exec server", &m.ID)
		case 15:
			return connectwire.SetBytes(f, "cursor exec server", &m.ExecIDView)
		case 19:
			return connectwire.SetBytes(f, "cursor exec server", &m.SpanContextView)
		case 55:
			return connectwire.SetBool(f, "cursor exec server", &m.AcceptHookAdditionalContexts)
		default:
			return consumeExecRequest(f, m)
		}
	})
}

func consumeExecRequest(f connectwire.Field, m *ExecServerMessage) error {
	if f.Kind != protowire.BytesType || f.Number < 2 || f.Number > 54 {
		return nil
	}
	m.resetRequest()
	switch f.Number {
	case 2:
		return connectwire.ConsumeNested(f, "cursor exec server", &m.Shell, ConsumeShellArgs)
	case 10:
		return connectwire.ConsumeNested(f, "cursor exec server", &m.RequestContext, ConsumeRequestContextArgs)
	case 11:
		return connectwire.ConsumeNested(f, "cursor exec server", &m.Mcp, ConsumeMcpArgs)
	case 14:
		return connectwire.ConsumeNested(f, "cursor exec server", &m.ShellStream, ConsumeShellArgs)
	}
	m.MessageNumber = f.Number
	return connectwire.SetBytes(f, "cursor exec server", &m.MessageView)
}
