#!/usr/bin/env python3
"""CI-D5c-C, Q7: el ayudante de ensayo-q7.sh. Solo biblioteca estándar; se lanza con `python3 -I`.

Habla con el daemon DEL ENSAYO (nunca con el servicio vivo: se niega a usar 8787 y 8788) por HTTP
de loopback, y lee los ficheros que el ensayo deja en su temporal. El token llega en Q7_TOKEN (del
entorno, no de los argumentos, que `ps` enseña) y no se imprime. La base del ensayo es una copia de
memoria real: de aquí no sale el texto de ninguna fila ni de ninguna consulta, solo números, estados
y, en los ficheros del temporal, los ids de los resultados.

  puerto                      un puerto de loopback libre
  salud    --url U --pid P    espera a que /health conteste y escribe sus números
  base     --url U ...        N llamadas memory_faro: top-10 por consulta, mediana y p95
  vigilar  --url U ...        /health hasta que el reembebido automático termina (o llega a la
                              mitad, con --mitad) y memory_faro a la vez
  comparar A B                cuántos ids del top-10 coinciden por consulta
  partes   DIR                la instantánea: directorios, partes, filas, ids repetidos, bytes
  doctor   FICHERO            la fila embedding_coverage de `doctor --json`
  registro FICHERO            el stderr del daemon: cuántos errores y cuántos `pool timed out`
  campo    FICHERO a.b.c      un valor de un JSON de los de arriba
"""
import argparse
import datetime
import hashlib
import json
import math
import os
import re
import socket
import sys
import threading
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid

PUERTOS_PROHIBIDOS = {8787, 8788, 5488}
TERMINALES = {"done", "failed", "busy"}
QUIETOS = {"idle", "off"}
LECTURAS_QUIETAS = 3
TOPE_LLAMADA_S = 150
PROTOCOLO = "2025-11-25"


# Sin intermediarios: un http_proxy del entorno no debe llevarse una petición de loopback.
ABRIR = urllib.request.build_opener(urllib.request.ProxyHandler({})).open


class Fallo(Exception):
    pass


def url_del_ensayo(url):
    partes = urllib.parse.urlparse(url)
    puerto, anfitrion = partes.port, partes.hostname
    if anfitrion != "127.0.0.1" or puerto is None or puerto in PUERTOS_PROHIBIDOS:
        sys.stderr.write("faro.py: esa dirección no es la del daemon del ensayo; no la toco\n")
        sys.exit(2)
    return url.rstrip("/")


def token():
    valor = os.environ.get("Q7_TOKEN", "")
    if not valor:
        sys.stderr.write("faro.py: falta Q7_TOKEN en el entorno\n")
        sys.exit(2)
    return valor


def vivo(pid):
    if pid is None:
        return True
    try:
        with open(f"/proc/{pid}/stat", encoding="utf-8", errors="replace") as f:
            estado = f.read().rsplit(") ", 1)[1].split()[0]
    except (OSError, IndexError):
        return False
    return estado not in ("Z", "X")


def percentil(valores, p):
    if not valores:
        return None
    orden = sorted(valores)
    return orden[max(0, math.ceil(p * len(orden)) - 1)]


def mediana(valores):
    if not valores:
        return None
    orden = sorted(valores)
    mitad = len(orden) // 2
    if len(orden) % 2:
        return orden[mitad]
    return (orden[mitad - 1] + orden[mitad]) / 2


def redondo(valor):
    return None if valor is None else round(valor, 1)


def instante(texto):
    """Un RFC 3339 de chrono (nueve decimales) como datetime."""
    if not texto:
        return None
    recortado = re.sub(r"(\.\d{6})\d+", r"\1", texto).replace("Z", "+00:00")
    try:
        return datetime.datetime.fromisoformat(recortado)
    except ValueError:
        return None


def leer_salud(url, tope=10):
    pedido = urllib.request.Request(url + "/health", headers={"Authorization": "Bearer " + token()})
    with ABRIR(pedido, timeout=tope) as respuesta:
        return json.loads(respuesta.read())


