package wire

import "google.golang.org/protobuf/encoding/protowire"

type RequestContextArgs struct {
	NotesSessionIDView          []byte
	WorkspaceIDView             []byte
	ReadOnlyPinnedTreeSHAView   []byte
	ReadOnlyPluginCacheRootView []byte
	UseCached                   bool
}

func AppendRequestContextArgs(dst []byte, m *RequestContextArgs) []byte {
	dst = appendRaw(dst, 2, m.NotesSessionIDView)
	dst = appendRaw(dst, 3, m.WorkspaceIDView)
	dst = appendRaw(dst, 4, m.ReadOnlyPinnedTreeSHAView)
	dst = appendRaw(dst, 5, m.ReadOnlyPluginCacheRootView)
	return appendBool(dst, 7, m.UseCached)
}

func ConsumeRequestContextArgs(b []byte, m *RequestContextArgs) error {
	return consumeFields(b, "cursor request context args", func(f field) error {
		switch f.number {
		case 2:
			return setBytes(f, "cursor request context args", &m.NotesSessionIDView)
		case 3:
			return setBytes(f, "cursor request context args", &m.WorkspaceIDView)
		case 4:
			return setBytes(f, "cursor request context args", &m.ReadOnlyPinnedTreeSHAView)
		case 5:
			return setBytes(f, "cursor request context args", &m.ReadOnlyPluginCacheRootView)
		case 7:
			return setBool(f, "cursor request context args", &m.UseCached)
		default:
			return nil
		}
	})
}

type RequestContext struct {
	Rules                         [][]byte
	EnvView                       []byte
	RepositoryInfo                [][]byte
	Tools                         []McpToolDefinition
	ConversationNotesListingView  []byte
	SharedNotesListingView        []byte
	GitRepos                      [][]byte
	ProjectLayouts                [][]byte
	McpInstructions               [][]byte
	DebugModeConfigView           []byte
	CloudRuleView                 []byte
	WebSearchEnabled              bool
	SkillOptionsView              []byte
	RepositoryInfoShouldQueryProd bool
	FileContents                  [][]byte
	UserIntentSummaryView         []byte
	CustomSubagents               [][]byte
	McpFileSystemOptionsView      []byte
}

func AppendRequestContext(dst []byte, m *RequestContext) []byte {
	dst = appendRepeatedBytes(dst, 2, m.Rules)
	dst = appendRaw(dst, 4, m.EnvView)
	dst = appendRepeatedBytes(dst, 6, m.RepositoryInfo)
	for i := range m.Tools {
		dst = appendMessage(dst, 7, &m.Tools[i], AppendMcpToolDefinition)
	}
	dst = appendRaw(dst, 8, m.ConversationNotesListingView)
	dst = appendRaw(dst, 9, m.SharedNotesListingView)
	dst = appendRepeatedBytes(dst, 11, m.GitRepos)
	dst = appendRepeatedBytes(dst, 13, m.ProjectLayouts)
	dst = appendRepeatedBytes(dst, 14, m.McpInstructions)
	dst = appendRaw(dst, 15, m.DebugModeConfigView)
	dst = appendRaw(dst, 16, m.CloudRuleView)
	dst = appendBool(dst, 17, m.WebSearchEnabled)
	dst = appendRaw(dst, 18, m.SkillOptionsView)
	dst = appendBool(dst, 19, m.RepositoryInfoShouldQueryProd)
	dst = appendRepeatedBytes(dst, 20, m.FileContents)
	dst = appendRaw(dst, 21, m.UserIntentSummaryView)
	dst = appendRepeatedBytes(dst, 22, m.CustomSubagents)
	return appendRaw(dst, 23, m.McpFileSystemOptionsView)
}

func ConsumeRequestContext(b []byte, m *RequestContext) error {
	return consumeFields(b, "cursor request context", func(f field) error {
		switch {
		case f.number <= 9:
			return consumeRequestContextCore(f, m)
		case f.number <= 16:
			return consumeRequestContextWorkspace(f, m)
		default:
			return consumeRequestContextOptions(f, m)
		}
	})
}

