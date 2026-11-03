# Updates gition to the latest release, where it's installed now.
#   powershell -c "irm https://raw.githubusercontent.com/berkslv/gition/main/scripts/update.ps1 | iex"

$ErrorActionPreference = 'Stop'
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

$repo = 'berkslv/gition'
$raw = "https://raw.githubusercontent.com/$repo/main/scripts"

$cmd = Get-Command gition -ErrorAction SilentlyContinue
if (-not $cmd) { throw "gition: gition is not on your PATH; install it with: powershell -c `"irm $raw/install.ps1 | iex`"" }
$dir = Split-Path $cmd.Source

# /releases/latest redirects to /releases/tag/<version>; avoids API rate limits.
$req = [Net.WebRequest]::Create("https://github.com/$repo/releases/latest")
$req.AllowAutoRedirect = $false
$res = $req.GetResponse()
$latest = ($res.Headers['Location'] -split '/')[-1]
$res.Close()
if ($latest -notlike 'v*') { throw 'gition: could not determine the latest version' }

$current = 'v' + ((& $cmd.Source --version) -split ' ')[1]
if ($current -eq $latest) {
    Write-Host "gition: $current is already the latest version"
    return
}
if ($dir -like '*\.cargo\bin') { throw 'gition: gition was installed with cargo; update it with: cargo install gition' }

Write-Host "gition: updating $current to $latest in $dir"
$env:GITION_INSTALL_DIR = $dir
$env:GITION_VERSION = $latest
$env:GITION_NO_MODIFY_PATH = '1'
irm "$raw/install.ps1" | iex