def numeros_de_salud(cuerpo):
    """Lo que el ensayo usa de /health, sin textos: runtime.vectors y runtime.vectors.auto."""
    vectores = (cuerpo.get("runtime") or {}).get("vectors") or {}
    auto = vectores.get("auto") or {}
    error = auto.get("last_error")
    inicio, fin = instante(auto.get("started_at")), instante(auto.get("finished_at"))
    return {
        "version": cuerpo.get("version"),
        "status": cuerpo.get("status"),
        "database": cuerpo.get("database"),
        "ready": cuerpo.get("ready"),
        "con_vectores": bool(vectores),
        "tag": vectores.get("tag"),
        "stale": vectores.get("stale"),
        "missing": vectores.get("missing"),
        "state": auto.get("state"),
        "older": auto.get("older"),
        "ahead": auto.get("ahead"),
        "encoded": auto.get("encoded"),
        "rewritten": auto.get("rewritten"),
        "changed": auto.get("changed"),
        "failed": auto.get("failed"),
        "con_error": error is not None,
        "last_error": None if error is None else str(error)[:200],
        "segundos_servicio": (fin - inicio).total_seconds() if inicio and fin else None,
    }


class Cliente:
    """Una sesión MCP por /mcp: initialize, y después tools/call memory_faro."""

    def __init__(self, url):
        self.url = url + "/mcp"
        self.sesion = uuid.uuid4().hex
        self.numero = 0
        self.tanda = uuid.uuid4().hex[:6]

    def enviar(self, cuerpo):
        pedido = urllib.request.Request(
            self.url,
            data=json.dumps(cuerpo).encode(),
            headers={
                "Content-Type": "application/json",
                "Accept": "application/json, text/event-stream",
                "Authorization": "Bearer " + token(),
                "Mcp-Session-Id": self.sesion,
                "Mcp-Client-Id": "ensayo",
            },
        )
        try:
            with ABRIR(pedido, timeout=TOPE_LLAMADA_S) as respuesta:
                crudo = respuesta.read()
        except urllib.error.HTTPError as e:
            raise Fallo(f"http {e.code}") from e
        except (urllib.error.URLError, OSError) as e:
            raise Fallo(type(e).__name__) from e
        if not crudo:
            return None
        try:
            return json.loads(crudo)
        except ValueError as e:
            raise Fallo("la respuesta no es JSON") from e

    def pedir(self, metodo, parametros):
        self.numero += 1
        respuesta = self.enviar(
            {"jsonrpc": "2.0", "id": self.numero, "method": metodo, "params": parametros}
        )
        if not isinstance(respuesta, dict):
            raise Fallo("sin respuesta")
        if respuesta.get("error") is not None:
            raise Fallo("error jsonrpc")
        return respuesta.get("result") or {}

    def iniciar(self):
        self.pedir(
            "initialize",
            {
                "protocolVersion": PROTOCOLO,
                "capabilities": {},
                "clientInfo": {"name": "ensayo-q7", "version": "1"},
            },
        )
        self.enviar({"jsonrpc": "2.0", "method": "notifications/initialized"})

    def faro(self, consulta, unica):
        """(milisegundos, ids del top-10, degradada). `unica` añade una marca que no se repite,
        para que el embebido de la consulta no salga de la caché de 256 entradas del daemon: sin
        ella, desde la segunda vuelta la búsqueda no pasa por la compuerta de ONNX y no mide nada."""
        if unica:
            consulta = f"{consulta} q7{self.tanda}{self.numero:05d}"
        argumentos = {"query": consulta, "limit": 10, "format": "compact", "track_access": False}
        antes = time.monotonic()
        resultado = self.pedir("tools/call", {"name": "memory_faro", "arguments": argumentos})
        ms = (time.monotonic() - antes) * 1000
        if resultado.get("isError"):
            raise Fallo("la herramienta devolvió isError")
        try:
            datos = json.loads(resultado["content"][0]["text"])
        except (KeyError, IndexError, TypeError, ValueError) as e:
            raise Fallo("el resultado no trae content[0].text con JSON") from e
        ids = []
        for fila in (datos.get("results") or [])[:10]:
            if fila.get("id") is not None:
                ids.append(str(fila["id"]))
            else:
                ids.append("e:" + hashlib.sha256(str(fila.get("e")).encode()).hexdigest()[:16])
        return ms, ids, bool(datos.get("degraded"))


def leer_consultas(ruta):
    with open(ruta, encoding="utf-8") as f:
        consultas = [linea.strip() for linea in f if linea.strip()]
    if len(consultas) != 5:
        sys.stderr.write(f"faro.py: hacen falta cinco consultas y hay {len(consultas)}\n")
        sys.exit(2)
    return consultas


