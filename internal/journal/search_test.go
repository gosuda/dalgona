package journal

import (
	"context"
	"encoding/json/jsontext"
	"errors"
	"os"
	"path/filepath"
	"slices"
	"strconv"
	"strings"
	"sync"
	"testing"

	json "encoding/json/v2"
)

func searchUser(text string) []byte {
	return []byte(`{"type":"user.message","item_id":"u","turn_id":"t","text":` + strconv.Quote(text) + `,"kind":"prompt"}`)
}

func searchAssistant(text string) []byte {
	return []byte(`{"type":"assistant.message","item_id":"a","turn_id":"t","text":` + strconv.Quote(text) + `,"reasoning":"","stop_reason":"end_turn"}`)
}

func searchNamed(name string) []byte {
	return []byte(`{"type":"thread.named","name":` + strconv.Quote(name) + `}`)
}

func searchToolOutput(text string) []byte {
	return []byte(`{"type":"tool.completed","call_id":"c","turn_id":"t","output":` + strconv.Quote(text) + `}`)
}

func searchBlobUser() []byte {
	return []byte(`{"type":"user.message","item_id":"u","turn_id":"t","text":"","text_blob":{"blob":"aa","bytes":10},"kind":"prompt"}`)
}

func appendRaw(t *testing.T, store *Store, id string, events ...[]byte) {
	t.Helper()
	if _, err := store.Append(context.Background(), id, events...); err != nil {
		t.Fatalf("Append %s: %v", id, err)
	}
}

func rawString(t *testing.T, value jsontext.Value) string {
	t.Helper()
	if len(value) == 0 {
		return ""
	}
	var text string
	if err := json.Unmarshal([]byte(value), &text); err != nil {
		t.Fatalf("rawString decode %s: %v", value, err)
	}
	return text
}

func oracleIndexedBody(t *testing.T, raw []byte) string {
	t.Helper()
	var fields map[string]jsontext.Value
	if err := json.Unmarshal(raw, &fields); err != nil {
		t.Fatalf("oracleIndexedBody decode %s: %v", raw, err)
	}
	switch rawString(t, fields["type"]) {
	case "user.message", "assistant.message":
		return rawString(t, fields["text"])
	case "thread.named":
		return rawString(t, fields["name"])
	}
	return ""
}

func bruteForceBestRow(t *testing.T, store *Store, term string) map[string]int {
	t.Helper()
	heads, err := store.Sessions()
	if err != nil {
		t.Fatalf("Sessions: %v", err)
	}
	expected := map[string]int{}
	for _, head := range heads {
		best := 0
		err := store.Replay(head.ID, func(_ uint64, ev []byte) error {
			body := oracleIndexedBody(t, ev)
			if count := strings.Count(strings.ToLower(body), strings.ToLower(term)); count > best {
				best = count
			}
			return nil
		})
		if err != nil {
			t.Fatalf("Replay %s: %v", head.ID, err)
		}
		if best > 0 {
			expected[head.ID] = best
		}
	}
	return expected
}

func requireSearchEqualsOracle(t *testing.T, store *Store, term string) []SearchHit {
	t.Helper()
	hits, err := store.Search(context.Background(), SearchQuery{Text: term})
	if err != nil {
		t.Fatalf("Search %q: %v", term, err)
	}
	expected := bruteForceBestRow(t, store, term)
	if len(hits) != len(expected) {
		t.Fatalf("hits = %d (%v), want %d (%v)", len(hits), hits, len(expected), expected)
	}
	seen := map[string]int{}
	for i := range hits {
		requireHitMatchesOracle(t, hits, i, expected, seen)
	}
	for session, count := range seen {
		if count != 1 {
			t.Fatalf("session %s appears %d times", session, count)
		}
	}
	return hits
}

