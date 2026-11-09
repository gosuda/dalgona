package daemon

import (
	"strings"
	"testing"
)

func TestSystemdUnitEscapesPercentAndDollar(t *testing.T) {
	unit := installLinuxUnit(t, "/opt/%app$/dalgona", "/home/%user%$x")
	if want := `ExecStart="/opt/%%app$/dalgona" daemon`; !strings.Contains(unit, want) {
		t.Fatalf("unit %q lacks escaped ExecStart %q", unit, want)
	}
	if want := `Environment="DALGONA_HOME=/home/%%user%%$x"`; !strings.Contains(unit, want) {
		t.Fatalf("unit %q lacks escaped Environment %q", unit, want)
	}
}