def escribir(ruta, datos):
    with open(ruta, "w", encoding="utf-8") as f:
        json.dump(datos, f)
        f.write("\n")


def top_de_cada_consulta(cliente, consultas):
    """El top-10 de cada consulta, tal cual (sin marca): es lo que se compara antes y después."""
    tops, tiempos, fallos = [], [], 0
    for consulta in consultas:
        try:
            ms, ids, _ = cliente.faro(consulta, unica=False)
            tops.append(ids)
            tiempos.append(ms)
        except Fallo:
            tops.append(None)
            fallos += 1
    return tops, tiempos, fallos


def orden_puerto(_args):
    for _ in range(50):
        with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
            s.bind(("127.0.0.1", 0))
            puerto = s.getsockname()[1]
        if puerto not in PUERTOS_PROHIBIDOS:
            print(puerto)
            return 0
    return 1


def orden_salud(args):
    url = url_del_ensayo(args.url)
    limite = time.monotonic() + args.tope
    while time.monotonic() < limite:
        if not vivo(args.pid):
            return 4
        try:
            print(json.dumps(numeros_de_salud(leer_salud(url))))
            return 0
        except (urllib.error.URLError, OSError, ValueError):
            time.sleep(0.5)
    return 3


def orden_base(args):
    url = url_del_ensayo(args.url)
    consultas = leer_consultas(args.consultas)
    cliente = Cliente(url)
    cliente.iniciar()
    tops, tiempos, fallos = top_de_cada_consulta(cliente, consultas)
    degradadas = 0
    for n in range(len(consultas), args.llamadas):
        try:
            ms, _, degradada = cliente.faro(consultas[n % len(consultas)], unica=True)
            tiempos.append(ms)
            degradadas += degradada
        except Fallo:
            fallos += 1
    escribir(
        args.salida,
        {
            "llamadas": max(args.llamadas, len(consultas)),
            "fallos": fallos,
            "degradadas": degradadas,
            "sin_resultados": sum(1 for top in tops if top is not None and not top),
            "mediana_ms": redondo(mediana(tiempos)),
            "p95_ms": redondo(percentil(tiempos, 0.95)),
            "ids": tops,
        },
    )
    return 0


class Vigia(threading.Thread):
    """Lee /health cada `cada` segundos hasta un estado final, la mitad o el tope."""

    def __init__(self, url, cada, tope, mitad, pid):
        super().__init__(daemon=True)
        self.url, self.cada, self.tope, self.mitad, self.pid = url, cada, tope, mitad, pid
        self.fin = threading.Event()
        self.motivo = "tope"
        self.ultima = {}
        self.older_inicial = None
        self.lecturas = 0
        self.lecturas_fallidas = 0
        self.inicio = time.monotonic()
        self.segundos = None

    def terminar(self, motivo):
        self.motivo = motivo
        self.segundos = time.monotonic() - self.inicio
        self.fin.set()

    def run(self):
        quietas = 0
        while time.monotonic() - self.inicio < self.tope:
            if not vivo(self.pid):
                return self.terminar("muerto")
            try:
                numeros = numeros_de_salud(leer_salud(self.url))
            except (urllib.error.URLError, OSError, ValueError):
                self.lecturas_fallidas += 1
                time.sleep(self.cada)
                continue
            self.lecturas += 1
            self.ultima = numeros
            estado, older, encoded = numeros["state"], numeros["older"] or 0, numeros["encoded"] or 0
            if self.older_inicial is None and older > 0:
                self.older_inicial = older
            if self.mitad and estado == "running" and older > 0 and 2 * encoded >= older:
                return self.terminar("mitad")
            if estado in TERMINALES:
                return self.terminar("final")
            # `idle` es también el valor con el que nace el proceso, antes de contar: solo cuenta
            # como final si se repite.
            quietas = quietas + 1 if estado in QUIETOS else 0
            if quietas >= LECTURAS_QUIETAS:
                return self.terminar("final")
            time.sleep(self.cada)
        return self.terminar("tope")


