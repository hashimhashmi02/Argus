# Argus installer (Windows / PowerShell).
#
# One-line install of the latest release:
#
#   irm https://raw.githubusercontent.com/hashimhashmi02/Argus/main/install.ps1 | iex
#
# With options -- a pinned version, or uninstalling -- the script has to be
# turned into a script block first, because a pipe into `iex` cannot carry
# arguments:
#
#   & ([scriptblock]::Create((irm https://raw.githubusercontent.com/hashimhashmi02/Argus/main/install.ps1))) -Version 0.1.0
#   & ([scriptblock]::Create((irm https://raw.githubusercontent.com/hashimhashmi02/Argus/main/install.ps1))) -Uninstall
#
# From a clone, or a zip built by scripts\package.ps1:
#
#   .\install.ps1 -FromZip dist\argus-0.1.0-x86_64-pc-windows-msvc.zip
#
# What it does, and nothing more: downloads the release zip and checks it
# against the release's SHA256SUMS, unpacks it into
# %LOCALAPPDATA%\Programs\Argus, puts that folder on your user PATH (for
# argus-cli and argus-engine), and adds Argus to the Start menu. No admin
# rights, no services, nothing outside your user profile. Uninstalling removes
# exactly those, and leaves your session history in %LOCALAPPDATA%\Argus
# unless you also pass -Purge.
#
# Why everything is inside a function: `irm | iex` runs this file as a string,
# so it has no script path and a top-level param() block could not receive
# arguments anyway. A function called with `@args` works for the pipe, the
# script block, and a local run alike.

