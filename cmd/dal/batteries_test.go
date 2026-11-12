package main

import (
	"log/slog"
	"testing"

	"github.com/gosuda/dalgona/internal/app"
	"github.com/gosuda/dalgona/internal/config"
)

func TestEvalBatteriesCarryOnlyTheScriptExtension(t *testing.T) {
	t.Setenv("DAL_HOME", t.TempDir())
	cfg, err := config.Load(config.ProductDal, t.TempDir(), config.Flags{})
	if err != nil {
		t.Fatal(err)
	}
	batteries, err := evalBatteries(t.Context(), app.Base{Config: cfg, Logger: slog.New(slog.DiscardHandler)})
	if err != nil {
		t.Fatal(err)
	}
	if batteries.Extension == nil {
		t.Fatal("evalBatteries left Extension nil")
	}
	if batteries.Tools != nil || batteries.Skills != nil || batteries.Runner != nil || batteries.Attach != nil || batteries.Close != nil {
		t.Fatalf("evalBatteries set a field beyond Extension: %+v", batteries)
	}
}
