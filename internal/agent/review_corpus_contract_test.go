package agent

import (
	"bytes"
	"crypto/sha256"
	"encoding/hex"
	json "encoding/json/v2"
	"os"
	"path/filepath"
	"reflect"
	"testing"
)

func TestIncompleteFilesRetainsEvidence(t *testing.T) {
	for _, tc := range []struct {
		name, change, pre, post, wantChange string
	}{
		{"modified", "update", "old\n", "new\n", "update"},
		{"added", "add", "", "new\n", "add"},
		{"deleted", "delete", "old\n", "", "update"},
	} {
		t.Run(tc.name, func(t *testing.T) {
			files := []repoFile{
				{path: "before.txt", change: "update", pre: []byte("a\n"), post: []byte("b\n")},
				{path: "target.txt", change: tc.change, pre: []byte(tc.pre), post: []byte(tc.post)},
				{path: "after.txt", change: "add", post: []byte("work\n")},
			}
			original := make([]repoFile, len(files))
			for i, f := range files {
				original[i] = repoFile{path: f.path, change: f.change, pre: bytes.Clone(f.pre), post: bytes.Clone(f.post)}
			}
			got, ok := incompleteFiles(t, files, "update target")
			if !ok || len(got) != len(files) {
				t.Fatalf("incomplete evidence = %+v, %v", got, ok)
			}
			assertIncompletePreimages(t, got, files)
			if string(got[1].post) != tc.pre || got[1].change != tc.wantChange {
				t.Fatalf("restored target = %+v", got[1])
			}
			assertIncompleteUnrelatedWork(t, got, files)
			assertIncompleteIndependentInputs(t, got, files, original)
		})
	}
}

func assertIncompleteIndependentInputs(t *testing.T, got, files, original []repoFile) {
	t.Helper()
	if len(got[1].post) > 0 {
		got[1].post[0] = 'X'
	}
	got[0].change = "delete"
	if !reflect.DeepEqual(files, original) {
		t.Fatal("original inputs changed")
	}
}

func assertIncompletePreimages(t *testing.T, got, files []repoFile) {
	t.Helper()
	for i := range files {
		if got[i].path != files[i].path || !bytes.Equal(got[i].pre, files[i].pre) {
			t.Fatalf("file %d lost its path, order, or pre-image", i)
		}
	}
}

func assertIncompleteUnrelatedWork(t *testing.T, got, files []repoFile) {
	t.Helper()
	if !reflect.DeepEqual(got[0], files[0]) || !reflect.DeepEqual(got[2], files[2]) {
		t.Fatal("unrelated work changed")
	}
}

func TestIncompleteFilesNeedsMatchAndRemainingAddedWork(t *testing.T) {
	for _, tc := range []struct {
		name, subject string
		files         []repoFile
	}{
		{"no match", "update missing", []repoFile{{path: "target.txt", change: "add", post: []byte("work\n")}}},
		{"only named work", "update target", []repoFile{{path: "target.txt", change: "update", pre: []byte("old\n"), post: []byte("new\n")}}},
		{"remaining deletion", "update target", []repoFile{{path: "target.txt", change: "add", post: []byte("work\n")}, {path: "other.txt", change: "delete", pre: []byte("gone\n")}}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			if _, ok := incompleteFiles(t, tc.files, tc.subject); ok {
				t.Fatal("ineligible incomplete case accepted")
			}
		})
	}
}

func TestWriteRatchetPreservesOnlyIdenticalMeasurementIdentity(t *testing.T) {
	t.Chdir(t.TempDir())
	if err := os.MkdirAll(filepath.Dir(corpusRatchetPath), 0o755); err != nil {
		t.Fatal(err)
	}
	corpus := []byte("synthetic corpus\n")
	sum := sha256.Sum256(corpus)
	sha := hex.EncodeToString(sum[:])
	precision, recall := 0.95, 0.6
	measurement := &ratchetMeasured{
		SoundFlagRate: 0.02,
		Errors:        7,
		Questions: map[string]ratchetQuestion{
			"lost_code":   {Precision: &precision, Recall: &recall, TrueFlags: 12},
			"placeholder": {Precision: nil, Recall: nil, TrueFlags: 0},
		},
	}
	for _, tc := range []struct {
		name, corpusSHA, judge string
		threshold              float64
		want                   *ratchetMeasured
	}{
		{"same", sha, corpusJudge, corpusThreshold, measurement},
		{"corpus", "different", corpusJudge, corpusThreshold, nil},
		{"judge", sha, "different", corpusThreshold, nil},
		{"threshold", sha, corpusJudge, 0.8, nil},
	} {
		t.Run(tc.name, func(t *testing.T) {
			prev := ratchetFile{CorpusSHA256: tc.corpusSHA, Judge: tc.judge, Threshold: tc.threshold, Measured: measurement}
			writeCorpusContractRatchet(t, prev)
			writeRatchet(t, corpus)
			got := readCorpusContractRatchet(t)
			if got.CorpusSHA256 != sha || got.Judge != corpusJudge || got.Threshold != corpusThreshold {
				t.Fatalf("wrong measurement identity: %+v", got)
			}
			if !reflect.DeepEqual(got.Measured, tc.want) {
				t.Fatalf("measurement for %s identity: got %+v, want %+v", tc.name, got.Measured, tc.want)
			}
		})
	}
}

func writeCorpusContractRatchet(t *testing.T, value ratchetFile) {
	t.Helper()
	raw, err := marshalRatchet(value)
	if err != nil {
		t.Fatal(err)
	}
	writeErr := os.WriteFile(corpusRatchetPath, raw, 0o600)
	if writeErr != nil {
		t.Fatal(writeErr)
	}
}

func readCorpusContractRatchet(t *testing.T) ratchetFile {
	t.Helper()
	raw, err := os.ReadFile(corpusRatchetPath)
	if err != nil {
		t.Fatal(err)
	}
	var got ratchetFile
	decodeErr := json.Unmarshal(raw, &got)
	if decodeErr != nil {
		t.Fatal(decodeErr)
	}
	return got
}