function Install-Argus {
    [CmdletBinding()]
    param(
        # A release tag such as 0.1.0 or v0.1.0. Defaults to the latest release.
        [string]$Version = "latest",
        # Install from a local zip instead of downloading one.
        [string]$FromZip,
        [string]$InstallDir = (Join-Path $env:LOCALAPPDATA "Programs\Argus"),
        # Do not add the install folder to the user PATH.
        [switch]$NoPath,
        # Do not add a Start menu shortcut.
        [switch]$NoShortcut,
        [switch]$Uninstall,
        # With -Uninstall: also delete %LOCALAPPDATA%\Argus -- the session
        # list, its state file and the engine log.
        [switch]$Purge
    )

    $ErrorActionPreference = "Stop"
    $repo = "hashimhashmi02/Argus"
    $assetPattern = '^argus-.+-x86_64-pc-windows-msvc\.zip$'
    $marker = Join-Path $InstallDir ".argus-install"
    $shortcut = Join-Path ([Environment]::GetFolderPath("Programs")) "Argus.lnk"
    $stateDir = Join-Path $env:LOCALAPPDATA "Argus"

    function Say([string]$text) { Write-Host "argus: $text" }

    # Anything Argus running from the install folder holds its files open, and
    # an engine that is stopped takes every agent session down with it. So
    # this never stops one: it says so and lets the user choose when.
    function Assert-NotRunning {
        $running = Get-Process -Name "Argus", "argus-engine", "argus-cli" -ErrorAction SilentlyContinue |
            Where-Object { $_.Path -and $_.Path.StartsWith($InstallDir, [StringComparison]::OrdinalIgnoreCase) }
        if (-not $running) { return }
        $names = ($running | ForEach-Object { "$($_.ProcessName) (pid $($_.Id))" }) -join ", "
        throw @"
Argus is running from $InstallDir`: $names.
Stopping the engine stops every agent session it owns, so this installer will not do it for you.
When your agents are at a good point, close the Argus window and run:
    Stop-Process -Name argus-engine
then run this again.
"@
    }

    # Only ever delete a folder this script made, so a mistyped -InstallDir
    # cannot wipe something else.
    function Remove-InstallDir {
        if (-not (Test-Path $InstallDir)) { return }
        $empty = -not (Get-ChildItem -Force $InstallDir | Select-Object -First 1)
        if (-not $empty -and -not (Test-Path $marker)) {
            throw "$InstallDir exists and was not made by this installer; refusing to replace it. Pick another -InstallDir, or remove it yourself."
        }
        Remove-Item -Recurse -Force $InstallDir
    }

    # The user PATH is edited in the registry, not through
    # [Environment]::SetEnvironmentVariable. That API reads `%USERPROFILE%\...`
    # entries already expanded and writes the value back as a plain string, so
    # every such entry would be frozen to today's expansion. Here the raw text
    # and its value kind are kept, and only this installer's own entry is
    # added or removed -- empty entries and a trailing `;` included.
    function Get-UserPath {
        $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey("Environment")
        try {
            $raw = $key.GetValue("Path", "", [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
            $kind = if ($null -ne $key.GetValue("Path")) { $key.GetValueKind("Path") } else { [Microsoft.Win32.RegistryValueKind]::ExpandString }
        } finally { $key.Close() }
        return @{ Entries = @($raw -split ";"); Kind = $kind }
    }

    function Set-UserPath($path) {
        $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey("Environment", $true)
        try { $key.SetValue("Path", ($path.Entries -join ";"), $path.Kind) } finally { $key.Close() }
        # Tell running programs -- Explorer above all, which starts the next
        # terminal -- that the environment changed.
        if (-not ("ArgusInstall.Native" -as [type])) {
            Add-Type -Namespace ArgusInstall -Name Native -MemberDefinition @"
[DllImport("user32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
public static extern IntPtr SendMessageTimeout(IntPtr hWnd, uint Msg, UIntPtr wParam, string lParam, uint fuFlags, uint uTimeout, out UIntPtr lpdwResult);
"@
        }
        $result = [UIntPtr]::Zero
        [void][ArgusInstall.Native]::SendMessageTimeout([IntPtr]0xffff, 0x1A, [UIntPtr]::Zero, "Environment", 2, 5000, [ref]$result)
    }

    function Test-SamePath([string]$a, [string]$b) {
        if (-not $a) { return $false }
        $a = [Environment]::ExpandEnvironmentVariables($a)
        return $a.TrimEnd("\").Equals($b.TrimEnd("\"), [StringComparison]::OrdinalIgnoreCase)
    }

    # ---- uninstall ---------------------------------------------------------
    if ($Uninstall) {
        Assert-NotRunning
        Remove-InstallDir
        if (Test-Path $shortcut) { Remove-Item -Force $shortcut }
        $path = Get-UserPath
        $kept = @($path.Entries | Where-Object { -not (Test-SamePath $_ $InstallDir) })
        if ($kept.Count -ne $path.Entries.Count) {
            $path.Entries = $kept
            Set-UserPath $path
        }
        if ($Purge) {
            if (Test-Path $stateDir) { Remove-Item -Recurse -Force $stateDir }
            Say "removed Argus, and its session history in $stateDir"
        } else {
            Say "removed Argus. Your session history is still in $stateDir (add -Purge to remove it too)."
        }
        return
    }

    # ---- fetch -------------------------------------------------------------
    # Windows PowerShell 5.1 still defaults to TLS 1.0, which GitHub refuses.
    [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
    # The progress bar makes Invoke-WebRequest many times slower on 5.1.
    $ProgressPreference = "SilentlyContinue"

    $work = Join-Path ([IO.Path]::GetTempPath()) ("argus-install-" + [Guid]::NewGuid().ToString("N"))
    New-Item -ItemType Directory -Force $work | Out-Null
    try {
        if ($FromZip) {
            $zip = (Resolve-Path $FromZip).Path
            $sums = Join-Path (Split-Path -Parent $zip) "SHA256SUMS"
            $label = Split-Path -Leaf $zip
        } else {
            $api = if ($Version -eq "latest") {
                "https://api.github.com/repos/$repo/releases/latest"
            } else {
                "https://api.github.com/repos/$repo/releases/tags/v$($Version.TrimStart('v'))"
            }
            Say "looking up $(if ($Version -eq 'latest') { 'the latest release' } else { "release $Version" })"
            try {
                $release = Invoke-RestMethod -Uri $api -Headers @{ "User-Agent" = "argus-installer" }
            } catch {
                $status = $_.Exception.Response.StatusCode.value__
                if ($status -eq 404) {
                    throw "no $(if ($Version -eq 'latest') { 'release' } else { "release $Version" }) is published at https://github.com/$repo/releases yet"
                }
                throw
            }
            $asset = $release.assets | Where-Object { $_.name -match $assetPattern } | Select-Object -First 1
            $sumAsset = $release.assets | Where-Object { $_.name -eq "SHA256SUMS" } | Select-Object -First 1
            if (-not $asset) { throw "release $($release.tag_name) has no Windows zip" }
            if (-not $sumAsset) { throw "release $($release.tag_name) has no SHA256SUMS to check the download against" }

            $zip = Join-Path $work $asset.name
            $sums = Join-Path $work "SHA256SUMS"
            $label = "$($asset.name) ($($release.tag_name))"
            Say "downloading $($asset.name)"
            Invoke-WebRequest -Uri $asset.browser_download_url -OutFile $zip -UseBasicParsing
            Invoke-WebRequest -Uri $sumAsset.browser_download_url -OutFile $sums -UseBasicParsing
        }

        # ---- verify --------------------------------------------------------
        $actual = (Get-FileHash -Algorithm SHA256 $zip).Hash.ToLowerInvariant()
        if (Test-Path $sums) {
            $name = Split-Path -Leaf $zip
            $line = Get-Content $sums | Where-Object { ($_ -split '\s+', 2)[1] -eq $name } | Select-Object -First 1
            if (-not $line) { throw "SHA256SUMS does not list $name" }
            $expected = ($line -split '\s+', 2)[0].ToLowerInvariant()
            if ($expected -ne $actual) {
                throw "checksum mismatch for $name`n  expected $expected`n  got      $actual`nNot installing it."
            }
            Say "checksum ok ($actual)"
        } elseif ($FromZip) {
            Say "warning: no SHA256SUMS beside $zip, so it was not checked"
        }

        # ---- install -------------------------------------------------------
        Assert-NotRunning
        $unpacked = Join-Path $work "unpacked"
        Expand-Archive -Path $zip -DestinationPath $unpacked
        foreach ($file in "Argus.exe", "argus-engine.exe", "argus-cli.exe", "manifests") {
            if (-not (Test-Path (Join-Path $unpacked $file))) { throw "$label is missing $file" }
        }
        Remove-InstallDir
        # Copied rather than moved: %TEMP% may be on another drive.
        New-Item -ItemType Directory -Force $InstallDir | Out-Null
        Set-Content -Path $marker -Value $label -Encoding ascii
        Copy-Item -Recurse -Path (Join-Path $unpacked "*") -Destination $InstallDir
    }
    finally {
        Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
    }

    if (-not $NoPath) {
        $path = Get-UserPath
        if (-not ($path.Entries | Where-Object { Test-SamePath $_ $InstallDir })) {
            # Into the last entry when it is empty -- a trailing `;` -- so the
            # value reads as if typed by hand, and uninstalling restores it.
            $entries = $path.Entries
            if ($entries.Count -gt 0 -and $entries[-1] -eq "") {
                $entries[-1] = $InstallDir
                $entries += ""
            } elseif ($entries.Count -eq 1 -and $entries[0] -eq "") {
                $entries = @($InstallDir)
            } else {
                $entries += $InstallDir
            }
            $path.Entries = $entries
            Set-UserPath $path
            Say "added $InstallDir to your PATH (new terminals will see it)"
        }
        if (-not (($env:Path -split ";") | Where-Object { $_ -and (Test-SamePath $_ $InstallDir) })) {
            $env:Path = "$env:Path;$InstallDir"
        }
    }

    if (-not $NoShortcut) {
        $shell = New-Object -ComObject WScript.Shell
        $link = $shell.CreateShortcut($shortcut)
        $link.TargetPath = Join-Path $InstallDir "Argus.exe"
        $link.WorkingDirectory = $env:USERPROFILE
        $link.Description = "Run coding agents side by side, and see which one needs you"
        $link.Save()
    }

    # The app's window is WebView2. Windows 11 ships it; some Windows 10
    # machines do not have it yet.
    $webview = "{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}"
    $hasWebView = @(
        "HKLM:\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\$webview",
        "HKLM:\SOFTWARE\Microsoft\EdgeUpdate\Clients\$webview",
        "HKCU:\SOFTWARE\Microsoft\EdgeUpdate\Clients\$webview"
    ) | Where-Object { Test-Path $_ }
    if (-not $hasWebView) {
        Say "warning: the Microsoft Edge WebView2 Runtime was not found; the Argus window needs it."
        Say "         get it from https://developer.microsoft.com/microsoft-edge/webview2/"
    }

    Say "installed $label into $InstallDir"
    Say "open Argus from the Start menu, or run: argus-cli --help"
}

Install-Argus @args