func requireHitMatchesOracle(t *testing.T, hits []SearchHit, i int, expected map[string]int, seen map[string]int) {
	t.Helper()
	hit := hits[i]
	want, ok := expected[hit.Session]
	if !ok {
		t.Fatalf("hit %d session %s is not in the oracle", i, hit.Session)
	}
	if hit.Matches != want {
		t.Fatalf("hit %s matches = %d, want %d", hit.Session, hit.Matches, want)
	}
	if i > 0 && hits[i-1].Matches < hit.Matches {
		t.Fatalf("hits are not ranked by matches: %d then %d", hits[i-1].Matches, hit.Matches)
	}
	seen[hit.Session]++
}

func TestMirrorHasFTS5(t *testing.T) {
	dir := t.TempDir()
	store := mustOpen(t, dir)
	defer closeStore(t, store)
	appendRaw(t, store, "s1", searchUser("needle in the haystack"))
	var ddl string
	if err := store.db.QueryRow("SELECT sql FROM sqlite_master WHERE name = 'thread_fts'").Scan(&ddl); err != nil {
		t.Fatalf("thread_fts missing: %v", err)
	}
	if !strings.Contains(ddl, "trigram") {
		t.Fatalf("thread_fts DDL lacks the trigram tokenizer: %s", ddl)
	}
	var hits int
	if err := store.db.QueryRow(`SELECT COUNT(*) FROM thread_fts WHERE thread_fts MATCH '"eed"'`).Scan(&hits); err != nil {
		t.Fatalf("substring match: %v", err)
	}
	if hits != 1 {
		t.Fatalf("substring matches = %d, want 1", hits)
	}
	if err := store.db.QueryRow(`SELECT COUNT(*) FROM thread_fts WHERE thread_fts MATCH '"xyz"'`).Scan(&hits); err != nil {
		t.Fatalf("absent trigram match: %v", err)
	}
	if hits != 0 {
		t.Fatalf("absent trigram matches = %d, want 0", hits)
	}
}

func TestSearchMatchesBruteForce(t *testing.T) {
	dir := t.TempDir()
	store := mustOpen(t, dir)
	defer closeStore(t, store)
	sessions := []struct {
		id     string
		events [][]byte
	}{
		{"t01", [][]byte{searchUser("needle in prose one")}},
		{"t02", [][]byte{searchUser("nothing relevant here")}},
		{"t03", [][]byte{searchUser("the needle and the needle again")}},
		{"t04", [][]byte{searchAssistant("needle answer")}},
		{"t05", [][]byte{searchNamed("needle title")}},
		{"t06", [][]byte{searchToolOutput("needle only in tool output")}},
		{"t07", [][]byte{searchBlobUser()}},
		{"t08", [][]byte{searchUser("need le without the word")}},
		{"t09", [][]byte{searchUser("intro text"), searchAssistant("reply without the word")}},
		{"t10", [][]byte{searchUser("needle once"), searchAssistant("needle twice needle")}},
		{"t11", [][]byte{searchUser("unrelated question")}},
		{"t12", [][]byte{searchNamed("plain name"), searchUser("needle late in the thread")}},
	}
	for _, session := range sessions {
		appendRaw(t, store, session.id, session.events...)
	}
	hits := requireSearchEqualsOracle(t, store, "needle")
	if len(hits) != 6 {
		t.Fatalf("hits = %d, want 6", len(hits))
	}
	kinds := map[string]string{}
	for _, hit := range hits {
		kinds[hit.Session] = hit.Kind
	}
	if kinds["t04"] != "assistant" || kinds["t05"] != "title" || kinds["t01"] != "user" {
		t.Fatalf("hit kinds = %v", kinds)
	}
	if hits[0].Matches != 2 {
		t.Fatalf("best rank = %d, want 2", hits[0].Matches)
	}
}

