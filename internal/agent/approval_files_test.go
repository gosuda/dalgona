package agent

import (
	"bytes"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/auth"
	"github.com/gosuda/dalgona/internal/dal/textdiff"
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
	for _, tc := range []struct {
		name     string
		preSize  int
		postSize int
		tooLarge bool
	}{
		{"pre at limit", textdiff.Limit, 1, false},
		{"pre above limit", textdiff.Limit + 1, 1, true},
		{"post at limit", 1, textdiff.Limit, false},
		{"post above limit", 1, textdiff.Limit + 1, true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			w := approvalFilesWorker(t)
			pre := bytes.Repeat([]byte("a"), tc.preSize)
			post := bytes.Repeat([]byte("x"), tc.postSize)
			files, err := w.approvalFiles([]tool.FileChange{
				{Path: "big.txt", PreSHA: "a", PostSHA: "b", Pre: pre, Post: post},
			})
			if err != nil {
				t.Fatal(err)
			}
			got := files[0]
			if got.TooLarge != tc.tooLarge {
				t.Fatalf("TooLarge = %v, want %v", got.TooLarge, tc.tooLarge)
			}
			if tc.tooLarge {
				assertApprovalBytesOmitted(t, got.Pre, got.Post)
			} else if !bytes.Equal(got.Pre, pre) || !bytes.Equal(got.Post, post) {
				t.Fatal("file at limit did not retain both approval sides")
			}
		})
	}
}

func assertApprovalBytesOmitted(t *testing.T, pre, post []byte) {
	t.Helper()
	if pre != nil || post != nil {
		t.Fatal("oversized file retained approval bytes")
	}
}
