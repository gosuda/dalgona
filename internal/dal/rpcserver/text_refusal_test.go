package rpcserver

import (
	"context"
	"encoding/json/jsontext"
	"errors"
	"io"
	"io/fs"
	"maps"
	"os"
	"path/filepath"
	"sync/atomic"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/rpc"

	"github.com/gosuda/dalgona/internal/dal/config"
	"github.com/gosuda/dalgona/internal/dal/journal"
)

type refusal struct {
	method  string
	params  map[string]any
	message string
}

type textInputs []map[string]string

func treeState(t *testing.T, root string) map[string]string {
	t.Helper()
	state := map[string]string{}
	rot, err := os.OpenRoot(root)
	if err != nil {
		if errors.Is(err, fs.ErrNotExist) {
			return state
		}
		t.Fatal(err)
	}
	defer func() { _ = rot.Close() }()
	err = filepath.WalkDir(root, func(path string, entry fs.DirEntry, walkErr error) error {
		if errors.Is(walkErr, fs.ErrNotExist) {
			return nil
		}
		if walkErr != nil || entry.IsDir() {
			state[path] = "dir"
			return walkErr
		}
		rel, relErr := filepath.Rel(root, path)
		if relErr != nil {
			return relErr
		}
		data, readErr := readRootFile(rot, rel)
		state[path] = string(data)
		return readErr
	})
	if err != nil {
		t.Fatal(err)
	}
	return state
}

func readRootFile(rot *os.Root, name string) ([]byte, error) {
	file, err := rot.Open(name)
	if err != nil {
		return nil, err
	}
	defer func() { _ = file.Close() }()
	return io.ReadAll(file)
}

func fileState(t *testing.T, path string) string {
	t.Helper()
	data, err := os.ReadFile(path)
	if errors.Is(err, fs.ErrNotExist) {
		return "absent"
	}
	if err != nil {
		t.Fatal(err)
	}
	return string(data)
}

type journalState struct {
	heads map[string]journal.SessionHead
	blobs map[string]string
}

func readJournalState(t *testing.T, r *rig) journalState {
	t.Helper()
	sessions, err := r.cfg.Journal.Sessions()
	if err != nil {
		t.Fatal(err)
	}
	heads := make(map[string]journal.SessionHead, len(sessions))
	for _, head := range sessions {
		heads[head.ID] = head
	}
	return journalState{heads: heads, blobs: treeState(t, filepath.Join(r.home, "blobs"))}
}

func (s journalState) equal(other journalState) bool {
	return maps.Equal(s.heads, other.heads) && maps.Equal(s.blobs, other.blobs)
}

func refuseAll(t *testing.T, c *wireClient, cases []refusal, state func() any, same func(before, after any) bool) {
	t.Helper()
	for _, tc := range cases {
		before := state()
		c.fails(tc.method, tc.params, rpc.CodeInvalidParams, tc.message)
		if after := state(); !same(before, after) {
			t.Fatalf("%s %s changed state from %v to %v", tc.method, tc.message, before, after)
		}
	}
}

func TestTextRefusalPrecedesJournalAppends(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	c := r.connect()
	threadID := startThread(t, r, c, nil)
	image := writeImage(t, "shot.png", pngSample)
	cases := []refusal{
		{"turn/start", map[string]any{"threadId": threadID, "input": textInputs{{"type": "text", "text": "keep"}, {"type": "text", "text": "a\x00b"}}}, "invalid text: input[1].text: disallowed_control"},
		{"turn/start", map[string]any{"threadId": threadID, "input": textInputs{{"type": "localImage", "path": image}, {"type": "text", "text": "\x00"}}}, "invalid text: input[1].text: disallowed_control"},
		{"turn/start", map[string]any{"threadId": threadID, "input": textInputs{{"type": "localImage", "path": image + "\x00"}}}, "invalid text: input[0].path: disallowed_control"},
		{"turn/start", map[string]any{"threadId": threadID, "input": textInputs{{"type": "mention", "name": "m", "path": "notes\x00.md"}}}, "invalid text: input[0].path: disallowed_control"},
		{"turn/start", map[string]any{"threadId": threadID, "input": textInputs{{"type": "image", "url": "data:image/png;base64,iVBORw0KGgo=\x00"}}}, "invalid text: input[0].url: disallowed_control"},
		{"turn/start", map[string]any{"threadId": threadID, "input": textInputs{{"type": "text", "text": "keep"}}, "model": "gpt\x00test"}, "invalid text: model: disallowed_control"},
		{"turn/steer", map[string]any{"threadId": threadID, "input": textInputs{{"type": "text", "text": "steer\x00"}}}, "invalid text: input[0].text: disallowed_control"},
		{"thread/name/set", map[string]any{"threadId": threadID, "name": "title\x00"}, "invalid text: name: disallowed_control"},
		{"review/start", map[string]any{"threadId": threadID, "target": map[string]any{"type": "custom", "instructions": "look\x00"}}, "invalid text: target.instructions: disallowed_control"},
		{"review/start", map[string]any{"threadId": threadID, "target": map[string]any{"type": "commit", "sha": "abcd", "title": "fix\x00"}}, "invalid text: target.title: disallowed_control"},
		{"thread/start", map[string]any{"cwd": r.cwd + "\x00"}, "invalid text: cwd: disallowed_control"},
		{"thread/start", map[string]any{"cwd": r.cwd, "model": "gpt\x00test"}, "invalid text: model: disallowed_control"},
		{"thread/start", map[string]any{"cwd": r.cwd, "model": "gpt-test", "modelProvider": "open\x00ai"}, "invalid text: modelProvider: disallowed_control"},
	}
	state := func() any { return readJournalState(t, r) }
	refuseAll(t, c, cases, state, func(before, after any) bool { return before.(journalState).equal(after.(journalState)) })
	accepted := "keep \u2028\u0085\ufeff\u200d\U000F0000 x\r\n"
	var started rpc.TurnStartResponse
	c.ok("turn/start", map[string]any{"threadId": threadID, "input": textInputs{{"type": "text", "text": accepted, "path": "ignored\x00"}}}, &started)
	c.collect()
	messages := userMessages(t, r, threadID)
	if len(messages) != 1 || messages[0].Text != accepted {
		t.Fatalf("user messages %+v, want one message with text %q", messages, accepted)
	}
}