def orden_vigilar(args):
    url = url_del_ensayo(args.url)
    consultas = leer_consultas(args.consultas)
    vigia = Vigia(url, args.cada_salud, args.tope, args.mitad, args.pid)
    cliente = Cliente(url)
    tiempos, fallos, degradadas, llamadas = [], 0, 0, 0
    try:
        cliente.iniciar()
    except Fallo:
        fallos += 1
    vigia.start()
    while not vigia.fin.is_set():
        antes = time.monotonic()
        llamadas += 1
        try:
            ms, _, degradada = cliente.faro(consultas[llamadas % len(consultas)], unica=True)
            tiempos.append(ms)
            degradadas += degradada
        except Fallo:
            fallos += 1
        vigia.fin.wait(max(0.0, args.cada_faro - (time.monotonic() - antes)))
    ultima = vigia.ultima
    tops = None
    if vigia.motivo == "final" and ultima.get("state") == "done":
        tops, _, fallos_del_top = top_de_cada_consulta(cliente, consultas)
        fallos += fallos_del_top
    datos = dict(ultima)
    datos.update(
        {
            "motivo": vigia.motivo,
            "older_inicial": vigia.older_inicial,
            "segundos_reloj": redondo(vigia.segundos),
            "lecturas": vigia.lecturas,
            "lecturas_fallidas": vigia.lecturas_fallidas,
            "llamadas": llamadas,
            "fallos": fallos,
            "degradadas": degradadas,
            "mediana_ms": redondo(mediana(tiempos)),
            "p95_ms": redondo(percentil(tiempos, 0.95)),
            "ids": tops,
        }
    )
    escribir(args.salida, datos)
    if vigia.motivo == "muerto":
        return 4
    if vigia.motivo == "tope":
        return 3
    if args.mitad and vigia.motivo != "mitad":
        return 5
    return 0


def orden_comparar(args):
    with open(args.antes, encoding="utf-8") as f:
        antes = json.load(f).get("ids") or []
    with open(args.despues, encoding="utf-8") as f:
        despues = json.load(f).get("ids") or []
    peor, mal = None, 0
    for n in range(5):
        a = antes[n] if n < len(antes) else None
        d = despues[n] if n < len(despues) else None
        if not a or d is None:
            print(f"consulta {n + 1}: sin top-10 con el que comparar")
            mal += 1
            continue
        iguales = len(set(a) & set(d))
        pedido = math.ceil(args.minimo * len(a) / 10)
        print(f"consulta {n + 1}: {iguales} de {len(a)} (mínimo {pedido})")
        peor = iguales if peor is None else min(peor, iguales)
        mal += iguales < pedido
    print(f"peor={'' if peor is None else peor} consultas_mal={mal}")
    return 1 if mal else 0


def orden_partes(args):
    """La instantánea bajo MEMORY_INDUSTRY_REEMBED_DIR: <dir>/<node_id>/<etiqueta>/part-NNNN.jsonl,
    con una cabecera y una fila por línea ({table, id, md5, model, vector})."""
    nodos = [e.path for e in os.scandir(args.dir) if e.is_dir()] if os.path.isdir(args.dir) else []
    instantaneas = [e.path for nodo in nodos for e in os.scandir(nodo) if e.is_dir()]
    vistos, filas, partes, a_medias, total, destinos = set(), 0, 0, 0, 0, set()
    for instantanea in instantaneas:
        for entrada in sorted(os.scandir(instantanea), key=lambda e: e.name):
            total += entrada.stat().st_size
            if entrada.name.endswith(".part"):
                a_medias += 1
                continue
            if not (entrada.name.startswith("part-") and entrada.name.endswith(".jsonl")):
                continue
            partes += 1
            with open(entrada.path, encoding="utf-8") as f:
                destinos.add(json.loads(f.readline()).get("to"))
                for linea in f:
                    fila = json.loads(linea)
                    filas += 1
                    vistos.add((fila["table"], fila["id"]))
    print(
        json.dumps(
            {
                "nodos": len(nodos),
                "instantaneas": len(instantaneas),
                "partes": partes,
                "a_medias": a_medias,
                "filas": filas,
                "unicas": len(vistos),
                "repetidas": filas - len(vistos),
                "destinos": len(destinos),
                "bytes": total,
            }
        )
    )
    return 0


COBERTURA_OK = re.compile(r"^(\d+) filas, todas con vector de (\S+)$")
COBERTURA_WARN = re.compile(
    r"^(\d+) filas: (\d+) sin vector y (\d+) vectores de otra generación(?: \(de [^)]*\))?; "
    r"la actual es (\S+?)(; reembebido en curso)?$"
)