func TestSearchShortQuery(t *testing.T) {
	dir := t.TempDir()
	store := mustOpen(t, dir)
	defer closeStore(t, store)
	appendRaw(t, store, "s1", searchUser("abc body"))
	for _, query := range []string{"", "ab", "needle ab"} {
		if _, err := store.Search(context.Background(), SearchQuery{Text: query}); !errors.Is(err, ErrQueryShort) {
			t.Fatalf("Search %q error = %v, want ErrQueryShort", query, err)
		}
	}
	hits, err := store.Search(context.Background(), SearchQuery{Text: "abc"})
	if err != nil {
		t.Fatalf("Search abc: %v", err)
	}
	if len(hits) != 1 || hits[0].Session != "s1" {
		t.Fatalf("Search abc hits = %v", hits)
	}
}

func ftsRowCount(t *testing.T, store *Store, sid string) int {
	t.Helper()
	var count int
	if err := store.db.QueryRow("SELECT COUNT(*) FROM thread_fts WHERE session = ?", sid).Scan(&count); err != nil {
		t.Fatalf("count thread_fts: %v", err)
	}
	return count
}

func TestMigrationBackfillsVersion2Mirror(t *testing.T) {
	dir := t.TempDir()
	store := mustOpen(t, dir)
	appendRaw(t, store, "s1", searchUser("needle before migration"))
	closeStore(t, store)
	raw := openRaw(t, dir)
	execAll(t, raw, []string{"DROP TABLE thread_fts", "PRAGMA user_version = 2"})
	if err := raw.Close(); err != nil {
		t.Fatalf("close raw state.db: %v", err)
	}
	reopened := mustOpen(t, dir)
	if version := queryUserVersion(t, reopened.db); version != 3 {
		closeStore(t, reopened)
		t.Fatalf("user_version = %d, want 3 after backfill", version)
	}
	hits, err := reopened.Search(context.Background(), SearchQuery{Text: "needle"})
	if err != nil {
		t.Fatalf("Search: %v", err)
	}
	if len(hits) != 1 || hits[0].Session != "s1" || hits[0].Matches != 1 {
		closeStore(t, reopened)
		t.Fatalf("hits = %v, want one s1 hit", hits)
	}
	if rows := ftsRowCount(t, reopened, "s1"); rows != 1 {
		closeStore(t, reopened)
		t.Fatalf("s1 index rows = %d, want 1", rows)
	}
	closeStore(t, reopened)
	again := mustOpen(t, dir)
	defer closeStore(t, again)
	if rows := ftsRowCount(t, again, "s1"); rows != 1 {
		t.Fatalf("s1 index rows after reopen = %d, want 1", rows)
	}
	requireSearchEqualsOracle(t, again, "needle")
}

func TestMigrationThreeIsIdempotent(t *testing.T) {
	dir := t.TempDir()
	store := mustOpen(t, dir)
	appendRaw(t, store, "s1", searchUser("needle one"))
	appendRaw(t, store, "s2", searchUser("needle two"))
	closeStore(t, store)
	execRaw(t, dir, []string{"DELETE FROM thread_fts WHERE session = 's2'", "PRAGMA user_version = 2"})
	reopened := mustOpen(t, dir)
	requireUserVersionThree(t, reopened, "after interrupted migration")
	requireFtsRowCounts(t, reopened, map[string]int{"s1": 1, "s2": 1})
	closeStore(t, reopened)

	execRaw(t, dir, []string{"DROP TABLE thread_fts", "PRAGMA user_version = 2"})
	openSharedConcurrently(t, dir)
	final := mustOpen(t, dir)
	defer closeStore(t, final)
	requireUserVersionThree(t, final, "after concurrent openers")
	requireSearchEqualsOracle(t, final, "needle")
}

func execRaw(t *testing.T, dir string, statements []string) {
	t.Helper()
	raw := openRaw(t, dir)
	execAll(t, raw, statements)
	if err := raw.Close(); err != nil {
		t.Fatalf("close raw state.db: %v", err)
	}
}

func requireUserVersionThree(t *testing.T, store *Store, when string) {
	t.Helper()
	if version := queryUserVersion(t, store.db); version != 3 {
		t.Fatalf("user_version = %d, want 3 %s", version, when)
	}
}