func TestTextRefusalPrecedesFileWrites(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	c := r.connect()
	dir := t.TempDir()
	kept := filepath.Join(dir, "kept.txt")
	if err := os.WriteFile(kept, []byte("kept"), 0o600); err != nil {
		t.Fatal(err)
	}
	cases := []refusal{
		{"fs/writeFile", map[string]any{"path": kept, "dataBase64": "bmV3\x00"}, "invalid text: dataBase64: disallowed_control"},
		{"fs/writeFile", map[string]any{"path": filepath.Join(dir, "new\x00.txt"), "dataBase64": "bmV3"}, "invalid text: path: disallowed_control"},
		{"fs/copy", map[string]any{"sourcePath": kept, "destinationPath": filepath.Join(dir, "copy\x00.txt")}, "invalid text: destinationPath: disallowed_control"},
		{"fs/copy", map[string]any{"sourcePath": kept + "\x00", "destinationPath": filepath.Join(dir, "copy.txt")}, "invalid text: sourcePath: disallowed_control"},
		{"fs/createDirectory", map[string]any{"path": filepath.Join(dir, "sub\x00")}, "invalid text: path: disallowed_control"},
		{"fs/remove", map[string]any{"path": kept + "\x00"}, "invalid text: path: disallowed_control"},
		{"fs/readFile", map[string]any{"path": kept + "\x00"}, "invalid text: path: disallowed_control"},
	}
	settings := r.cfg.Config.SettingsPath()
	settingCases := []refusal{
		{"config/value/write", map[string]any{"keyPath": "model", "value": "openai/gpt\x00test"}, "invalid text: value: disallowed_control"},
		{"config/batchWrite", map[string]any{"edits": []map[string]any{{"keyPath": "model", "value": "openai/gpt\x00test", "mergeStrategy": "replace"}}}, "invalid text: edits[0].value: disallowed_control"},
	}
	files := func() any { return treeState(t, dir) }
	same := func(before, after any) bool { return maps.Equal(before.(map[string]string), after.(map[string]string)) }
	refuseAll(t, c, cases, files, same)
	settingsFile := func() any { return fileState(t, settings) }
	refuseAll(t, c, settingCases, settingsFile, func(before, after any) bool { return before == after })
}

func TestTextRefusalPrecedesCredentialUpdates(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	c := r.connect()
	cases := []refusal{
		{"account/login/start", map[string]any{"type": "apiKey", "provider": "openai", "apiKey": loginKeyText + "\n"}, "invalid text: apiKey: line_break_not_allowed"},
		{"account/login/start", map[string]any{"type": "apiKey", "provider": "openai", "apiKey": "sk-\u2028" + loginKeyText}, "invalid text: apiKey: line_break_not_allowed"},
		{"account/login/start", map[string]any{"type": "apiKey", "provider": "openai", "apiKey": "sk-\x00" + loginKeyText}, "invalid text: apiKey: disallowed_control"},
	}
	credentials := filepath.Join(r.home, "auth.json")
	state := func() any { return fileState(t, credentials) }
	refuseAll(t, c, cases, state, func(before, after any) bool { return before == after })
}

