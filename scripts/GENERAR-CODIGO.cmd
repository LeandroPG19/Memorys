@echo off
rem Doble clic en la PC madre: crea el codigo de emparejamiento de una persona y lo muestra
rem en grande, con el nombre y la IP de la madre. No pide administrador: lo hace el usuario
rem de Windows que instalo MemoryIndustry en este equipo.
rem Sin etiquetas ni saltos: un .cmd con finales de linea LF sueltos los rompe.
setlocal
if not exist "%~dp0aviso-madre.ps1" (
  echo No encuentro aviso-madre.ps1 junto a este archivo. Descomprime primero el zip completo y vuelve a hacer doble clic.
  pause
  exit /b 1
)
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0aviso-madre.ps1" -Modo Codigo
set "CODIGO=%errorlevel%"
echo.
pause
exit /b %CODIGO%
