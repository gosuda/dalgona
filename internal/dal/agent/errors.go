package agent

import "errors"

var (
	ErrThreadRunning  = errors.New("agent: thread is running")
	ErrArchived       = errors.New("agent: thread is archived")
	ErrTurnNotOnPath  = errors.New("agent: turn is not on the active path")
	ErrUnknownTurn    = errors.New("agent: unknown turn")
	ErrUnknownRequest = errors.New("agent: unknown approval request")
	ErrDamaged        = errors.New("agent: session is damaged")
	ErrClosed         = errors.New("agent: thread is closed")
	ErrNeedsOverrides = errors.New("agent: advisor session needs its tools and prompt")
	ErrTurnNotRunning = errors.New("agent: turn is not running")
)

type stateError struct {
	message string
	kind    error
}

func (e stateError) Error() string { return e.message }
func (e stateError) Unwrap() error { return e.kind }

func runningError(id string) error {
	return stateError{message: "thread " + id + " is running", kind: ErrThreadRunning}
}

func archivedError(id string) error {
	return stateError{message: "thread " + id + " is archived", kind: ErrArchived}
}

func notOnPathError(turn string) error {
	return stateError{message: "turn " + turn + " is not on the active path", kind: ErrTurnNotOnPath}
}

func unknownTurnError(turn string) error {
	return stateError{message: "unknown turn " + turn, kind: ErrUnknownTurn}
}

func turnNotRunningError(turn string) error {
	return stateError{message: "turn " + turn + " is not running", kind: ErrTurnNotRunning}
}
