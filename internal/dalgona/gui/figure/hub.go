package figure

import "sync"

const outcomeLimit = 64

type Hub struct {
	Supervisor *Supervisor
	mu         sync.Mutex
	results    map[string]Outcome
	order      []string
}

func NewHub(sup *Supervisor, wake func()) *Hub {
	if sup == nil {
		return nil
	}
	h := &Hub{Supervisor: sup, results: map[string]Outcome{}}
	go func() {
		for o := range sup.Outcomes() {
			h.publish(o)
			if wake != nil {
				wake()
			}
		}
	}()
	return h
}

func (h *Hub) publish(o Outcome) {
	h.mu.Lock()
	defer h.mu.Unlock()
	if _, ok := h.results[o.Key]; !ok {
		h.order = append(h.order, o.Key)
	}
	h.results[o.Key] = o
	if len(h.order) > outcomeLimit {
		delete(h.results, h.order[0])
		h.order = h.order[1:]
	}
}

func (h *Hub) Result(key string) (Outcome, bool) {
	h.mu.Lock()
	defer h.mu.Unlock()
	o, ok := h.results[key]
	return o, ok
}
