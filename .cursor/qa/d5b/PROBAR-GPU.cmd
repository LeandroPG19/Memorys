@echo off
rem CI-D5b: la prueba de ONNX Runtime 1.31.1 con la tarjeta de este Windows. Doble clic, o escribirlo en una consola.
rem Lanza probar-gpu.sh (que esta junto a este archivo) con Git Bash; no pide administrador y no toca el daemon,
rem ~\.cache\cuba-memorys, ~\.cache\cuda12-runtime (solo los lee) ni el PATH del sistema.
rem   PROBAR-GPU.cmd        los pasos V0-V9
rem   PROBAR-GPU.cmd dml    ademas V8, DirectML 1.24.4 (opcional)
setlocal
set "HERE=%~dp0"
set "HERE_SLASH=%HERE:\=/%"
set "GITBASH=C:\Program Files\Git\bin\bash.exe"
if not exist "%GITBASH%" (
  echo No encuentro Git Bash en "%GITBASH%". Instala Git for Windows y vuelve a abrir este archivo.
  pause
  exit /b 1
)
"%GITBASH%" "%HERE_SLASH%probar-gpu.sh" %*
set "RC=%ERRORLEVEL%"
echo.
if "%RC%"=="0" (
  echo TODO OK. Copia el bloque RESULTADO D5b de arriba, o el log de la carpeta logs\, en la conversacion.
) else (
  echo HAY PASOS EN FALLA ^(codigo %RC%^). Copia el bloque RESULTADO D5b de arriba, o el log de la carpeta logs\, en la conversacion.
)
pause
exit /b %RC%
