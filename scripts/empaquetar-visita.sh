#!/usr/bin/env bash
# Builds the TWO visit packages, so nobody has to choose between launchers:
#   memory-industry-0.29-ENSAYO.zip       only ENSAYO-se-desinstala-al-final.cmd
#   memory-industry-0.29-EQUIPO-REAL.zip  only INSTALAR-EQUIPO-REAL.cmd, QUITAR-TODO.cmd, RESULTADO.cmd
# plus, in each, the Windows binary, the rehearsal script and the sheets somebody needs at
# a machine where nothing can be rebuilt.
#
#   ./scripts/empaquetar-visita.sh                    dist-visita/memory-industry-0.29-{ENSAYO,EQUIPO-REAL}.zip
#   ./scripts/empaquetar-visita.sh --aceptar-obsoleto build it although the .exe predates the sources
#
# The .exe is cross-compiled elsewhere and NEVER rebuilt here: this script only
# refuses to ship one that is older than the code it is supposed to come from, because
# a package that carries last week's binary reads as "the 0.29 that was rehearsed"
# and is not.
#
#   MEMORY_INDUSTRY_WIN_EXE   path to the .exe (default: the cargo-xwin release target)
#
# No `zip` dependency: python3's zipfile writes it, so the same script works on a
# machine that only has what the gate already needs.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PAQUETE="0.29"
EXE="${MEMORY_INDUSTRY_WIN_EXE:-$HOME/.cache/cargo-target/memory-industry-win/x86_64-pc-windows-msvc/release/memory-industry.exe}"
SALIDA="$ROOT/dist-visita"
HOJA="$ROOT/docs/visita-pc-madre.md"
GUIA="$ROOT/docs/despliegue-lan.md"
SCRIPT_PS="$ROOT/scripts/ensayo-madre.ps1"

die() {
  echo "NO SE EMPAQUETA: $*" >&2
  exit 1
}

aceptar_obsoleto=0
for arg in "$@"; do
  case "$arg" in
    --aceptar-obsoleto) aceptar_obsoleto=1 ;;
    -h | --help)
      sed -n '2,16p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *) die "argumento desconocido: $arg (ver --help)" ;;
  esac
done

command -v python3 >/dev/null || die "falta python3 (escribe el zip)"
command -v sha256sum >/dev/null || die "falta sha256sum"
[[ -f "$SCRIPT_PS" ]] || die "no existe $SCRIPT_PS"
for lanzador in ENSAYO-se-desinstala-al-final.cmd INSTALAR-EQUIPO-REAL.cmd QUITAR-TODO.cmd RESULTADO.cmd; do
  [[ -f "$ROOT/scripts/$lanzador" ]] || die "no existe scripts/$lanzador"
done
[[ -f "$GUIA" ]] || die "no existe $GUIA"

# --- El script de ensayo tiene que poder leerlo Windows PowerShell 5.1 ----------
# Un .ps1 sin BOM se lee con la pagina de codigos ANSI: cualquier byte fuera de
# ASCII cambia de significado de un equipo a otro.
if LC_ALL=C grep -nP '[^\x00-\x7F]' "$SCRIPT_PS" >/dev/null; then
  die "$SCRIPT_PS tiene caracteres fuera de ASCII; Windows PowerShell 5.1 los leeria segun la pagina de codigos del equipo"
fi
if command -v pwsh >/dev/null; then
  # shellcheck disable=SC2016  # the PowerShell code is meant to reach pwsh unexpanded
  errores="$(PS_ARCHIVO="$SCRIPT_PS" pwsh -NoProfile -Command '
    $e = $null; $t = $null
    [void][System.Management.Automation.Language.Parser]::ParseFile($env:PS_ARCHIVO, [ref]$t, [ref]$e)
    @($e).Count' 2>/dev/null || echo "?")"
  [[ "$errores" == "0" ]] || die "ensayo-madre.ps1 no pasa el analizador de PowerShell ($errores errores)"
