package auth_test

import (
	"strings"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/auth"
)

func TestLiveMaskWaitsForLongerMultilineLiteral(t *testing.T) {
	const short = "credential-literal"
	const long = short + "\ncontinuation"
	t.Setenv("DAL_LIVE_SHORT_PASSWORD", short)
	t.Setenv("DAL_LIVE_Z_PASSWORD", long)
	t.Setenv("DAL_LIVE_A_PASSWORD", long)
	store, err := auth.NewWithPatterns(t.TempDir(), nil, auth.WithBuiltinPatterns(false))
	if err != nil {
		t.Fatal(err)
	}
	live, err := store.NewLiveMask()
	if err != nil {
		t.Fatal(err)
	}
	first := []byte(short + "\n")
	got, used, err := live.MaskPrefix(first, live.SafeBytes(first, 0), 64<<10)
	if err != nil || got != "" || used != 0 {
		t.Fatalf("partial longer credential produced %q, used=%d, err=%v", got, used, err)
	}
	all := []byte(long + "\n")
	got, used, err = live.MaskPrefix(all, live.SafeBytes(all, 0), 64<<10)
	if err != nil || got != "{{secret:DAL_LIVE_A_PASSWORD}}\n" || used != len(all) {
		t.Fatalf("resolved credential produced %q, used=%d, err=%v", got, used, err)
	}
}

func TestLiveMaskKeepsRecordedBuiltinWhenDiscoveryDisabled(t *testing.T) {
	home := t.TempDir()
	value := awsKey("R")
	store := newBuiltinStore(t, home)
	if text, err := store.MaskWithError(value); err != nil || text != "{{secret:builtin0:0}}" {
		t.Fatalf("record builtin mapping: %v", err)
	}
	reloaded, err := auth.NewWithPatterns(home, nil, auth.WithBuiltinPatterns(false))
	if err != nil {
		t.Fatal(err)
	}
	live, err := reloaded.NewLiveMask()
	if err != nil {
		t.Fatal(err)
	}
	window := []byte("embedded:" + value + "\n")
	got, used, err := live.MaskPrefix(window, live.SafeBytes(window, 0), 64<<10)
	if err != nil || strings.Contains(got, value) || got != "embedded:{{secret:builtin0:0}}\n" || used != len(window) {
		t.Fatalf("reloaded live mask failed, bytes used %d, error %v", used, err)
	}
}
