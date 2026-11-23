//go:build !unix

package patch

import "testing"

func pinnedDirName(p *dirPin) string { return p.root.Name() }

func TestCreatedDirectorySyncAfterTheLastOperation(t *testing.T) {
	tests := []syncOrderCase{
		{"IO-05/rename into a new directory", "[a.txt@{refA}]\nMV sub/z.txt\n", map[string]string{"a.txt": absent, "sub/z.txt": "a-before\n"}, 4, []string{"", "sub", "", "sub"}},
		{"IO-05/move into nested missing directories", "[a.txt@{refA}]\nMV out/generated/a.txt\n", map[string]string{"a.txt": absent, "out/generated/a.txt": "a-before\n"}, 4, []string{"", "out/generated", "", "out/generated"}},
	}
	for _, test := range tests {
		for _, style := range profileStyles {
			t.Run(test.name+"/"+string(style), func(t *testing.T) { requireSyncAfterLastOperation(t, style, test) })
		}
	}
}
