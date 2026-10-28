# 0.32 G — El grafo se instala solo y se usa a fondo (FalkorDB como Postgres)

Especificación (rol especificador, Swarm Forge). Rama `feat/0.32-analisis-y-sota`, HEAD `9677095`.
No se compiló, no se corrió cargo ni docker. Todo sale de leer `rust/src/{setup,graph_db,graph_cli,doctor,setup_agent,main}.rs`,
`docker-compose.yml`, `npm/postinstall.js`, `packaging/`, los handlers y `git log -S`. Lo que no se pudo verificar lo dice así.
Parte de la «Decisión de Leandro (2026-10-28)» al final de `B8-graph-proyeccion.md`: el grafo se mantiene, se completa, se instala
con el proyecto y se usa a fondo. B8 (A, B, C, D) cubre inyección y errores; **esta pieza va después de B8** y no repite nada de B8.

## 0. Resumen en seis líneas

1. Hoy Postgres se aprovisiona solo (`setup.rs:13-109`: `docker run` en la primera ejecución de cualquier comando) y FalkorDB **no**:
   es un perfil de compose (`docker-compose.yml:31-45`) que el usuario tiene que levantar a mano, con la imagen sin fijar (`:latest`).
2. `graph set falkor` sólo escribe `graph.env` (`graph_cli.rs:105-132`) y `doctor` falla con el consejo
   `docker compose --profile graph up -d` (`doctor.rs:1283`), un fichero que un usuario de npm/binario **no tiene** (npm sólo baja el binario, `npm/postinstall.js`).
3. Aunque el contenedor exista, el grafo **no es explotable**: de unas **40 líneas en 13 ficheros** que escriben en `brain_entities`/`brain_relations` (grep, sin separar lo que sólo toca otra tabla)
   sólo **3 handlers** proyectan (alma create, puente create/delete, artefacto put); `reconcile` nunca borra; y el nodo se llama por
   `name` a secas, cuando desde la migración 0064 las entidades son únicas por `(name, project_id)` (§1.4).
4. Las lecturas (faro multi-hop, `puente traverse`, `hipotesis explain`) **sí** usan el grafo cuando está encendido, pero un fallo se ve
   igual que «sin vecinos» (`if let Ok`).
5. Diseño: `memory-industry graph up|down`, un interruptor persistido en `graph.env`, apagado por defecto; arranque del servidor **no fatal**
   (al revés que Postgres); imagen fijada por digest.
6. Una sola decisión de Leandro (§7): ¿encendido por defecto en instalaciones nuevas con Docker?

## 1. Estado actual (ruta:línea, verificado en el árbol)

### 1.1 Cómo se aprovisiona Postgres (el patrón a imitar)

| Qué | Ruta:línea |
|---|---|
| Constantes: contenedor `memory-industry-db`, legado `cuba-memorys-db`, volumen `memory_industry_data`, imagen `pgvector/pgvector:pg18` (por tag, sin digest), puerto 5488 | `setup.rs:4-11` |
| `resolve_database_url()`: `DATABASE_URL` manda; modo nube sale con error; contenedor Running → URL; si no hay Docker, imprime opciones y `exit(1)` | `setup.rs:13-53` |
| Estado por `docker ps -a --filter name=^NAME$ --format {{.Status}}` (Running / Stopped / NotFound / Unknown) | `setup.rs:328-379` |
| Creación: `docker run -d --name … -p {listen_address()}:5488:5432 -v VOL:/var/lib/postgresql --health-cmd … --restart unless-stopped IMAGEN` | `setup.rs:395-424` |
| Espera: `docker exec … pg_isready` cada 500 ms hasta 60 s; si no llega, `exit(1)` | `setup.rs:442-469` |
| Quién lo dispara: **cada** comando que abre base (`resolve_database_url` en 20 sitios: `main.rs:358`, `cli.rs:56`, `doctor.rs:1609`, `protocol.rs:341`…) | grep |
| Contraseña generada y guardada `0600` en `~/.cache/memory-industry/pgpass` | `setup.rs:266-293` |
| Test del estado del contenedor: sólo `parse_container_status` (puro). **Nada** prueba los argumentos de `docker run` ni el flujo con un `docker` falso | `setup.rs:476-508` |

Diferencias que importan: Postgres es **obligatorio** (falla → `exit(1)`); el grafo es **opcional** (falla → el servidor arranca igual).
Postgres admite `CUBA_PG_BIND` (`setup.rs:222-228`); para el grafo no habrá equivalente (§2.3).

### 1.2 Cómo se configura hoy el grafo

| Qué | Ruta:línea |
|---|---|
| Variables `MEMORY_INDUSTRY_GRAPH_DB` / `_URL` / `_NAME` (+ alias `CUBA_*`); por defecto `off` | `graph_db.rs:89-125` |
| `graph.env` en `~/.config/memory-industry/` (Windows: `AppData/Roaming/…`); **no pisa** una variable ya definida en el entorno | `graph_db.rs:50-87` |
| Se carga al arrancar, antes del runtime | `main.rs:119-124`, `startup_environment_order.rs` |
| `graph set falkor|neo4j|off`: escribe `graph.env` con la URL por omisión `redis://127.0.0.1:6389` (falkor) o `http://127.0.0.1:7474` (neo4j) y no arranca nada | `graph_cli.rs:105-132` |
| `graph help` manda a `docker compose --profile graph up -d` | `graph_cli.rs:93-96` |
| Compose: servicio `graph`, perfil `graph`, `falkordb/falkordb:latest`, `127.0.0.1:6389:6379`, volumen `memory-industry-graph:/data`, 0,5 CPU, 512 M, **sin healthcheck** | `docker-compose.yml:31-45` |
| `doctor`: `graph_db` ok/fail/ok(off) según `status_summary()`; el fail aconseja el compose | `doctor.rs:1251-1286` |
| `setup check` (`setup_agent.rs:154`) audita **configs de clientes MCP**, no Postgres ni grafo: hoy no sabe nada del grafo | `setup_agent.rs:154-200` |
| README: sólo tres filas de variables (`README.md:343-345`); **no hay una sección «Graph»**, ni en `rust/README.md`, `packaging/README.md`, `docs/` ni en `memory-industry.env.example` | grep |
| `git log -S"graph up"` y `-S"memory-industry-graph"`: sólo `e0dc719` (0.25.0, el compose) y refactors de HOME. **No está hecho** | git |

### 1.3 Distancia hasta «se instala solo»

Hoy, para tener grafo un usuario npm tiene que: conseguir el `docker-compose.yml` del repo, correr `docker compose --profile graph up -d`,
correr `memory-industry graph set falkor`, reiniciar el cliente MCP y, sin que nada se lo diga, correr `graph reconcile` para poblarlo.
Cuatro pasos, dos de ellos fuera del producto, y ninguno comprobado por `doctor`. Distancia: **un comando** (`graph up`) más el
código de §2, más la cobertura de §3 para que «encendido» signifique «al día». Sin §3 el aprovisionamiento daría un grafo vacío o rancio que
el usuario no sabría distinguir de uno sano.

