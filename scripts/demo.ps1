<#
.SYNOPSIS
    A narrated tour of what Argus can do today, using the real binaries.

.DESCRIPTION
    Builds argus-engine and argus-cli, starts a private engine, and walks through:

      1. three shells at once -- one idle, one busy, one asking a question
      2. reading the blocked one's screen and answering it from the CLI
      3. two agents in one repository, each in its own git worktree
      4. crashing the engine and restarting it: the list survives
      5. stopping it on purpose: finished, not lost

    Everything runs against a private pipe and a scratch state file in %TEMP%,
    so it never touches your real Argus state or a running engine.

    -WithClaude adds two acts driving a real Claude Code session: one that
    approves a file write, and one that crashes the engine mid-conversation and
    resumes the same conversation. That sends two small prompts on your Claude
    account, and needs `claude` to be installed and logged in (`claude /login`).

.EXAMPLE
    .\scripts\demo.ps1

.EXAMPLE
    .\scripts\demo.ps1 -WithClaude
#>
param(
    [switch]$WithClaude,
    # Seconds to pause between acts, so the output can be followed live.
    [double]$Pace = 1.5
)

# Not "Stop": in Windows PowerShell 5.1 that turns any native command's stderr
# into a terminating error, and this script deliberately runs commands that are
# expected to fail (polling an engine that is still starting, typing into a lost
# session). Failures that matter are thrown explicitly instead.
$repo = Split-Path -Parent $PSScriptRoot
Set-Location $repo

# ---- helpers ---------------------------------------------------------------

function Act([string]$title) {
    Start-Sleep -Seconds $Pace
    Write-Host ""
    Write-Host ("=" * 78) -ForegroundColor DarkCyan
    Write-Host "  $title" -ForegroundColor Cyan
    Write-Host ("=" * 78) -ForegroundColor DarkCyan
}

function Say([string]$text) { Write-Host "  $text" -ForegroundColor DarkGray }

# Echo a command the way a person would type it, then run it.
function Show-Argus {
    Write-Host ""
    Write-Host "PS> argus-cli $($args -join ' ')" -ForegroundColor Yellow
    # 2>&1 and "$_" so an error prints as its message, the way it would in a
    # terminal, rather than wrapped in PowerShell's NativeCommandError report.
    & $script:cli --pipe $script:pipe @args 2>&1 | ForEach-Object { Write-Host "$_" }
}

# Run the CLI silently and return its parsed --json reply.
function CliJson { & $script:cli --pipe $script:pipe --json @args | ConvertFrom-Json }

function Wait-Status([string]$id, [string[]]$want, [int]$seconds = 30) {
    $deadline = (Get-Date).AddSeconds($seconds)
    while ((Get-Date) -lt $deadline) {
        $info = (CliJson list).sessions | Where-Object { $_.id -eq $id }
        if ($info -and $want -contains $info.status) { return $info }
        Start-Sleep -Milliseconds 200
    }
    throw "session $id did not reach $($want -join '/') within ${seconds}s"
}

function Start-Engine {
    $p = Start-Process -FilePath $script:engineExe `
        -ArgumentList "--pipe", $script:pipe, "--state", "`"$script:state`"" `
        -NoNewWindow -PassThru -RedirectStandardError $script:engineLog -RedirectStandardOutput "$script:scratch\engine-out.txt"
    $null = $p.Handle  # keep the handle so the exit code stays readable
    for ($i = 0; $i -lt 100; $i++) {
        & $script:cli --pipe $script:pipe list *> $null
        if ($LASTEXITCODE -eq 0) { return $p }
        if ($p.HasExited) { break }
        Start-Sleep -Milliseconds 100
    }
    Get-Content $script:engineLog -ErrorAction SilentlyContinue | ForEach-Object { Write-Host $_ }
    # Kill it before giving up. The caller's cleanup only knows about an engine
    # this function returned, so one that started but never answered would
    # otherwise be left running -- holding the binary locked, which then breaks
    # the next `cargo build`. Found exactly that way.
    if (-not $p.HasExited) { Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue }
    throw "the engine did not start"
}

function Show-EngineLog {
    Get-Content $script:engineLog | ForEach-Object { Write-Host "  $_" -ForegroundColor DarkGreen }
}

# ---- setup -----------------------------------------------------------------

