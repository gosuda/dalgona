package wire_test

import (
	"bytes"
	"math/rand/v2"
	"reflect"
	"slices"
	"testing"

	"github.com/gosuda/dalgona/internal/model/cursor/wire"
	"google.golang.org/protobuf/encoding/protowire"
)

func randomBytes(r *rand.Rand) []byte {
	out := make([]byte, 1+r.IntN(9))
	for i := range out {
		out[i] = byte(r.Uint32() & 0xff)
	}
	return out
}

func randomList(r *rand.Rand) [][]byte {
	out := make([][]byte, 1+r.IntN(3))
	for i := range out {
		out[i] = randomBytes(r)
	}
	return out
}

func checkRoundTrip[T any](
	t *testing.T,
	name string,
	makeValue func(*rand.Rand) T,
	appendValue func([]byte, *T) []byte,
	consumeValue func([]byte, *T) error,
) {
	t.Helper()
	t.Run(name, func(t *testing.T) {
		t.Parallel()
		r := rand.New(rand.NewPCG(0x5eed, 0xc0dec))
		for range 64 {
			want := makeValue(r)
			framed := appendValue([]byte{0xa5}, &want)
			var got T
			if err := consumeValue(framed[1:], &got); err != nil {
				t.Fatalf("decode: %v", err)
			}
			if !reflect.DeepEqual(got, want) {
				t.Fatalf("round trip mismatch\n got: %#v\nwant: %#v", got, want)
			}
		}
	})
}