### 1.4 Por qué el grafo «encendido» hoy no sirve a fondo (hallazgos nuevos, no están en B8)

**H-G1. Escritores que no proyectan.** `grep` de `INSERT INTO brain_entities|brain_relations`, `DELETE FROM brain_entities|brain_relations`
y `UPDATE brain_entities SET name` fuera de `graph_db.rs`: unas 40 líneas en 13 ficheros. Proyectan **sólo** `alma.rs:75` (create), `puente.rs:73,90,127` y
`artefacto.rs:259,301`. No proyectan: `alma update` (renombrar, `alma.rs:~97`) y `alma delete` (`:~120`), `forget.rs:61`, `ingesta.rs:697,728`
(auto-extracción: el escritor desatendido más productivo), `codegraph_cli.rs:163,229,267` (cientos de relaciones por corrida),
`graph/autolink.rs:110` (REM), `sync/chunk.rs:545,566` y `handlers/sync.rs:1694,2005,2605` (importar), `dedupe_cli.rs:357…1181` (fusionar),
`cronica.rs:843,1194,1246`, `eco.rs:592,706,778,832`, `zafra.rs:539,586`, `decreto.rs:33`.
(Leí las líneas de `alma`, `forget`, `ingesta` y `autolink`; de `cronica`, `eco`, `zafra`, `decreto`, `sync` y `dedupe` sólo el `grep`: el
implementador abre cada una y confirma si toca entidades/relaciones o sólo observaciones.)
Consecuencia: el grafo de FalkorDB envejece desde el primer `cuba_ingesta`; `faro` multi-hop razona sobre una foto vieja.

**H-G2. Los borrados no existen en el grafo.** No hay `unproject_entity`; `forget` y `alma delete` borran en Postgres y dejan el nodo y
sus aristas. `reconcile` (`graph_db.rs:772-905`) sólo hace `MERGE`: un nodo fantasma sobrevive para siempre, incluso a un `reconcile`.
Un nodo borrado por «olvido» (derecho a borrar) sigue saliendo en `faro` como vecino y `observations_for_entities` ya no encuentra sus
observaciones: el nombre se filtra sin contenido.

**H-G3. Identidad del nodo = `name` sin proyecto.** La migración `0064_entities_are_unique_per_project.up.sql:1-12` quitó la unicidad global
(`UNIQUE NULLS NOT DISTINCT (name, project_id)`). El grafo clava `MERGE (e:Entity {name: …})` (`graph_db.rs:438-453`; la rama `project_id: Some`
no tiene llamante, B8 §1.1) y `reconcile` lee **todas** las entidades sin filtrar proyecto (`:802`). Dos proyectos con una entidad `api` colapsan
en un nodo, sus relaciones se mezclan, y `faro` en el proyecto A recibe como vecinos nombres del proyecto B;
`observations_for_entities` (`faro.rs:1173-1196`) trae por **nombre**, sin filtrar proyecto. La ruta de Postgres respeta el alcance; la del grafo
no. **Es un defecto de alcance (scope) que sólo existe con el grafo encendido**, y encenderlo «a fondo» lo convierte en fuga entre proyectos.
Si RLS lo corta en `observations_for_entities` (no lo comprobé) queda al menos la fuga de nombres por `ppr_names`/`paths`.

**H-G4. Un fallo de lectura es indistinguible de «sin vecinos».** `faro.rs:821,827`, `puente.rs:151`, `hipotesis.rs:57` usan `if let Ok(..)`; con el
grafo encendido y caído, el cliente recibe la respuesta de Postgres sin aviso. B8 §7 lo deja para C4; aquí se necesita (§3.3).

**H-G5. `reconcile` es upsert-sólo y no es incremental.** Cuesta O(todas las filas) por llamada (3 recorridos paginados); B8 lo hace veraz pero
no resuelve H-G2.

## 2. Diseño mínimo: provisionar FalkorDB igual que Postgres

Decisiones técnicas mías, cada una con su reversión. Ninguna cambia lo que ve la planta salvo el interruptor (§7).

### 2.1 Módulo y reparto

Un módulo nuevo **`rust/src/graph_provision.rs`** (hermano de `setup.rs`: Postgres se queda como está; no se refactoriza `setup.rs` «de paso»).
Contiene (nombres orientativos):

- Constantes: `GRAPH_CONTAINER = "memory-industry-graph"`, `GRAPH_VOLUME = "memory-industry-graph"` (mismos nombres que el compose, para que
  quien ya lo usó **adopte** el contenedor y el volumen en vez de duplicarlos), `GRAPH_PORT = 6389`, `GRAPH_IMAGE` (§2.2).
- `graph_run_args(port: u16) -> Vec<String>`: **función pura** que devuelve los argumentos de `docker run`. Es lo que se prueba sin Docker.
- `ensure_graph(port) -> Result<GraphState, ProvisionError>`: no-op si ya responde, `start` si existe parado, `run` si no existe; espera la
  disponibilidad con el **propio** sondeo RESP (`PING`), no con `docker exec` (la imagen no necesita `redis-cli` aquí). Tope 30 s.
- `stop_graph()` y `purge_graph()`.

Reutiliza (no copia) `inspect_container`/`ContainerState` de `setup.rs`: se vuelven `pub(crate)` y se parametrizan por nombre
(ya reciben el nombre: `inspect_container(name)`, `setup.rs:336`). Un cambio de visibilidad, no de comportamiento.

### 2.2 El contenedor (contrato de `graph_run_args`)

