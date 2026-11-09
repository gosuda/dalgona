package journal

import (
	"context"
	"fmt"
	"testing"
)

func BenchmarkOpenManySessions(b *testing.B) {
	dir := b.TempDir()
	store, err := Open(dir)
	if err != nil {
		b.Fatalf("Open: %v", err)
	}
	for s := range 30 {
		id := fmt.Sprintf("bench-%02d", s)
		batch := make([][]byte, 0, 1500)
		for n := range 1500 {
			batch = append(batch, event(n))
		}
		if _, err := store.Append(context.Background(), id, batch...); err != nil {
			_ = store.Close()
			b.Fatalf("Append: %v", err)
		}
	}
	if err := store.Close(); err != nil {
		b.Fatalf("Close: %v", err)
	}
	b.ResetTimer()
	b.ReportAllocs()
	for range b.N {
		reopened, err := Open(dir)
		if err != nil {
			b.Fatalf("reopen: %v", err)
		}
		if err := reopened.Close(); err != nil {
			b.Fatalf("close: %v", err)
		}
	}
}
