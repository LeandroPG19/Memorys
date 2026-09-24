# Handoff judge (Windows): a wrapper, deliberately. Same cure as
# quality-gate.ps1, for the same drift.
#
# This used to be a port of validar-handoff.sh, and it stayed at shape while
# the .sh learned the two-pass protocol in 0.28: a commit that has to exist,
# `tests: written|frozen` compared against the red commit, and `paths:`. A
# handoff the .sh refuses for editing a frozen test went through here as OK,
# on Windows, which is where the orchestrator runs.
#
# One judge, one implementation. Git Bash is resolved explicitly because the
# `bash` on PATH under Windows is WSL's, which cannot translate a D:\ working
# directory and exits without reading the script. Backslashes become slashes
# so Git Bash reads a Windows path the way PowerShell completed it.
#
#   .\scripts\validar-handoff.ps1 .cursor\handoffs\<stamp>.yml
#   .\scripts\validar-handoff.ps1 --self-test
param(
  [string]$Path,
  [Parameter(ValueFromRemainingArguments = $true)] [string[]]$Rest
)

$ErrorActionPreference = 'Continue'

$bash = @(
  'C:/Program Files/Git/bin/bash.exe',
  'C:/Program Files (x86)/Git/bin/bash.exe'
) | Where-Object { Test-Path $_ } | Select-Object -First 1

if (-not $bash) {
  Write-Host 'FALTA Git Bash. El juez de handoffs de este repo es un script de shell:'
  Write-Host '  instala Git for Windows, o corre  bash scripts/validar-handoff.sh <fichero.yml>  en un entorno POSIX.'
  Write-Host 'No se afirma NADA sobre el handoff.'
  exit 2
}

$judge = (Join-Path $PSScriptRoot 'validar-handoff.sh') -replace '\\', '/'
$handed = @(@($Path) + @($Rest) | Where-Object { $_ } | ForEach-Object { $_ -replace '\\', '/' })

& $bash $judge @handed
exit $LASTEXITCODE
