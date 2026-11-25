package terminal

import (
	"strings"

	"github.com/charmbracelet/x/ansi"

	"github.com/gosuda/dalgona/internal/dalgona/vtstate"
)

const MaxSearchMatches = 1000

type Match struct {
	Start Point
	End   Point
}

type SearchResult struct {
	Matches   []Match
	FirstLine int64
	EndLine   int64
	Limited   bool
}

type searchCell struct {
	text  string
	at    Point
	width uint8
}

func (r *Replica) Search(query string, caseSensitive bool) SearchResult {
	first, end := r.Lines()
	result := SearchResult{FirstLine: first, EndLine: end}
	needle := graphemes(query)
	if len(needle) == 0 {
		return result
	}
	var cells []vtstate.Cell
	var logical []searchCell
	for line := first; line < end && !result.Limited; line++ {
		var wrapped bool
		cells, wrapped, _ = r.AppendLine(cells[:0], line)
		for x, c := range cells {
			if !c.Continuation() {
				logical = append(logical, searchCell{text: c.Text, at: Point{line, x}, width: c.Width})
			}
		}
		if wrapped && line+1 < end {
			continue
		}
		result.find(logical, needle, caseSensitive)
		logical = logical[:0]
	}
	return result
}

func (s *SearchResult) find(haystack []searchCell, needle []string, caseSensitive bool) {
	for i := 0; i+len(needle) <= len(haystack); i++ {
		if !matchesAt(haystack[i:], needle, caseSensitive) {
			continue
		}
		last := haystack[i+len(needle)-1]
		end := last.at
		end.Column += int(max(last.width, 1)) - 1
		s.Matches = append(s.Matches, Match{Start: haystack[i].at, End: end})
		if len(s.Matches) == MaxSearchMatches {
			s.Limited = true
			return
		}
		i += len(needle) - 1
	}
}

func matchesAt(haystack []searchCell, needle []string, caseSensitive bool) bool {
	for j, want := range needle {
		got := haystack[j].text
		if got != want && (caseSensitive || !strings.EqualFold(got, want)) {
			return false
		}
	}
	return true
}

func graphemes(s string) []string {
	var out []string
	for s != "" {
		cluster, _ := ansi.FirstGraphemeCluster(s, ansi.GraphemeWidth)
		if cluster == "" {
			break
		}
		out = append(out, cluster)
		s = s[len(cluster):]
	}
	return out
}