| Punto | Decisión | Por qué / reversión |
|---|---|---|
| Nombre | `memory-industry-graph` **estable** | **Desvío de tu encargo (`{proyecto}-{rol}-{AAAAMMDD}`):** ese formato es el de la guardia que protege **mi** shell cuando yo lanzo contenedores; el producto, en la máquina del usuario, tiene que **encontrar** su contenedor por nombre en la siguiente ejecución, como hace `memory-industry-db` (`setup.rs:4`, `:336`). Con fecha en el nombre cada día crearía un contenedor nuevo, abandonaría el volumen y duplicaría memoria. Los contenedores que lanza el QA a mano sí llevan el nombre con fecha (§6). Se revierte cambiando una constante, pero rompe la búsqueda. |
| Imagen | `falkordb/falkordb:v4.22.0@sha256:4ac83f55062d364dcf151692635138b0bd2e12578019a56eb86ef53d067b116b` | **Candidata, NO verificada por mí con `docker pull`**: la leí de la API pública de Docker Hub (`hub.docker.com/v2/repositories/falkordb/falkordb/tags`) a través de una herramienta que resume la página. Tags vistos el 2026-10-28: `v4.22.3` (publicado **hoy**), `v4.22.2` (10-20), `v4.22.1` (10-15), `v4.22.0` (09-30, digest de índice multi-arquitectura arriba), `v4.20.7` (09-24). Elijo la más vieja de la serie 4.22 por prudencia de cadena de suministro (<2 semanas = sin tiempo de que la comunidad la mire). El implementador **vuelve a leer** el digest en el momento, lo compara y lo pega en el handoff; si difiere, manda el que lea él y lo anota. `docker run` acepta `tag@sha256:`; el digest es el que ancla. Se actualiza con un commit que cambie **una** constante (el compose la lee de la misma, §2.6). |
| Dependencia nueva | La imagen es una dependencia de cadena de suministro nueva **para el producto** | **Antes de implementar:** `socket` (`depscore`) y `osv` sobre la imagen/versión, y mirar el changelog de FalkorDB entre `v4.22.0` y `v4.22.3` por correcciones de seguridad. Si salen avisos, la pieza no empieza. No hay dependencias de Cargo nuevas. |
| Puerto | `-p 127.0.0.1:6389:6379`, **literal**, sin variable de entorno que lo cambie | El servidor RESP no tiene `AUTH` ni TLS (B8 §7). Un `0.0.0.0` lo publicaría en la LAN (con red `mirrored` de WSL2 se ve desde la planta). El test exige que ningún argumento contenga `0.0.0.0` ni un `-p` sin IP. Nunca `-P` (la imagen expone además un puerto de interfaz web: no se publica). 6389 y no 6379 para no chocar con un Redis del usuario (igual que el compose). |
| Volumen | `-v memory-industry-graph:/data` (volumen **nombrado**) | **El punto de montaje `/data` lo trae el compose existente y no lo verifiqué contra la imagen** (podría ser otro directorio de datos). Q-real lo comprueba (crear, reiniciar el contenedor, leer). Si el montaje fuera incorrecto no se pierde verdad: el grafo es proyección y `graph reconcile` lo reconstruye; sólo se pierde tiempo. |
| Reinicio | `--restart unless-stopped` | Como Postgres. |
| Recursos | `--memory 512m --cpus 0.5` | Los del compose. FalkorDB guarda el grafo en RAM: **no medí** cuánto ocupa un grafo real (el de producción de Leandro, ~266 entidades, es minúsculo). Si pasa el tope Docker lo mata y reinicia: el síntoma son `failed_ops` y `reconcile` (§4). Sin variable nueva hasta que alguien lo mida. |
| Healthcheck | ninguno en Docker; espera por `PING` | Un fallo de arranque se ve como «no responde en 30 s». |

### 2.3 Interruptor y modo por defecto

- **Apagado por defecto.** Sin `graph.env` nada cambia: ni Docker, ni puerto, ni memoria. Es el comportamiento de hoy
  (`backend() == Off`, `graph_db.rs:89-100`) y no se toca.
  Razones: Docker puede no existir (el producto soporta `DATABASE_URL` externo y modo nube sin Docker, `setup.rs:14-26`), cuesta 512 MB, la
  imagen es una dependencia nueva sin historial en este repo, y Postgres es la verdad: ver §7 si prefieres otra cosa.
- Interruptor = **un comando**: `memory-industry graph up`.
  1. Comprueba Docker (`docker --version`); si no está, error con el mismo texto que `setup.rs:36-53` y **sin** escribir `graph.env`.
  2. `ensure_graph(6389)`.
  3. Escribe `graph.env` con `MEMORY_INDUSTRY_GRAPH_DB=falkor`, `MEMORY_INDUSTRY_GRAPH_URL=redis://127.0.0.1:6389` y
     `MEMORY_INDUSTRY_GRAPH_PROVISION=docker` (nueva; marca que **este producto** gestiona el contenedor).
  4. Si el grafo está vacío y Postgres tiene filas, corre `reconcile` y muestra el informe de B8. Con `--no-reconcile` no.
  5. Imprime el estado y la línea que reinicia el cliente MCP (las variables viven en `graph.env`, que el daemon carga al arrancar).
- `memory-industry graph down`: `docker stop` (deja el volumen), escribe `MEMORY_INDUSTRY_GRAPH_DB=off` en `graph.env`.
  `graph down --purge`: además `docker rm` y `docker volume rm`. Sólo pierde una **proyección** (se reconstruye con `graph up`), pero borra
  un volumen, así que exige la bandera explícita y sólo toca el contenedor/volumen **con los nombres propios**; si el contenedor existe
  pero **no** tiene la etiqueta `org.memory-industry.managed=true` (la que `graph_run_args` pone con `--label`), no lo toca y lo dice.
- `graph set falkor|neo4j|off` **sigue igual** (apunta a un servidor que el usuario gestiona: no hay `PROVISION`). `graph set falkor` deja de
  mandar al compose en su ayuda y manda a `graph up`.
- **Arranque (`serve`, cada comando que abre base):** si `backend() == Falkor` **y** `PROVISION == docker` **y** la URL es la gestionada
  (`redis://127.0.0.1:6389`): `ensure_graph` **en segundo plano**, con tope, y **sin** `exit(1)`. Si falla: `tracing::warn!`, `last_error` y
  `failed_ops` del grafo (B8) lo dicen, el servidor arranca y Postgres sigue siendo la verdad. Un grafo caído **nunca** impide arrancar la memoria.
  Si `PROVISION` falta (el usuario apuntó a su propio servidor), no se toca Docker en absoluto.
- Se despliega en `main.rs` junto a `resolve_database_url` (`main.rs:358`), no dentro de él: Postgres no aprende nada del grafo.

### 2.4 Qué hace `doctor`

Reemplaza el bloque `graph_db` (`doctor.rs:1251-1286`) por estados distintos, cada uno con su consejo exacto:

