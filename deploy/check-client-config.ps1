# Validate a Claude Code / Desktop Claude JSON config after editing it.
#
# WHY THIS EXISTS (korg #931). Wiring cleo to the fleet in sprint 004, an agent
# rewrote ~/.claude.json from PowerShell with `Set-Content -Encoding UTF8`.
# On Windows PowerShell 5.1 that writes a UTF-8 BOM and CRLF endings.
# JSON.parse rejects a leading BOM, so the client could not read its own
# config, moved it aside to .claude.json.backup, and regenerated a default --
# silently dropping EVERY configured MCP server, not just the one being added.
# Nothing warned at write time; the client reported "no MCP servers configured"
# rather than "your config is corrupt".
#
# THE RULES, in order of preference:
#   1. Do not hand-write this file. `claude mcp add` owns its format.
#   2. If you must write it directly, use
#        [IO.File]::WriteAllText($p, $json, (New-Object System.Text.UTF8Encoding $false))
#      -- NOT Set-Content. On PS 5.1, `-Encoding UTF8` means WITH BOM, and the
#      bare default means ANSI. WriteAllText with an explicit UTF8Encoding($false)
#      behaves the same on 5.1 and 7.
#   3. Normalize CRLF to LF: $json = $json -replace "`r`n", "`n"
#   4. Run this script afterwards. A write you did not verify is a write you
#      did not make.
#
# Usage:
#   .\check-client-config.ps1
#   .\check-client-config.ps1 -Expect klams,korg,kaed-kai,kaed-kubs0
#   .\check-client-config.ps1 -Path D:\some\claude_desktop_config.json
#
# Exits 0 if every check passes, 1 otherwise. Prints server names and URLs,
# never header values.

param(
    [string]   $Path   = (Join-Path $env:USERPROFILE '.claude.json'),
    [string[]] $Expect = @()
)

# Invoked via `powershell -File`, every argument arrives as a string, so
# `-Expect a,b,c` binds as ONE element "a,b,c" rather than three. Split it
# ourselves so both that and a real PowerShell array work.
$Expect = @($Expect | ForEach-Object { $_ -split ',' } | Where-Object { $_ -ne '' })

$ok = $true
function Pass($m) { Write-Output "PASS: $m" }
function Fail($m) { Write-Output "FAIL: $m"; $script:ok = $false }

if (-not (Test-Path $Path)) { Fail "no such file: $Path"; exit 1 }
Write-Output "checking $Path"

# --- byte level -----------------------------------------------------------

$bytes = [IO.File]::ReadAllBytes($Path)
if ($bytes.Length -eq 0) { Fail "file is empty"; exit 1 }

if ($bytes.Length -ge 3 -and $bytes[0] -eq 0xEF -and $bytes[1] -eq 0xBB -and $bytes[2] -eq 0xBF) {
    Fail "UTF-8 BOM present -- JSON.parse will reject this and the client will discard the file"
} else {
    Pass "no BOM"
}

$text = [IO.File]::ReadAllText($Path)
$crlf = ([regex]::Matches($text, "`r`n")).Count
if ($crlf -gt 0) {
    # Not fatal to a parser, but it means a non-native writer touched the file,
    # which is the same smell that produced the BOM.
    Write-Output "WARN: $crlf CRLF sequences (the client writes LF; something else wrote this)"
} else {
    Pass "LF line endings"
}

# --- parse level ----------------------------------------------------------

$j = $null
try {
    $j = $text | ConvertFrom-Json
    Pass "parses as JSON"
} catch {
    Fail ("does not parse: {0}" -f $_.Exception.Message)
    exit 1
}

# --- content level --------------------------------------------------------

if (-not $j.PSObject.Properties.Name.Contains('mcpServers')) {
    Fail "no mcpServers key -- if you expected servers here, the client has already regenerated this file"
} else {
    $got = @($j.mcpServers.PSObject.Properties.Name)
    if ($got.Count -eq 0) {
        Fail "mcpServers is empty"
    } else {
        Pass ("mcpServers: {0}" -f ($got -join ', '))
    }
    foreach ($w in $Expect) {
        if ($got -contains $w) { Pass "expected server present: $w" }
        else { Fail "expected server MISSING: $w" }
    }
    foreach ($n in $got) {
        $e = $j.mcpServers.$n
        if (-not $e.url -and -not $e.command) { Fail "$n has neither url nor command" }
        $auth = if ($e.headers.Authorization) { 'has-auth' } else { 'no-auth' }
        Write-Output ("  {0,-14} {1,-6} {2}  [{3}]" -f $n, $e.type, $e.url, $auth)
    }
}

if ($ok) { Write-Output "OK"; exit 0 } else { Write-Output "CONFIG IS BROKEN"; exit 1 }
