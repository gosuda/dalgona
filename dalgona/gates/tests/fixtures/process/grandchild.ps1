$ErrorActionPreference = "Stop"
$pidFile = $args[0]
[System.IO.File]::WriteAllText($pidFile, "$PID`n", [System.Text.Encoding]::ASCII)
while ($true) {
    Start-Sleep -Seconds 1
}