| Situación | Check | Mensaje / consejo |
|---|---|---|
| `off` | ok | «grafo apagado (Postgres es la verdad). Para encenderlo: `memory-industry graph up`» |
| falkor, `PROVISION=docker`, Docker ausente | **warn** | «el grafo gestionado necesita Docker» + consejo de instalar o de `graph set falkor` con tu propio servidor |
| falkor, `PROVISION=docker`, contenedor ausente/parado | fail | `memory-industry graph up` (sustituye al `docker compose --profile graph …`, que ya no se recomienda) |
| falkor, alcanzable, `failed_ops == 0` | ok | `reachable · projected_ops=N · failed_ops=0` |
| falkor, alcanzable, `failed_ops > 0` o `last_error` | **warn** | `projected_ops=N failed_ops=M last_error=…` + `memory-industry graph reconcile` (B8) |
| falkor, alcanzable, pero el puerto **no es FalkorDB** (`GRAPH.LIST` da error) | fail | «:6389 responde pero no es FalkorDB» (`GRAPH.LIST` está en la documentación de FalkorDB según recuerdo; **no lo verifiqué**, Q-real lo confirma) |
| falkor, alcanzable, **desfase**: nodos en el grafo ≠ entidades en Postgres | **warn** | «el grafo está desfasado: N entidades en Postgres, M en el grafo» + `graph reconcile` (§3.2). Un `MATCH (n:Entity) RETURN count(n)` por `GRAPH.RO_QUERY` |
| `graph.env` dice falkor pero una variable de entorno del cliente MCP dice `off` (el entorno **gana**: `graph_db.rs:80-82`) | **warn** | «MEMORY_INDUSTRY_GRAPH_DB=off en el entorno anula graph.env» |
| imagen en uso ≠ imagen fijada | warn | «el contenedor usa otra imagen que la fijada: `graph down --purge && graph up`» (`docker inspect --format {{.Config.Image}}`) |
| `docker` no responde dentro de `doctor` | warn, no fail | doctor no se cuelga: tope por comando |

`doctor` sigue siendo de **sólo lectura**: no arranca contenedores.

### 2.5 Qué hace `setup check`

`setup check` audita clientes MCP, no infraestructura (`setup_agent.rs:154`). Sólo se añade **una sección** al final, «Grafo»:
lee `graph.env`, y reporta (sin escribir nada) la divergencia del último caso de §2.4 (un bloque `env` de un cliente que fija
`MEMORY_INDUSTRY_GRAPH_DB`/`_URL` distinto de `graph.env`; hoy el cliente gana y nadie lo ve). Si `graph.env` no existe, no imprime nada.
`setup` no gana subcomando `graph`: es `graph up`. (Alternativa rechazada: que `setup` pregunte «¿grafo?» en interactivo; el producto no tiene
asistente interactivo y se añadiría una superficie por una pregunta que se contesta con un comando.)

### 2.6 Compose, README y contrato

- `docker-compose.yml` usa la **misma** imagen fijada y añade la etiqueta. Un test de contrato (en el estilo de `doc_contract.rs`) lee
  `docker-compose.yml` y exige que su `image:` del servicio `graph` sea idéntica a `GRAPH_IMAGE` del código: una sola fuente, no dos.
  Sin ese test, la próxima subida de versión cambiaría un sitio y no el otro.
- README: una sección «Graph projection (FalkorDB)» con `graph up` / `down` / `reconcile` / `status` y qué hace cada uno; la tabla
  de variables añade `MEMORY_INDUSTRY_GRAPH_PROVISION`. `doc_contract.rs` ya cruza las variables que el código lee con las documentadas:
  la variable nueva tiene que salir en la tabla.
- `memory-industry.env.example`/`packaging/`: **no** se tocan (el grafo vive en `graph.env`, no en el env file del servicio).
  Riesgo: el servicio systemd/Windows necesita ver el mismo `HOME` que el usuario para leer `graph.env` (`envs::home()`); se comprueba en
  Q-real con el daemon instalado, no se supone.
- `graph help`: «Postgres remains source of truth» + los tres comandos.

## 3. Usarlo a fondo

### 3.1 Qué lecturas usan el grafo hoy (verificado), y cuándo

| Lectura | Dónde | Con el grafo encendido |
|---|---|---|
| `cuba_faro` multi-hop/asociativo | `faro.rs:809-830` (`multi_hop_context`, llamado desde `:1033`) | PPR alrededor de las entidades de la consulta + camino y bola de saltos; añade hasta 3 observaciones de los nodos que PageRank encontró |
| `cuba_puente traverse` | `puente.rs:151` | Falkor primero (`provenance: "falkor"`), cae a la ruta de Postgres si falla o vuelve vacío |
| `cuba_hipotesis explain` | `hipotesis.rs:57` | Falkor primero, cae a Postgres |
| CLI `graph traverse`/`tokens` | `graph_cli.rs:27-92` | Falkor; **sin** Postgres de reserva |
| `status`/`whoami`/`/health` | `whoami.rs:67`, `http.rs:3083` | `status_summary()` (el `/health` público recorta `last_error`, `http.rs:2369`) |

Es decir: **las herramientas de lectura sí aprovechan el grafo cuando está encendido; lo que no hay es forma de saber si lo hicieron con datos
sanos.** El trabajo no es añadir herramientas: es que lo encendido sea correcto.

### 3.2 Lo que hay que construir para que sea correcto (tajadas G2, G3, G4)

**G2 — cobertura (H-G1, H-G2, H-G5).**
- Un solo helper de escritura **por fila** y su inverso: `project_entity` ya existe; se añaden `unproject_entity(name, project)` (con
  `DETACH DELETE` de **ese** nodo, plantilla constante, valor por `cypher_literal` de B8) y el renombrado (`unproject` + `project`).
  Se cablean: `alma update`, `alma delete`, `forget`.
- Para los escritores en lote (`codegraph build`, `ingesta auto_extract`, REM autolink, `sync import`, `dedupe` fusiones, `eco`, `cronica`,
  `zafra`, `decreto`): **no** se cablea fila a fila (unas 40 líneas, cada una con su transacción). Al terminar la operación, si `backend() == Falkor`,
  llaman a **una** función, `graph_db::resync_after_bulk(pool)`, que corre `reconcile`. Los errores salen por el mecanismo de B8
  (`graph_projection_failed`). Por qué no incremental ahora: `reconcile` es O(filas) pero con el grafo del usuario (cientos/miles de
  nodos) cuesta segundos; **no medí** su tiempo con 10 000 entidades. Si pasa de un umbral que el implementador mida (propongo 2 s con
  1 000 entidades, a ajustar con el dato), se difiere a la próxima pasada de REM en vez de ir en línea. Se revierte quitando la llamada.
- `reconcile` pasa a ser **autoritativo** con `--rebuild`: `GRAPH.DELETE <nombre>` y reproyectar todo (cura los nodos fantasma de H-G2).
  Sin `--rebuild` conserva su comportamiento de B8 (upsert). Durante el `--rebuild` las lecturas ven un grafo parcial; `puente`/`hipotesis`
  caen a Postgres y `faro` pierde contexto multi-hop un momento. Se imprime una línea que lo avisa. (Alternativa: construir en un grafo
  paralelo y `GRAPH.COPY`/renombrar: **no verifiqué** que exista en la versión fijada; se rechaza por eso.)
- `graph up` ejecuta `reconcile` si el grafo está vacío (§2.3).

