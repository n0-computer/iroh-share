$ErrorActionPreference = 'Stop'
$root = (Resolve-Path "$PSScriptRoot/../..").Path
$version = python -c "import tomllib; print(tomllib.load(open('Cargo.toml','rb'))['workspace']['package']['version'])"
if ($LASTEXITCODE -ne 0) { throw 'Could not read workspace version' }
$compiler = "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe"
if (!(Test-Path $compiler)) {
    choco install innosetup --version=6.4.3 --yes --no-progress
    if ($LASTEXITCODE -ne 0) { throw 'Inno Setup installation failed' }
}
if (!(Test-Path $compiler)) { throw 'Inno Setup compiler not found' }
& $compiler "/DAppVersion=$version" "/DBuildDir=$root\target\x86_64-pc-windows-msvc\release" "$PSScriptRoot\blobtorrent.iss"
if ($LASTEXITCODE -ne 0) { throw 'Installer compilation failed' }
$installer = Get-Item "$root/dist/blobtorrent-$version-windows-x64-setup.exe"
$hash = (Get-FileHash $installer.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
Set-Content -Path "$($installer.FullName).sha256" -Value "$hash  $($installer.Name)" -Encoding ascii