func consumeRequestContextCore(f field, m *RequestContext) error {
	switch f.number {
	case 2:
		return appendBytesField(f, "cursor request context", &m.Rules)
	case 4:
		return setBytes(f, "cursor request context", &m.EnvView)
	case 6:
		return appendBytesField(f, "cursor request context", &m.RepositoryInfo)
	case 7:
		return consumeRepeated(f, "cursor request context", &m.Tools, ConsumeMcpToolDefinition)
	case 8:
		return setBytes(f, "cursor request context", &m.ConversationNotesListingView)
	case 9:
		return setBytes(f, "cursor request context", &m.SharedNotesListingView)
	}
	return nil
}

func consumeRequestContextWorkspace(f field, m *RequestContext) error {
	switch f.number {
	case 11:
		return appendBytesField(f, "cursor request context", &m.GitRepos)
	case 13:
		return appendBytesField(f, "cursor request context", &m.ProjectLayouts)
	case 14:
		return appendBytesField(f, "cursor request context", &m.McpInstructions)
	case 15:
		return setBytes(f, "cursor request context", &m.DebugModeConfigView)
	case 16:
		return setBytes(f, "cursor request context", &m.CloudRuleView)
	}
	return nil
}

func consumeRequestContextOptions(f field, m *RequestContext) error {
	switch f.number {
	case 17:
		return setBool(f, "cursor request context", &m.WebSearchEnabled)
	case 18:
		return setBytes(f, "cursor request context", &m.SkillOptionsView)
	case 19:
		return setBool(f, "cursor request context", &m.RepositoryInfoShouldQueryProd)
	case 20:
		return appendBytesField(f, "cursor request context", &m.FileContents)
	case 21:
		return setBytes(f, "cursor request context", &m.UserIntentSummaryView)
	case 22:
		return appendBytesField(f, "cursor request context", &m.CustomSubagents)
	case 23:
		return setBytes(f, "cursor request context", &m.McpFileSystemOptionsView)
	}
	return nil
}

type RequestContextSuccess struct {
	RequestContext      *RequestContext
	ServedFromDiskCache bool
}

func AppendRequestContextSuccess(dst []byte, m *RequestContextSuccess) []byte {
	dst = appendOptionalMessage(dst, 1, m.RequestContext, AppendRequestContext)
	return appendBool(dst, 2, m.ServedFromDiskCache)
}

func ConsumeRequestContextSuccess(b []byte, m *RequestContextSuccess) error {
	return consumeFields(b, "cursor request context success", func(f field) error {
		switch f.number {
		case 1:
			return consumeNested(f, "cursor request context success", &m.RequestContext, ConsumeRequestContext)
		case 2:
			return setBool(f, "cursor request context success", &m.ServedFromDiskCache)
		default:
			return nil
		}
	})
}

type RequestContextResult struct {
	Success      *RequestContextSuccess
	ResultNumber int32
	ResultView   []byte
}

func AppendRequestContextResult(dst []byte, m *RequestContextResult) []byte {
	if m.Success != nil {
		return appendMessage(dst, 1, m.Success, AppendRequestContextSuccess)
	}
	if m.ResultNumber != 0 {
		return appendVariant(dst, protowire.Number(m.ResultNumber), m.ResultView)
	}
	return dst
}

func ConsumeRequestContextResult(b []byte, m *RequestContextResult) error {
	return consumeFields(b, "cursor request context result", func(f field) error {
		if f.number < 1 || f.number > 3 {
			return nil
		}
		*m = RequestContextResult{}
		if f.number == 1 {
			return consumeNested(f, "cursor request context result", &m.Success, ConsumeRequestContextSuccess)
		}
		m.ResultNumber = f.number
		return setBytes(f, "cursor request context result", &m.ResultView)
	})
}