func TestMessageRoundTrips(t *testing.T) {
	t.Parallel()
	checkRoundTrip(t, "model parameter", func(r *rand.Rand) wire.ModelParameter {
		return wire.ModelParameter{IDView: randomBytes(r), ValueView: randomBytes(r)}
	}, wire.AppendModelParameter, wire.ConsumeModelParameter)
	checkRoundTrip(t, "requested model", func(r *rand.Rand) wire.RequestedModel {
		return wire.RequestedModel{ModelIDView: randomBytes(r), MaxMode: true, Parameters: []wire.ModelParameter{{IDView: randomBytes(r), ValueView: randomBytes(r)}}, CredentialNumber: 4, CredentialsView: randomBytes(r)}
	}, wire.AppendRequestedModel, wire.ConsumeRequestedModel)
	checkRoundTrip(t, "model details", func(r *rand.Rand) wire.ModelDetails {
		return wire.ModelDetails{ModelIDView: randomBytes(r), ThinkingDetailsView: randomBytes(r), DisplayModelIDView: randomBytes(r), DisplayNameView: randomBytes(r), DisplayShortView: randomBytes(r), Aliases: randomList(r), MaxMode: true, CredentialNumber: 8, CredentialsView: randomBytes(r)}
	}, wire.AppendModelDetails, wire.ConsumeModelDetails)
	checkRoundTrip(t, "usable models request", func(r *rand.Rand) wire.GetUsableModelsRequest {
		return wire.GetUsableModelsRequest{CustomModelIDs: randomList(r)}
	}, wire.AppendGetUsableModelsRequest, wire.ConsumeGetUsableModelsRequest)
	checkRoundTrip(t, "usable models response", func(r *rand.Rand) wire.GetUsableModelsResponse {
		return wire.GetUsableModelsResponse{Models: []wire.ModelDetails{{ModelIDView: randomBytes(r), DisplayNameView: randomBytes(r), Aliases: randomList(r)}}}
	}, wire.AppendGetUsableModelsResponse, wire.ConsumeGetUsableModelsResponse)
	checkRoundTrip(t, "conversation", func(r *rand.Rand) wire.ConversationState {
		return wire.ConversationState{RootPromptMessages: randomList(r), TurnsOld: randomList(r), Todos: randomList(r), PendingToolCalls: randomList(r), TokenDetailsView: randomBytes(r), SummaryView: randomBytes(r), PlanView: randomBytes(r), Turns: randomList(r), PreviousWorkspaces: randomList(r), Mode: -3, SummaryArchiveView: randomBytes(r), SummaryArchives: randomList(r), SelfSummaryCount: 17, ReadPaths: randomList(r)}
	}, wire.AppendConversationState, wire.ConsumeConversationState)
	checkRoundTrip(t, "user message", func(r *rand.Rand) wire.UserMessage {
		return wire.UserMessage{TextView: randomBytes(r), MessageIDView: randomBytes(r), SelectedContextView: randomBytes(r), Mode: -2, Simulated: true, BestOfNGroupIDView: randomBytes(r), TryBestOfN: true, RichTextView: randomBytes(r)}
	}, wire.AppendUserMessage, wire.ConsumeUserMessage)
	checkRoundTrip(t, "user action", func(r *rand.Rand) wire.UserMessageAction {
		return wire.UserMessageAction{UserMessage: &wire.UserMessage{TextView: randomBytes(r), MessageIDView: randomBytes(r)}, RequestContextView: randomBytes(r), SendToInteractionListener: true}
	}, wire.AppendUserMessageAction, wire.ConsumeUserMessageAction)
	checkRoundTrip(t, "conversation action", func(r *rand.Rand) wire.ConversationAction {
		return wire.ConversationAction{User: &wire.UserMessageAction{UserMessage: &wire.UserMessage{TextView: randomBytes(r)}}}
	}, wire.AppendConversationAction, wire.ConsumeConversationAction)
	checkRoundTrip(t, "MCP definition", func(r *rand.Rand) wire.McpToolDefinition {
		return wire.McpToolDefinition{NameView: randomBytes(r), DescriptionView: randomBytes(r), InputSchemaView: randomBytes(r), ProviderIdentifierView: randomBytes(r), ToolNameView: randomBytes(r), InputSchemaJSONView: randomBytes(r)}
	}, wire.AppendMcpToolDefinition, wire.ConsumeMcpToolDefinition)
	checkRoundTrip(t, "run request", func(r *rand.Rand) wire.AgentRunRequest {
		return wire.AgentRunRequest{Conversation: &wire.ConversationState{RootPromptMessages: randomList(r), Turns: randomList(r)}, Action: &wire.ConversationAction{User: &wire.UserMessageAction{UserMessage: &wire.UserMessage{TextView: randomBytes(r)}}}, ModelDetails: &wire.ModelDetails{ModelIDView: randomBytes(r)}, Tools: []wire.McpToolDefinition{{NameView: randomBytes(r), InputSchemaView: randomBytes(r)}}, ConversationIDView: randomBytes(r), FileSystemOptionsView: randomBytes(r), SkillOptionsView: randomBytes(r), CustomSystemPromptView: randomBytes(r), RequestedModel: &wire.RequestedModel{ModelIDView: randomBytes(r), MaxMode: true}}
	}, wire.AppendAgentRunRequest, wire.ConsumeAgentRunRequest)
	checkRoundTrip(t, "MCP argument", func(r *rand.Rand) wire.McpArgument {
		return wire.McpArgument{NameView: randomBytes(r), ValueView: randomBytes(r)}
	}, wire.AppendMcpArgument, wire.ConsumeMcpArgument)
	checkRoundTrip(t, "MCP args", func(r *rand.Rand) wire.McpArgs {
		return wire.McpArgs{NameView: randomBytes(r), Args: []wire.McpArgument{{NameView: randomBytes(r), ValueView: randomBytes(r)}}, ToolCallIDView: randomBytes(r), ProviderIdentifierView: randomBytes(r), ToolNameView: randomBytes(r), SmartModeApprovalView: randomBytes(r), ApprovalOnly: true, SkipApproval: true, ServerIdentifierView: randomBytes(r)}
	}, wire.AppendMcpArgs, wire.ConsumeMcpArgs)
	checkRoundTrip(t, "MCP content item", func(r *rand.Rand) wire.McpContentItem {
		return wire.McpContentItem{ContentNumber: 2, ContentView: randomBytes(r)}
	}, wire.AppendMcpContentItem, wire.ConsumeMcpContentItem)
	checkRoundTrip(t, "MCP success", func(r *rand.Rand) wire.McpSuccess {
		return wire.McpSuccess{Content: []wire.McpContentItem{{ContentNumber: 1, ContentView: randomBytes(r)}, {ContentNumber: 2, ContentView: randomBytes(r)}}, IsError: true}
	}, wire.AppendMcpSuccess, wire.ConsumeMcpSuccess)
	checkRoundTrip(t, "MCP result", func(r *rand.Rand) wire.McpResult {
		return wire.McpResult{Success: &wire.McpSuccess{Content: []wire.McpContentItem{{ContentNumber: 1, ContentView: randomBytes(r)}}, IsError: true}}
	}, wire.AppendMcpResult, wire.ConsumeMcpResult)
	checkRoundTrip(t, "exec client", func(r *rand.Rand) wire.ExecClientMessage {
		return wire.ExecClientMessage{ID: 71, ExecIDView: randomBytes(r), McpResult: &wire.McpResult{Success: &wire.McpSuccess{IsError: true}}, LocalExecutionTimeMS: -19, HookAdditionalContext: randomList(r)}
	}, wire.AppendExecClientMessage, wire.ConsumeExecClientMessage)
	checkRoundTrip(t, "MCP dry-run approval", func(r *rand.Rand) wire.ExecClientMessage {
		return wire.ExecClientMessage{ID: r.Uint32(), McpResult: &wire.McpResult{ResultNumber: 7, ResultView: []byte{}}}
	}, wire.AppendExecClientMessage, wire.ConsumeExecClientMessage)
	checkRoundTrip(t, "shell args", func(r *rand.Rand) wire.ShellArgs {
		return wire.ShellArgs{CommandView: randomBytes(r), WorkingDirectoryView: randomBytes(r), Timeout: -9, ToolCallIDView: randomBytes(r), SimpleCommands: randomList(r), InputRedirect: true, OutputRedirect: true, ParsingResultView: randomBytes(r), RequestedSandboxPolicyView: randomBytes(r), FileOutputThresholdBytes: 1 << 40, Background: true, SkipApproval: true, TimeoutBehavior: -4, HardTimeout: -8, DescriptionView: randomBytes(r), ClassifierResultView: randomBytes(r), CloseStdin: true, OutputNotificationView: randomBytes(r), SmartModeApprovalView: randomBytes(r), HookApprovalRequirementView: randomBytes(r), ConversationIDView: randomBytes(r)}
	}, wire.AppendShellArgs, wire.ConsumeShellArgs)
	checkRoundTrip(t, "exec server", func(r *rand.Rand) wire.ExecServerMessage {
		return wire.ExecServerMessage{ID: 55, ExecIDView: randomBytes(r), SpanContextView: randomBytes(r), Mcp: &wire.McpArgs{NameView: randomBytes(r), ApprovalOnly: true}, AcceptHookAdditionalContexts: true}
	}, wire.AppendExecServerMessage, wire.ConsumeExecServerMessage)
	checkRoundTrip(t, "exec shell stream", func(r *rand.Rand) wire.ExecServerMessage {
		return wire.ExecServerMessage{ID: r.Uint32(), ShellStream: &wire.ShellArgs{CommandView: randomBytes(r), Timeout: r.Int32() - r.Int32()}}
	}, wire.AppendExecServerMessage, wire.ConsumeExecServerMessage)
	checkRoundTrip(t, "request context args", func(r *rand.Rand) wire.RequestContextArgs {
		return wire.RequestContextArgs{NotesSessionIDView: randomBytes(r), WorkspaceIDView: randomBytes(r), ReadOnlyPinnedTreeSHAView: randomBytes(r), ReadOnlyPluginCacheRootView: randomBytes(r), UseCached: r.IntN(2) == 1}
	}, wire.AppendRequestContextArgs, wire.ConsumeRequestContextArgs)
	checkRoundTrip(t, "request context", func(r *rand.Rand) wire.RequestContext {
		return wire.RequestContext{Rules: randomList(r), EnvView: randomBytes(r), RepositoryInfo: randomList(r), Tools: []wire.McpToolDefinition{{NameView: randomBytes(r), ToolNameView: randomBytes(r), InputSchemaView: randomBytes(r)}}, ConversationNotesListingView: randomBytes(r), SharedNotesListingView: randomBytes(r), GitRepos: randomList(r), ProjectLayouts: randomList(r), McpInstructions: randomList(r), DebugModeConfigView: randomBytes(r), CloudRuleView: randomBytes(r), WebSearchEnabled: r.IntN(2) == 1, SkillOptionsView: randomBytes(r), RepositoryInfoShouldQueryProd: r.IntN(2) == 1, FileContents: randomList(r), UserIntentSummaryView: randomBytes(r), CustomSubagents: randomList(r), McpFileSystemOptionsView: randomBytes(r)}
	}, wire.AppendRequestContext, wire.ConsumeRequestContext)
	checkRoundTrip(t, "request context success", func(r *rand.Rand) wire.RequestContextSuccess {
		return wire.RequestContextSuccess{RequestContext: &wire.RequestContext{CloudRuleView: randomBytes(r)}, ServedFromDiskCache: true}
	}, wire.AppendRequestContextSuccess, wire.ConsumeRequestContextSuccess)
	checkRoundTrip(t, "request context rejected", func(r *rand.Rand) wire.RequestContextResult {
		return wire.RequestContextResult{ResultNumber: r.Int32N(2) + 2, ResultView: randomBytes(r)}
	}, wire.AppendRequestContextResult, wire.ConsumeRequestContextResult)
	checkRoundTrip(t, "exec request context result", func(r *rand.Rand) wire.ExecClientMessage {
		return wire.ExecClientMessage{ID: r.Uint32(), RequestContextResult: &wire.RequestContextResult{Success: &wire.RequestContextSuccess{RequestContext: &wire.RequestContext{Tools: []wire.McpToolDefinition{{NameView: randomBytes(r)}}}}}, LocalExecutionTimeMS: r.Int32() - r.Int32()}
	}, wire.AppendExecClientMessage, wire.ConsumeExecClientMessage)
	checkRoundTrip(t, "exec request context args", func(r *rand.Rand) wire.ExecServerMessage {
		return wire.ExecServerMessage{ID: r.Uint32(), RequestContext: &wire.RequestContextArgs{WorkspaceIDView: randomBytes(r), UseCached: true}}
	}, wire.AppendExecServerMessage, wire.ConsumeExecServerMessage)
	checkRoundTrip(t, "text delta", func(r *rand.Rand) wire.TextDelta {
		return wire.TextDelta{TextView: randomBytes(r)}
	}, wire.AppendTextDelta, wire.ConsumeTextDelta)
	checkRoundTrip(t, "thinking delta", func(r *rand.Rand) wire.ThinkingDelta {
		return wire.ThinkingDelta{TextView: randomBytes(r)}
	}, wire.AppendThinkingDelta, wire.ConsumeThinkingDelta)
	checkRoundTrip(t, "token delta", func(r *rand.Rand) wire.TokenDelta {
		return wire.TokenDelta{Tokens: r.Int32() - r.Int32()}
	}, wire.AppendTokenDelta, wire.ConsumeTokenDelta)
	checkRoundTrip(t, "MCP call", func(r *rand.Rand) wire.McpToolCall {
		return wire.McpToolCall{Args: &wire.McpArgs{NameView: randomBytes(r)}, Result: &wire.McpResult{Success: &wire.McpSuccess{}}, DescriptionView: randomBytes(r)}
	}, wire.AppendMcpToolCall, wire.ConsumeMcpToolCall)
	checkRoundTrip(t, "tool call", func(r *rand.Rand) wire.ToolCall {
		return wire.ToolCall{Mcp: &wire.McpToolCall{Args: &wire.McpArgs{NameView: randomBytes(r)}}, ToolCallIDView: randomBytes(r)}
	}, wire.AppendToolCall, wire.ConsumeToolCall)
	checkRoundTrip(t, "tool started", func(r *rand.Rand) wire.ToolCallStartedUpdate {
		return wire.ToolCallStartedUpdate{CallIDView: randomBytes(r), ToolCall: &wire.ToolCall{ToolCallIDView: randomBytes(r)}, ModelCallIDView: randomBytes(r)}
	}, wire.AppendToolCallStartedUpdate, wire.ConsumeToolCallStartedUpdate)
	checkRoundTrip(t, "tool completed", func(r *rand.Rand) wire.ToolCallCompletedUpdate {
		return wire.ToolCallCompletedUpdate{CallIDView: randomBytes(r), ToolCall: &wire.ToolCall{ToolCallIDView: randomBytes(r)}, ModelCallIDView: randomBytes(r)}
	}, wire.AppendToolCallCompletedUpdate, wire.ConsumeToolCallCompletedUpdate)
	checkRoundTrip(t, "partial tool", func(r *rand.Rand) wire.PartialToolCallUpdate {
		return wire.PartialToolCallUpdate{CallIDView: randomBytes(r), ToolCall: &wire.ToolCall{ToolCallIDView: randomBytes(r)}, ArgsTextView: randomBytes(r), ModelCallIDView: randomBytes(r)}
	}, wire.AppendPartialToolCallUpdate, wire.ConsumePartialToolCallUpdate)
	checkRoundTrip(t, "tool delta", func(r *rand.Rand) wire.ToolCallDeltaUpdate {
		return wire.ToolCallDeltaUpdate{CallIDView: randomBytes(r), ToolCallDeltaView: randomBytes(r), ModelCallIDView: randomBytes(r)}
	}, wire.AppendToolCallDeltaUpdate, wire.ConsumeToolCallDeltaUpdate)
	checkRoundTrip(t, "turn ended", func(r *rand.Rand) wire.TurnEndedUpdate {
		return wire.TurnEndedUpdate{Input: -1, Output: 1 << 50, CacheRead: 23, CacheWrite: 45, Reasoning: 67}
	}, wire.AppendTurnEndedUpdate, wire.ConsumeTurnEndedUpdate)
	checkRoundTrip(t, "interaction", func(r *rand.Rand) wire.InteractionUpdate {
		return wire.InteractionUpdate{Started: &wire.ToolCallStartedUpdate{CallIDView: randomBytes(r)}}
	}, wire.AppendInteractionUpdate, wire.ConsumeInteractionUpdate)
	checkRoundTrip(t, "token interaction", func(r *rand.Rand) wire.InteractionUpdate {
		return wire.InteractionUpdate{TokenDelta: &wire.TokenDelta{Tokens: r.Int32() - r.Int32()}}
	}, wire.AppendInteractionUpdate, wire.ConsumeInteractionUpdate)
	checkRoundTrip(t, "tool delta interaction", func(r *rand.Rand) wire.InteractionUpdate {
		return wire.InteractionUpdate{ToolDelta: &wire.ToolCallDeltaUpdate{CallIDView: randomBytes(r)}}
	}, wire.AppendInteractionUpdate, wire.ConsumeInteractionUpdate)
	checkRoundTrip(t, "server message", func(r *rand.Rand) wire.AgentServerMessage {
		return wire.AgentServerMessage{Exec: &wire.ExecServerMessage{ID: 9, Mcp: &wire.McpArgs{NameView: randomBytes(r)}}}
	}, wire.AppendAgentServerMessage, wire.ConsumeAgentServerMessage)
	checkRoundTrip(t, "client message", func(r *rand.Rand) wire.AgentClientMessage {
		return wire.AgentClientMessage{Exec: &wire.ExecClientMessage{ID: 9, ResultNumber: 2, ResultView: randomBytes(r)}}
	}, wire.AppendAgentClientMessage, wire.ConsumeAgentClientMessage)
}