**G3 — identidad por proyecto (H-G3).**
- El nodo se identifica por `(name, project)` donde `project` es el `project_id` como texto o `""` (el caso `NULL`). La plantilla pasa a
  `MERGE (e:Entity {name: $n, project: $p})`; la arista `REL` une los dos nodos **del mismo proyecto de sus entidades en Postgres**.
  La proyección toma el `project_id` de la propia fila (`alma create` ya lo calcula, `alma.rs:61`).
- Las lecturas reciben el proyecto actual (`crate::project::current_project_id`, ya usado por `alma`) y filtran `WHERE e.project = $p`.
  Si el proyecto es `NULL` (sin proyecto ligado), ve sólo `""`, **igual que** la ruta de Postgres.
- `observations_for_entities` (`faro.rs:1173`) también filtra por el proyecto actual: es el segundo hueco.
- El **grafo ya existente** de quien lo usó antes de G3 tiene nodos sin `project`: `reconcile --rebuild` los reemplaza; `doctor` marca el
  desfase por el contador; `graph up` sobre un grafo antiguo lo dice. Es una **migración de datos de una proyección**: no toca Postgres.
- Depende de B8 tajada A (`cypher_literal`, plantillas constantes). Si B8 elimina la rama `project_id: Some` de `project_entity_full`
  (B8 §7), **G3 la devuelve** con la forma de arriba: se coordina para que B8 A no la borre si G va a continuación, o se reescribe en G3.

**G4 — el fallo de lectura se ve (H-G4).** Con `backend() == Falkor` y una lectura (`faro`, `puente traverse`, `hipotesis explain`) que
devuelve `Err`, la respuesta lleva `graph_degraded: true` y `graph_error` **sólo cuando pasó** (la convención de C4: una llamada sana no lleva la
clave; `degraded.rs`) y sigue sirviendo la ruta de Postgres. Un resultado vacío **correcto** no es `degraded`. Con `Off`: ni clave ni coste.

### 3.3 Lo que NO se añade y es una decisión de producto (§7)

FalkorDB sabe hacer cosas que la ruta de Postgres no (camino más corto entre dos entidades, comunidades, subgrafos por consulta Cypher). Una
herramienta nueva o una acción nueva (p. ej. `cuba_puente action=path from=A to=B`) cambiaría lo que ven los agentes y la superficie MCP
(`mcp.md`: nombres, `inputSchema`, descubrimiento). **No se especifica ahora.** Lo que sí queda hecho: lo que existe pasa a ser correcto,
al día, con alcance de proyecto y con avisos.

**Paridad (prueba de «usar a fondo»):** `puente traverse` y `hipotesis explain` tienen dos implementaciones (BFS sobre Falkor y SQL).
Con los mismos datos y profundidad 1..3 deben devolver el mismo conjunto de nodos. Hoy nadie lo comprueba. Es un test de QA manual contra el
contenedor real (Q-real), con el resultado pegado en el handoff.

## 4. Gherkin

Fondo de la tajada G1: «un `docker` falso en un directorio temporal al frente de `PATH` (script que anota sus argumentos en un fichero y
responde según un fichero de estado: `Up …`, `Exited …`, vacío, o `exit 1`), `HOME` en un directorio desechable, y un servidor RESP falso en
`127.0.0.1:0` que contesta `+PONG`, y `*0` a `GRAPH.LIST`». El puerto viaja como parámetro (`ensure_graph(port)`), nunca el 6389 en tests.

