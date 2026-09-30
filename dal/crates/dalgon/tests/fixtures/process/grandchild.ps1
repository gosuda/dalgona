$ErrorActionPreference = 'Stop'
$child = Start-Process -FilePath 'powershell.exe' -ArgumentList @('-NoProfile', '-Command', 'Start-Sleep -Seconds 300') -PassThru
[System.IO.File]::WriteAllText($args[0], "$($child.Id)`n")
$child.WaitForExit()
