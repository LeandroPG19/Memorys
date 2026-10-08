#!/usr/bin/env bash
# Builds the visit package: the Windows binary, the rehearsal script and the sheets
# somebody needs at a machine where nothing can be rebuilt.
#
#   ./scripts/empaquetar-visita.sh                    dist-visita/memory-industry-0.29-visita.zip
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
NOMBRE="memory-industry-${PAQUETE}-visita"
EXE="${MEMORY_INDUSTRY_WIN_EXE:-$HOME/.cache/cargo-target/memory-industry-win/x86_64-pc-windows-msvc/release/memory-industry.exe}"
SALIDA="$ROOT/dist-visita"
ZIP="$SALIDA/$NOMBRE.zip"
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
      sed -n '2,13p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *) die "argumento desconocido: $arg (ver --help)" ;;
  esac
done

command -v python3 >/dev/null || die "falta python3 (escribe el zip)"
command -v sha256sum >/dev/null || die "falta sha256sum"
[[ -f "$SCRIPT_PS" ]] || die "no existe $SCRIPT_PS"
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
rm -f "$ZIP"

# --- El zip ------------------------------------------------------------------------
# Todo el trabajo esta en python para que el contenido, las sumas y el zip salgan de
# UNA lectura de cada fichero: las sumas que lleva el paquete son las de los bytes
# que lleva el zip.
EXE="$EXE" ZIP="$ZIP" NOMBRE="$NOMBRE" SCRIPT_PS="$SCRIPT_PS" GUIA="$GUIA" HOJA="$HOJA" \
  INCLUYE_HOJA="$incluye_hoja" PAQUETE="$PAQUETE" python3 -I - <<'PY'
import hashlib
import os
import zipfile

exe, destino, carpeta = os.environ["EXE"], os.environ["ZIP"], os.environ["NOMBRE"]


def leer(ruta):
    with open(ruta, "rb") as f:
        return f.read()


def crlf(datos):
    # Los .txt se abren en el Bloc de notas de cualquier Windows, tambien en los viejos
    # que no entienden un salto de linea suelto.
    return datos.replace(b"\r\n", b"\n").replace(b"\n", b"\r\n")


LEEME = """\
MemoryIndustry {paquete} - paquete de visita
=============================================

Esto instala MemoryIndustry como servicio de Windows (tarea programada que arranca
con el equipo, regla de firewall) y comprueba que funciona. Sirve igual para ensayarlo
en una PC propia y para repetirlo en el equipo real.

Antes de empezar
  1. Descomprimir el zip ENTERO en una carpeta local y fija, por ejemplo
     C:\\memory-industry-visita  (no ejecutar desde dentro del zip, ni desde una
     unidad de red, ni desde Descargas/Escritorio/OneDrive: la ruta del .exe queda
     escrita en la tarea).
  2. Comprobar el binario:  Get-FileHash .\\memory-industry.exe -Algorithm SHA256
     y comparar con SHA256SUMS.txt (mayusculas o minusculas, da igual). Si no
     coincide, parar.
  3. Si el zip vino de internet o del correo, desbloquear:
       Get-ChildItem -Recurse | Unblock-File
  4. Abrir PowerShell COMO ADMINISTRADOR en esa carpeta. Sin administrador el script
     se niega y no deja nada a medias.

Los tres comandos
  1) Antes del reinicio  (unos minutos; en el peor caso, unos 20)
       .\\ensayo-madre.ps1 -Fase Antes
     Si la directiva de ejecucion de scripts lo impide:
       powershell -NoProfile -ExecutionPolicy Bypass -File .\\ensayo-madre.ps1 -Fase Antes
     Opciones: -Ip <ipv4 de este equipo> (por omision, la primera IPv4 privada de un
     adaptador activo), -Puerto <n> (8797 por omision), -Salida <carpeta>
     (.\\ensayo-informe por omision).
     Tiene que terminar con  VEREDICTO: PASA.  Si dice FALLA, no seguir: mandar la
     carpeta ensayo-informe tal cual.

  2) Reiniciar SIN iniciar sesion
       Restart-Computer
     Reiniciar, no apagar (con Inicio rapido, "Apagar" no reinicia el kernel). Al
     llegar a la pantalla de inicio de sesion, NO entrar: esperar 6 minutos. Desde
     OTRA maquina de la LAN:  curl http://<ip>:<puerto>/health  (es la unica prueba
     real del firewall; desde el propio equipo no se puede).

  3) Despues del reinicio
       .\\ensayo-madre.ps1 -Fase Despues
     Tiene que terminar con  VEREDICTO: PASA  y muestra cuantos segundos tardo el
     servicio en responder desde el arranque.

Para dejar el equipo como estaba (SOLO en un ensayo; en el equipo real no)
       .\\ensayo-madre.ps1 -Fase Limpiar            quita la tarea, la regla y el aviso
                                                    de reinicio; deja el env file
       .\\ensayo-madre.ps1 -Fase Limpiar -BorrarEnv  borra tambien el env file (token) y el log

Que se lleva de vuelta
  La carpeta ensayo-informe: informe.txt (lectura humana), antes.json,
  despues-arranque.json y despues.json (maquina), mas las salidas de cada comando.
  No contiene el token: solo su largo y una huella SHA-256 para comparar antes y
  despues. El log se copia con el token redactado.

Contenido
  memory-industry.exe     el binario (suma en SHA256SUMS.txt)
  ensayo-madre.ps1        el ensayo
  README-VISITA.txt       la hoja de 5 pasos
  docs\\despliegue-lan.md  la guia de despliegue
  SHA256SUMS.txt          sumas SHA-256 de lo anterior
""".format(paquete=os.environ["PAQUETE"])

archivos = [("memory-industry.exe", leer(exe))]
archivos.append(("ensayo-madre.ps1", leer(os.environ["SCRIPT_PS"])))
if os.environ["INCLUYE_HOJA"] == "1":
    archivos.append(("README-VISITA.txt", crlf(leer(os.environ["HOJA"]))))
archivos.append(("docs/despliegue-lan.md", leer(os.environ["GUIA"])))

sumas = "".join(
    "{}  {}\n".format(hashlib.sha256(datos).hexdigest(), nombre) for nombre, datos in archivos
)
archivos.append(("leeme.txt", crlf(LEEME.encode("ascii"))))
archivos.append(("SHA256SUMS.txt", sumas.encode("ascii")))

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
PY

zip_bytes="$(stat -c %s "$ZIP")"
zip_sha="$(sha256sum "$ZIP" | cut -d' ' -f1)"
echo
echo "Paquete : $ZIP"
echo "Tamano  : $zip_bytes bytes ($((zip_bytes / 1024 / 1024)) MiB)"
echo "SHA-256 : $zip_sha"
echo "Binario : $(sha256sum "$EXE" | cut -d' ' -f1)  (la que lleva SHA256SUMS.txt)"
echo "Anota la suma del zip aparte (en el USB no): es lo que se compara al llegar."
