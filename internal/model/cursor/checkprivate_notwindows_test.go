//go:build !windows

package cursor_test

import (
	"crypto/sha256"
	"encoding/hex"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/gosuda/dalgona/internal/model/cursor"
	"github.com/gosuda/dalgona/internal/model/cursor/wire"
)

func TestStateDirFilesAreOwnerOnly(t *testing.T) {
	dir := filepath.Join(t.TempDir(), "thread-1")
	serverBlob := []byte("server-turn")
	blobID := sha256.Sum256(serverBlob)
	_, server := newServer(t, func(c *fakeConn) {
		checkpoint := wire.AppendConversationState(nil, &wire.ConversationState{RootPromptMessages: c.run.Conversation.RootPromptMessages[:1], SummaryView: []byte("summary")})
		c.send(&wire.AgentServerMessage{MessageNumber: wire.ServerCheckpointNumber, MessageView: checkpoint})
		c.send(&wire.AgentServerMessage{MessageNumber: wire.ServerKvNumber, MessageView: wire.AppendKvServerMessage(nil, &wire.KvServerMessage{ID: 1, Set: &wire.SetBlobArgs{BlobIDView: blobID[:], BlobDataView: serverBlob}})})
		c.next()
		c.endTurn()
	})
	provider := newProvider(server, time.Second)
	stream, err := provider.StreamWith(t.Context(), userRequest("one"), cursor.StreamOptions{StateDir: dir})
	if err != nil {
		t.Fatal(err)
	}
	if out := drain(t, stream); out.failure != nil {
		t.Fatalf("turn failed: %v", out.failure.Err)
	}
	for _, path := range []string{filepath.Join(dir, "checkpoint"), filepath.Join(dir, "blobs", hex.EncodeToString(blobID[:]))} {
		info, statErr := os.Stat(path)
		if statErr != nil || info.Mode().Perm() != 0o600 {
			t.Fatalf("%s: info = %v, err = %v", path, info, statErr)
		}
	}
}