func TestHandAssembledRunRequest(t *testing.T) {
	t.Parallel()
	state := protowire.AppendTag(nil, 1, protowire.BytesType)
	state = protowire.AppendBytes(state, []byte("prompt-blob"))
	model := protowire.AppendTag(nil, 1, protowire.BytesType)
	model = protowire.AppendString(model, "model-a")
	definition := protowire.AppendTag(nil, 1, protowire.BytesType)
	definition = protowire.AppendString(definition, "tool-a")
	tools := protowire.AppendTag(nil, 1, protowire.BytesType)
	tools = protowire.AppendBytes(tools, definition)
	frame := appendField(1, state)
	frame = append(frame, appendField(3, model)...)
	frame = append(frame, appendField(4, tools)...)
	var got wire.AgentRunRequest
	if err := wire.ConsumeAgentRunRequest(frame, &got); err != nil {
		t.Fatal(err)
	}
	if got.Conversation == nil || len(got.Conversation.RootPromptMessages) != 1 || string(got.Conversation.RootPromptMessages[0]) != "prompt-blob" {
		t.Fatalf("conversation = %#v", got.Conversation)
	}
	if got.ModelDetails == nil || string(got.ModelDetails.ModelIDView) != "model-a" {
		t.Fatalf("model = %#v", got.ModelDetails)
	}
	if len(got.Tools) != 1 || string(got.Tools[0].NameView) != "tool-a" {
		t.Fatalf("tools = %#v", got.Tools)
	}
}

