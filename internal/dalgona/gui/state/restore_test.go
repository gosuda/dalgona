package state

import (
	"maps"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"testing"
	"testing/synctest"
)

var (
	boxThread   = ThreadKey{Endpoint: "box", Thread: "t9"}
	boxDraft    = DraftKey{Endpoint: "box", Window: "w2", Thread: "t9"}
	otherWindow = DraftKey{Endpoint: "local", Window: "w2", Thread: "t1"}
	savedDrafts = map[DraftKey]Draft{
		localDraft:  {Text: "unsent plan", Attachments: []Attachment{{ID: "a1", Kind: AttachmentText, Name: "notes.txt", Text: "context"}}},
		boxDraft:    {Text: "box draft"},
		otherWindow: {Text: "same thread in another window"},
	}
)

func saveSession(t *testing.T, dir string) {
	synctest.Test(t, func(t *testing.T) {
		runOwner(t, dir, func(t *testing.T, p *fakePort) {
			p.owner.Submit(OpenView{Window: "w1", Thread: localThread})
			p.owner.Submit(OpenView{Window: "w2", Thread: boxThread})
			p.owner.Submit(OpenView{Window: "w2", Thread: localThread})
			for key, d := range savedDrafts {
				d.Revision = 4
				p.owner.Submit(PutDraft{Key: key, Draft: d})
			}
			synctest.Wait()
		})
	})
}

func restoredView(t *testing.T, dir string) (*View, int) {
	var v *View
	sent := 0
	synctest.Test(t, func(t *testing.T) {
		runOwner(t, dir, func(t *testing.T, p *fakePort) {
			v = settled(p)
			p.mu.Lock()
			sent = len(p.log) + p.connects["local"] + p.connects["box"]
			p.mu.Unlock()
		})
	})
	return v, sent
}

func draftsOf(v *View, keys ...DraftKey) map[DraftKey]Draft {
	out := map[DraftKey]Draft{}
	for _, key := range keys {
		if d := v.Draft(key); !d.empty() {
			out[key] = Draft{Text: d.Text, Attachments: d.Attachments}
		}
	}
	return out
}

func sameDrafts(a, b map[DraftKey]Draft) bool {
	return maps.EqualFunc(a, b, func(x, y Draft) bool {
		return x.Text == y.Text && slices.Equal(x.Attachments, y.Attachments)
	})
}

func sameWindows(a, b []WindowView) bool {
	return slices.EqualFunc(a, b, func(x, y WindowView) bool { return x.ID == y.ID && slices.Equal(x.Threads, y.Threads) })
}

func TestRestoreRecreatesWindowsAndDraftsWithoutSendingAnything(t *testing.T) {
	dir := t.TempDir()
	saveSession(t, dir)
	v, sent := restoredView(t, dir)
	want := []WindowView{{ID: "w1", Threads: []ThreadKey{localThread}}, {ID: "w2", Threads: []ThreadKey{boxThread, localThread}}}
	if !sameWindows(v.Windows(), want) || sent != 0 {
		t.Fatalf("restored windows %+v after %d sends, want %+v and none", v.Windows(), sent, want)
	}
	if got := draftsOf(v, localDraft, boxDraft, otherWindow); !sameDrafts(got, savedDrafts) {
		t.Fatalf("restored drafts %+v, want %+v", got, savedDrafts)
	}
	tv, ok := v.Thread(boxThread)
	if !ok || tv.Live || tv.Offline {
		t.Fatalf("a restored remote thread reads %+v, want an idle reference", tv)
	}
}

