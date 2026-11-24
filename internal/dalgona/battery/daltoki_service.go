package battery

import "github.com/gosuda/dalgona/internal/dalgona/daltoki"

func (d *Daemon) Daltoki() *daltoki.Service {
	return d.daltoki
}