func requireFtsRowCounts(t *testing.T, store *Store, want map[string]int) {
	t.Helper()
	for sid, wantRows := range want {
		if rows := ftsRowCount(t, store, sid); rows != wantRows {
			t.Fatalf("%s index rows = %d, want %d", sid, rows, wantRows)
		}
	}
}

func openSharedConcurrently(t *testing.T, dir string) {
	t.Helper()
	var wg sync.WaitGroup
	stores := make([]*Store, 2)
	errs := make([]error, 2)
	for i := range stores {
		wg.Go(func() {
			stores[i], errs[i] = OpenShared(dir)
		})
	}
	wg.Wait()
	for i, err := range errs {
		if err != nil {
			t.Fatalf("concurrent OpenShared %d: %v", i, err)
		}
	}
	for i, shared := range stores {
		if err := shared.Close(); err != nil {
			t.Fatalf("close shared %d: %v", i, err)
		}
	}
}

func TestSearchSurvivesMirrorRebuild(t *testing.T) {
	dir := t.TempDir()
	store := mustOpen(t, dir)
	appendRaw(t, store, "s1", searchUser("needle survives rebuild"))
	closeStore(t, store)
	for _, name := range []string{"state.db", "state.db-wal", "state.db-shm"} {
		if err := removeIfExists(filepath.Join(dir, name)); err != nil {
			t.Fatalf("remove %s: %v", name, err)
		}
	}
	rebuilt := mustOpen(t, dir)
	defer closeStore(t, rebuilt)
	if rows := ftsRowCount(t, rebuilt, "s1"); rows != 1 {
		t.Fatalf("s1 index rows after rebuild = %d, want 1", rows)
	}
	requireSearchEqualsOracle(t, rebuilt, "needle")
}

func removeIfExists(path string) error {
	if err := os.Remove(path); err != nil && !errors.Is(err, os.ErrNotExist) {
		return err
	}
	return nil
}

func searchCreated(extra string) []byte {
	return []byte(`{"type":"session.created","id":"x","cwd":"/work"` + extra + `}`)
}

func searchHitSessions(t *testing.T, store *Store, query SearchQuery) []string {
	t.Helper()
	hits, err := store.Search(context.Background(), query)
	if err != nil {
		t.Fatalf("Search %q: %v", query.Text, err)
	}
	sessions := make([]string, 0, len(hits))
	for _, hit := range hits {
		sessions = append(sessions, hit.Session)
	}
	return sessions
}

func requireSessions(t *testing.T, got []string, want ...string) {
	t.Helper()
	if !slices.Equal(got, want) {
		t.Fatalf("sessions = %v, want %v", got, want)
	}
}

func appendEphemeralFixture(t *testing.T, store *Store) {
	t.Helper()
	appendRaw(t, store, "a-ephemeral", searchCreated(`,"ephemeral":true`), searchUser("needle needle in the ephemeral session"))
	appendRaw(t, store, "b-plain", searchCreated(""), searchUser("needle in the plain session"))
	appendRaw(t, store, "c-false", searchCreated(`,"ephemeral":false`), searchUser("needle in the explicit false session"))
	appendRaw(t, store, "d-flagged-message", []byte(`{"type":"user.message","item_id":"u","turn_id":"t","text":"needle in a flagged message","kind":"prompt","ephemeral":true}`))
}

func TestSearchExcludesEphemeralSession(t *testing.T) {
	dir := t.TempDir()
	store := mustOpen(t, dir)
	defer closeStore(t, store)
	appendEphemeralFixture(t, store)
	if _, ok := bruteForceBestRow(t, store, "needle")["a-ephemeral"]; !ok {
		t.Fatal("the ephemeral session holds no needle text, so the exclusion would prove nothing")
	}
	if rows := ftsRowCount(t, store, "a-ephemeral"); rows != 1 {
		t.Fatalf("a-ephemeral index rows = %d, want 1", rows)
	}
	requireSessions(t, searchHitSessions(t, store, SearchQuery{Text: "needle"}), "b-plain", "c-false", "d-flagged-message")
}

