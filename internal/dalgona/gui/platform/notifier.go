package platform

import "errors"

var ErrNotificationsOff = errors.New("desktop notifications are off; allow them in Settings first")

const notificationActivationQueue = 16
