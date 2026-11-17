package journal

import (
	"testing"
)

func TestStatusFindsForeignSharedSession(t *testing.T) {
	dir := t.TempDir()
	first := mustOpenShared(t, dir)
	second := mustOpenShared(t, dir)
	defer closeStore(t, first)
	defer closeStore(t, second)

	release := mustLockSession(t, dir, "s")
	if err := appendOneByOne(first, "s", 0, 3); err != nil {
		mustRelease(t, release)
		t.Fatalf("append: %v", err)
	}
	mustRelease(t, release)

	listed, err := second.Sessions()
	if err != nil {
		t.Fatalf("Sessions: %v", err)
	}
	found := false
	for _, head := range listed {
		if head.ID == "s" {
			found = true
		}
	}
	if !found {
		t.Fatal("Sessions does not list the foreign session")
	}
	head, err := second.Status("s")
	if err != nil {
		t.Fatalf("Status hides the foreign session: %v", err)
	}
	if head.HeadSeq != 3 || head.Status != SessionOK {
		t.Fatalf("Status = %+v, want head 3 ok", head)
	}
}
