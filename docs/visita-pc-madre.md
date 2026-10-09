# Visita a la PC madre: hoja de 5 pasos

Para quien instala MemoryIndustry 0.29 en la PC madre sin poder volver a probar.
Todo lo de esta hoja ya se ensayó antes en otra PC Windows con el mismo `.exe`;
aquí solo se confirma que ocurre lo mismo en la máquina real. Dura unos 20
minutos, más un reinicio.

## Antes de salir

- El paquete `memory-industry-0.29-EQUIPO-REAL.zip` en un USB, y su
  suma SHA-256 anotada aparte.
- Un administrador de la PC madre presente (el aviso de administrador sale al
  hacer doble clic). Sin ese permiso no se instala nada a medias.
- La IP fija que tendrá la PC madre. Si aún no la tiene, el paso 1 la fija.
- El nombre de la PC madre dentro de la empresa (el que ve en
  Sistema > Nombre del equipo).

## En la PC madre

1. **IP fija (solo si aún no la tiene).** En PowerShell como administrador, el
   procedimiento exacto está en `despliegue-lan.md`, sección «Fixed address on
   the host». La dirección tiene que quedar fuera del rango que reparte el
   DHCP: pregunta a quien administra la red, o mira `ipconfig /all` en otra
   máquina para ver el rango. Después de fijarla, `ipconfig` debe mostrarla.
2. **Copiar.** Usa el paquete `memory-industry-0.29-EQUIPO-REAL.zip` (no el de
   ENSAYO) y descomprímelo en `C:\memory-industry-visita`. No hace falta comparar
   la suma a mano: la prueba de humo del paso siguiente lo hace y, si el `.exe` no
   es el ensayado, para antes de instalar nada.
3. **Doble clic en `INSTALAR-EQUIPO-REAL.cmd`** y acepta el aviso de administrador
   con **Sí**, sin escribir otra contraseña (el resultado se abre en el escritorio
   de quien está sentado). Hace, en orden:
   - **Prueba de humo** (2 minutos, solo lectura, no instala nada). Cada fila sale
     `OK`, `AVISO` o `PARA`. Con un solo `PARA` se detiene y dice qué hacer: no
     sigas, manda la carpeta `ensayo-informe` tal cual.
   - **Dirección IP.** Muestra las direcciones del equipo numeradas y propone la de la
     ruta principal. Comprueba que es la IP fija; si no, escribe el número de la
     correcta. Si es de DHCP, avisa en rojo: fija la IP antes (paso 1).
   - **Base de datos.** Enter usa `postgresql://cuba:…@127.0.0.1:5488/brain` (la del
     repositorio); o escribe otra URL (no se muestra). La base está en Docker
     Desktop de este mismo equipo, que arranca **al iniciar sesión, no al encender**:
     tras un reinicio el servicio arranca degradado y recupera sus herramientas
     solo cuando Docker está arriba (los bucles de fondo rem y sync, solo tras el
     siguiente reinicio del servicio). Que la base no responda en ese momento es un
     `AVISO`, no un fallo.
   - **Instala y prueba** (incluye matarlo y ver que vuelve). Si la red es
     **Pública**, ofrece cambiarla a Privada (`Set-NetConnectionProfile`) y, si no
     se cambia, falla: con red Pública ningún cliente entra.
   - **Reinicio.** Lista las sesiones de otras personas y los reinicios pendientes.
     Pregunta «Reiniciar ahora? (S/N)»; si hay otras sesiones abiertas hay que
     escribir **SI** entero. Reinicia con 60 segundos de margen; se cancela con
     `shutdown /a`.
4. **Inicia sesión cuando quieras.** Al volver, el propio equipo comprueba que el
   servicio arrancó solo, antes de que nadie entrara, y abre `RESULTADO.txt` con
   `PASA` o `FALLA` en grande. En la PC madre el servicio **se queda instalado**
   aunque falle. Si el equipo inicia sesión solo (sin contraseña), esa prueba
   concreta no vale y lo dice; lo demás sí. `RESULTADO.cmd` lo repite.
5. **Desde otra máquina de la red**, abre `http://<IP-FIJA>:8797/health`: es la
   única prueba real del firewall. Copia la carpeta `ensayo-informe` al USB.

Para quitarlo todo: `QUITAR-TODO.cmd` (pide confirmación). El otro paquete,
`ENSAYO`, es para una PC de prueba y se desinstala solo al final.

## Si algo falla

- No improvises cambios en la configuración: manda `ensayo-informe` completa.
  No contiene el token (solo su largo y una huella de comparación).
- Para dejar la PC como estaba: `QUITAR-TODO.cmd`, o `.\ensayo-madre.ps1 -Fase Limpiar` (como
  administrador, desde `C:\ProgramData\MemoryIndustry\paquete`). Quita la tarea, la regla del cortafuegos y el aviso de reinicio,
  y deja los archivos de datos. Si la instalación existía antes del ensayo, se
  niega salvo `-Forzar`.
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
