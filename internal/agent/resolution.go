package agent

const (
	interruptedReason = "the turn was interrupted"
	cancelledReason   = "cancelled by user"
)

func ApprovalResolution(decision, reason string) string {
	switch {
	case decision == "accept" || decision == "accept_session":
		return "answered"
	case decision == "cancel" && reason == cancelledReason:
		return "cancelled"
	case decision == "cancel" && reason == interruptedReason:
		return "interrupted"
	}
	return "declined"
}
