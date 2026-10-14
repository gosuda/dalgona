$ErrorActionPreference = 'Stop'
# Records the long-lived process the cancellation sweep must kill. A nested
# Start-Process inherits the stripped exec environment and hangs on
# non-interactive CI sessions, so the fixture writes its own pid.
[System.IO.File]::WriteAllText($args[0], "$PID`n")
Start-Sleep -Seconds 300
