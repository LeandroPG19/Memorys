# Second judge (Windows): CRAP/complexity + mutation of the rust/src diff.
# SIL = .\scripts\merge-gate.sh via Git Bash. This script does not run it.
param(
  [string]$Repo = (Get-Location).Path
)

$ErrorActionPreference = 'Continue'
Set-Location $Repo
Write-Host '=== quality-gate (MemoryIndustry - CRAP/lizard + mutacion del diff) ==='
Write-Host 'NO MIRA: SIL (fmt, clippy -D, tests --ignored, e2e, deny, audit, codigo-muerto, crap-gate floor, mutants-gate mmr/rrf/cache).'
Write-Host 'NO CORRE: ./scripts/merge-gate.sh'
Write-Host 'SIL: ./scripts/merge-gate.sh  (alias: ./scripts/como-el-ci.sh todo)'
Write-Host 'HAY scripts/merge-gate.sh - este script NO lo sustituye.'

$diff = @()
if (Get-Command git -ErrorAction SilentlyContinue) {
  $diff = @(git diff --name-only HEAD 2>$null)
  if (-not $diff) { $diff = @(git diff --name-only --cached 2>$null) }
}
Write-Host "diff: $($diff -join ', ')"
if (-not $diff) { Write-Host 'SIN DIFF: no hay archivos tocados en HEAD/index. No se afirma cobertura del cambio.' }

$fail = 0
$rs = @($diff | Where-Object { $_ -like '*.rs' })
$py = @($diff | Where-Object { $_ -like '*.py' })
$src = @()
foreach ($f in $rs) {
  $n = $f -replace '\\', '/'
  if ($n -like 'rust/src/*') { $src += ($n -replace '^rust/', '') }
}

if (-not (Test-Path 'rust/Cargo.toml')) {
  if ($src.Count -gt 0) { Write-Host 'FALTA rust/Cargo.toml'; $fail = 1 }
} elseif ($src.Count -gt 0) {
  if (Get-Command lizard -ErrorAction SilentlyContinue) {
    Push-Location rust
    lizard @src
    Pop-Location
  } elseif (Get-Command python -ErrorAction SilentlyContinue) {
    Push-Location rust
    python -m lizard @src
    if ($LASTEXITCODE -ne 0 -and $fail -eq 0) { $fail = 2 }
    Pop-Location
  } else {
    Write-Host 'FALTA lizard (CRAP/cc). Puerta CRAP 6; CC <= 4 orienta.'
    if ($fail -eq 0) { $fail = 2 }
  }
  $hasMutants = Get-Command cargo-mutants -ErrorAction SilentlyContinue
  if (-not $hasMutants) {
    Push-Location rust
    cargo mutants --version 2>$null | Out-Null
    if ($LASTEXITCODE -eq 0) { $hasMutants = $true }
    Pop-Location
  }
  if ($hasMutants) {
    $lib = @()
    foreach ($f in $src) {
      if ($f -eq 'src/main.rs') {
        Write-Host "skip $f - bin-only; cargo mutants -- --lib never executes it"
      } elseif ($f -like 'src/handlers/*') {
        # Measured 2026-09-18: 362 MISSED under --lib, 245 in reflexion alone.
        # Handlers are async+DB; --lib never awaits them. SIL --ignored + e2e do.
        Write-Host "skip $f - MCP handler; cargo mutants -- --lib never awaits them (SIL --ignored + e2e)"
      } elseif ($f -in @('src/cli.rs','src/codegraph_cli.rs','src/recall_cli.rs','src/doctor.rs','src/setup_agent.rs')) {
        Write-Host "skip $f - CLI/doctor surface; cargo mutants -- --lib does not drive these entrypoints"
      } else {
        $lib += $f
      }
    }
    if ($lib.Count -eq 0) {
      Write-Host 'diff rust/src is bin-only - cargo mutants --lib does not apply.'
    } else {
      $files = @()
      foreach ($f in $lib) { $files += @('--file', $f) }
      # Same poison as quality-gate.sh: a shared CARGO_TARGET_DIR lets leftover
      # mutant artifacts from mutants-gate fail the unmutated baseline.
      Remove-Item Env:CARGO_TARGET_DIR -ErrorAction SilentlyContinue
      $diffFile = Join-Path $env:TEMP ("quality-src-" + [guid]::NewGuid().ToString() + ".diff")
      git diff --relative=rust HEAD -- rust/src | Set-Content -Path $diffFile -Encoding utf8
      if (-not (Get-Item $diffFile).Length) {
        git diff --relative=rust --cached -- rust/src | Set-Content -Path $diffFile -Encoding utf8
      }
      $inDiff = @()
      if ((Get-Item $diffFile).Length -gt 0) { $inDiff = @('--in-diff', $diffFile) }
      Push-Location rust
      cargo mutants @files @inDiff `
        --exclude-re 'fetch_adjacency|list_resources|read_resource|run_checks_with|upsert_symbol|upsert_placeholder_entity|builtin_retrieval_set|backfill_unscoped|observation_in_scope|run_project|run_check|run_write|workspace_client_id' `
        --timeout 90 --jobs 2 --gitignore=false -- --lib
      if ($LASTEXITCODE -ne 0) { $fail = 1 }
      Pop-Location
      Remove-Item $diffFile -ErrorAction SilentlyContinue
    }
  } else {
    Write-Host 'FALTA cargo-mutants. Endurecedor no puede cerrar.'
    if ($fail -eq 0) { $fail = 2 }
  }
} elseif ($rs.Count -gt 0) {
  Write-Host 'diff .rs fuera de rust/src - lizard/mutants del producto no aplican.'
}

if ($py.Count -gt 0) {
  if (Get-Command radon -ErrorAction SilentlyContinue) {
    radon cc @py -s -n D
  } elseif ((Get-Command lizard -ErrorAction SilentlyContinue) -or (Get-Command python -ErrorAction SilentlyContinue)) {
    Write-Host 'radon ausente; lizard mide los .py del diff (misma puerta CRAP/cc).'
    if (Get-Command lizard -ErrorAction SilentlyContinue) {
      lizard @py
    } else {
      python -m lizard @py
      if ($LASTEXITCODE -ne 0 -and $fail -eq 0) { $fail = 2 }
    }
  } else {
    Write-Host 'FALTA radon. Puerta CRAP 6; CC <= 4 orienta.'
    if ($fail -eq 0) { $fail = 2 }
  }
}

Write-Host "=== fin quality-gate exit=$fail ==="
exit $fail
