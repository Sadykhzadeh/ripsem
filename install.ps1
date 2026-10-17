# Installs the latest ripsem release on Windows:
#
#   irm https://raw.githubusercontent.com/Sadykhzadeh/ripsem/master/install.ps1 | iex
#
# The files go to %LOCALAPPDATA%\Programs\ripsem, or to $env:RIPSEM_INSTALL_DIR
# when it is set, and that directory is added to the user's PATH.

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

$repo = 'Sadykhzadeh/ripsem'
if ($env:PROCESSOR_ARCHITECTURE -ne 'AMD64') {
    throw "ripsem has no prebuilt binary for $($env:PROCESSOR_ARCHITECTURE) Windows; see https://github.com/$repo#install"
}
$dir = $env:RIPSEM_INSTALL_DIR
if (-not $dir) { $dir = Join-Path $env:LOCALAPPDATA 'Programs\ripsem' }

# curl.exe ships with Windows 10 1803 and later and downloads many times faster
# than Invoke-WebRequest in Windows PowerShell.
$curl = Join-Path $env:SystemRoot 'System32\curl.exe'
function Get-Url($url, $path) {
    if (Test-Path $curl) {
        & $curl -fsSL -o $path $url
        if ($LASTEXITCODE -ne 0) { throw "downloading $url failed" }
    } else {
        Invoke-WebRequest $url -OutFile $path -UseBasicParsing
    }
}

$tag = (Invoke-RestMethod "https://api.github.com/repos/$repo/releases/latest").tag_name
$name = "ripsem-$tag-x86_64-pc-windows-msvc"
$base = "https://github.com/$repo/releases/download/$tag"

$tmp = Join-Path ([IO.Path]::GetTempPath()) ([Guid]::NewGuid())
New-Item -ItemType Directory $tmp | Out-Null
try {
    Write-Host "downloading ripsem $tag for x86_64-pc-windows-msvc"
    $zip = Join-Path $tmp "$name.zip"
    $sums = Join-Path $tmp 'SHA256SUMS'
    Get-Url "$base/$name.zip" $zip
    Get-Url "$base/SHA256SUMS" $sums

    $line = Get-Content $sums | Where-Object { $_ -match " \*?$([regex]::Escape("$name.zip"))$" }
    $expected = ($line -split '\s+')[0]
    if ((Get-FileHash $zip -Algorithm SHA256).Hash -ne $expected) {
        throw "$name.zip does not match its checksum in SHA256SUMS"
    }
    Expand-Archive $zip -DestinationPath $tmp

    New-Item -ItemType Directory -Force $dir | Out-Null
    Copy-Item (Join-Path $tmp "$name\ripsem.exe"), (Join-Path $tmp "$name\DirectML.dll") $dir -Force
} finally {
    Remove-Item $tmp -Recurse -Force
}

$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if (($userPath -split ';') -notcontains $dir) {
    $parts = @($userPath -split ';' | Where-Object { $_ }) + $dir
    [Environment]::SetEnvironmentVariable('Path', ($parts -join ';'), 'User')
    $env:Path = "$env:Path;$dir"
    Write-Host "added $dir to your PATH; open a new terminal to run it as ripsem"
}
Write-Host "installed ripsem $tag to $dir"

if (-not (Test-Path (Join-Path $env:SystemRoot 'System32\vcruntime140.dll'))) {
    Write-Host 'ripsem needs the Microsoft Visual C++ Redistributable: winget install Microsoft.VCRedist.2015+.x64'
}
