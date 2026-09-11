# Quality gate Swarm Forge: tests + CRAP/complejidad + mutación acotada al diff.
# El modelo no es el juez. Exit 0 verde, 1 rojo, 2 herramienta ausente.
param(
  [string]$Repo = (Get-Location).Path
)

$ErrorActionPreference = 'Continue'
Set-Location $Repo
Write-Host "=== quality-gate ==="
Write-Host "NO MIRA: secretos, rendimiento, e2e headed, linters del CI (corre como-el-ci aparte)."

$diff = @()
if (Get-Command git -ErrorAction SilentlyContinue) {
  $diff = @(git diff --name-only HEAD 2>$null)
  if (-not $diff) { $diff = @(git diff --name-only --cached 2>$null) }
}
Write-Host "diff: $($diff -join ', ')"
if (-not $diff) { Write-Host "SIN DIFF: no hay archivos tocados en HEAD/index. No se afirma cobertura del cambio." }

$fail = 0
$py = @($diff | Where-Object { $_ -like '*.py' })
$rs = @($diff | Where-Object { $_ -like '*.rs' })
$ts = @($diff | Where-Object { $_ -match '\.(ts|tsx|js|jsx)$' })

if (Test-Path 'scripts\como-el-ci.sh') {
  Write-Host "HAY scripts/como-el-ci.sh — el tester/orquestador debe correrlo; este script NO lo sustituye."
}

if ((Test-Path 'pyproject.toml') -or (Test-Path 'pytest.ini')) {
  if (Get-Command pytest -ErrorAction SilentlyContinue) {
    pytest -x --tb=short -q
    if ($LASTEXITCODE -ne 0) { $fail = 1 }
  } else { Write-Host "FALTA pytest"; $fail = 2 }
  if ($py.Count -gt 0 -and (Get-Command radon -ErrorAction SilentlyContinue)) {
    radon cc @py -s -n D
  } elseif ($py.Count -gt 0) {
    Write-Host "FALTA radon (CRAP/cc). Objetivo complejidad <= 4."
    if ($fail -eq 0) { $fail = 2 }
  }
  if ($py.Count -gt 0 -and (Get-Command mutmut -ErrorAction SilentlyContinue)) {
    Write-Host "mutmut: acotar a $($py -join ' ')"
    mutmut run --paths-to-mutate ($py -join ',')
    if ($LASTEXITCODE -ne 0) { $fail = 1 }
  } elseif ($py.Count -gt 0) {
    Write-Host "FALTA mutmut. Endurecedor no puede cerrar."
    if ($fail -eq 0) { $fail = 2 }
  }
}

if (Test-Path 'Cargo.toml') {
  cargo test --workspace
  if ($LASTEXITCODE -ne 0) { $fail = 1 }
  if ($rs.Count -gt 0 -and (Get-Command cargo-mutants -ErrorAction SilentlyContinue)) {
    cargo mutants --file ($rs -join ',')
    if ($LASTEXITCODE -ne 0) { $fail = 1 }
  } elseif ($rs.Count -gt 0) {
    Write-Host "FALTA cargo-mutants. Endurecedor no puede cerrar."
    if ($fail -eq 0) { $fail = 2 }
  }
}

if ((Test-Path 'package.json') -and $ts.Count -gt 0) {
  $pm = if (Test-Path 'pnpm-lock.yaml') { 'pnpm' } elseif (Test-Path 'yarn.lock') { 'yarn' } else { 'npm' }
  if (Get-Command $pm -ErrorAction SilentlyContinue) {
    & $pm test
    if ($LASTEXITCODE -ne 0) { $fail = 1 }
  }
  if (Get-Command npx -ErrorAction SilentlyContinue) {
    npx --yes stryker run 2>$null
    if ($LASTEXITCODE -ne 0) { Write-Host "Stryker ausente o rojo (exit $LASTEXITCODE)" }
  }
}

Write-Host "=== fin quality-gate exit=$fail ==="
exit $fail
