#!/usr/bin/env pwsh
# rtk-hook-version: 3
# RTK Claude Code hook (PowerShell) — rewrites commands to use rtk for token savings.
# Requires: rtk >= 0.23.0
#
# This is a thin delegating hook: all rewrite logic lives in `rtk rewrite`,
# which is the single source of truth (src/discover/registry.rs).
# To add or change rewrite rules, edit the Rust registry — not this file.
#
# Exit code protocol for `rtk rewrite`:
#   0 + stdout  Rewrite found, no deny/ask rule matched → auto-allow
#   1           No RTK equivalent → pass through unchanged
#   2           Deny rule matched → pass through (Claude Code native deny handles it)
#   3 + stdout  Ask rule matched → rewrite but let Claude Code prompt the user

param()

# Check if rtk is available
if (-not (Get-Command rtk -ErrorAction SilentlyContinue)) {
    [Console]::Error.WriteLine("[rtk] WARNING: rtk is not installed or not in PATH. Hook cannot rewrite commands. Install: https://github.com/rtk-ai/rtk#installation")
    exit 0
}

# Version guard: rtk rewrite was added in 0.23.0.
# Cache the version check to avoid spawning extra processes on every hook call.
$CacheDir = if ($env:LOCALAPPDATA) {
    Join-Path $env:LOCALAPPDATA "rtk"
} else {
    Join-Path $HOME ".cache" "rtk"
}
$CacheFile = Join-Path $CacheDir "hook-version-ok"

if (-not (Test-Path $CacheFile)) {
    $RtkVersionRaw = & rtk --version 2>$null
    if ($RtkVersionRaw -match 'rtk (\d+)\.(\d+)\.(\d+)') {
        $Major = [int]$Matches[1]
        $Minor = [int]$Matches[2]
        # Require >= 0.23.0
        if ($Major -eq 0 -and $Minor -lt 23) {
            [Console]::Error.WriteLine("[rtk] WARNING: rtk $RtkVersionRaw is too old (need >= 0.23.0). Upgrade: cargo install rtk")
            exit 0
        }
    }
    New-Item -ItemType Directory -Path $CacheDir -Force | Out-Null
    New-Item -ItemType File -Path $CacheFile -Force | Out-Null
}

# Read stdin (the JSON hook payload from Claude Code)
$StdinContent = [Console]::In.ReadToEnd()

try {
    $HookInput = $StdinContent | ConvertFrom-Json -ErrorAction Stop
} catch {
    exit 0
}

$CMD = $HookInput.tool_input.command
if ([string]::IsNullOrEmpty($CMD)) {
    exit 0
}

# Rewrite bare `ls` and `ls <args>` to `Get-ChildItem` before any other logic.
if ($CMD -eq 'ls' -or $CMD -match '^ls\s+') {
    $CMD = $CMD -replace '^ls\b', 'Get-ChildItem'
    $HookInput.tool_input.command = $CMD
}

# Delegate all rewrite + permission logic to the Rust binary.
$REWRITTEN = (& rtk rewrite "$CMD" 2>$null) -join "`n"
$ExitCode = $LASTEXITCODE

switch ($ExitCode) {
    0 {
        # Rewrite found, no permission rules matched — safe to auto-allow.
        # If the output is identical, the command was already using RTK.
        if ($CMD -eq $REWRITTEN) { exit 0 }
    }
    1 {
        # No RTK equivalent — pass through unchanged.
        exit 0
    }
    2 {
        # Deny rule matched — let Claude Code's native deny rule handle it.
        exit 0
    }
    3 {
        # Ask rule matched — rewrite the command but do NOT auto-allow so that
        # Claude Code prompts the user for confirmation.
        # (fall through to output)
    }
    default {
        exit 0
    }
}

# Mutate the parsed input with the rewritten command
$HookInput.tool_input.command = $REWRITTEN

if ($ExitCode -eq 3) {
    # Ask: rewrite the command, omit permissionDecision so Claude Code prompts.
    $OutputObj = [PSCustomObject]@{
        hookSpecificOutput = [PSCustomObject]@{
            hookEventName = "PreToolUse"
            updatedInput  = $HookInput.tool_input
        }
    }
} else {
    # Allow: rewrite the command and auto-allow.
    $OutputObj = [PSCustomObject]@{
        hookSpecificOutput = [PSCustomObject]@{
            hookEventName            = "PreToolUse"
            permissionDecision       = "allow"
            permissionDecisionReason = "RTK auto-rewrite"
            updatedInput             = $HookInput.tool_input
        }
    }
}

$OutputObj | ConvertTo-Json -Depth 10 -Compress
