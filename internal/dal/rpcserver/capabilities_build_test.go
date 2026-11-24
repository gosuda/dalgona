package rpcserver

import (
	"crypto/sha256"
	"encoding/hex"
	"os"
	"testing"

	"github.com/gosuda/dalgona/internal/dal/rpc"

	"github.com/gosuda/dalgona/internal/dal/config"
)

func TestCapabilitiesReportTheExecutableBuild(t *testing.T) {
	exe, err := os.Executable()
	if err != nil {
		t.Fatal(err)
	}
	data, err := os.ReadFile(exe)
	if err != nil {
		t.Fatal(err)
	}
	sum := sha256.Sum256(data)
	r := newRig(t, config.ApprovalYolo)
	c := r.connectNative(ServeOptions{})
	var caps rpc.Capabilities
	c.ok("dalgona/capabilities/read", map[string]any{}, &caps)
	if want := hex.EncodeToString(sum[:])[:16]; caps.BackendBuild != want || config.Build() != want {
		t.Fatalf("backendBuild = %q, config.Build = %q, want %q from the executable bytes", caps.BackendBuild, config.Build(), want)
	}
}