func TestTextRefusalPrecedesReceiptReservation(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	c := r.connectNative(ServeOptions{})
	threadID := c.startThread(r)
	operationID := rpc.NewOperationID()
	params := map[string]any{"threadId": threadID, "name": "draft\x00", rpc.OperationMember: operationID}
	receipts := filepath.Join(r.home, receiptDirName)
	before := readJournalState(t, r)
	beforeReceipts := treeState(t, receipts)
	refused := c.call("thread/name/set", params)
	if refused.Error == nil || refused.Error.Code != rpc.CodeInvalidParams || refused.Error.Message != "invalid text: name: disallowed_control" {
		t.Fatalf("thread/name/set error %+v, want -32602 invalid text: name: disallowed_control", refused.Error)
	}
	if !before.equal(readJournalState(t, r)) || !maps.Equal(beforeReceipts, treeState(t, receipts)) {
		t.Fatal("refused text changed the journal or reserved a receipt")
	}
	params["name"] = "draft"
	c.ok("thread/name/set", params, nil)
}

func TestTextRefusalPrecedesDraftEdit(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	var edits atomic.Int32
	edit := func(context.Context, jsontext.Value) (jsontext.Value, error) {
		edits.Add(1)
		return nil, &rpc.RPCError{Code: rpc.CodeInternal, Message: "draft edited"}
	}
	c := r.connectNative(ServeOptions{OperationExtensions: map[string]Extension{"dalgona/daltoki/draft/edit": edit}})
	receipts := filepath.Join(r.home, receiptDirName)
	draft := func(title, body string) map[string]any {
		return map[string]any{"draftId": "d-1", "expectedRevision": "r-1", "title": title, "body": body, operationIDMember: rpc.NewOperationID()}
	}
	checkRefusedDrafts(t, c, receipts, draft, &edits)
	checkAcceptedDraft(t, c, draft, &edits)
}

func checkRefusedDrafts(t *testing.T, c *nativeClient, receipts string, draft func(string, string) map[string]any, edits *atomic.Int32) {
	t.Helper()
	cases := []struct {
		params  map[string]any
		message string
	}{
		{draft("fix\x00", "body"), "invalid text: title: disallowed_control"},
		{draft("fix", "line one\n\x00"), "invalid text: body: disallowed_control"},
	}
	for _, tc := range cases {
		before := treeState(t, receipts)
		refused := c.call("dalgona/daltoki/draft/edit", tc.params)
		if refused.Error == nil || refused.Error.Code != rpc.CodeInvalidParams || refused.Error.Message != tc.message {
			t.Fatalf("draft edit error %+v, want -32602 %s", refused.Error, tc.message)
		}
		if edits.Load() != 0 || !maps.Equal(before, treeState(t, receipts)) {
			t.Fatalf("refused draft edit ran %d times or reserved a receipt", edits.Load())
		}
	}
}

func checkAcceptedDraft(t *testing.T, c *nativeClient, draft func(string, string) map[string]any, edits *atomic.Int32) {
	t.Helper()
	if accepted := c.call("dalgona/daltoki/draft/edit", draft("fix", "line one\nline two")); accepted.Error == nil || accepted.Error.Message != "draft edited" || edits.Load() != 1 {
		t.Fatalf("valid draft edit: error %+v, edits %d, want the extension's own reply and one edit", accepted.Error, edits.Load())
	}
}

func TestTextRefusalPrecedesScriptRun(t *testing.T) {
	r := newRig(t, config.ApprovalYolo)
	var runs atomic.Int32
	run := func(context.Context, jsontext.Value) (jsontext.Value, error) {
		runs.Add(1)
		return nil, &rpc.RPCError{Code: rpc.CodeInternal, Message: "script ran"}
	}
	c := r.connectWith(ServeOptions{Extensions: map[string]Extension{"dalgona/script/run": run}})
	valid := map[string]any{"entry": "flow.star", "source": "print(1)\n", "cwd": r.cwd, "args": map[string]any{"k": []any{"ok", 1}}}
	with := func(name string, value any) map[string]any {
		params := maps.Clone(valid)
		params[name] = value
		return params
	}
	cases := []refusal{
		{"dalgona/script/run", with("entry", "flow\x00.star"), "invalid text: entry: disallowed_control"},
		{"dalgona/script/run", with("source", "print(1)\x00"), "invalid text: source: disallowed_control"},
		{"dalgona/script/run", with("cwd", r.cwd+"\x00"), "invalid text: cwd: disallowed_control"},
		{"dalgona/script/run", with("args", map[string]any{"k": []any{"ok", "v\x00"}}), "invalid text: args: disallowed_control"},
	}
	state := func() any { return runs.Load() }
	refuseAll(t, c, cases, state, func(before, after any) bool { return before == after })
	if m := c.call("dalgona/script/run", valid); m.Error == nil || m.Error.Message != "script ran" || runs.Load() != 1 {
		t.Fatalf("valid script run: error %+v, runs %d, want the extension's own reply and one run", m.Error, runs.Load())
	}
}
