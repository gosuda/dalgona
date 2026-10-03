$ErrorActionPreference = 'Stop'
# -NoNewWindow keeps Start-Process on CreateProcess; without it the
# console-window creation hangs on non-interactive CI sessions.
$child = Start-Process -FilePath 'powershell.exe' -ArgumentList @('-NoProfile', '-Command', 'Start-Sleep -Seconds 300') -PassThru -NoNewWindow
[System.IO.File]::WriteAllText($args[0], "$($child.Id)`n")
$child.WaitForExit()
