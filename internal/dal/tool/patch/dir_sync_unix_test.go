//go:build unix

package patch

import (
	"testing"

	"github.com/gosuda/dalgona/internal/dal/tool"
)

func pinnedDirName(p *dirPin) string { return p.file.Name() }

func TestCreatedDirectoryParentsSyncAfterTheLastOperation(t *testing.T) {
	tests := []syncOrderCase{
		{"IO-05/rename into a new directory", "[a.txt@{refA}]\nMV sub/z.txt\n", map[string]string{"a.txt": absent, "sub/z.txt": "a-before\n"}, 6, []string{"", "sub", "", "", "sub", ""}},
		{"IO-05/move into nested missing directories", "[a.txt@{refA}]\nMV out/generated/a.txt\n", map[string]string{"a.txt": absent, "out/generated/a.txt": "a-before\n"}, 8, []string{"out/generated", "", "out", "", "out/generated", "", "out", ""}},
	}
	for _, test := range tests {
		for _, style := range profileStyles {
			t.Run(test.name+"/"+string(style), func(t *testing.T) { requireSyncAfterLastOperation(t, style, test) })
		}
	}
}

func TestDirectorySyncAttemptsEveryPinAfterAFailure(t *testing.T) {
	t.Run("IO-05/rename source and destination", func(t *testing.T) {
		f := newOutcomeFixture(t)
		twoFiles(t, f)
		calls := 0
		failDirectorySync(t, &calls)
		err := f.run(t, HashlineLight, "[a.txt@{refA}]\nMV sub/z.txt\n")
		if err == nil {
			t.Fatal("the call succeeded although its directory sync failed")
		}
		want := withPaths(writeFailure(tool.CommitComplete, tool.ReasonSyncFailed), "a.txt", "after", "sub/z.txt", "after")
		requireWrappedOutcome(t, err, HashlineLight, want)
		if calls != 3 {
			t.Fatalf("directory syncs attempted = %d, want the source, the destination, and the parent of the created directory", calls)
		}
	})
}
