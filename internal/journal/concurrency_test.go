package journal

import (
	"context"
	"sync"
	"sync/atomic"
	"testing"
	"testing/synctest"
)

func goAppend(ctx context.Context, store *Store, sid string, events ...[]byte) <-chan error {
	done := make(chan error, 1)
	go func() {
		_, err := store.Append(ctx, sid, events...)
		done <- err
	}()
	return done
}

func awaitDone(t *testing.T, done <-chan error, what string) {
	t.Helper()
	select {
	case err := <-done:
		if err != nil {
			t.Fatalf("%s: %v", what, err)
		}
	default:
		t.Fatalf("%s did not finish", what)
	}
}

func gatingSyncfs(t *testing.T, store *Store, ctx context.Context) (func(), func()) {
	t.Helper()
	oldHook := syncfsHook
	store.syncfsRoot = "test"
	release := make(chan struct{})
	releaseIt := sync.OnceFunc(func() { close(release) })
	var gated atomic.Bool
	syncfsHook = func(string) error {
		if gated.CompareAndSwap(false, true) {
			select {
			case <-release:
			case <-ctx.Done():
			}
		}
		return nil
	}
	return func() { syncfsHook = oldHook }, releaseIt
}

func observeReplay(t *testing.T, ctx context.Context, store *Store, calls *int) error {
	t.Helper()
	*calls++
	head, err := store.Status("s")
	if err != nil {
		return err
	}
	if head.HeadSeq != 3 {
		t.Errorf("head inside replay = %d, want 3", head.HeadSeq)
	}
	if _, err := store.Append(ctx, "witness", event(*calls)); err != nil {
		t.Errorf("append inside replay: %v", err)
	}
	return nil
}

func requireContiguous(t *testing.T, seqs []uint64, want int) {
	t.Helper()
	if len(seqs) != want {
		t.Fatalf("replayed %d events, want %d", len(seqs), want)
	}
	for i, seq := range seqs {
		if seq != uint64(i+1) {
			t.Fatalf("seq %d at position %d, want %d", seq, i, i+1)
		}
	}
}

func TestAppendSessionsProceedInParallel(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		store := mustOpen(t, t.TempDir())
		defer closeStore(t, store)
		ctx := t.Context()
		if _, err := store.Append(ctx, "a", event(0)); err != nil {
			t.Fatal(err)
		}
		if _, err := store.Append(ctx, "b", event(0)); err != nil {
			t.Fatal(err)
		}
		restore, release := gatingSyncfs(t, store, ctx)
		defer restore()
		defer release()
		aDone := goAppend(ctx, store, "a", event(1))
		synctest.Wait()
		bDone := goAppend(ctx, store, "b", event(1))
		synctest.Wait()
		awaitDone(t, bDone, "append to idle session")
		release()
		synctest.Wait()
		awaitDone(t, aDone, "gated append")
	})
}

func TestReplayCallbackRunsWithoutStoreLock(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		store := mustOpen(t, t.TempDir())
		defer closeStore(t, store)
		ctx := t.Context()
		appendEvents(t, store, "s", 3)
		var calls int
		done := make(chan error, 1)
		go func() {
			done <- store.Replay("s", func(_ uint64, _ []byte) error {
				return observeReplay(t, ctx, store, &calls)
			})
		}()
		synctest.Wait()
		awaitDone(t, done, "replay with store calls in callback")
		if calls != 3 {
			t.Fatalf("replay calls = %d, want 3", calls)
		}
	})
}

func TestAppendSameSessionKeepsChain(t *testing.T) {
	synctest.Test(t, func(t *testing.T) {
		store := mustOpen(t, t.TempDir())
		defer closeStore(t, store)
		ctx := t.Context()
		finished := make(chan error, 4)
		for range 4 {
			go func() {
				events := make([][]byte, 25)
				for i := range events {
					events[i] = event(i)
				}
				_, err := store.Append(ctx, "s", events...)
				finished <- err
			}()
		}
		for range 4 {
			if err := <-finished; err != nil {
				t.Fatalf("append: %v", err)
			}
		}
		var seqs []uint64
		if err := store.Replay("s", func(seq uint64, _ []byte) error {
			seqs = append(seqs, seq)
			return nil
		}); err != nil {
			t.Fatalf("replay: %v", err)
		}
		requireContiguous(t, seqs, 100)
	})
}
