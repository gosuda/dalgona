package agent

import (
	"bytes"
	"testing"

	"github.com/gosuda/dalgona/internal/auth"
	"github.com/gosuda/dalgona/internal/textdiff"
	"github.com/gosuda/dalgona/internal/tool"
)

func approvalFilesWorker(t *testing.T) *worker {
	t.Helper()
	store, err := auth.NewWithPatterns(t.TempDir(), []string{`token-[a-z0-9]{8,}`})
	if err != nil {
		t.Fatal(err)
	}
	return &worker{t: &Thread{host: &Host{cfg: HostConfig{Auth: store}}}}
}

func TestApprovalFilesMarkAdded(t *testing.T) {
	w := approvalFilesWorker(t)
	files, err := w.approvalFiles([]tool.FileChange{
		{Path: "new.txt", PostSHA: "b", Post: []byte("hi")},
	})
	if err != nil {
		t.Fatal(err)
	}
	added := files[0]
	if !added.Added || added.Deleted || added.Pre != nil || string(added.Post) != "hi" {
		t.Fatalf("added file = %+v, want Added with the post-image only", added)
	}
}

func TestApprovalFilesMarkDeleted(t *testing.T) {
	w := approvalFilesWorker(t)
	files, err := w.approvalFiles([]tool.FileChange{
		{Path: "gone.txt", PreSHA: "a", Pre: []byte("bye")},
	})
	if err != nil {
		t.Fatal(err)
	}
	deleted := files[0]
	if deleted.Added || !deleted.Deleted || string(deleted.Pre) != "bye" || deleted.Post != nil {
		t.Fatalf("deleted file = %+v, want Deleted with the pre-image only", deleted)
	}
}

func TestApprovalFilesMaskSecrets(t *testing.T) {
	w := approvalFilesWorker(t)
	files, err := w.approvalFiles([]tool.FileChange{
		{Path: "new.txt", PostSHA: "b", Post: []byte("key token-abcdefgh1234\n")},
	})
	if err != nil {
		t.Fatal(err)
	}
	post := files[0].Post
	if bytes.Contains(post, []byte("token-abcdefgh1234")) || !bytes.Contains(post, []byte("{{secret:")) {
		t.Fatalf("post = %q, want the secret masked", post)
	}
}

func TestApprovalFilesOmitLargeSide(t *testing.T) {
	w := approvalFilesWorker(t)
	files, err := w.approvalFiles([]tool.FileChange{
		{Path: "big.txt", PreSHA: "a", PostSHA: "b", Pre: bytes.Repeat([]byte("a"), textdiff.Limit+1), Post: []byte("x")},
	})
	if err != nil {
		t.Fatal(err)
	}
	big := files[0]
	if !big.TooLarge || big.Pre != nil || big.Post != nil {
		t.Fatalf("large file = %+v, want TooLarge with no bytes", big)
	}
}
