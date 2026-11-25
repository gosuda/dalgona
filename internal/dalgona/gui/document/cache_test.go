package document

import "testing"

func TestGeometryKeyChangesWhenReferenceResolves(t *testing.T) {
	d, err := New(Header{Identity: committed(t, "ref-proj"), Format: FormatMarkdown, Open: true}, "lead paragraph\n\nsee [docs] now\n\ntail text\n")
	if err != nil {
		t.Fatal(err)
	}
	before := NewGeometryKey(d, 1, 0)
	if len(d.Block(1).Links) != 0 {
		t.Fatalf("unresolved paragraph links = %+v, want none", d.Block(1).Links)
	}
	next, _, err := d.Append("\n[docs]: https://example.com/\n")
	if err != nil {
		t.Fatal(err)
	}
	if len(next.Block(1).Links) == 0 {
		t.Fatal("paragraph gained no link after its reference definition arrived")
	}
	after := NewGeometryKey(next, 1, 0)
	if after.Block != before.Block {
		t.Fatalf("finished block key changed from %+v to %+v, want the same key so the test isolates the projection", before.Block, after.Block)
	}
	if after == before {
		t.Fatal("geometry key did not change when the reference resolution changed the projection")
	}
}