Act "Building argus-engine and argus-cli"
cargo build -q -p argus-engine -p argus-cli
if ($LASTEXITCODE -ne 0) { throw "build failed" }
$engineExe = Join-Path $repo "target\debug\argus-engine.exe"
$cli       = Join-Path $repo "target\debug\argus-cli.exe"
$scratch   = Join-Path $env:TEMP "argus-demo-$PID"
$state     = Join-Path $scratch "state.json"
$engineLog = Join-Path $scratch "engine.log"
$pipe      = "argus-demo-$PID"
New-Item -ItemType Directory -Force $scratch | Out-Null
Say "scratch state: $state"
Say "private pipe:  $pipe"

$engine = $null
try {
    # ---- act 1 -------------------------------------------------------------
    Act "1. Start the engine: a daemon that owns every agent session"
    $engine = Start-Engine
    Show-EngineLog

    Act "2. Three shells at once"
    Say "Each one runs in its own real Windows pseudo-console (ConPTY)."
    $idle  = ((& $cli --pipe $pipe spawn shell) -split '\s+')[0]
    $busy  = ((& $cli --pipe $pipe spawn shell) -split '\s+')[0]
    $asker = ((& $cli --pipe $pipe spawn shell) -split '\s+')[0]
    foreach ($id in $idle, $busy, $asker) { Wait-Status $id "idle" | Out-Null }
    Show-Argus list

    Act "3. Give them something to do"
    Say "One gets a long command. One gets asked a question it must wait on."
    Show-Argus send $busy  "Start-Sleep 600"
    Show-Argus send $asker "Read-Host 'Deploy to production. Are you sure (y/n)'"
    Wait-Status $busy "working" | Out-Null
    Wait-Status $asker "needs_you" | Out-Null
    Show-Argus list
    Say "The blocked shell is at the top: that is the window you should look at."

    Act "4. See exactly what it is asking"
    Say "Not a log -- the emulated screen, exactly as you would see it."
    Show-Argus screen $asker.Substring(0, 4)

    Act "5. Answer it without switching windows"
    Show-Argus send $asker.Substring(0, 4) "n"
    Wait-Status $asker "idle" | Out-Null
    Show-Argus list
    Say "Answered, so it dropped back to idle and down the list."

    Say "Now let that shell finish for good."
    Show-Argus send $asker.Substring(0, 4) "exit"
    Wait-Status $asker "done" | Out-Null
    Show-Argus list

    # ---- act 2: worktrees ----------------------------------------------------
    Act "6. Two agents in one repository, without collisions"
    Say "Agents that share a checkout overwrite each other. Give each one a"
    Say "branch and the engine puts it in a git worktree of its own."
    $repo = Join-Path $scratch "shop-api"
    New-Item -ItemType Directory -Force $repo | Out-Null
    git -C $repo init -q -b main
    Set-Content (Join-Path $repo "README.md") "shop-api"
    git -C $repo add . *> $null
    git -C $repo -c user.name="Argus Demo" -c user.email="demo@argus.invalid" commit -q -m "first"
    Say "Repository: $repo (throwaway, one commit on main)"

    Show-Argus spawn shell --cwd $repo --worktree feature/login
    Show-Argus spawn shell --cwd $repo --worktree fix/cart-total
    foreach ($id in (CliJson list).sessions | Where-Object { $_.branch }) {
        Wait-Status $id.id "idle" 60 | Out-Null
    }
    Show-Argus list
    Say "A BRANCH column appears as soon as any session has a worktree."

    Say "Each one is a real checkout of the same repository, on its own branch:"
    git -C $repo worktree list | ForEach-Object { Write-Host "  $_" -ForegroundColor DarkGreen }
    Say "The main checkout is untouched -- still on $(git -C $repo branch --show-current)."
    Say "Worktrees outlive their sessions: killing an agent keeps what it wrote."

    # ---- act 3: crash ------------------------------------------------------
    Act "7. Crash the engine"
    Say "A hard kill -- no chance to save or clean up, as in a real crash."
    Stop-Process -Id $engine.Id -Force
    $engine.WaitForExit()
    Say "Engine gone. Every shell still running inside it died with it --"
    Say "including the idle one, which was still a live process."
    Say "What it had already written to disk:"
    Get-Content $state | ForEach-Object { Write-Host "  $_" -ForegroundColor DarkGreen }

    Act "8. Start it again"
    $engine = Start-Engine
    Show-EngineLog
    Show-Argus list
    Say "Same ids, worktree branches included. The shell that exited before the"
    Say "crash is still 'done (0)'. Everything that was running is 'lost', not"
    Say "'done': those processes went down with the engine, so they never finished."

    Act "9. A lost session cannot be typed into"
    Show-Argus send $busy.Substring(0, 4) "dir"

    # ---- optional: a real agent --------------------------------------------
    if ($WithClaude) {
        Act "10. A real agent: Claude Code"
        $project = Join-Path $scratch "claude-project"
        New-Item -ItemType Directory -Force $project | Out-Null
        Say "Working folder: $project (throwaway)"
        $claude = ((& $cli --pipe $pipe spawn claude-code --cwd $project) -split '\s+')[0]
        Wait-Status $claude "idle" 60 | Out-Null
        Show-Argus screen $claude.Substring(0, 4)

        Say "Asking it to create a file, which normally needs your permission."
        Show-Argus send $claude.Substring(0, 4) "Create a file named demo.txt containing the word hello. Do nothing else."

        $seen = @(); $approved = $false
        $deadline = (Get-Date).AddSeconds(120)
        while ((Get-Date) -lt $deadline) {
            $info = (CliJson list).sessions | Where-Object { $_.id -eq $claude }
            $now = "$($info.status) ($($info.rule))"
            if ($seen[-1] -ne $now) { Say "status -> $now"; $seen += $now }
            if (-not $approved -and $info.status -eq "needs_you") {
                Show-Argus screen $claude.Substring(0, 4)
                Say "It is asking. Approving from the CLI:"
                Show-Argus send $claude.Substring(0, 4) --key enter
                $approved = $true
            }
            if ($info.status -eq "idle" -and $seen.Count -gt 1) { break }
            Start-Sleep -Milliseconds 300
        }
        Show-Argus screen $claude.Substring(0, 4)
        if (Test-Path (Join-Path $project "demo.txt")) {
            Say "demo.txt was created: $(Get-Content (Join-Path $project 'demo.txt'))"
        } else {
            Say "demo.txt was not created -- see the screen above for why."
        }

        Act "11. Crash with Claude mid-conversation, then resume it"
        $uuid = ((Get-Content $state | ConvertFrom-Json).sessions | Where-Object { $_.id -eq $claude }).agent_session
        Say "Argus started it with its own conversation id: $uuid"
        Stop-Process -Id $engine.Id -Force
        $engine.WaitForExit()
        $engine = Start-Engine
        Show-Argus list
        Say "Lost, and marked *: its manifest says how to resume, and the engine"
        Say "kept the folder and the conversation id it needs."
        Show-Argus resume $claude.Substring(0, 4)
        Wait-Status $claude "idle" 60 | Out-Null
        Say "Same id, same folder, same conversation. Asking it what it remembers:"
        Show-Argus send $claude.Substring(0, 4) "What is the name of the file you created earlier? Reply with the file name only."
        Wait-Status $claude "working" 30 | Out-Null
        Wait-Status $claude "idle" 120 | Out-Null
        Show-Argus screen $claude.Substring(0, 4)
    }

    # ---- last act: stopping on purpose ---------------------------------------
    Act "$(if ($WithClaude) { 12 } else { 10 }). Stop the engine on purpose"
    Say "A crash loses whatever was running. A deliberate stop does not."
    Say "One more shell, so there is something live to stop:"
    $last = ((& $cli --pipe $pipe spawn shell) -split '\s+')[0]
    Wait-Status $last "idle" | Out-Null
    Show-Argus shutdown
    $engine.WaitForExit(10000) | Out-Null
    Say "Engine exited: $($engine.HasExited). What it left on disk:"
    (Get-Content $state | ConvertFrom-Json).sessions | ForEach-Object {
        $mark = if ($_.id -eq $last) { "  <- stopped by the shutdown" } else { "" }
        $what = if ($_.lost) { "lost" } else { "done" }
        Write-Host ("  {0}  {1}{2}" -f $_.id, $what, $mark) -ForegroundColor DarkGreen
    }
    Say "The ones the crash took are still 'lost' -- that is history, and it stays."
    Say "The shell that was running just now is 'done': stopped first, then recorded."

    Act "Done"
    Say "Run it yourself any time: .\scripts\demo.ps1"
    Say "Add -WithClaude for two more acts driving a real Claude Code session."
}
finally {
    if ($engine -and -not $engine.HasExited) {
        foreach ($s in (CliJson list).sessions) { & $cli --pipe $pipe kill $s.id *> $null }
        Start-Sleep -Milliseconds 300
        Stop-Process -Id $engine.Id -Force -ErrorAction SilentlyContinue
    }
    Remove-Item -Recurse -Force $scratch -ErrorAction SilentlyContinue
}
