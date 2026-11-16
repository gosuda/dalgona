package owners

type (
	Store interface {
		Load(key string) (string, bool)
		Save(key, value string)
	}

	Memory struct {
		items map[string]string
	}

	Index [2]int

	Hook func()

	Watcher interface {
		Watch(path string) error
	}
)

func (m *Memory) Load(key string) (string, bool) {
	value, ok := m.items[key]
	return value, ok
}

func (m *Memory) Save(key, value string) {
	m.items[key] = value
}

func (i Index) First() int {
	return i[0]
}

func (h Hook) Run() {
	h()
}
