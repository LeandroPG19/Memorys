# Visita a la PC madre: hoja de 4 pasos

Para quien instala MemoryIndustry 0.29 en la PC madre sin poder volver a probar.
Todo lo de esta hoja ya se ensayó antes en otra PC Windows con el mismo `.exe`;
aquí solo se confirma que ocurre lo mismo en la máquina real. Dura unos 20
minutos, más un reinicio.

La madre queda en **modo automático**: escucha en la IP que tenga en cada momento
(y la sigue si el DHCP se la cambia), y los equipos la encuentran solos. No hay que
fijar ni elegir ninguna IP.

## Antes de salir

- En un USB, los paquetes `memory-industry-0.29-EQUIPO-REAL.zip` (la madre) y
  `memory-industry-0.29-CLIENTE.zip` (cada equipo que se conecta), y sus sumas
  SHA-256 anotadas aparte.
- Un administrador de la PC madre presente (el aviso de administrador sale al
  hacer doble clic). Sin ese permiso no se instala nada a medias.
- El nombre de la PC madre dentro de la empresa (el que ve en
  Sistema > Nombre del equipo).

## En la PC madre

1. **Copiar.** Usa el paquete `memory-industry-0.29-EQUIPO-REAL.zip` (no el de
   ENSAYO) y descomprímelo en `C:\memory-industry-visita`. No hace falta comparar
   la suma a mano: la prueba de humo del paso siguiente lo hace y, si el `.exe` no
   es el ensayado, para antes de instalar nada.
2. **Doble clic en `INSTALAR-EQUIPO-REAL.cmd`** y acepta el aviso de administrador
   con **Sí**, sin escribir otra contraseña (el resultado se abre en el escritorio
   de quien está sentado). Hace, en orden:
   - **Prueba de humo** (2 minutos, solo lectura, no instala nada). Cada fila sale
     `OK`, `AVISO` o `PARA`. Con un solo `PARA` se detiene y dice qué hacer: no
     sigas, manda la carpeta `ensayo-informe` tal cual.
   - **IP detectada.** Solo la muestra; no se elige ni se confirma. Si es del DHCP
     no pasa nada: la madre la sigue.
   - **Base de datos.** Enter usa `postgresql://cuba:…@127.0.0.1:5488/brain` (la del
     repositorio); o escribe otra URL (no se muestra). La base está en Docker
     Desktop de este mismo equipo, que arranca **al iniciar sesión, no al encender**:
     tras un reinicio el servicio arranca degradado y recupera sus herramientas
     solo cuando Docker está arriba (los bucles de fondo rem y sync, solo tras el
     siguiente reinicio del servicio). Que la base no responda en ese momento es un
     `AVISO`, no un fallo.
   - **Instala y prueba** (incluye matarlo y ver que vuelve): el servicio, las dos
     reglas de firewall (TCP y UDP del puerto 8787) y la tarea «MemoryIndustry Madre
     aviso». Si la red es **Pública**, ofrece cambiarla a Privada
     (`Set-NetConnectionProfile`) y, si no se cambia, falla: con red Pública ningún
     cliente entra.
   - **Reinicio.** Lista las sesiones de otras personas y los reinicios pendientes.
     Pregunta «Reiniciar ahora? (S/N)»; si hay otras sesiones abiertas hay que
     escribir **SI** entero. Reinicia con 60 segundos de margen; se cancela con
     `shutdown /a`.
3. **Inicia sesión cuando quieras.** Al volver, el propio equipo comprueba que el
   servicio arrancó solo, antes de que nadie entrara, y abre `RESULTADO.txt` con
   `PASA` o `FALLA` en grande. En la PC madre el servicio **se queda instalado**
   aunque falle. Si el equipo inicia sesión solo (sin contraseña), esa prueba
   concreta no vale y lo dice; lo demás sí. `RESULTADO.cmd` lo repite. Además sale
   el aviso «Madre activa: NOMBRE en IP».
4. **Desde otra máquina de la red**, abre `http://<IP>:8787/health` con la IP del
   aviso: es la única prueba real del firewall. Copia la carpeta `ensayo-informe`
   al USB.

Para quitarlo todo: `QUITAR-TODO.cmd` (pide confirmación). El otro paquete,
`ENSAYO`, es para una PC de prueba y se desinstala solo al final.

## El aviso de la madre

Cada vez que alguien inicia sesión en la madre:

- «Madre activa: NOMBRE en IP», si todo va bien.
- Si la IP cambió desde la última vez, una ventana: «La IP cambió de A a B. Los
  equipos la encuentran solos.» con dos botones. **Dejar automática** no toca nada.
  **Fijar B** pide administrador y deja esa IP fija con la máscara, la puerta de
  enlace y los DNS que ya tiene; la ventana explica el riesgo: si el router reparte
  esa misma IP a otro equipo, chocan. Pide que la reserven para la madre.
- Si la madre no responde, una ventana con la ruta del registro y `doctor`.

## Clientes (cada PC que se conecta)

1. **En la madre**, doble clic en `GENERAR-CODIGO.cmd` (con el usuario de Windows
   que instaló). Pide el nombre de la persona y muestra en grande el código, el
   nombre y la IP de la madre. El código vale 24 horas y un solo uso.
2. **En el equipo de la persona**, sin administrador: descomprime
   `memory-industry-0.29-CLIENTE.zip` en una carpeta suya y haz doble clic en
   `CONECTAR-A-LA-MADRE.cmd`. Escribe el código. Busca la madre solo; si no la
   encuentra, pide su nombre o su IP (los de la pantalla del código).
3. Al terminar dice **CONECTADO** (o qué falló) y deja un informe sin el código en
   `%LOCALAPPDATA%\MemoryIndustry\conectar-informe.txt`. Reinicia Claude Code o
   Cursor.

Desde entonces el equipo se conecta solo en cada inicio de sesión y avisa si pierde
la madre. La configuración de Claude Code o Cursor no lleva ni la IP ni ninguna
clave, así que no hay que tocarla si la madre cambia de dirección. El detalle está
en `despliegue-lan.md`.

## Si algo falla

- No improvises cambios en la configuración: manda `ensayo-informe` completa.
  No contiene el token (solo su largo y una huella de comparación).
- Para dejar la PC como estaba: `QUITAR-TODO.cmd`, o `.\ensayo-madre.ps1 -Fase Limpiar` (como
  administrador, desde `C:\ProgramData\MemoryIndustry\paquete`). Quita la tarea, las
  reglas del cortafuegos, el aviso de la madre y el aviso de reinicio, y deja los
  archivos de datos. Si la instalación existía antes del ensayo, se niega salvo
  `-Forzar`.
- El registro del servicio está en
  `%LOCALAPPDATA%\MemoryIndustry\memory-industry.log` del usuario que instaló;
  su última línea de error suele decir la causa («is already in use» es otro
  programa usando el puerto 8787).
