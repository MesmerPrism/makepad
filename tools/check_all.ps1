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
    if (Test-Path "tools\makepad_fork_format.py") {
        Invoke-Checked "changed-file format" "python" @("tools\makepad_fork_format.py", "--changed", "--check")
    }
    if (Test-Path "tools\check_morphospace_makepad_guards.py") {
        Invoke-Checked "Morphospace Makepad guardrails" "python" @("tools\check_morphospace_makepad_guards.py")
    }
    if (Test-Path "tools\check_android_generated_output_stability.py") {
        Invoke-Checked "Android generated-output stability" "python" @("tools\check_android_generated_output_stability.py")
    }
    Invoke-Checked "git whitespace check" "git" @("diff", "--check")
    Invoke-Checked "Makepad widgets check" "cargo" @("check", "-p", "makepad-widgets")
} finally {
    Pop-Location
}