func TestHandAssembledInteraction(t *testing.T) {
	t.Parallel()
	usage := protowire.AppendTag(nil, 1, protowire.VarintType)
	usage = protowire.AppendVarint(usage, 91)
	interaction := appendField(14, usage)
	server := appendField(1, interaction)
	var update wire.AgentServerMessage
	if err := wire.ConsumeAgentServerMessage(server, &update); err != nil {
		t.Fatal(err)
	}
	if update.Interaction == nil || update.Interaction.Ended == nil || update.Interaction.Ended.Input != 91 {
		t.Fatalf("update = %#v", update)
	}
}

func TestHandAssembledExec(t *testing.T) {
	t.Parallel()
	arg := appendField(1, []byte("path"))
	arg = append(arg, appendField(2, []byte("go.mod"))...)
	args := appendField(1, []byte("read"))
	args = append(args, appendField(2, arg)...)
	args = protowire.AppendTag(args, 7, protowire.VarintType)
	args = protowire.AppendVarint(args, 1)
	exec := appendField(11, args)
	exec = protowire.AppendTag(exec, 1, protowire.VarintType)
	exec = protowire.AppendVarint(exec, 44)
	var request wire.ExecServerMessage
	if err := wire.ConsumeExecServerMessage(exec, &request); err != nil {
		t.Fatal(err)
	}
	if request.ID != 44 || request.Mcp == nil || !request.Mcp.ApprovalOnly || len(request.Mcp.Args) != 1 || string(request.Mcp.Args[0].ValueView) != "go.mod" {
		t.Fatalf("exec = %#v", request)
	}
}

