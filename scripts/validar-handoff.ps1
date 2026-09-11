# Valida un YAML de handoff Swarm Forge. Exit 0 = ok, 1 = malformado.
param(
  [Parameter(Mandatory = $true)]
  [string]$Path
)

$ErrorActionPreference = 'Stop'
$roles = @(
  'especificador', 'implementador', 'mejorador',
  'arquitecto', 'endurecedor', 'qa'
)
$required = @('from', 'to', 'type', 'task', 'commit', 'evidence')

if (-not (Test-Path -LiteralPath $Path)) {
  Write-Error "no existe: $Path"
  exit 1
}

$map = @{}
Get-Content -LiteralPath $Path | ForEach-Object {
  if ($_ -match '^\s*#' -or $_ -match '^\s*$') { return }
  if ($_ -match '^(from|to|type|task|commit|evidence)\s*:\s*(.*)$') {
    $map[$Matches[1]] = $Matches[2].Trim().Trim('"').Trim("'")
  }
}

foreach ($k in $required) {
  if (-not $map.ContainsKey($k) -or [string]::IsNullOrWhiteSpace($map[$k])) {
    Write-Host "FALTA campo: $k"
    exit 1
  }
}

if ($map['from'] -notin $roles) { Write-Host "from invalido: $($map['from'])"; exit 1 }
if ($map['to'] -notin $roles) { Write-Host "to invalido: $($map['to'])"; exit 1 }
if ($map['type'] -notin @('git_handoff', 'note')) { Write-Host "type invalido"; exit 1 }

$commit = $map['commit']
if ($map['type'] -eq 'note') {
  if ($commit -ne 'none' -and $commit -notmatch '^[0-9a-f]{7,40}$') {
    Write-Host "commit invalido para note"
    exit 1
  }
} else {
  if ($commit -ne 'none' -and $commit -notmatch '^[0-9a-f]{7,40}$') {
    Write-Host "commit invalido: $commit"
    exit 1
  }
}

Write-Host "OK $Path $($map['from'])->$($map['to'])"
exit 0