```gherkin
# language: es
Característica: el contenedor del grafo se crea como se prometió

  Escenario: los argumentos de docker run
    Dado la función que arma los argumentos con el puerto 6389
    Cuando se leen
    Entonces contienen "-p" seguido de "127.0.0.1:6389:6379"
    Y ningún argumento contiene "0.0.0.0" ni es "-P"
    Y el nombre es "memory-industry-graph" y el volumen "memory-industry-graph:/data"
    Y la imagen es exactamente la constante, con "@sha256:" y 64 hexadecimales, y no termina en ":latest"
    Y lleva "--restart unless-stopped", "--memory 512m" y la etiqueta "org.memory-industry.managed=true"

  Escenario: el compose y el código dicen la misma imagen
    Dado el servicio "graph" de docker-compose.yml
    Entonces su "image:" es idéntica a la constante GRAPH_IMAGE

Característica: graph up y graph down

  Escenario: primera vez
    Dado que no existe el contenedor y Docker responde
    Cuando se ejecuta "graph up"
    Entonces el docker falso recibe exactamente un "run" con los argumentos de arriba
    Y graph.env contiene falkor, redis://127.0.0.1:6389 y MEMORY_INDUSTRY_GRAPH_PROVISION=docker
    Y sale con código 0

  Escenario: ya existe parado
    Dado un contenedor "Exited (0)"
    Cuando se ejecuta "graph up"
    Entonces el docker falso recibe "start" y ningún "run"

  Escenario: ya está corriendo
    Dado un contenedor "Up 3 hours"
    Cuando se ejecuta "graph up" dos veces
    Entonces no hay ni "run" ni "start", y graph.env queda igual (idempotente)

  Escenario: no hay Docker
    Dado que "docker --version" falla
    Cuando se ejecuta "graph up"
    Entonces sale con código distinto de 0 con el texto de instalar Docker
    Y graph.env no existe ni cambió

  Escenario: el puerto lo ocupa algo que no es FalkorDB
    Dado un servidor RESP que contesta PONG pero error a GRAPH.LIST, y el contenedor no existe
    Cuando se ejecuta "graph up"
    Entonces no hay "run", sale distinto de 0 y el mensaje dice que ese puerto no es FalkorDB

  Escenario: el contenedor no arranca a tiempo
    Dado un servidor que nunca contesta al PING
    Cuando se ejecuta "graph up"
    Entonces vuelve en menos de 40 segundos con código distinto de 0 y dice "docker logs memory-industry-graph"

  Escenario: down conserva el volumen
    Cuando se ejecuta "graph down"
    Entonces el docker falso recibe "stop" y ningún "rm" ni "volume rm", y graph.env dice off

  Escenario: purgar exige la bandera y respeta lo ajeno
    Dado un contenedor llamado "memory-industry-graph" sin la etiqueta de gestión
    Cuando se ejecuta "graph down --purge"
    Entonces no hay "rm" ni "volume rm" y el mensaje explica por qué

Característica: el arranque no depende del grafo

  Escenario: el grafo gestionado falla al arrancar
    Dado graph.env con falkor y PROVISION=docker y un docker falso que sale con 1
    Cuando arranca el servidor contra una base desechable
    Entonces el servidor atiende y la respuesta de "status" lleva last_error del grafo
    Y una llamada a cuba_alma create funciona

  Escenario: un servidor propio no se toca
    Dado graph.env con falkor y sin PROVISION
    Cuando arranca el servidor
    Entonces el docker falso no recibe ningún comando

  Escenario: apagado por defecto
    Dado que no existe graph.env
    Cuando arranca el servidor y se ejecuta doctor
    Entonces no se invoca docker y el check graph_db es ok con el texto "graph up"

Característica: doctor y setup check cuentan la verdad

  Esquema del escenario: estados de doctor
    Dado <situación>
    Cuando se ejecuta doctor
    Entonces el check graph_db es <nivel> y su consejo contiene "<consejo>"
    Ejemplos:
      | situación                                             | nivel | consejo                       |
      | graph.env off                                         | ok    | graph up                      |
      | falkor, PROVISION, contenedor parado                  | fail  | graph up                      |
      | falkor alcanzable, failed_ops 2                       | warn  | graph reconcile               |
      | falkor alcanzable, 40 entidades en PG y 25 en el grafo | warn  | graph reconcile               |
      | falkor alcanzable, el puerto no es FalkorDB           | fail  | no es FalkorDB                |
      | entorno del cliente dice off y graph.env falkor       | warn  | anula graph.env               |

  Escenario: doctor no escribe
    Cuando se ejecuta doctor con cualquier situación
    Entonces el docker falso no recibe "run", "start", "stop" ni "rm"

Característica: lo encendido está al día (G2)

  Escenario: borrar una entidad la borra del grafo
    Dado la entidad "A" con una relación a "B", proyectada
    Cuando se borra "A" con cuba_alma delete
    Entonces el servidor falso recibe un único DETACH DELETE de ese nodo con cuerpo constante y el literal "A" aparte

  Escenario: olvidar una entidad la borra del grafo
    Cuando se ejecuta cuba_forget sobre "A"
    Entonces ocurre lo mismo

  Escenario: renombrar
    Cuando se renombra "A" a "A2"
    Entonces el servidor recibe el borrado de "A" y la proyección de "A2"

  Escenario: un lote termina con el grafo al día
    Dado el grafo encendido y "codegraph build" que crea 5 entidades y 4 relaciones
    Cuando termina
    Entonces el servidor falso recibió la proyección de las 5 entidades y 4 relaciones, y un fallo de ese paso sale como graph_projection_failed

  Escenario: reconcile --rebuild quita fantasmas
    Dado un grafo con el nodo "Z" que ya no existe en Postgres
    Cuando se ejecuta "graph reconcile --rebuild"
    Entonces el servidor recibe GRAPH.DELETE antes de la primera proyección y "Z" no se vuelve a proyectar

Característica: el grafo respeta el proyecto (G3)

  Escenario: dos proyectos, una entidad con el mismo nombre
    Dado "api" en el proyecto P1 con una relación a "db1" y "api" en P2 con una relación a "db2"
    Cuando se proyectan ambos
    Entonces el servidor recibe dos MERGE distintos, con project "P1" y "P2"

  Escenario: faro en P1 no ve vecinos de P2
    Dado el grafo de arriba (servidor falso con filas por proyecto) y la consulta de P1 sobre "api"
    Entonces los vecinos devueltos son "db1" y no "db2"
    Y la consulta Cypher lleva el filtro de proyecto como literal, nunca concatenado

  Escenario: las observaciones de los nodos también
    Dado dos entidades "api" con observaciones distintas en P1 y P2
    Cuando faro en P1 expande los nodos
    Entonces sólo salen observaciones de P1

Característica: el fallo de lectura se ve (G4)

  Escenario: el grafo está caído y se pregunta
    Dado graph.env falkor y un servidor que cierra sin responder
    Cuando se llama a cuba_puente traverse
    Entonces la respuesta sigue siendo la de Postgres y lleva graph_degraded true y graph_error

  Escenario: sin vecinos de verdad
    Dado un grafo sano sin vecinos para "A"
    Cuando se llama a traverse
    Entonces la respuesta no lleva claves graph_*

  Escenario: apagado
    Dado el grafo apagado
    Cuando se llama a faro, puente traverse y hipotesis explain
    Entonces ninguna respuesta lleva claves graph_*
```

## 5. Procedimiento QA (con sabotajes)

### 5.1 Sin Docker real (entran al juez, `--lib` donde se pueda)

Lo que se prueba sin contenedor: (a) los **argumentos** (`graph_run_args`, función pura: Q-G1); (b) el **flujo** con un `docker` falso:
un script `sh` escrito por el test en un directorio temporal que se antepone a `PATH` con `ScopedEnv` bajo `GLOBAL_STATE_GUARD`
(`envs.rs:62-87`, como `setup.rs:517-519`); `setup.rs` ya llama a `Command::new("docker")`, que resuelve por `PATH`, así que no hace falta
una abstracción nueva (`nucleo.md`: nada de un trait con una sola implementación); (c) el **sondeo** con un servidor RESP falso en `127.0.0.1:0`
(B8 §5.0: nunca un puerto fijo ni uno sin oyente). Los tests de CLI (`graph up` como proceso) van en `rust/tests/` con `HOME` desechable
y **su propio** guardián de entorno (`smoke_test.rs:405-480` los escanea; leerlo antes). Los de `doctor` usan `tests/doctor_world/mod.rs`,
que ya neutraliza `MEMORY_INDUSTRY_GRAPH_DB`/`CUBA_GRAPH_DB` (`:26-27`); la variable `PROVISION` entra en esa lista.

| # | Qué | Debe pasar | Cubre |
|---|---|---|---|
| Q-G1 | `graph_run_args(p)` | contiene `127.0.0.1:{p}:6379`; ninguna cadena `0.0.0.0`; sin `-P`; imagen con `@sha256:` y 64 hex; sin `:latest`; etiqueta | puerto, imagen |
| Q-G2 | Contrato compose ↔ código | misma imagen | una sola fuente |
| Q-G3 | `graph up` con docker falso: no existe / parado / corriendo ×2 / sin docker / puerto ajeno / no arranca | según Gherkin; exactamente los comandos esperados (el falso los registra); `graph.env` sólo cuando procede | aprovisionamiento |
| Q-G4 | `graph down` y `--purge` con y sin etiqueta | sin `rm` salvo con bandera **y** etiqueta | seguridad de datos |
| Q-G5 | Arranque con docker que falla | el servidor atiende; `last_error` presente; sin `exit` | no fatal |
| Q-G6 | `doctor` en los seis estados de la tabla | nivel y consejo; **sin** comandos que escriban | doctor |
| Q-G7 | `setup check` con un cliente que fija `GRAPH_DB=off` y `graph.env` falkor | línea de divergencia; sin `graph.env`, ninguna línea | setup check |
| Q-G8 | G2: `alma delete`, `forget`, renombrar, `codegraph build`, `ingesta auto_extract`, `sync import`, `dedupe` con servidor falso | el servidor ve el borrado o la proyección | cobertura |
| Q-G9 | `reconcile --rebuild` | `GRAPH.DELETE` primero; el fantasma no vuelve | H-G2 |
| Q-G10 | G3: dos proyectos con `api` en base desechable y servidor falso con filas por proyecto | dos nodos; faro de P1 no ve `db2`; observaciones de P1 | H-G3 |
| Q-G11 | G4: servidor que cierra / sano sin vecinos / apagado | `graph_degraded` sólo en el primero | H-G4 |

