package report

import (
	"os"
	"time"
)

const (
	reportReadRetries    = 100
	reportReadRetryDelay = 20 * time.Millisecond
)

func readReportFile(path string) ([]byte, error) {
	for i := 0; ; i++ {
		raw, err := os.ReadFile(path)
		if err == nil || !isSharingViolation(err) || i >= reportReadRetries {
			return raw, err
		}
		time.Sleep(reportReadRetryDelay)
	}
}