func TestHandAssembledMcpDryRunResult(t *testing.T) {
	t.Parallel()
	result := protowire.AppendTag(nil, 7, protowire.BytesType)
	result = protowire.AppendBytes(result, nil)
	message := appendField(11, result)
	var got wire.ExecClientMessage
	if err := wire.ConsumeExecClientMessage(message, &got); err != nil {
		t.Fatal(err)
	}
	if got.McpResult == nil || got.McpResult.ResultNumber != 7 || got.McpResult.ResultView == nil || len(got.McpResult.ResultView) != 0 {
		t.Fatalf("MCP dry-run result = %#v", got.McpResult)
	}
}

func TestHandAssembledRequestContextTools(t *testing.T) {
	t.Parallel()
	tool := appendField(1, []byte("read"))
	tool = append(tool, appendField(4, []byte("pi-agent"))...)
	requestContext := appendField(7, tool)
	success := appendField(1, requestContext)
	success = protowire.AppendTag(success, 2, protowire.VarintType)
	success = protowire.AppendVarint(success, 1)
	exec := protowire.AppendTag(nil, 1, protowire.VarintType)
	exec = protowire.AppendVarint(exec, 12)
	exec = append(exec, appendField(10, appendField(1, success))...)
	var got wire.ExecClientMessage
	if err := wire.ConsumeExecClientMessage(exec, &got); err != nil {
		t.Fatal(err)
	}
	result := got.RequestContextResult
	if got.ID != 12 || result == nil || result.Success == nil || !result.Success.ServedFromDiskCache || result.Success.RequestContext == nil {
		t.Fatalf("exec = %#v", got)
	}
	assertReadTool(t, result.Success.RequestContext.Tools)
}

