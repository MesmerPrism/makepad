$ErrorActionPreference = "Stop"

function Invoke-Checked {
    param(
        [Parameter(Mandatory=$true)]
        [string]$Name,
        [Parameter(Mandatory=$true)]
        [string]$File,
        [string[]]$Arguments = @()
    )

    & $File @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "$Name failed with exit code $LASTEXITCODE"
    }
}

$RepoRoot = Resolve-Path (Join-Path $PSScriptRoot "..")
Push-Location $RepoRoot
try {
    if (Test-Path "tools\rusty_xr_format.py") {
        Invoke-Checked "changed-file format" "python" @("tools\rusty_xr_format.py", "--changed", "--check")
    }
    Invoke-Checked "git whitespace check" "git" @("diff", "--check")
    Invoke-Checked "Makepad widgets check" "cargo" @("check", "-p", "makepad-widgets")
} finally {
    Pop-Location
}
