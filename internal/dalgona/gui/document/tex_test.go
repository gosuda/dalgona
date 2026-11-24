package document

import (
	"strings"
	"testing"
)

func TestWriteSourceKeepsExactTeXBytes(t *testing.T) {
	d, err := New(Header{Identity: committed(t, "tex-exact"), Format: FormatMarkdown, Open: true}, "$$\n  x^2\n$$")
	if err != nil {
		t.Fatal(err)
	}
	found := false
	for i := range d.Len() {
		if d.Block(i).Kind != KindMath {
			continue
		}
		found = true
		var out strings.Builder
		if err := d.WriteSource(i, &out); err != nil {
			t.Fatal(err)
		}
		if out.String() != "\n  x^2\n" {
			t.Fatalf("Copy TeX wrote %q, want the exact formula body bytes", out.String())
		}
	}
	if !found {
		t.Fatal("no math block parsed from the display formula")
	}
}

func TestFingerprintStoredOnBlockBuild(t *testing.T) {
	d, err := New(Header{Identity: committed(t, "fingerprint-stored"), Format: FormatMarkdown, Open: true}, "some paragraph text")
	if err != nil {
		t.Fatal(err)
	}
	got := d.Block(0).Fingerprint
	if got == 0 {
		t.Fatal("built block carries no projection fingerprint")
	}
	if key := NewGeometryKey(d, 0, 0); key.Projection != got {
		t.Fatalf("geometry key uses %x, want the stored fingerprint %x", key.Projection, got)
	}
}