def orden_doctor(args):
    # `doctor --json` escribe su informe en una línea; se toma la última que sea un objeto.
    informe = {}
    try:
        with open(args.fichero, encoding="utf-8") as f:
            for linea in f:
                if linea.startswith("{"):
                    try:
                        informe = json.loads(linea)
                    except ValueError:
                        pass
    except OSError:
        pass
    fila = next((c for c in informe.get("checks", []) if c.get("name") == "embedding_coverage"), None)
    datos = {
        "fallos_de_doctor": informe.get("failed"),
        "avisos_de_doctor": informe.get("warnings"),
        "estado": None if fila is None else fila.get("status"),
        "leida": False,
        "total": None,
        "missing": None,
        "stale": None,
        "tag": None,
        "en_curso": False,
    }
    detalle = "" if fila is None else fila.get("detail", "")
    bien, aviso = COBERTURA_OK.match(detalle), COBERTURA_WARN.match(detalle)
    if bien:
        datos.update(leida=True, total=int(bien[1]), missing=0, stale=0, tag=bien[2])
    elif aviso:
        datos.update(
            leida=True,
            total=int(aviso[1]),
            missing=int(aviso[2]),
            stale=int(aviso[3]),
            tag=aviso[4],
            en_curso=aviso[5] is not None,
        )
    print(json.dumps(datos))
    return 0


def orden_registro(args):
    lineas = errores = avisos = pool = 0
    try:
        with open(args.fichero, encoding="utf-8", errors="replace") as f:
            for linea in f:
                lineas += 1
                pool += "pool timed out" in linea.lower()
                try:
                    nivel = json.loads(linea).get("level")
                except (ValueError, AttributeError):
                    continue
                errores += nivel == "ERROR"
                avisos += nivel == "WARN"
    except OSError:
        return 1
    print(json.dumps({"lineas": lineas, "errores": errores, "avisos": avisos, "pool_timed_out": pool}))
    return 0


def orden_campo(args):
    # Un fichero que falta o que no es JSON no da valor: quien lo lee decide qué es un valor vacío.
    try:
        with open(args.fichero, encoding="utf-8") as f:
            valor = json.load(f)
    except (OSError, ValueError):
        return 0
    for clave in args.clave.split("."):
        valor = valor.get(clave) if isinstance(valor, dict) else None
    if isinstance(valor, bool):
        print(int(valor))
    elif valor is not None:
        print(valor)
    return 0


def main():
    analizador = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawTextHelpFormatter)
    ordenes = analizador.add_subparsers(dest="orden", required=True)

    ordenes.add_parser("puerto").set_defaults(hacer=orden_puerto)

    p = ordenes.add_parser("salud")
    p.add_argument("--url", required=True)
    p.add_argument("--tope", type=float, default=400)
    p.add_argument("--pid", type=int)
    p.set_defaults(hacer=orden_salud)

    p = ordenes.add_parser("base")
    p.add_argument("--url", required=True)
    p.add_argument("--consultas", required=True)
    p.add_argument("--llamadas", type=int, default=20)
    p.add_argument("--salida", required=True)
    p.set_defaults(hacer=orden_base)

    p = ordenes.add_parser("vigilar")
    p.add_argument("--url", required=True)
    p.add_argument("--consultas", required=True)
    p.add_argument("--tope", type=float, default=3600)
    p.add_argument("--cada-salud", type=float, default=5)
    p.add_argument("--cada-faro", type=float, default=2)
    p.add_argument("--mitad", action="store_true")
    p.add_argument("--pid", type=int)
    p.add_argument("--salida", required=True)
    p.set_defaults(hacer=orden_vigilar)

    p = ordenes.add_parser("comparar")
    p.add_argument("antes")
    p.add_argument("despues")
    p.add_argument("--minimo", type=int, default=8)
    p.set_defaults(hacer=orden_comparar)

    p = ordenes.add_parser("partes")
    p.add_argument("dir")
    p.set_defaults(hacer=orden_partes)

    for nombre, hacer in (("doctor", orden_doctor), ("registro", orden_registro)):
        p = ordenes.add_parser(nombre)
        p.add_argument("fichero")
        p.set_defaults(hacer=hacer)

    p = ordenes.add_parser("campo")
    p.add_argument("fichero")
    p.add_argument("clave")
    p.set_defaults(hacer=orden_campo)

    args = analizador.parse_args()
    return args.hacer(args)


if __name__ == "__main__":
    sys.exit(main())