else
  echo "AVISO: no hay pwsh aqui; no se comprobo que ensayo-madre.ps1 parsee" >&2
fi

# --- El binario -----------------------------------------------------------------
[[ -f "$EXE" ]] || die "no existe el binario de Windows: $EXE
  Construyelo (no lo hace este script) desde rust/ con la toolchain de Windows, p. ej.
    CARGO_TARGET_DIR=\$HOME/.cache/cargo-target/memory-industry-win cargo xwin build --release --target x86_64-pc-windows-msvc
  o apunta MEMORY_INDUSTRY_WIN_EXE a uno ya construido."
if [[ "$(head -c 2 "$EXE")" != "MZ" ]]; then
  die "$EXE no es un ejecutable de Windows (no empieza por MZ): parece de otra plataforma"
fi

# La fuente mas reciente: el ultimo commit que toco el codigo y, ademas, lo que
# esta modificado o sin seguir ahora mismo. Los mtime a secas no sirven en un
# worktree recien creado, donde todos los ficheros llevan la hora del checkout.
FUENTES=(rust/src rust/Cargo.toml rust/Cargo.lock)
fuente_ts=0
fuente_por="mtime de los ficheros"
if git -C "$ROOT" rev-parse --git-dir >/dev/null 2>&1; then
  fuente_por="ultimo commit que toca rust/src o Cargo.*"
  fuente_ts="$(git -C "$ROOT" log -1 --format=%ct -- "${FUENTES[@]}" 2>/dev/null || echo 0)"
  fuente_ts="${fuente_ts:-0}"
  while IFS= read -r f; do
    [[ -n "$f" && -e "$ROOT/$f" ]] || continue
    t="$(stat -c %Y "$ROOT/$f")"
    if ((t > fuente_ts)); then
      fuente_ts="$t"
      fuente_por="cambio sin commitear en $f"
    fi
  done < <(git -C "$ROOT" ls-files -m -o --exclude-standard -- "${FUENTES[@]}")
else
  while IFS= read -r t; do
    t="${t%.*}"
    ((t > fuente_ts)) && fuente_ts="$t"
  done < <(find "$ROOT/rust/src" "$ROOT/rust/Cargo.toml" "$ROOT/rust/Cargo.lock" -type f -printf '%T@\n')
fi
exe_ts="$(stat -c %Y "$EXE")"
if ((exe_ts < fuente_ts)); then
  msg="el binario es MAS VIEJO que las fuentes:
  binario  $(date -d "@$exe_ts" '+%F %T')  $EXE
  fuentes  $(date -d "@$fuente_ts" '+%F %T')  ($fuente_por)
  Reconstruyelo (cargo xwin build --release --target x86_64-pc-windows-msvc, con su CARGO_TARGET_DIR) y repite."
  if ((aceptar_obsoleto == 0)); then
    die "$msg
  Si ese commit es solo de pruebas o documentacion y lo sabes, --aceptar-obsoleto lo empaqueta de todos modos."
  fi
  echo "AVISO (--aceptar-obsoleto): $msg" >&2
fi

# --- La hoja ---------------------------------------------------------------------
if [[ -f "$HOJA" ]]; then
  incluye_hoja=1
else
  incluye_hoja=0
  echo "AVISO: no existe docs/visita-pc-madre.md: el paquete saldra SIN README-VISITA.txt" >&2
fi

mkdir -p "$SALIDA"
rm -f "$SALIDA/memory-industry-$PAQUETE-ENSAYO.zip" "$SALIDA/memory-industry-$PAQUETE-EQUIPO-REAL.zip" "$SALIDA/memory-industry-$PAQUETE-visita.zip"

# --- Los zips ----------------------------------------------------------------------
# Todo el trabajo esta en python para que el contenido, las sumas y el zip salgan de
# UNA lectura de cada fichero: las sumas que lleva cada paquete son las de los bytes
# que lleva su zip.
EXE="$EXE" SALIDA="$SALIDA" SCRIPT_PS="$SCRIPT_PS" SCRIPTS="$ROOT/scripts" GUIA="$GUIA" HOJA="$HOJA" \
  INCLUYE_HOJA="$incluye_hoja" PAQUETE="$PAQUETE" python3 -I - <<'PY'
