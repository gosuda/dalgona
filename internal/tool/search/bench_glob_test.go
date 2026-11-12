package search

import "testing"

func BenchmarkMatchingFilesAll(b *testing.B) {
	entries := shuffledEntries(200000)
	if _, err := matchingFiles(entries, "**"); err != nil {
		b.Fatal(err)
	}
	benchSamples(b, nil, func() {
		if _, err := matchingFiles(entries, "**"); err != nil {
			b.Fatal(err)
		}
	})
}
