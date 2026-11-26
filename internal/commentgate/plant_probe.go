package commentgate

import "fmt"

// plantProbe is a deliberate vet defect: %d formats a string.
// It proves the rebuilt CI graph fails the vet gates on every lane.
var plantProbe = fmt.Sprintf("%d", "not-a-number")