import hashlib
import os
import zipfile

exe, salida, paquete = os.environ["EXE"], os.environ["SALIDA"], os.environ["PAQUETE"]


def leer(ruta):
    with open(ruta, "rb") as f:
        return f.read()


def crlf(datos):
    # Los .txt y .cmd se abren en cualquier Windows, tambien en los viejos que no
    # entienden un salto de linea suelto.
    return datos.replace(b"\r\n", b"\n").replace(b"\n", b"\r\n")


def lanzador(nombre):
    return (nombre, crlf(leer(os.path.join(os.environ["SCRIPTS"], nombre))))


# LEEME.txt: seis lineas como mucho. Lo demas va en AYUDA.txt.
LEEME_ENSAYO = """\
1. Descomprime.
2. Doble clic en ENSAYO-se-desinstala-al-final.cmd.
3. Acepta el aviso de administrador y responde S a las preguntas.
4. Inicia sesion cuando vuelva el equipo: el resultado se abre solo.
5. Al final se desinstala todo; el informe queda en la carpeta ensayo-informe.
6. Avisame.
"""

LEEME_REAL = """\
1. Descomprime.
2. Doble clic en INSTALAR-EQUIPO-REAL.cmd, acepta el aviso de administrador.
3. Confirma la direccion IP y la base de datos; responde S al reinicio.
4. Inicia sesion cuando quieras: el resultado se abre solo (RESULTADO.cmd lo repite).
5. Si algo sale mal: QUITAR-TODO.cmd lo deja como estaba.
6. Avisame.
"""

AYUDA_COMUN = """\
Antes de instalar, el lanzador hace una PRUEBA DE HUMO de solo lectura (no instala nada):
cuenta, paquete, .exe, directivas, Task Scheduler, red, base de datos y sesiones. Si algo
esta en PARA, se detiene y dice que hacer. Despues copia el paquete a
C:\\ProgramData\\MemoryIndustry\\paquete (solo SYSTEM y administradores pueden modificarlo) y
ejecuta todo desde alli.

Fases sueltas (PowerShell COMO ADMINISTRADOR, en C:\\ProgramData\\MemoryIndustry\\paquete)
  .\\ensayo-madre.ps1 -Fase Humo       la prueba de humo, sola
  .\\ensayo-madre.ps1 -Fase Antes      primera parte
  .\\ensayo-madre.ps1 -Fase Despues    comprobacion tras el reinicio
  .\\ensayo-madre.ps1 -Fase Limpiar    retira tarea, regla y avisos (deja el env file)
  Si la directiva de ejecucion lo impide:
    powershell -NoProfile -ExecutionPolicy Bypass -File .\\ensayo-madre.ps1 -Fase Humo

Que se lleva de vuelta
  La carpeta ensayo-informe (junto a los lanzadores): RESULTADO.txt (lectura humana),
  informe.txt, humo.json, antes.json, despues.json y la transcripcion de cada fase.
  No contiene el token ni la clave de la base de datos: solo el largo del token y una
  huella para comparar.

Prueba que ningun ensayo puede hacer desde el propio equipo
  El firewall: desde OTRA maquina de la red, abrir http://<ip>:<puerto>/health.
"""

AYUDA_ENSAYO = """\
MemoryIndustry {p} - ENSAYO
===========================
Un solo lanzador: ENSAYO-se-desinstala-al-final.cmd. Pregunta "Es un equipo de PRUEBA?",
hace la prueba de humo, instala, reinicia una vez y, al volver a iniciar sesion, comprueba
que el servicio arranco solo y RETIRA todo lo instalado. El informe se conserva.
La base de datos del ensayo es una URL inalcanzable a proposito: el servicio arranca
degradado y no toca Docker ni ninguna base real.

""" + AYUDA_COMUN

