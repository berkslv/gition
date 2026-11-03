# Installs gition from GitHub Releases.
#   powershell -c "irm https://raw.githubusercontent.com/berkslv/gition/main/scripts/install.ps1 | iex"
# Env: GITION_VERSION (e.g. v0.1.0, default latest),
#      GITION_INSTALL_DIR (default %LOCALAPPDATA%\Programs\gition),
#      GITION_NO_MODIFY_PATH=1 to leave the user PATH alone.

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

$repo = 'berkslv/gition'
$installDir = if ($env:GITION_INSTALL_DIR) { $env:GITION_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA 'Programs\gition' }

$arch = switch ($env:PROCESSOR_ARCHITECTURE) {
    'AMD64' { 'x86_64' }
    'ARM64' { 'aarch64' }
    default { throw "gition: unsupported architecture: $env:PROCESSOR_ARCHITECTURE" }
}
$target = "$arch-pc-windows-msvc"

$version = $env:GITION_VERSION
if (-not $version) {
    # /releases/latest redirects to /releases/tag/<version>; avoids API rate limits.
    $req = [Net.WebRequest]::Create("https://github.com/$repo/releases/latest")
    $req.AllowAutoRedirect = $false
    $res = $req.GetResponse()
    $version = ($res.Headers['Location'] -split '/')[-1]
    $res.Close()
    if ($version -notlike 'v*') { throw 'gition: could not determine the latest version' }
}

$name = "gition-$version-$target"
$base = "https://github.com/$repo/releases/download/$version"
$tmp = Join-Path ([IO.Path]::GetTempPath()) ([Guid]::NewGuid())
New-Item -ItemType Directory -Path $tmp | Out-Null

try {
    Write-Host "gition: downloading $name"
    $zip = Join-Path $tmp "$name.zip"
    Invoke-WebRequest "$base/$name.zip" -OutFile $zip -UseBasicParsing
    $sums = (Invoke-WebRequest "$base/SHA256SUMS" -UseBasicParsing).Content
    if ($sums -is [byte[]]) { $sums = [Text.Encoding]::UTF8.GetString($sums) }

    $line = ($sums -split "`n") | Where-Object { $_ -match "\s$([regex]::Escape("$name.zip"))\s*$" } | Select-Object -First 1
    if (-not $line) { throw "gition: no checksum for $name.zip" }
    $expected = ($line -split '\s+')[0]
    $actual = (Get-FileHash $zip -Algorithm SHA256).Hash
    if ($expected -ne $actual) { throw "gition: checksum mismatch for $name.zip" }

    Expand-Archive $zip -DestinationPath $tmp -Force
    New-Item -ItemType Directory -Path $installDir -Force | Out-Null
    Copy-Item (Join-Path $tmp "$name\gition.exe") (Join-Path $installDir 'gition.exe') -Force
    Write-Host "gition: installed $version to $installDir\gition.exe"
}
finally {
    Remove-Item $tmp -Recurse -Force -ErrorAction SilentlyContinue
}

if ($env:GITION_NO_MODIFY_PATH -ne '1') {
    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    $entries = if ($userPath) { $userPath -split ';' } else { @() }
    if ($entries -notcontains $installDir) {
        [Environment]::SetEnvironmentVariable('Path', (($entries + $installDir) -join ';').Trim(';'), 'User')
        $env:Path = "$env:Path;$installDir"
        Write-Host "gition: added $installDir to your PATH (open a new terminal to use it)"
    }
}
