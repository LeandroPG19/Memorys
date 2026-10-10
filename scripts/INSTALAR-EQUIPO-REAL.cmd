@echo off
rem Doble clic: instala MemoryIndustry en el EQUIPO REAL y lo prueba con UN reinicio. El servicio SE QUEDA instalado.
rem Se eleva solo, comprueba y copia el paquete a una carpeta que solo SYSTEM y los
rem administradores pueden modificar, y ejecuta alli ensayo-madre.ps1 -Fase Todo.
rem Sin etiquetas ni goto: un .cmd con saltos de linea LF sueltos los rompe. Cada salida
rem con error hace pause: la ventana no se cierra sin que se lea el motivo.
setlocal
set "FASE=Todo"
set "EXTRA=-Conservar"
set "PREGUNTA="
set "COPIA=siempre"
set "YA="
set "HACER="
pushd "%~dp0"
if not exist "%~dp0ensayo-madre.ps1" (
  echo No encuentro ensayo-madre.ps1 junto a este archivo. Descomprime primero el zip completo y vuelve a hacer doble clic.
  pause
  exit /b 1
)
if /i "%~1"=="elevado" set "YA=1"
if not defined YA (
  fltmc >nul 2>&1
  if not errorlevel 1 set "YA=1"
)
if not defined YA (
  echo Se pedira permiso de administrador. Aceptalo en el aviso que va a salir.
  set "LANZA=%~f0"
  powershell -NoProfile -ExecutionPolicy Bypass -Command "Start-Process -FilePath $env:LANZA -ArgumentList 'elevado' -Verb RunAs -ErrorAction Stop"
  if errorlevel 1 (
    echo.
    echo No se concedio el permiso de administrador. Sin el no se puede continuar.
    pause
  )
  exit /b
)
rem (sin pregunta previa)
set "PAQ_ORIGEN=%~dp0"
powershell -NoProfile -Command "$p=$env:PAQ_ORIGEN; $m=''; if ($p -match '^\\\\') {$m='esta en la red'} elseif ($p -notmatch '^[\x20-\x7E]+$') {$m='tiene caracteres que no son ASCII'} elseif ($p.Contains([string][char]37)) {$m='tiene un signo de porcentaje'} elseif ((New-Object IO.DriveInfo($p.Substring(0,1))).DriveType -ne 'Fixed') {$m='no esta en un disco fijo del equipo'}; if ($m) { Write-Host ('No se puede usar esta carpeta: ' + $m + '. Copia el paquete a C:\memory-industry-visita y vuelve a hacer doble clic.'); exit 3 }"
if errorlevel 1 (
  pause
  exit /b 1
)
powershell -NoProfile -Command "Get-ChildItem -LiteralPath $env:PAQ_ORIGEN -Recurse -File -Force | Unblock-File"
set "DEST=%ProgramData%\MemoryIndustry\paquete"
set "SALIDA=%~dp0ensayo-informe"
if /i "%COPIA%"=="siempre" set "HACER=1"
if not exist "%DEST%\ensayo-madre.ps1" set "HACER=1"
if defined HACER (
  echo Copiando el paquete a una carpeta protegida...
  schtasks /End /TN memory-industry >nul 2>&1
  if exist "%DEST%" rd /s /q "%DEST%"
  if exist "%DEST%" (
    echo No pude borrar la copia anterior en %DEST%. Detiene el servicio con QUITAR-TODO o reinicia y repite.
    pause
    exit /b 1
  )
  mkdir "%DEST%"
  icacls "%DEST%" /inheritance:r /grant:r "*S-1-5-18:(OI)(CI)F" "*S-1-5-32-544:(OI)(CI)F" >nul
  if errorlevel 1 (
    echo No pude proteger la carpeta %DEST%.
    pause
    exit /b 1
  )
  robocopy "%~dp0." "%DEST%" /E /XD ensayo-informe /NFL /NDL /NJH /NJS /NP >nul
  if errorlevel 8 (
    echo No pude copiar el paquete a %DEST%.
    pause
    exit /b 1
  )
)
powershell -NoProfile -ExecutionPolicy Bypass -File "%DEST%\ensayo-madre.ps1" -Fase %FASE% %EXTRA% -Salida "%SALIDA%"
set "CODIGO=%errorlevel%"
echo.
pause
exit /b %CODIGO%