AYUDA_REAL = """\
MemoryIndustry {p} - EQUIPO REAL
================================
INSTALAR-EQUIPO-REAL.cmd   prueba de humo, elegir la IP y la base, instalar, reiniciar una
                           vez y comprobar. El servicio SE QUEDA instalado.
RESULTADO.cmd              vuelve a comprobar y mostrar el resultado.
QUITAR-TODO.cmd            quita el servicio (tarea y regla de firewall). Pide confirmacion.

Con Docker Desktop, la base arranca al INICIAR SESION, no al encender el equipo: despues
de un reinicio el servicio arranca degradado y recupera sus herramientas solo cuando Docker
este arriba (los bucles de fondo rem y sync, solo tras el siguiente reinicio del servicio).

""" + AYUDA_COMUN


def paquete_zip(sufijo, lanzadores, leeme, ayuda, con_hoja):
    carpeta = "memory-industry-{}-{}".format(paquete, sufijo)
    archivos = [("memory-industry.exe", leer(exe)), ("ensayo-madre.ps1", leer(os.environ["SCRIPT_PS"]))]
    archivos += [lanzador(n) for n in lanzadores]
    if con_hoja and os.environ["INCLUYE_HOJA"] == "1":
        archivos.append(("README-VISITA.txt", crlf(leer(os.environ["HOJA"]))))
    archivos.append(("docs/despliegue-lan.md", leer(os.environ["GUIA"])))
    archivos.append(("LEEME.txt", crlf(leeme.encode("ascii"))))
    archivos.append(("AYUDA.txt", crlf(ayuda.format(p=paquete).encode("ascii"))))
    # SHA256SUMS.txt lista todo lo anterior, calculado sobre los bytes que van en el zip.
    # La prueba de humo lo lee y se niega a instalar si el .exe (o cualquiera de estos
    # ficheros) no coincide.
    sumas = "".join("{}  {}\n".format(hashlib.sha256(d).hexdigest(), n) for n, d in archivos)
    archivos.append(("SHA256SUMS.txt", sumas.encode("ascii")))
    destino = os.path.join(salida, carpeta + ".zip")
    with zipfile.ZipFile(destino, "w", zipfile.ZIP_DEFLATED, compresslevel=9) as z:
        for nombre, datos in archivos:
            info = zipfile.ZipInfo(carpeta + "/" + nombre, date_time=(2026, 1, 1, 0, 0, 0))
            info.compress_type = zipfile.ZIP_DEFLATED
            info.external_attr = 0o644 << 16
            z.writestr(info, datos)
    with zipfile.ZipFile(destino) as z:
        malo = z.testzip()
        if malo:
            raise SystemExit("el zip recien escrito tiene un fichero corrupto: " + malo)
        print("Contenido de {}:".format(os.path.basename(destino)))
        for i in z.infolist():
            print("  {:>12}  {}".format(i.file_size, i.filename))


paquete_zip("ENSAYO", ["ENSAYO-se-desinstala-al-final.cmd"], LEEME_ENSAYO, AYUDA_ENSAYO, False)
paquete_zip("EQUIPO-REAL", ["INSTALAR-EQUIPO-REAL.cmd", "QUITAR-TODO.cmd", "RESULTADO.cmd"], LEEME_REAL, AYUDA_REAL, True)
PY

echo
for z in "$SALIDA/memory-industry-$PAQUETE-ENSAYO.zip" "$SALIDA/memory-industry-$PAQUETE-EQUIPO-REAL.zip"; do
  zip_bytes="$(stat -c %s "$z")"
  echo "Paquete : $z"
  echo "Tamano  : $zip_bytes bytes ($((zip_bytes / 1024 / 1024)) MiB)"
  echo "SHA-256 : $(sha256sum "$z" | cut -d' ' -f1)"
  echo
done
echo "Binario : $(sha256sum "$EXE" | cut -d' ' -f1)  (el que lleva SHA256SUMS.txt)"
echo "Anota las sumas de los zips aparte (en el USB no): es lo que se compara al llegar."