func assertReadTool(t *testing.T, tools []wire.McpToolDefinition) {
	t.Helper()
	if len(tools) != 1 || string(tools[0].NameView) != "read" || string(tools[0].ProviderIdentifierView) != "pi-agent" {
		t.Fatalf("tools = %#v", tools)
	}
}

func TestOneofLastClientVariantWins(t *testing.T) {
	t.Parallel()
	exec := protowire.AppendTag(nil, 1, protowire.VarintType)
	exec = protowire.AppendVarint(exec, 4)
	frame := slices.Concat(appendField(1, appendField(5, []byte("conversation"))), appendField(2, exec))
	var client wire.AgentClientMessage
	if err := wire.ConsumeAgentClientMessage(frame, &client); err != nil {
		t.Fatal(err)
	}
	if client.Run != nil || client.Exec == nil || client.Exec.ID != 4 {
		t.Fatalf("client = %#v", client)
	}
}

func TestOneofLastInteractionVariantWins(t *testing.T) {
	t.Parallel()
	update := slices.Concat(appendField(1, appendField(1, []byte("hi"))), appendField(13, nil))
	var interaction wire.InteractionUpdate
	if err := wire.ConsumeInteractionUpdate(update, &interaction); err != nil {
		t.Fatal(err)
	}
	if interaction.Text != nil || interaction.VariantNumber != 13 {
		t.Fatalf("interaction = %#v", interaction)
	}
}

func TestOneofLastMcpResultWins(t *testing.T) {
	t.Parallel()
	var mcp wire.McpResult
	if err := wire.ConsumeMcpResult(slices.Concat(appendField(1, nil), appendField(7, nil)), &mcp); err != nil {
		t.Fatal(err)
	}
	if mcp.Success != nil || mcp.ResultNumber != 7 {
		t.Fatalf("MCP result = %#v", mcp)
	}
}

func TestHandAssembledModels(t *testing.T) {
	t.Parallel()
	model := appendField(1, []byte("model-b"))
	model = protowire.AppendTag(model, 7, protowire.VarintType)
	model = protowire.AppendVarint(model, 1)
	response := appendField(1, model)
	var models wire.GetUsableModelsResponse
	if err := wire.ConsumeGetUsableModelsResponse(response, &models); err != nil {
		t.Fatal(err)
	}
	if len(models.Models) != 1 || !models.Models[0].MaxMode || string(models.Models[0].ModelIDView) != "model-b" {
		t.Fatalf("models = %#v", models.Models)
	}
}

func TestHandAssembledActionVariant(t *testing.T) {
	t.Parallel()
	var action wire.ConversationAction
	if err := wire.ConsumeConversationAction(appendField(2, []byte{0x0a, 0}), &action); err != nil {
		t.Fatal(err)
	}
	if action.ActionNumber != 2 || !bytes.Equal(action.ActionView, []byte{0x0a, 0}) {
		t.Fatalf("action = %#v", action)
	}
}

func TestHandAssembledToolVariant(t *testing.T) {
	t.Parallel()
	var call wire.ToolCall
	if err := wire.ConsumeToolCall(appendField(1, []byte("shell")), &call); err != nil {
		t.Fatal(err)
	}
	if call.VariantNumber != 1 || string(call.VariantView) != "shell" {
		t.Fatalf("call = %#v", call)
	}
}

