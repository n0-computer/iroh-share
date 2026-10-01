$ErrorActionPreference = 'Stop'
if ($env:CI -ne 'true') { throw 'Run this installation test only on an ephemeral CI runner' }
$root = (Resolve-Path "$PSScriptRoot/../..").Path
$installer = (Get-ChildItem "$root/dist/*-windows-x64-setup.exe" | Select-Object -First 1).FullName
$app = "$env:LOCALAPPDATA\Programs\Iroh Share"
$state = "$env:LOCALAPPDATA\iroh-share"
$config = "$env:APPDATA\iroh-share-gui"
$logs = "$root/installer-test-logs"
New-Item -ItemType Directory -Force $logs | Out-Null
function Install($label) {
    $process = Start-Process $installer -ArgumentList @('/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', "/LOG=`"$logs/$label.log`"") -PassThru
    if (!$process.WaitForExit(120000)) { throw "Installer process timed out" }
    if ($process.ExitCode -ne 0) { throw "Installer failed: $($process.ExitCode)" }
}
function Invoke-IrohShare([string[]] $arguments) {
    $output = & "$app/iroh-share.exe" @arguments
    if ($LASTEXITCODE -ne 0) { throw "CLI failed: $arguments" }
    return $output
}
try {
    Install 'install'
    foreach ($name in @('iroh-share.exe', 'iroh-share-background.exe', 'iroh-share-gui.exe', 'iroh-share-tui.exe')) {
        if (!(Test-Path "$app/$name")) { throw "Missing $name" }
    }
    $startup = (Get-ItemProperty 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run').'Iroh Share'
    if ($startup -ne "`"$app\iroh-share-background.exe`"") { throw "Wrong login command: $startup" }
    Invoke-IrohShare -arguments @('list') | Out-Null
    if (!(Test-Path "$config/client.json")) { throw 'GUI was not paired' }
    $clientConfig = Get-Content "$config/client.json" -Raw
    $identity = (Get-FileHash "$config/control-client.key").Hash
    if (!(Test-Path "$config/local-endpoint")) { throw 'GUI local filesystem mode was not configured' }
    $source = "$env:TEMP/iroh-share-installer-test.txt"
    Set-Content $source 'Installer lifecycle check'
    Invoke-IrohShare -arguments @('share', $source) | Out-Null
    $deadline = (Get-Date).AddSeconds(30)
    do {
        $listing = (Invoke-IrohShare -arguments @('list')) -join "`n"
        if ($listing -match 'Seeding') { break }
        Start-Sleep -Milliseconds 250
    } while ((Get-Date) -lt $deadline)
    if ($listing -notmatch 'Seeding') { throw 'Test share did not finish' }
    Install 'upgrade'
    if ((Get-Content "$config/client.json" -Raw) -ne $clientConfig) { throw 'Upgrade changed configured daemon' }
    if ((Get-FileHash "$config/control-client.key").Hash -ne $identity) { throw 'Upgrade replaced GUI identity' }
    if (((Invoke-IrohShare -arguments @('list')) -join "`n") -notmatch 'Seeding') { throw 'Upgrade lost saved data' }
    $process = Start-Process "$app/unins000.exe" -ArgumentList @('/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', "/LOG=`"$logs/uninstall.log`"") -PassThru
    if (!$process.WaitForExit(120000)) { throw "Installer process timed out" }
    if ($process.ExitCode -ne 0) { throw 'Uninstall failed' }
    if (Test-Path "$app/iroh-share.exe") { throw 'Uninstall left installed binary' }
    if (Test-Path "$state/control.addr") { throw 'Uninstall did not stop daemon' }
    if (Get-ItemProperty 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run' -Name 'Iroh Share' -ErrorAction SilentlyContinue) { throw 'Uninstall left login entry' }
    if (!(Test-Path $source) -or !(Test-Path "$state/names.json") -or !(Test-Path "$config/control-client.key")) { throw 'Uninstall removed user data' }
    Write-Host 'PASS: install, local pairing, login registration, share, upgrade, graceful uninstall, and data retention'
} finally {
    foreach ($name in @('daemon.log', 'launcher.log')) {
        if (Test-Path "$state/$name") { Copy-Item "$state/$name" $logs }
    }
}
