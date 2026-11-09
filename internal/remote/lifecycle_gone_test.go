package remote

import (
	"context"
	"testing"

	"github.com/gosuda/dalgona/internal/config"
	"github.com/gosuda/dalgona/internal/sandboxsvc"
	"github.com/gosuda/dalgona/internal/sandboxsvc/fake"
)

func TestDestroy_treatsGoneBoxAsDestroyed(t *testing.T) {
	b := NewBinder(Options{Config: config.Config{Home: t.TempDir()}})
	svc := fake.New()
	b.newSvc = func(context.Context, config.RunnerSpec) (sandboxsvc.Service, error) {
		return svc, nil
	}
	ctx := t.Context()
	id, err := svc.Create(ctx, sandboxsvc.Spec{Name: "lost"})
	if err != nil {
		t.Fatal(err)
	}
	if err = svc.Destroy(ctx, id); err != nil {
		t.Fatal(err)
	}
	if err = b.destroy(ctx, "box", boxKey{service: "fake", id: id}); err != nil {
		t.Fatalf("destroy error = %v, want gone box treated as success", err)
	}
	books, err := b.readLedger()
	if err != nil {
		t.Fatal(err)
	}
	if !books.destroyed[boxKey{service: "fake", id: id}] {
		t.Fatal("ledger lacks destroyed line for gone box")
	}
}