### 5.2 Con FalkorDB real (**manual, una vez, antes de cerrar; no entra al juez**)

El repo prohíbe `#[ignore]` y los saltos, y el juez no corre Docker real. Se corre a mano y el resultado va en el handoff de QA; si falla,
la pieza no cierra. Contenedor de QA con la guardia: `--name memorys-falkor-qa-20261028` (o la fecha del día) y `-p 127.0.0.1::6379`; **el
puerto lo asigna el sistema**; la prueba apunta `MEMORY_INDUSTRY_GRAPH_URL` ahí. **El agente especificador no ejecutó nada de esto.**

| # | Qué | Debe pasar |
|---|---|---|
| Q-real1 | `graph up` de verdad (usa el 6389: sólo en una máquina donde esté libre) y `docker inspect` del contenedor | imagen = constante con su digest; publicado `127.0.0.1:6389`; etiqueta; límites |
| Q-real2 | Crear 3 entidades y 2 relaciones, reiniciar el contenedor, `graph traverse` | los datos siguen (valida `/data`) o, si no, `graph reconcile` los repone y se **anota** que el volumen no persistía |
| Q-real3 | `GRAPH.LIST` contra la imagen fijada | existe y responde (valida la sonda de «no es FalkorDB») |
| Q-real4 | Los 13 nombres hostiles de B8 Q9 sobre el contenedor de §2.2 | idénticos a B8 |
| Q-real5 | **Paridad:** datos con ≥ 30 entidades y ciclos; `puente traverse` y `hipotesis explain` con el grafo encendido y apagado, profundidad 1, 2 y 3 | mismo conjunto de nodos; si difiere, se documenta la diferencia y se decide cuál es la correcta |
| Q-real6 | Dos proyectos con `api`; `faro` en cada uno | sin cruce de vecinos ni de observaciones |
| Q-real7 | `graph down`, `graph up`; luego `graph down --purge`, `graph up` | datos recompuestos por `reconcile`; el volumen se recrea |
| Q-real8 | Daemon instalado (systemd o tarea) con `graph.env` en el `HOME` del usuario | el daemon ve el grafo; `/health` con token lo dice |
| Q-real9 | Medir RAM del contenedor (`docker stats --no-stream`) con el grafo del usuario y con uno sintético de 10 000 entidades | se pega el número; decide si 512 m basta |

### 5.3 Sabotajes (cada uno debe poner un test en rojo)

| # | Sabotaje en producción | Test que debe ponerse rojo |
|---|---|---|
| SG1 | `graph_run_args` publica `0.0.0.0:6389:6379` o quita la IP | Q-G1 |
| SG2 | La imagen vuelve a `:latest` o pierde el digest | Q-G1 y Q-G2 |
| SG3 | El compose cambia de imagen y el código no | Q-G2 |
| SG4 | `graph up` hace `run` aunque el contenedor ya exista | Q-G3 (parado / corriendo) |
| SG5 | `graph up` escribe `graph.env` aunque no haya Docker | Q-G3 |
| SG6 | Un fallo de `ensure_graph` en el arranque llama a `exit(1)` | Q-G5 |
| SG7 | `--purge` borra sin comprobar la etiqueta, o `down` borra el volumen | Q-G4 |
| SG8 | `doctor` arranca el contenedor | Q-G6 («doctor no escribe») |
| SG9 | `doctor` da ok con `failed_ops > 0` o con el desfase | Q-G6 |
| SG10 | `alma delete` / `forget` dejan de desproyectar | Q-G8 (uno por handler) |
| SG11 | Un escritor en lote vuelve a no llamar a `resync_after_bulk` | Q-G8 (uno por escritor) |
| SG12 | `reconcile --rebuild` omite el `GRAPH.DELETE` | Q-G9 |
| SG13 | El `MERGE` vuelve a `{name}` sin `project` | Q-G10 |
| SG14 | Se quita el filtro de proyecto de la lectura o de `observations_for_entities` | Q-G10 |
| SG15 | El `degraded` sale también en una lectura sana sin vecinos, o no sale con el grafo caído | Q-G11 |
| SG16 | Con el grafo apagado, una respuesta lleva `graph_*` | Q-G11 |

Los tests se escriben **antes** (pasada 1 del implementador); el orquestador los corre y ve el rojo contra el código actual y commitea
`test(rojo)`. Después va el código sin tocar los tests (`swarm-forge.md`).

## 6. Tajadas, orden y riesgos

Todas después de **B8 A, B, C y D** (G reutiliza `cypher_literal`, `failed_ops`, `graph_projection_failed` y el tiempo de conexión de D:
sin él, `ensure_graph` y el sondeo podrían colgar un worker con una IP que no responde, B8 R5). Antes de G1: **socket + osv + changelog de la imagen** (§2.2).

| Tajada | Contenido | Ficheros | Tests | Orden |
|---|---|---|---|---|
| **G0 (previa)** | socket/osv de `falkordb/falkordb` en la versión elegida; releer el digest; elegir tag definitivo | ninguno | evidencia en el handoff | primero, sin código |
| **G1: instalar** | `graph_provision.rs`, `graph up/down`, `PROVISION`, arranque no fatal, `doctor`, `setup check`, compose pinneado + contrato, README | `graph_provision.rs` (nuevo), `graph_cli.rs`, `setup.rs` (sólo visibilidad), `main.rs`, `doctor.rs`, `setup_agent.rs`, `docker-compose.yml`, `README.md`, `lib.rs` | Q-G1…Q-G7; SG1…SG9 | 1.º |
| **G2: al día** | `unproject_entity`, renombrado, `resync_after_bulk`, `reconcile --rebuild` | `graph_db.rs`, `handlers/{alma,forget,ingesta,cronica,eco,zafra,decreto,sync}.rs`, `codegraph_cli.rs`, `dedupe_cli.rs`, `sync/chunk.rs`, `graph/autolink.rs` | Q-G8, Q-G9; SG10…SG12 | 2.º, tras G1 (toca muchos handlers: **en serie**, un implementador) |
| **G3: por proyecto** | identidad `(name, project)` en escritura y lectura, `observations_for_entities` | `graph_db.rs`, `handlers/faro.rs`, `alma.rs`, `puente.rs` | Q-G10; SG13, SG14 | 3.º (toca `graph_db.rs`: serie con G2) |
| **G4: el fallo se ve** | `graph_degraded` en faro/puente/hipotesis | `handlers/{faro,puente,hipotesis}.rs`, `degraded.rs` | Q-G11; SG15, SG16 | 4.º (comparte `faro.rs`/`puente.rs` con G3: serie) |
| **QA real** | Q-real1…9 | — | manual | al final; resultado en el handoff de QA |

