# Visita a la PC madre: hoja de 5 pasos

Para quien instala MemoryIndustry 0.29 en la PC madre sin poder volver a probar.
Todo lo de esta hoja ya se ensayó antes en otra PC Windows con el mismo `.exe`;
aquí solo se confirma que ocurre lo mismo en la máquina real. Dura unos 20
minutos, más un reinicio.

## Antes de salir

- La carpeta del paquete (`memory-industry-0.29-visita.zip`) en un USB, y su
  suma SHA-256 anotada aparte.
- Un administrador de la PC madre presente. El script se niega a correr sin
  permisos de administrador y no deja nada a medias.
- La IP fija que tendrá la PC madre. Si aún no la tiene, el paso 1 la fija.
- El nombre de la PC madre dentro de la empresa (el que ve en
  Sistema > Nombre del equipo).

## En la PC madre

1. **IP fija (solo si aún no la tiene).** En PowerShell como administrador, el
   procedimiento exacto está en `despliegue-lan.md`, sección «Fixed address on
   the host». La dirección tiene que quedar fuera del rango que reparte el
   DHCP: pregunta a quien administra la red, o mira `ipconfig /all` en otra
   máquina para ver el rango. Después de fijarla, `ipconfig` debe mostrarla.
2. **Copiar y comprobar.** Descomprime el paquete en `C:\memory-industry-visita`
   y compara la suma:
   `Get-FileHash .\memory-industry.exe -Algorithm SHA256` contra `SHA256SUMS.txt`.
   Si no coincide, para: el archivo se corrompió o no es el ensayado.
3. **Antes del reinicio.** PowerShell como administrador, en esa carpeta:
   `.\ensayo-madre.ps1 -Fase Antes -Ip <IP-FIJA>`.
   Tarda unos 6 minutos (espera a que el servicio vuelva tras matarlo). Debe
   terminar con `VEREDICTO: PASA`. Si dice `FALLA`, no sigas: manda la carpeta
   `ensayo-informe` tal cual.
4. **Reiniciar sin iniciar sesión.** Reinicia la PC, y cuando llegue a la pantalla
   de inicio de sesión **espera 5 minutos sin entrar**. Eso es lo que prueba que
   el servicio arranca con nadie conectado.
5. **Después del reinicio.** Inicia sesión como administrador y ejecuta
   `.\ensayo-madre.ps1 -Fase Despues`. Debe decir `VEREDICTO: PASA` y
   mostrar cuántos segundos tardó el servicio en responder tras el arranque.
   Copia la carpeta `ensayo-informe` al USB.

## Si algo falla

- No improvises cambios en la configuración: manda `ensayo-informe` completa.
  No contiene el token (solo su largo y una huella de comparación).
- Para dejar la PC como estaba: `.\ensayo-madre.ps1 -Fase Limpiar`. Quita la
  tarea, la regla del cortafuegos y el aviso de reinicio, y deja los archivos de
  datos.
- El registro del servicio está en
  `%LOCALAPPDATA%\MemoryIndustry\memory-industry.log` del usuario que instaló;
  su última línea de error suele decir la causa («is not assigned to this
  machine» es una IP que la PC no tiene; «is already in use» es otro programa
  usando el puerto).

## Clientes (cada PC que se conecta)

Se emparejan una sola vez con un código que dura un día y que se genera en la
PC madre. El procedimiento y los comandos están en `despliegue-lan.md`; con el
puente instalado, la configuración de Claude Code o Cursor no lleva ni la IP
ni ninguna clave, así que no hay que tocarla si la PC madre cambia de
dirección.