func TestRestoreSkipsADamagedDraftAndIgnoresAnInterruptedWrite(t *testing.T) {
	dir := t.TempDir()
	saveSession(t, dir)
	if err := os.WriteFile(filepath.Join(dir, draftFile(boxDraft)), []byte(`{"endpoint":"box","win`), 0o600); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(dir, "."+manifestName+"-interrupted"), []byte(`{"version":1,"windows":[`), 0o600); err != nil {
		t.Fatal(err)
	}
	v, sent := restoredView(t, dir)
	wantDrafts := maps.Clone(savedDrafts)
	delete(wantDrafts, boxDraft)
	got := draftsOf(v, localDraft, boxDraft, otherWindow)
	if !sameDrafts(got, wantDrafts) || len(v.Windows()) != 2 || sent != 0 {
		t.Fatalf("after damage: drafts %+v windows %+v sends %d", got, v.Windows(), sent)
	}
	if notices := noticeTexts(v); len(notices) != 1 || !strings.Contains(notices[0], "Could not restore a saved draft") {
		t.Fatalf("notices %q", notices)
	}
}

func TestRestoreOfAnUnreadableManifestStartsEmptyAndSaysSo(t *testing.T) {
	dir := t.TempDir()
	if err := os.WriteFile(filepath.Join(dir, manifestName), []byte(`{"version":9,"windows":[]}`), 0o600); err != nil {
		t.Fatal(err)
	}
	v, sent := restoredView(t, dir)
	if notices := noticeTexts(v); len(v.Windows()) != 0 || sent != 0 || len(notices) != 1 || !strings.Contains(notices[0], "unknown format") {
		t.Fatalf("windows %+v sends %d notices %q", v.Windows(), sent, notices)
	}
}

func TestDraftsBeyondTheSizeLimitStayInMemoryMarkedUnsaved(t *testing.T) {
	dir := t.TempDir()
	huge := Draft{Text: strings.Repeat("x", draftFileLimit), Revision: 1}
	synctest.Test(t, func(t *testing.T) {
		runOwner(t, dir, func(t *testing.T, p *fakePort) {
			p.owner.Submit(PutDraft{Key: localDraft, Draft: huge})
			p.owner.Submit(PutDraft{Key: boxDraft, Draft: Draft{Text: "small", Revision: 1}})
			v := settled(p)
			if !v.Unsaved(localDraft) || v.Unsaved(boxDraft) || v.Draft(localDraft).Text != huge.Text {
				t.Fatalf("unsaved local %v box %v, draft kept %v", v.Unsaved(localDraft), v.Unsaved(boxDraft), v.Draft(localDraft).Text == huge.Text)
			}
		})
	})
	v, _ := restoredView(t, dir)
	if v.Draft(localDraft).Text != "" || v.Draft(boxDraft).Text != "small" {
		t.Fatalf("restored oversized %d bytes and small %q", len(v.Draft(localDraft).Text), v.Draft(boxDraft).Text)
	}
}

func TestWindowDraftsListsOnlyThatWindowsUnsentContent(t *testing.T) {
	typed := DraftKey{Endpoint: "local", Window: "w1", Thread: "a"}
	attached := DraftKey{Endpoint: "box", Window: "w1", Thread: "a"}
	cleared := DraftKey{Endpoint: "local", Window: "w1", Thread: "e"}
	queued := DraftKey{Endpoint: "local", Window: "w1", Thread: "q"}
	elsewhere := DraftKey{Endpoint: "local", Window: "w2", Thread: "a"}
	v := Snapshot{
		Drafts: map[DraftKey]Draft{
			typed: {Text: "x"}, attached: {Attachments: []Attachment{{ID: "i", Kind: AttachmentImage}}},
			cleared: {Revision: 3}, elsewhere: {Text: "y"},
		},
		FollowUps: map[DraftKey][]FollowUp{queued: {{ID: 1, Text: "later"}}, cleared: {}, elsewhere: {{ID: 2}}},
	}.View()
	want := []DraftKey{attached, typed, queued}
	if got := v.WindowDrafts("w1"); !slices.Equal(got, want) {
		t.Fatalf("window drafts %+v, want %+v", got, want)
	}
	if got := v.WindowDrafts("w3"); len(got) != 0 {
		t.Fatalf("a window without drafts lists %+v", got)
	}
}