Si hay que cortar: **G1 sin G2/G3 no se publica como «grafo instalable»**: daría un contenedor que parece sano y razona sobre datos rancios y
mezclados entre proyectos. El corte razonable es G1+G2+G3 y diferir G4 (el aviso), que no corrompe nada. G3 es lo único de la lista que es
**un defecto de seguridad de alcance** y no una mejora: no se aplaza detrás de G4.

### Riesgos

- **Imagen nueva sin auditar por mí**: dependencia de cadena de suministro (digest sin verificar, tag reciente). Mitigación: G0, digest, etiqueta de
  gestión, 127.0.0.1, sin `AUTH` pero sin salir de la máquina. El riesgo residual de FalkorDB sin `AUTH` en loopback: cualquier proceso local con acceso
  al puerto lee y escribe el grafo; es el mismo modelo de confianza que el Postgres en `127.0.0.1:5488`, y no se amplía.
- **Puerto 6389 ocupado** por otra cosa: `graph up` lo detecta (Gherkin) y no pisa nada.
- **`--purge` sobre algo ajeno**: la etiqueta de gestión lo impide.
- **`/data` no es el directorio de datos de la imagen** (sin verificar): sólo cuesta tiempo (reconcile), no datos; Q-real2 lo cierra.
- **512 m**: sin medir; Q-real9.
- **`reconcile` en línea tras cada lote**: coste sin medir; umbral y diferido (§3.2).
- **`graph.env` y el daemon**: se supone el mismo `HOME`; Q-real8.
- **Cambiar el modo de un grafo existente (G3)**: los nodos antiguos sin `project` quedan hasta `--rebuild`; `doctor` lo avisa.
- **CRAP**: `doctor.rs` ya es larga (`run_checks_with`); el bloque `graph_db` se extrae a una función propia al tocarlo (el cambio obliga),
  y la base de `HEAD` no puede subir. `main.rs`/`setup_agent.rs` están excluidos de mutación (`quality-gate.sh`): la decisión que importa
  (`graph_run_args`, las reglas de `--purge`, qué estado de `doctor` toca) **vive en `graph_provision.rs`**, que sí se muta. Se **añade** `graph_provision.rs`
  al alcance de `cargo mutants --in-diff` y no a la lista de exclusión.
- **Claves nuevas** (`failed_*`, `graph_degraded`, `PROVISION`): se añaden, ninguna existente cambia de nombre; `cli_contract.rs:192` (`graph` imprime su uso) debe
  seguir verde con los subcomandos nuevos.

## 7. Lo que NO entra, y qué es de Leandro

**No entra:**
- Neo4j: B8 lo deja seguro por construcción y Leandro lo mantiene; **G no lo aprovisiona** (no hay imagen verificada, su API HTTP exige autenticación y
  el cuerpo de B8 §1.5 sigue sin confirmar; `graph set neo4j` sigue siendo «apunta a tu servidor»). Si se quiere el mismo `graph up` para Neo4j,
  es una pieza aparte con su propia verificación.
- `AUTH`/TLS hacia FalkorDB, o cambiar el puerto/`bind` por variable.
- Una herramienta MCP o acción nueva que aproveche Cypher (camino más corto, comunidades): §3.3.
- Cola de reintentos o proyección diferida (B8 §7); incrementalidad fina de `reconcile`.
- Cambios a `setup.rs` más allá de visibilidad; empaquetar el contenedor en npm/`postinstall` (instalar un paquete npm no debe levantar Docker; lo hace `graph up`).
- Reemplazar el cliente RESP (B8 §2.4).

**Decisiones mías (técnicas, con su reversión):** nombre estable del contenedor (§2.2); imagen candidata `v4.22.0@sha256:…` en lugar de `latest`
(se cambia una constante); apagado por defecto (se invierte con una línea, §7); arranque no fatal (una línea); `--purge` sólo con etiqueta; reconcile
en línea tras lotes con umbral medido; `--rebuild` en lugar de grafo paralelo; `GRAPH.LIST` como sonda; G3 antes que G4.

**Decisión que SÍ es de Leandro (una sola, de alcance de producto):**
**¿El grafo va encendido por defecto en una instalación nueva donde hay Docker?** Hoy lo pidió «instalado con el proyecto». Recomendado: **no en 0.32**.
Que se instale con el proyecto = que `graph up` lo deje funcionando con **un** comando verificable, no que cada usuario cargue 512 MB y una imagen
nueva sin que lo pida. Reabrir cuando existan los números de Q-real5 (paridad) y Q-real9 (memoria) y se pueda decir qué gana el usuario a cambio.
Consecuencias: *apagado* (recomendado) = nada cambia para quien no lo pide; *encendido* = la primera ejecución de cualquier comando descarga e
inicia una imagen extra y cualquier fallo de Docker aparece en `doctor` de gente que no lo eligió. Mientras responde, la pieza avanza con apagado.

## 8. Handoff (para que lo escriba el orquestador en `.cursor/handoffs/`)

El encargo limitaba la escritura a este fichero; el YAML queda aquí, listo para copiar (sólo después de que B8 A–D estén en la rama y G0 esté hecho):

```yaml
from: especificador
to: implementador
type: note
task: "0.32 G: el grafo FalkorDB se instala solo (graph up/down, graph_provision.rs, imagen fijada por digest, arranque no fatal, doctor y setup check) y es correcto cuando esta encendido (borrados y lotes proyectados, reconcile --rebuild, identidad por proyecto, graph_degraded); spec en backlog/0.32-especificaciones/G-grafo-instalable.md (tajadas G1, G2, G3, G4 en serie, tras B8 A-D y G0 socket/osv)"
commit: none
evidence: "none (solo lectura: Read de B8, setup.rs, graph_cli.rs, graph_db.rs parcial, doctor.rs, setup_agent.rs, docker-compose.yml, npm/postinstall.js; grep de escritores de brain_entities/brain_relations y de graph_db:: en rust/src; git log -S; WebFetch de la API de tags de Docker Hub (falkordb/falkordb), resumida por la herramienta: digest NO verificado con docker pull; no se compilo ni se ejecuto docker ni cargo)"
```

## Decisión de Leandro (2026-10-28)

El grafo viene **apagado por defecto y `memory-industry setup` lo ofrece** (pregunta sí/no; con un «sí» queda levantado y configurado).
`memory-industry graph up` lo levanta en un comando. Sin cambio de modo para quien no lo pida. Se revierte con `graph down`.
