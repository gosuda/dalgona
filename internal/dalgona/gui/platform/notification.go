package platform

type NotificationPermission uint8

const (
	PermissionUndecided NotificationPermission = iota
	PermissionGranted
	PermissionDenied
	PermissionUnavailable
)

type TurnEvent uint8

const (
	TurnWaiting TurnEvent = iota + 1
	TurnCompleted
	TurnFailed
	TurnInterrupted
)

func (e TurnEvent) title() string {
	switch e {
	case TurnWaiting:
		return "Waiting for your reply"
	case TurnCompleted:
		return "Done"
	case TurnFailed:
		return "Failed"
	case TurnInterrupted:
		return "Stopped"
	}
	return ""
}

type NotificationState struct {
	Permission NotificationPermission
	Enabled    bool
	Focused    bool
}

type Notification struct {
	Title string
	Body  string
	Tag   string
}

func (s NotificationState) Notice(event TurnEvent, threadID, threadName string) (Notification, bool) {
	if s.Permission != PermissionGranted || !s.Enabled || s.Focused {
		return Notification{}, false
	}
	title := event.title()
	if title == "" || threadID == "" || threadName == "" {
		return Notification{}, false
	}
	return Notification{Title: title, Body: threadName, Tag: threadID}, true
}
