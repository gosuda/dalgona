//go:build linux

package daemon

import (
	"errors"
	"testing"
)

func TestRefuseSandboxedPeer(t *testing.T) {
	readErr := errors.New("daemon: read peer status: gone")
	cases := []struct {
		name          string
		marks         sandboxMarks
		wantSandboxed bool
		wantErr       bool
	}{
		{name: "clean peer connects", marks: sandboxMarks{}, wantSandboxed: false, wantErr: false},
		{name: "sandboxed peer refused", marks: sandboxMarks{peer: true}, wantSandboxed: true, wantErr: true},
		{name: "clean peer connects when daemon marked", marks: sandboxMarks{daemon: true}, wantSandboxed: false, wantErr: false},
		{name: "sandboxed peer connects when daemon marked", marks: sandboxMarks{peer: true, daemon: true}, wantSandboxed: false, wantErr: false},
		{name: "unreadable peer refused", marks: sandboxMarks{readErr: readErr}, wantSandboxed: false, wantErr: true},
		{name: "unreadable peer refused when daemon marked", marks: sandboxMarks{daemon: true, readErr: readErr}, wantSandboxed: false, wantErr: true},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			err := refuseSandboxedPeer(tc.marks)
			if !tc.wantErr {
				if err != nil {
					t.Fatalf("refused clean peer: %v", err)
				}
				return
			}
			if err == nil {
				t.Fatal("accepted sandboxed peer")
			}
			if got := errors.Is(err, errSandboxed); got != tc.wantSandboxed {
				t.Fatalf("sandboxed=%v, want %v: %v", got, tc.wantSandboxed, err)
			}
		})
	}
}
