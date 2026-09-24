$ErrorActionPreference = 'Stop'
$root = (Resolve-Path "$PSScriptRoot/../..").Path
$installer = (Get-ChildItem "$root/dist/*-windows-x64-setup.exe" | Select-Object -First 1).FullName
$app = "$env:LOCALAPPDATA\Programs\Blobtorrent"
$state = "$env:LOCALAPPDATA\blobtorrent"
$config = "$env:APPDATA\blobtorrent-gui"
$logs = "$root/installer-test-logs"
New-Item -ItemType Directory -Force $logs | Out-Null
function Install($label) {
    $process = Start-Process $installer -ArgumentList @('/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', "/LOG=`"$logs/$label.log`"") -PassThru
    if (!$process.WaitForExit(120000)) { throw "Installer process timed out" }
    if ($process.ExitCode -ne 0) { throw "Installer failed: $($process.ExitCode)" }
}
function Cli([string[]] $arguments) {
    $output = & "$app/blobtorrent.exe" @arguments
    if ($LASTEXITCODE -ne 0) { throw "CLI failed: $arguments" }
    return $output
}
try {
    Install 'install'
    foreach ($name in @('blobtorrent.exe', 'blobtorrent-background.exe', 'blobtorrent-gui.exe', 'blobtorrent-tui.exe')) {
        if (!(Test-Path "$app/$name")) { throw "Missing $name" }
    }
    $startup = (Get-ItemProperty 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run').Blobtorrent
    if ($startup -ne "`"$app\blobtorrent-background.exe`"") { throw "Wrong login command: $startup" }
    Cli -arguments @('list') | Out-Null
    if (!(Test-Path "$config/client.json")) { throw 'GUI was not paired' }
    $clientConfig = Get-Content "$config/client.json" -Raw
    $identity = (Get-FileHash "$config/control-client.key").Hash
    if (!(Test-Path "$config/local-endpoint")) { throw 'GUI local filesystem mode was not configured' }
    $source = "$env:TEMP/blobtorrent-installer-test.txt"
    Set-Content $source 'Installer lifecycle check'
    Cli -arguments @('share', $source) | Out-Null
    $deadline = (Get-Date).AddSeconds(30)
    do {
        $listing = (Cli -arguments @('list')) -join "`n"
        if ($listing -match 'Seeding') { break }
        Start-Sleep -Milliseconds 250
    } while ((Get-Date) -lt $deadline)
    if ($listing -notmatch 'Seeding') { throw 'Test share did not finish' }
    Install 'upgrade'
    if ((Get-Content "$config/client.json" -Raw) -ne $clientConfig) { throw 'Upgrade changed configured daemon' }
    if ((Get-FileHash "$config/control-client.key").Hash -ne $identity) { throw 'Upgrade replaced GUI identity' }
    if (((Cli -arguments @('list')) -join "`n") -notmatch 'Seeding') { throw 'Upgrade lost saved data' }
    $process = Start-Process "$app/unins000.exe" -ArgumentList @('/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', "/LOG=`"$logs/uninstall.log`"") -PassThru
    if (!$process.WaitForExit(120000)) { throw "Installer process timed out" }
    if ($process.ExitCode -ne 0) { throw 'Uninstall failed' }
    if (Test-Path "$app/blobtorrent.exe") { throw 'Uninstall left installed binary' }
    if (Test-Path "$state/control.addr") { throw 'Uninstall did not stop daemon' }
    if (Get-ItemProperty 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run' -Name Blobtorrent -ErrorAction SilentlyContinue) { throw 'Uninstall left login entry' }
    if (!(Test-Path $source) -or !(Test-Path "$state/names.json") -or !(Test-Path "$config/control-client.key")) { throw 'Uninstall removed user data' }
    Write-Host 'PASS: install, local pairing, login registration, share, upgrade, graceful uninstall, and data retention'
} finally {
    foreach ($name in @('daemon.log', 'launcher.log')) {
        if (Test-Path "$state/$name") { Copy-Item "$state/$name" $logs }
    }
}
