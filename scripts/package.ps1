<#
.SYNOPSIS
    Build a release of Argus as one zip, the way install.ps1 expects it.

.DESCRIPTION
    Builds argus-engine, argus-cli and the desktop app in release mode, then
    lays them out in one folder with the shipped manifests:

        Argus.exe            the desktop app
        argus-engine.exe     the daemon the app starts
        argus-cli.exe        the command line client
        manifests\*.json     the agents the engine can launch

    That folder is zipped as dist\argus-<version>-x86_64-pc-windows-msvc.zip,
    beside a SHA256SUMS file that install.ps1 checks the download against.

    The version is the workspace's, from Cargo.toml. The release workflow runs
    exactly this script, so a local build and a published one are the same.

.EXAMPLE
    .\scripts\package.ps1
#>
param(
    # Skip `npm ci` when node_modules is already there, for a faster local run.
    [switch]$SkipNpmInstall
)

# Not "Stop": in Windows PowerShell 5.1 that turns a native command's progress
# on stderr into a terminating error, and cargo reports progress on stderr.
# Every native step checks its exit code instead.
$repo = Split-Path -Parent $PSScriptRoot
Set-Location $repo

function Step([string]$text) { Write-Host "==> $text" -ForegroundColor Cyan }
function Check([string]$what) { if ($LASTEXITCODE -ne 0) { throw "$what failed (exit $LASTEXITCODE)" } }

$version = (Select-String -Path Cargo.toml -Pattern '^version\s*=\s*"([^"]+)"' |
    Select-Object -First 1).Matches[0].Groups[1].Value
if (-not $version) { throw "could not read the workspace version from Cargo.toml" }
# The app carries its own version in two more places. A release whose window
# reports a different version than its binaries is worse than no release.
$appVersion = (Get-Content app\src-tauri\tauri.conf.json -Raw | ConvertFrom-Json).version
$npmVersion = (Get-Content app\package.json -Raw | ConvertFrom-Json).version
# The npm package downloads the release named after its own version, so it
# has to agree too.
$pkgVersion = (Get-Content npm\package.json -Raw | ConvertFrom-Json).version
if ($appVersion -ne $version -or $npmVersion -ne $version -or $pkgVersion -ne $version) {
    throw "versions disagree: Cargo.toml $version, tauri.conf.json $appVersion, app\package.json $npmVersion, npm\package.json $pkgVersion"
}
$target = "x86_64-pc-windows-msvc"
$name = "argus-$version-$target"
$dist = Join-Path $repo "dist"
$stage = Join-Path $dist $name
$zip = Join-Path $dist "$name.zip"

Step "Argus $version for $target"

Step "Building argus-engine and argus-cli"
cargo build --release --locked -p argus-engine -p argus-cli
Check "cargo build"

Step "Building the desktop app"
Push-Location app
try {
    if (-not $SkipNpmInstall -or -not (Test-Path node_modules)) {
        npm ci
        Check "npm ci"
    }
    # The bare executable: the zip is the installer's unit, not an MSI. The
    # app finds argus-engine.exe and manifests\ beside itself.
    npx tauri build --no-bundle --ci
    Check "tauri build"
}
finally {
    Pop-Location
}

Step "Assembling $name"
Remove-Item -Recurse -Force $stage, $zip -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force (Join-Path $stage "manifests") | Out-Null
$release = Join-Path $repo "target\release"
Copy-Item (Join-Path $release "argus-app.exe") (Join-Path $stage "Argus.exe")
Copy-Item (Join-Path $release "argus-engine.exe") $stage
Copy-Item (Join-Path $release "argus-cli.exe") $stage
Copy-Item (Join-Path $repo "manifests\*.json") (Join-Path $stage "manifests")
Copy-Item README.md, LICENSE-MIT, LICENSE-APACHE $stage

# The archive holds the folder's contents, not the folder, so it unpacks
# straight into wherever the installer points it.
Compress-Archive -Path (Join-Path $stage "*") -DestinationPath $zip
$hash = (Get-FileHash -Algorithm SHA256 $zip).Hash.ToLowerInvariant()
# The `sha256sum` format, so it can be checked on any platform too.
Set-Content -Path (Join-Path $dist "SHA256SUMS") -Value "$hash  $name.zip" -Encoding ascii

Step "Done"
Get-ChildItem $stage -Recurse -File | ForEach-Object {
    "{0,10:N0}  {1}" -f $_.Length, $_.FullName.Substring($stage.Length + 1)
}
Write-Host ""
Write-Host "  $zip"
Write-Host "  sha256 $hash"