func TestEmptyMessageVariants(t *testing.T) {
	t.Parallel()
	tests := []struct {
		name        string
		number      protowire.Number
		appendValue func([]byte) []byte
	}{
		{"cancel action", 3, func(dst []byte) []byte {
			return wire.AppendConversationAction(dst, &wire.ConversationAction{ActionNumber: 3})
		}},
		{"client heartbeat", 7, func(dst []byte) []byte {
			return wire.AppendAgentClientMessage(dst, &wire.AgentClientMessage{MessageNumber: 7})
		}},
		{"server checkpoint", 3, func(dst []byte) []byte {
			return wire.AppendAgentServerMessage(dst, &wire.AgentServerMessage{MessageNumber: 3})
		}},
		{"interaction heartbeat", 13, func(dst []byte) []byte {
			return wire.AppendInteractionUpdate(dst, &wire.InteractionUpdate{VariantNumber: 13})
		}},
		{"tool call", 1, func(dst []byte) []byte {
			return wire.AppendToolCall(dst, &wire.ToolCall{VariantNumber: 1})
		}},
		{"MCP content", 1, func(dst []byte) []byte {
			return wire.AppendMcpContentItem(dst, &wire.McpContentItem{ContentNumber: 1})
		}},
		{"MCP approved", 7, func(dst []byte) []byte {
			return wire.AppendMcpResult(dst, &wire.McpResult{ResultNumber: 7})
		}},
		{"exec result", 2, func(dst []byte) []byte {
			return wire.AppendExecClientMessage(dst, &wire.ExecClientMessage{ResultNumber: 2})
		}},
		{"exec request", 3, func(dst []byte) []byte {
			return wire.AppendExecServerMessage(dst, &wire.ExecServerMessage{MessageNumber: 3})
		}},
		{"requested model credentials", 4, func(dst []byte) []byte {
			return wire.AppendRequestedModel(dst, &wire.RequestedModel{CredentialNumber: 4})
		}},
		{"model credentials", 8, func(dst []byte) []byte {
			return wire.AppendModelDetails(dst, &wire.ModelDetails{CredentialNumber: 8})
		}},
	}
	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			want := protowire.AppendTag(nil, tc.number, protowire.BytesType)
			want = protowire.AppendBytes(want, nil)
			if got := tc.appendValue(nil); !bytes.Equal(got, want) {
				t.Fatalf("frame = %x, want %x", got, want)
			}
		})
	}
}

func TestUnknownFieldsAndTruncation(t *testing.T) {
	t.Parallel()
	decoders := decoderTable()
	for _, tc := range decoders {
		t.Run(tc.name+" unknown fields", func(t *testing.T) {
			t.Parallel()
			if err := tc.decode(unknownFields()); err != nil {
				t.Fatalf("unknown fields: %v", err)
			}
		})
		t.Run(tc.name+" truncated", func(t *testing.T) {
			t.Parallel()
			if err := tc.decode([]byte{0x0a, 5, 'x'}); err == nil {
				t.Fatal("truncated bytes field was accepted")
			}
		})
	}
}

func TestUnknownFieldWireTypes(t *testing.T) {
	t.Parallel()
	var got wire.TextDelta
	if err := wire.ConsumeTextDelta(unknownFields(), &got); err != nil {
		t.Fatal(err)
	}
	truncated := [][]byte{
		protowire.AppendTag(nil, 1001, protowire.VarintType),
		protowire.AppendTag(nil, 1002, protowire.Fixed32Type),
		protowire.AppendTag(nil, 1003, protowire.Fixed64Type),
		protowire.AppendTag(nil, 1005, protowire.StartGroupType),
	}
	for _, frame := range truncated {
		if err := wire.ConsumeTextDelta(frame, &got); err == nil {
			t.Fatalf("truncated frame %x was accepted", frame)
		}
	}
}

func TestDecodedViewsAliasInput(t *testing.T) {
	t.Parallel()
	input := appendField(1, []byte("abc"))
	var got wire.TextDelta
	if err := wire.ConsumeTextDelta(input, &got); err != nil {
		t.Fatal(err)
	}
	input[len(input)-1] = 'z'
	if string(got.TextView) != "abz" {
		t.Fatalf("view copied input: %q", got.TextView)
	}
}

type decoderCase struct {
	name   string
	decode func([]byte) error
}

