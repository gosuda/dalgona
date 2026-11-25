package agent

import (
	"bytes"
	"errors"
	"io"
	"testing"
)

func TestExecSpillRejectsReorderedEncryptedRecords(t *testing.T) {
	snapshot, err := newSpillSnapshot(t.Context(), 2*spillFixturePage)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		if err := snapshot.close(); err != nil {
			t.Error(err)
		}
	})
	page := bytes.Repeat([]byte{'A'}, spillFixturePage)
	if err := snapshot.append(0, page); err != nil {
		t.Fatal(err)
	}
	page[0] = 'B'
	if err := snapshot.append(spillFixturePage, page); err != nil {
		t.Fatal(err)
	}
	swapScratchRecord(t, snapshot)
	var one [1]byte
	if n, err := snapshot.ReadAt(one[:], spillFixturePage); err == nil || errors.Is(err, io.EOF) || n != 0 {
		t.Fatalf("reading record from the wrong position = %d bytes, %v; want authentication failure", n, err)
	}
}

func swapScratchRecord(t *testing.T, snapshot *spillSnapshot) {
	t.Helper()
	stat, err := snapshot.file.Stat()
	if err != nil {
		t.Fatal(err)
	}
	record := make([]byte, stat.Size()/2)
	if _, err := snapshot.file.ReadAt(record, 0); err != nil {
		t.Fatal(err)
	}
	if _, err := snapshot.file.WriteAt(record, int64(len(record))); err != nil {
		t.Fatal(err)
	}
}
