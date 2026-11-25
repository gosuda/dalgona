Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

function Invoke-Checked([string] $program) {
    & $program @args
    if ($LASTEXITCODE -ne 0) {
        throw "desktop-package: $program failed with exit code $LASTEXITCODE"
    }
}

function Assert-PeMachine([string] $path, [uint16] $machine) {
    $reader = [IO.BinaryReader]::new([IO.File]::OpenRead($path))
    try {
        if ($reader.ReadUInt16() -ne 0x5A4D) {
            throw "desktop-package: missing DOS signature in $path"
        }
        $reader.BaseStream.Position = 0x3C
        $reader.BaseStream.Position = $reader.ReadUInt32()
        if ($reader.ReadUInt32() -ne 0x4550 -or $reader.ReadUInt16() -ne $machine) {
            throw "desktop-package: unexpected PE signature or architecture in $path"
        }
    } finally {
        $reader.Dispose()
    }
}

function Assert-MsiPlatform([string] $path, [string] $platform) {
    $installer = New-Object -ComObject WindowsInstaller.Installer
    $summary = $null
    try {
        $summary = $installer.GetType().InvokeMember('SummaryInformation', 'GetProperty', $null, $installer, @($path, 0))
        $template = $summary.GetType().InvokeMember('Property', 'GetProperty', $null, $summary, @(7))
        $schema = $summary.GetType().InvokeMember('Property', 'GetProperty', $null, $summary, @(14))
        if (($template -split ';')[0] -cne $platform -or [int]$schema -lt 500) {
            throw "desktop-package: expected MSI platform $platform and schema >= 500, got $template and $schema"
        }
    } finally {
        if ($null -ne $summary) {
            [void][Runtime.InteropServices.Marshal]::FinalReleaseComObject($summary)
        }
        [void][Runtime.InteropServices.Marshal]::FinalReleaseComObject($installer)
    }
}

function Assert-MsiToastRegistration([string] $path) {
    [xml] $metadata = Get-Content -Raw -LiteralPath $path
    $ns = [Xml.XmlNamespaceManager]::new($metadata.NameTable)
    $ns.AddNamespace('w', 'http://wixtoolset.org/schemas/v4/wxs')

    $shortcut = $metadata.SelectNodes('//w:Shortcut', $ns)
    if ($shortcut.Count -ne 1 -or $shortcut[0].GetAttribute('Directory') -cne 'ProgramMenuFolder' -or
        $shortcut[0].GetAttribute('Target') -cne '[#DalgonaGui]') {
        throw 'desktop-package: MSI must create one Start menu shortcut targeting dalgona-gui.exe'
    }
    $props = $shortcut[0].SelectNodes('w:ShortcutProperty', $ns)
    $appId = $shortcut[0].SelectSingleNode('w:ShortcutProperty[@Key="System.AppUserModel.ID" and @Value="Gosuda.Dalgona"]', $ns)
    $activator = $shortcut[0].SelectSingleNode('w:ShortcutProperty[@Key="System.AppUserModel.ToastActivatorCLSID" and @Value="{4C824C81-04CD-4609-A755-D11526E6CDFE}"]', $ns)
    if ($props.Count -ne 2 -or $null -eq $appId -or $null -eq $activator) {
        throw 'desktop-package: MSI shortcut lacks the dalgona application and toast activator identity'
    }

    $key = 'Software\Classes\CLSID\{4C824C81-04CD-4609-A755-D11526E6CDFE}\LocalServer32'
    $servers = @($metadata.SelectNodes('//w:RegistryValue[@Root="HKLM" and not(@Name)]', $ns) |
        Where-Object { $_.GetAttribute('Key') -ceq $key })
    $cleanup = @($metadata.SelectNodes('//w:RegistryKey[@Root="HKLM" and @ForceDeleteOnUninstall="yes"]', $ns) |
        Where-Object { $_.GetAttribute('Key') -ceq $key })
    if ($servers.Count -ne 1 -or $servers[0].GetAttribute('Value') -cne '"[#DalgonaGui]"' -or $cleanup.Count -ne 1) {
        throw 'desktop-package: MSI must register the quoted LocalServer32 executable path and remove the key on uninstall'
    }
}

$thumbprint = $env:DALGONA_WINDOWS_SIGN_SHA1
if ($thumbprint -notmatch '\A[0-9a-fA-F]{40}\z') {
    throw 'desktop-package: set DALGONA_WINDOWS_SIGN_SHA1 to the 40-digit thumbprint of a code-signing certificate in CurrentUser\My'
}
[Uri] $timestamp = $null
if (-not [Uri]::TryCreate($env:DALGONA_WINDOWS_TIMESTAMP_URL, [UriKind]::Absolute, [ref]$timestamp) -or
    $timestamp.Scheme -ne 'https' -or $timestamp.UserInfo) {
    throw 'desktop-package: set DALGONA_WINDOWS_TIMESTAMP_URL to an HTTPS RFC 3161 timestamp endpoint without embedded credentials'
}

