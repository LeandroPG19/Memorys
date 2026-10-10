@echo off
rem Doble clic en un equipo CLIENTE: lo conecta con la PC madre de MemoryIndustry, una sola vez.
rem No pide administrador: todo queda en la carpeta y en las tareas de este usuario.
rem Sin etiquetas ni saltos: un .cmd con finales de linea LF sueltos los rompe. Cada salida
rem hace pause: la ventana no se cierra sin que se lea el resultado.
setlocal
if not exist "%~dp0conectar-cliente.ps1" (
  echo No encuentro conectar-cliente.ps1 junto a este archivo. Descomprime primero el zip completo y vuelve a hacer doble clic.
  pause
  exit /b 1
)
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0conectar-cliente.ps1"
set "CODIGO=%errorlevel%"
echo.
pause
exit /b %CODIGO%