func TestSearchLimitCountsOnlyPersistentSessions(t *testing.T) {
	dir := t.TempDir()
	store := mustOpen(t, dir)
	defer closeStore(t, store)
	appendEphemeralFixture(t, store)
	requireSessions(t, searchHitSessions(t, store, SearchQuery{Text: "needle", Limit: 1}), "b-plain")
	requireSessions(t, searchHitSessions(t, store, SearchQuery{Text: "needle", Limit: 2}), "b-plain", "c-false")
}

func TestSearchEphemeralExclusionSurvivesBackfillAndRebuild(t *testing.T) {
	dir := t.TempDir()
	store := mustOpen(t, dir)
	appendEphemeralFixture(t, store)
	closeStore(t, store)
	execRaw(t, dir, []string{"DROP TABLE thread_fts", "PRAGMA user_version = 2"})
	backfilled := mustOpen(t, dir)
	requireUserVersionThree(t, backfilled, "after backfill")
	requireFtsRowCounts(t, backfilled, map[string]int{"a-ephemeral": 1, "b-plain": 1})
	requireSessions(t, searchHitSessions(t, backfilled, SearchQuery{Text: "needle"}), "b-plain", "c-false", "d-flagged-message")
	closeStore(t, backfilled)
	for _, name := range []string{"state.db", "state.db-wal", "state.db-shm"} {
		if err := removeIfExists(filepath.Join(dir, name)); err != nil {
			t.Fatalf("remove %s: %v", name, err)
		}
	}
	rebuilt := mustOpen(t, dir)
	defer closeStore(t, rebuilt)
	requireFtsRowCounts(t, rebuilt, map[string]int{"a-ephemeral": 1, "b-plain": 1})
	requireSessions(t, searchHitSessions(t, rebuilt, SearchQuery{Text: "needle"}), "b-plain", "c-false", "d-flagged-message")
}

func mirrorRowCount(t *testing.T, store *Store, query, sid string) int {
	t.Helper()
	var count int
	if err := store.db.QueryRow(query, sid).Scan(&count); err != nil {
		t.Fatalf("count %q for %s: %v", query, sid, err)
	}
	return count
}

func TestRemoveSessionDeletesSearchRows(t *testing.T) {
	dir := t.TempDir()
	store := mustOpen(t, dir)
	defer closeStore(t, store)
	appendRaw(t, store, "s1", searchUser("needle in the removed session"), searchNamed("needle title"))
	appendRaw(t, store, "s2", searchUser("needle in the kept session"))
	requireSessions(t, searchHitSessions(t, store, SearchQuery{Text: "needle"}), "s1", "s2")
	if err := removeSession(store.db, "s1"); err != nil {
		t.Fatalf("removeSession s1: %v", err)
	}
	requireSessions(t, searchHitSessions(t, store, SearchQuery{Text: "needle"}), "s2")
	requireFtsRowCounts(t, store, map[string]int{"s1": 0, "s2": 1})
	for _, sid := range []string{"s1", "s2"} {
		want := 0
		if sid == "s2" {
			want = 1
		}
		if rows := mirrorRowCount(t, store, "SELECT COUNT(*) FROM events WHERE session = ?", sid); rows != want {
			t.Fatalf("%s events rows = %d, want %d", sid, rows, want)
		}
		if rows := mirrorRowCount(t, store, "SELECT COUNT(*) FROM sessions WHERE id = ?", sid); rows != want {
			t.Fatalf("%s sessions rows = %d, want %d", sid, rows, want)
		}
	}
	if err := removeSession(store.db, "s1"); err != nil {
		t.Fatalf("removeSession s1 again: %v", err)
	}
	requireSessions(t, searchHitSessions(t, store, SearchQuery{Text: "needle"}), "s2")
}