$hostOS = Invoke-Checked go env GOHOSTOS
$targetOS = Invoke-Checked go env GOOS
if ($hostOS -ne 'windows' -or $targetOS -ne 'windows') {
    throw 'desktop-package: the MSI must be built on Windows with GOOS=windows'
}
$hostArch = Invoke-Checked go env GOHOSTARCH
$targetArch = Invoke-Checked go env GOARCH
$processor = Get-CimInstance Win32_Processor | Select-Object -First 1 -ExpandProperty Architecture
$nativeArch, $platform, $machine = switch ($processor) {
    9 { 'amd64', 'x64', 0x8664 }
    12 { 'arm64', 'Arm64', 0xAA64 }
    default { throw "desktop-package: unsupported Windows processor architecture $processor" }
}
if ($hostArch -ne $nativeArch -or $targetArch -ne $nativeArch) {
    throw "desktop-package: use native Windows/$nativeArch Go with GOARCH=$nativeArch (toolchain $hostArch, target $targetArch)"
}

$version = $env:DALGONA_PACKAGE_VERSION
if (-not $version) {
    $tags = @(Invoke-Checked git tag --points-at HEAD --list 'v[0-9]*')
    if ($tags.Count -ne 1) {
        throw 'desktop-package: set DALGONA_PACKAGE_VERSION to major.minor.patch or build at an exact vMAJOR.MINOR.PATCH tag'
    }
    $version = $tags[0].Substring(1)
}
if ($version -notmatch '\A(0|[1-9][0-9]{0,2})\.(0|[1-9][0-9]{0,2})\.(0|[1-9][0-9]{0,4})\z' -or
    [int]$Matches[1] -gt 255 -or [int]$Matches[2] -gt 255 -or [int]$Matches[3] -gt 65535) {
    throw 'desktop-package: MSI version must be major.minor.patch (limits 255.255.65535)'
}

$wixCommand = Get-Command wix.exe -CommandType Application -ErrorAction SilentlyContinue
if (-not $wixCommand) { throw 'desktop-package: put the WiX 5 or later wix.exe on PATH' }
$wix = $wixCommand.Source
$signToolCommand = Get-Command signtool.exe -CommandType Application -ErrorAction SilentlyContinue
if (-not $signToolCommand) { throw 'desktop-package: put the Windows SDK signtool.exe on PATH' }
$signTool = $signToolCommand.Source

$dist = [IO.Directory]::CreateDirectory((Join-Path (Get-Location).Path 'dist')).FullName
$work = Join-Path $dist ('.dalgona-windows-' + [Guid]::NewGuid().ToString('N'))
$payload = Join-Path $work 'payload'
[void][IO.Directory]::CreateDirectory($payload)
try {
    $gui = Join-Path $payload 'dalgona-gui.exe'
    $daemon = Join-Path $payload 'dalgona.exe'
    $env:CGO_ENABLED = '0'
    Invoke-Checked go build -tags gui -trimpath '-ldflags=-H=windowsgui' -o $gui ./cmd/dalgona-gui
    Invoke-Checked go build -trimpath -o $daemon ./cmd/dalgona
    Assert-PeMachine $gui $machine
    Assert-PeMachine $daemon $machine
    Invoke-Checked $signTool sign /sha1 $thumbprint /fd SHA256 /tr $timestamp.AbsoluteUri /td SHA256 $gui $daemon

    Copy-Item -LiteralPath LICENSE.md, THIRD_PARTY_NOTICES.md, assets/brand/dalgona.svg -Destination $payload
    $name = "dalgona-gui_${version}_${platform}.msi"
    $msi = Join-Path $work $name
    Invoke-Checked $wix build -arch $platform.ToLowerInvariant() -d "PackageVersion=$version" -b $payload -o $msi deploy/desktop/dalgona.wxs
    Assert-MsiPlatform $msi $platform

    $extract = Join-Path $work 'extract'
    $decompiled = Join-Path $work 'package.wxs'
    Invoke-Checked $wix msi decompile -x $extract -out $decompiled $msi
    Assert-MsiToastRegistration $decompiled
    $files = @{
        DalgonaGui = $gui
        DalgonaDaemon = $daemon
        ProjectLicense = 'LICENSE.md'
        ThirdPartyNotices = 'THIRD_PARTY_NOTICES.md'
        DalgonaMark = 'assets/brand/dalgona.svg'
    }
    $extracted = Join-Path $extract 'File'
    foreach ($entry in $files.GetEnumerator()) {
        $want = (Get-FileHash -LiteralPath $entry.Value).Hash
        $got = (Get-FileHash -LiteralPath (Join-Path $extracted $entry.Key)).Hash
        if ($got -ne $want) { throw "desktop-package: MSI payload differs for $($entry.Key)" }
    }
    Invoke-Checked $signTool verify /pa /tw $gui $daemon
    Invoke-Checked $signTool sign /sha1 $thumbprint /fd SHA256 /tr $timestamp.AbsoluteUri /td SHA256 $msi
    Invoke-Checked $signTool verify /pa /tw $msi
    Move-Item -Force -LiteralPath $msi -Destination (Join-Path $dist $name)
    Write-Output "desktop-package: dist/$name verified; clean-system install, launch, upgrade, rollback and uninstall did not run"
} finally {
    Remove-Item -LiteralPath $work -Recurse -Force
}
