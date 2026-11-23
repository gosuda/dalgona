package daemon

import (
	"testing"

	"github.com/gosuda/dalgona/internal/dalgona/client"
)

func TestDialHomeReachesAServedDaemon(t *testing.T) {
	home := shortHome(t)
	const build = "00112233445566ff"
	cancel, done := startServe(t, t.Context(), home, identityServe(build, 7, nil))
	defer stopRun(t, cancel, done)
	conn, err := client.DialHome(home)
	if err != nil {
		t.Fatalf("DialHome: %v", err)
	}
	info, probe, err := client.ProbeConn(t.Context(), conn)
	if err != nil || probe != client.ProbeReady || info.Build != build || info.PID != 7 {
		t.Fatalf("client.ProbeConn = %+v, %v, %v; want build %s, pid 7, ready", info, probe, err, build)
	}
}