func decoderTable() []decoderCase {
	return []decoderCase{
		{"model parameter", func(b []byte) error { return wire.ConsumeModelParameter(b, new(wire.ModelParameter)) }},
		{"requested model", func(b []byte) error { return wire.ConsumeRequestedModel(b, new(wire.RequestedModel)) }},
		{"model details", func(b []byte) error { return wire.ConsumeModelDetails(b, new(wire.ModelDetails)) }},
		{"models request", func(b []byte) error { return wire.ConsumeGetUsableModelsRequest(b, new(wire.GetUsableModelsRequest)) }},
		{"models response", func(b []byte) error { return wire.ConsumeGetUsableModelsResponse(b, new(wire.GetUsableModelsResponse)) }},
		{"conversation", func(b []byte) error { return wire.ConsumeConversationState(b, new(wire.ConversationState)) }},
		{"user message", func(b []byte) error { return wire.ConsumeUserMessage(b, new(wire.UserMessage)) }},
		{"user action", func(b []byte) error { return wire.ConsumeUserMessageAction(b, new(wire.UserMessageAction)) }},
		{"action", func(b []byte) error { return wire.ConsumeConversationAction(b, new(wire.ConversationAction)) }},
		{"MCP definition", func(b []byte) error { return wire.ConsumeMcpToolDefinition(b, new(wire.McpToolDefinition)) }},
		{"run request", func(b []byte) error { return wire.ConsumeAgentRunRequest(b, new(wire.AgentRunRequest)) }},
		{"MCP argument", func(b []byte) error { return wire.ConsumeMcpArgument(b, new(wire.McpArgument)) }},
		{"MCP args", func(b []byte) error { return wire.ConsumeMcpArgs(b, new(wire.McpArgs)) }},
		{"MCP content item", func(b []byte) error { return wire.ConsumeMcpContentItem(b, new(wire.McpContentItem)) }},
		{"MCP success", func(b []byte) error { return wire.ConsumeMcpSuccess(b, new(wire.McpSuccess)) }},
		{"MCP result", func(b []byte) error { return wire.ConsumeMcpResult(b, new(wire.McpResult)) }},
		{"exec client", func(b []byte) error { return wire.ConsumeExecClientMessage(b, new(wire.ExecClientMessage)) }},
		{"shell args", func(b []byte) error { return wire.ConsumeShellArgs(b, new(wire.ShellArgs)) }},
		{"exec server", func(b []byte) error { return wire.ConsumeExecServerMessage(b, new(wire.ExecServerMessage)) }},
		{"request context args", func(b []byte) error { return wire.ConsumeRequestContextArgs(b, new(wire.RequestContextArgs)) }},
		{"request context", func(b []byte) error { return wire.ConsumeRequestContext(b, new(wire.RequestContext)) }},
		{"request context success", func(b []byte) error { return wire.ConsumeRequestContextSuccess(b, new(wire.RequestContextSuccess)) }},
		{"request context result", func(b []byte) error { return wire.ConsumeRequestContextResult(b, new(wire.RequestContextResult)) }},
		{"text", func(b []byte) error { return wire.ConsumeTextDelta(b, new(wire.TextDelta)) }},
		{"thinking", func(b []byte) error { return wire.ConsumeThinkingDelta(b, new(wire.ThinkingDelta)) }},
		{"token delta", func(b []byte) error { return wire.ConsumeTokenDelta(b, new(wire.TokenDelta)) }},
		{"MCP call", func(b []byte) error { return wire.ConsumeMcpToolCall(b, new(wire.McpToolCall)) }},
		{"tool call", func(b []byte) error { return wire.ConsumeToolCall(b, new(wire.ToolCall)) }},
		{"tool started", func(b []byte) error { return wire.ConsumeToolCallStartedUpdate(b, new(wire.ToolCallStartedUpdate)) }},
		{"tool completed", func(b []byte) error { return wire.ConsumeToolCallCompletedUpdate(b, new(wire.ToolCallCompletedUpdate)) }},
		{"partial tool", func(b []byte) error { return wire.ConsumePartialToolCallUpdate(b, new(wire.PartialToolCallUpdate)) }},
		{"tool delta", func(b []byte) error { return wire.ConsumeToolCallDeltaUpdate(b, new(wire.ToolCallDeltaUpdate)) }},
		{"turn end", func(b []byte) error { return wire.ConsumeTurnEndedUpdate(b, new(wire.TurnEndedUpdate)) }},
		{"interaction", func(b []byte) error { return wire.ConsumeInteractionUpdate(b, new(wire.InteractionUpdate)) }},
		{"server", func(b []byte) error { return wire.ConsumeAgentServerMessage(b, new(wire.AgentServerMessage)) }},
		{"client", func(b []byte) error { return wire.ConsumeAgentClientMessage(b, new(wire.AgentClientMessage)) }},
	}
}

func unknownFields() []byte {
	out := protowire.AppendTag(nil, 1001, protowire.VarintType)
	out = protowire.AppendVarint(out, 123456)
	out = protowire.AppendTag(out, 1002, protowire.Fixed32Type)
	out = protowire.AppendFixed32(out, 0xdeadbeef)
	out = protowire.AppendTag(out, 1003, protowire.Fixed64Type)
	out = protowire.AppendFixed64(out, 0x1234567890)
	out = append(out, appendField(1004, []byte("unknown"))...)
	out = protowire.AppendTag(out, 1005, protowire.StartGroupType)
	out = protowire.AppendTag(out, 1, protowire.VarintType)
	out = protowire.AppendVarint(out, 7)
	out = protowire.AppendTag(out, 1005, protowire.EndGroupType)
	return out
}

func appendField(number protowire.Number, value []byte) []byte {
	out := protowire.AppendTag(nil, number, protowire.BytesType)
	return protowire.AppendBytes(out, value)
}
