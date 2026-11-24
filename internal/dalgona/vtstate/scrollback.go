package vtstate

const (
	MaxScrollbackRows  = 10000
	MaxScrollbackBytes = 1 << 20
)

type scrollback struct {
	rows   []row
	oldest int
	bytes  int
	pushed uint64
}

func (s *scrollback) len() int {
	return len(s.rows) - s.oldest
}

func (s *scrollback) row(i int) row {
	return s.rows[s.oldest+i]
}

func (s *scrollback) push(r row) {
	kept := r.trimmed()
	s.rows = append(s.rows, kept)
	s.bytes += kept.textBytes()
	s.pushed++
	for s.len() > MaxScrollbackRows || s.bytes > MaxScrollbackBytes {
		s.evictOldest()
	}
}

func (s *scrollback) evictOldest() {
	s.bytes -= s.rows[s.oldest].textBytes()
	s.rows[s.oldest] = row{}
	s.oldest++
	if s.oldest*2 < len(s.rows) {
		return
	}
	live := copy(s.rows, s.rows[s.oldest:])
	clear(s.rows[live:])
	s.rows = s.rows[:live]
	s.oldest = 0
}

func (s *scrollback) clear() {
	clear(s.rows)
	s.rows = s.rows[:0]
	s.oldest = 0
	s.bytes = 0
}

func (s *scrollback) truncate(columns int) {
	for i := s.oldest; i < len(s.rows); i++ {
		r := &s.rows[i]
		if len(r.cells) <= columns {
			continue
		}
		s.bytes -= r.textBytes()
		r.resize(columns)
		*r = r.trimmed()
		s.bytes += r.textBytes()
	}
}
