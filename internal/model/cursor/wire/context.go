package wire

import (
	"github.com/gosuda/dalgona/internal/model/connectwire"
	"google.golang.org/protobuf/encoding/protowire"
)

type RequestContextArgs struct {
	NotesSessionIDView          []byte
	WorkspaceIDView             []byte
	ReadOnlyPinnedTreeSHAView   []byte
	ReadOnlyPluginCacheRootView []byte
	UseCached                   bool
}

func AppendRequestContextArgs(dst []byte, m *RequestContextArgs) []byte {
	dst = connectwire.AppendRaw(dst, 2, m.NotesSessionIDView)
	dst = connectwire.AppendRaw(dst, 3, m.WorkspaceIDView)
	dst = connectwire.AppendRaw(dst, 4, m.ReadOnlyPinnedTreeSHAView)
	dst = connectwire.AppendRaw(dst, 5, m.ReadOnlyPluginCacheRootView)
	return connectwire.AppendBool(dst, 7, m.UseCached)
}

func ConsumeRequestContextArgs(b []byte, m *RequestContextArgs) error {
	return connectwire.ConsumeFields(b, "cursor request context args", func(f connectwire.Field) error {
		switch f.Number {
		case 2:
			return connectwire.SetBytes(f, "cursor request context args", &m.NotesSessionIDView)
		case 3:
			return connectwire.SetBytes(f, "cursor request context args", &m.WorkspaceIDView)
		case 4:
			return connectwire.SetBytes(f, "cursor request context args", &m.ReadOnlyPinnedTreeSHAView)
		case 5:
			return connectwire.SetBytes(f, "cursor request context args", &m.ReadOnlyPluginCacheRootView)
		case 7:
			return connectwire.SetBool(f, "cursor request context args", &m.UseCached)
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
	dst = connectwire.AppendRepeatedBytes(dst, 2, m.Rules)
	dst = connectwire.AppendRaw(dst, 4, m.EnvView)
	dst = connectwire.AppendRepeatedBytes(dst, 6, m.RepositoryInfo)
	for i := range m.Tools {
		dst = connectwire.AppendMessage(dst, 7, &m.Tools[i], AppendMcpToolDefinition)
	}
	dst = connectwire.AppendRaw(dst, 8, m.ConversationNotesListingView)
	dst = connectwire.AppendRaw(dst, 9, m.SharedNotesListingView)
	dst = connectwire.AppendRepeatedBytes(dst, 11, m.GitRepos)
	dst = connectwire.AppendRepeatedBytes(dst, 13, m.ProjectLayouts)
	dst = connectwire.AppendRepeatedBytes(dst, 14, m.McpInstructions)
	dst = connectwire.AppendRaw(dst, 15, m.DebugModeConfigView)
	dst = connectwire.AppendRaw(dst, 16, m.CloudRuleView)
	dst = connectwire.AppendBool(dst, 17, m.WebSearchEnabled)
	dst = connectwire.AppendRaw(dst, 18, m.SkillOptionsView)
	dst = connectwire.AppendBool(dst, 19, m.RepositoryInfoShouldQueryProd)
	dst = connectwire.AppendRepeatedBytes(dst, 20, m.FileContents)
	dst = connectwire.AppendRaw(dst, 21, m.UserIntentSummaryView)
	dst = connectwire.AppendRepeatedBytes(dst, 22, m.CustomSubagents)
	return connectwire.AppendRaw(dst, 23, m.McpFileSystemOptionsView)
}

func ConsumeRequestContext(b []byte, m *RequestContext) error {
	return connectwire.ConsumeFields(b, "cursor request context", func(f connectwire.Field) error {
		switch {
		case f.Number <= 9:
			return consumeRequestContextCore(f, m)
		case f.Number <= 16:
			return consumeRequestContextWorkspace(f, m)
		default:
			return consumeRequestContextOptions(f, m)
		}
	})
}

func consumeRequestContextCore(f connectwire.Field, m *RequestContext) error {
	switch f.Number {
	case 2:
		return connectwire.AppendBytesField(f, "cursor request context", &m.Rules)
	case 4:
		return connectwire.SetBytes(f, "cursor request context", &m.EnvView)
	case 6:
		return connectwire.AppendBytesField(f, "cursor request context", &m.RepositoryInfo)
	case 7:
		return connectwire.ConsumeRepeated(f, "cursor request context", &m.Tools, ConsumeMcpToolDefinition)
	case 8:
		return connectwire.SetBytes(f, "cursor request context", &m.ConversationNotesListingView)
	case 9:
		return connectwire.SetBytes(f, "cursor request context", &m.SharedNotesListingView)
	}
	return nil
}

func consumeRequestContextWorkspace(f connectwire.Field, m *RequestContext) error {
	switch f.Number {
	case 11:
		return connectwire.AppendBytesField(f, "cursor request context", &m.GitRepos)
	case 13:
		return connectwire.AppendBytesField(f, "cursor request context", &m.ProjectLayouts)
	case 14:
		return connectwire.AppendBytesField(f, "cursor request context", &m.McpInstructions)
	case 15:
		return connectwire.SetBytes(f, "cursor request context", &m.DebugModeConfigView)
	case 16:
		return connectwire.SetBytes(f, "cursor request context", &m.CloudRuleView)
	}
	return nil
}

func consumeRequestContextOptions(f connectwire.Field, m *RequestContext) error {
	switch f.Number {
	case 17:
		return connectwire.SetBool(f, "cursor request context", &m.WebSearchEnabled)
	case 18:
		return connectwire.SetBytes(f, "cursor request context", &m.SkillOptionsView)
	case 19:
		return connectwire.SetBool(f, "cursor request context", &m.RepositoryInfoShouldQueryProd)
	case 20:
		return connectwire.AppendBytesField(f, "cursor request context", &m.FileContents)
	case 21:
		return connectwire.SetBytes(f, "cursor request context", &m.UserIntentSummaryView)
	case 22:
		return connectwire.AppendBytesField(f, "cursor request context", &m.CustomSubagents)
	case 23:
		return connectwire.SetBytes(f, "cursor request context", &m.McpFileSystemOptionsView)
	}
	return nil
}

type RequestContextSuccess struct {
	RequestContext      *RequestContext
	ServedFromDiskCache bool
}

func AppendRequestContextSuccess(dst []byte, m *RequestContextSuccess) []byte {
	dst = connectwire.AppendOptionalMessage(dst, 1, m.RequestContext, AppendRequestContext)
	return connectwire.AppendBool(dst, 2, m.ServedFromDiskCache)
}

func ConsumeRequestContextSuccess(b []byte, m *RequestContextSuccess) error {
	return connectwire.ConsumeFields(b, "cursor request context success", func(f connectwire.Field) error {
		switch f.Number {
		case 1:
			return connectwire.ConsumeNested(f, "cursor request context success", &m.RequestContext, ConsumeRequestContext)
		case 2:
			return connectwire.SetBool(f, "cursor request context success", &m.ServedFromDiskCache)
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
		return connectwire.AppendMessage(dst, 1, m.Success, AppendRequestContextSuccess)
	}
	if m.ResultNumber != 0 {
		return connectwire.AppendVariant(dst, protowire.Number(m.ResultNumber), m.ResultView)
	}
	return dst
}

func ConsumeRequestContextResult(b []byte, m *RequestContextResult) error {
	return connectwire.ConsumeFields(b, "cursor request context result", func(f connectwire.Field) error {
		if f.Number < 1 || f.Number > 3 {
			return nil
		}
		*m = RequestContextResult{}
		if f.Number == 1 {
			return connectwire.ConsumeNested(f, "cursor request context result", &m.Success, ConsumeRequestContextSuccess)
		}
		m.ResultNumber = f.Number
		return connectwire.SetBytes(f, "cursor request context result", &m.ResultView)
	})
}
