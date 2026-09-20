# Second judge (Windows): a wrapper, deliberately.
#
# This used to be a second implementation of quality-gate.sh, and the two
# drifted exactly where nobody was looking: the .sh grew a contract and this
# one did not, so its lizard branch never checked $LASTEXITCODE and its CRAP
# gate could not fail at all. The .sh then gained a complexity baseline and a
# QG_BASE ref, and reproducing either here would just reopen the gap.
#
# One judge, one implementation. Git Bash is resolved explicitly because the
# `bash` on PATH under Windows is WSL's, which cannot translate a D:\ working
# directory and exits without reading the script.
param(
  [string]$Repo = (Get-Location).Path,
  [Parameter(ValueFromRemainingArguments = $true)] [string[]]$Rest
)

$ErrorActionPreference = 'Continue'
Set-Location $Repo

$bash = @(
  'C:/Program Files/Git/bin/bash.exe',
  'C:/Program Files (x86)/Git/bin/bash.exe'
) | Where-Object { Test-Path $_ } | Select-Object -First 1

if (-not $bash) {
  Write-Host 'FALTA Git Bash. El juez de este repo es un script de shell:'
  Write-Host '  instala Git for Windows, o corre  bash scripts/quality-gate.sh  en un entorno POSIX.'
  Write-Host 'No se afirma NADA sobre el cambio.'
  exit 2
}

& $bash 'scripts/quality-gate.sh' @Rest
exit $LASTEXITCODE
