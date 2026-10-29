# argus-agents

Run several coding agents side by side on Windows, each in its own real
terminal and, if you like, its own git worktree. Argus watches every screen and
tells you which agent is **working**, which **needs you**, which is **idle**,
and which is **done**, so none of them sits on a permission prompt you missed.

Claude Code and PowerShell work out of the box; any other terminal agent is one
JSON manifest away.

## Install

Windows 10 or 11, x64, Node 18 or later:

```powershell
npm install -g argus-agents
```

Installing downloads the matching release zip from
[GitHub](https://github.com/hashimhashmi02/Argus/releases), checks it against
that release's `SHA256SUMS`, and unpacks it inside the package. Nothing is
written anywhere else. If npm skips install scripts (`--ignore-scripts`, or an
npm that asks you to allow them), the same download happens the first time you
run `argus`, `argus-cli` or `argus-engine`.

## Use

```powershell
argus
```

opens the desktop app. If no engine is running, the app offers to start one.
The engine runs in the background, so closing the window never stops an agent.

From a terminal:

```powershell
argus-cli spawn claude-code --cwd C:\src\shop-api --worktree feature/login
```

```powershell
argus-cli list
```

```text
ID        AGENT        BRANCH         STATUS     FOR  PID    RULE
3fa1c2d4  claude-code  feature/login  NEEDS YOU  12s  17776  permission-prompt
9b0e1172  claude-code  fix/cart       working    40s  28056  busy
```

```powershell
argus-cli approve 3f
```

`argus-cli --help` lists the rest: `screen`, `send`, `watch`, `kill`,
`resume`, `shutdown`.

## Upgrade or uninstall

Stop the engine first. Windows won't replace a program that's running:

```powershell
argus-cli shutdown
```

```powershell
npm install -g argus-agents@latest
```

```powershell
npm uninstall -g argus-agents
```

Session history lives in `%LOCALAPPDATA%\Argus` and is kept either way.

## More

Keys, worktrees, resuming and how it works:
[github.com/hashimhashmi02/Argus](https://github.com/hashimhashmi02/Argus).

Dual-licensed under MIT or Apache 2.0.
