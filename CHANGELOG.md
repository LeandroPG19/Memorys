# Changelog

All notable changes to cuba-memorys are documented here. Format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), versioning follows
[SemVer](https://semver.org/) for the Rust crate (`Cargo.toml`). PyPI
versioning is independent (~ +1.0 offset since v0.6.0 era to allow wheel
revisions without binary changes).

## [Unreleased]

### Cursor: initialize en SSE dejaba todos los chats en 405

Cursor anuncia `roots` en `initialize`. El daemon respondía ese handshake
como `text/event-stream` con un `roots/list` encima. El cliente Streamable
HTTP de Cursor daba el initialize por bueno (Settings verde) y el POST
siguiente (`tools/call`) salía 405; al reintentar, SSE 404. Un chat nuevo
reutiliza esa conexión rota. `initialize` vuelve a ser JSON siempre; las
roots se siguen adoptando si el cliente las manda luego en JSON-RPC.

### Un nombre fusionado volvía a la vida al escribir sobre él

`dedupe --apply` mueve todo al ganador y guarda el nombre perdedor en
`brain_entity_aliases` — una tabla que se escribía y **nadie leía**. Como
`ensure_entity_typed` buscaba solo por `brain_entities.name`, la siguiente
observación escrita con el nombre viejo creaba una entidad nueva y **deshacía la
fusión en silencio**. Ahora resuelve por alias antes de crear, con una consulta
que solo corre cuando el nombre no existe.

### La puerta escribe su propio código de salida

`scripts/run-all-tests.sh` deja el código en `~/.cache/cuba-gate/run.exit`,
capturado antes de que la limpieza pise `$?`, y fuera de `/tmp` porque `/tmp` se
barre al reiniciar. Dejárselo a quien la lanza no funciona: el 16-ago-2026 se
lanzó tres veces con `nohup` sin capturar nada y el resultado se leyó de un
fichero que no iba a aparecer. Con contrato en `doc_contract.rs`, que además
exige que el trap se arme antes del primer paso que puede fallar.

### Una migración ya publicada no se puede editar

`a_migration_that_has_already_shipped_cannot_be_edited` congela con SHA-384 el
conjunto de migraciones hasta la 0060. sqlx hashea cada fichero aplicado y se
niega a arrancar contra una base que corrió otra versión, así que editar una
inutiliza toda instalación existente — no el CI, que siempre migra desde cero.
Ya pasó con `e96df5d`, y en esta misma sesión un arreglo de una palabra en un
comentario de la 0031 estuvo a punto de repetirlo.

### Muestreo del juez: el test no podía fallar

`sample_observations_spans_the_whole_timeline` sembraba exactamente
`JUDGE_SAMPLE_SIZE` observaciones, así que `ntile(6)` daba un cubo por fila y el
test pasaba estratificara o no la consulta. Ahora siembra el triple con
importancia decreciente: sin estratificar, la muestra se queda en el tercio más
antiguo y el test lo dice.

### Menos superficie

- `search::rrf::fuse` y `RankedResult`: sin un solo llamante fuera de sus dos
  tests, mientras `faro.rs` reimplementa RRF en línea. Una segunda
  implementación no usada invita a arreglar la que no corre.
- `create-app-role.sql` se nombraba en cuatro sitios y no existía. Las
  migraciones 0031 y 0055 lo mandan correr y **no se pueden editar** —están
  aplicadas y sqlx hashea el fichero—, así que en vez de corregir el texto se
  corrigió la realidad: el SQL vive ahora en `scripts/create-app-role.sql`, donde
  esos comentarios siempre dijeron, y el binario lo embebe desde ahí con
  `include_str!` en lugar de al revés. `psql -f scripts/create-app-role.sql` y
  `cuba-memorys secure` son la misma copia. Verificado ejecutándolo: deja
  `cuba_app` con `rolsuper=f, rolbypassrls=f`.
  `a_file_an_applied_migration_tells_you_to_run_still_exists` impide que vuelva a
  moverse; si falta, el crate ni siquiera compila.
- `README.md` ofrecía `anthropic_api` como valor de `CUBA_JUDGE`: `judge.rs` lo
  hace caer al caso por defecto sin avisar. `rust/README.md` describía un
  directorio `scripts/` que ya no existe.

### Una migración nueva ya no puede repetir la deuda de la 0.22

`rust/tests/migrations_contract.rs` escanea `migrations/*.up.sql` por
`CREATE TRIGGER` y `ADD CONSTRAINT` sin guarda — ninguno de los dos admite
`IF NOT EXISTS` — y falla si el fichero no está en la lista de excepciones ya
aplicadas. 0057 y 0059 llegaron sin guarda después de que la 0.23.0 escribiera
la deuda en el CHANGELOG; ahora una migración nueva con el mismo defecto rompe
la puerta en vez de sumarse a la lista. Las 14 migraciones ya aplicadas y no
reaplicables (0029 en adelante) quedan excusadas con su motivo, porque
editarlas rompe el checksum SHA-384 de sqlx en toda base que ya las corrió.

### Documentos que prometían lo que el binario no tiene

- `rust/Cargo.toml:description` (lo que viaja a crates.io) anunciaba «Allen
  interval + ADWIN drift + MI tagging» y «optional Prometheus /metrics»: `mi
  tagging` y `/metrics` no existen en el código desde hace versiones, y Allen y
  ADWIN nunca tuvieron un llamante fuera de sus propios tests. También decía
  «29 tools» cuando el binario por defecto produce 28. Corregido, y
  `rust/Cargo.toml` se suma al contrato de conteo de tools en `doc_contract.rs`
  — antes se quedaba fuera del escaneo y por eso llevaba dos versiones
  mintiendo sin que nada lo notara.
- `server.json` ofrecía `ANTHROPIC_API_KEY` y `anthropic_api` como valor de
  `CUBA_JUDGE`: la feature que los leía se borró en la 0.23.0, y
  `CUBA_JUDGE=anthropic_api` caía al caso por defecto sin ningún aviso. Fuera.

### 383 líneas sin un solo llamante

`cognitive::adwin`, `cognitive::allen` y `cognitive::calibration` — ya
señaladas en `docs/PLAN-MEJORAS-v0.11.md` y sobrevivientes a trece versiones
mientras sus compañeras de esa lista (`mi_tagging`, `temporal_query`) sí se
borraron. Verificado con `grep -rw` sobre `src/`, `tests/` y `examples/`: fuera
de sus propios `#[cfg(test)]`, la única mención en todo el repo eran las tres
líneas que las declaraban en `cognitive/mod.rs`.

### Seis ficheros huérfanos, ~78 KB

`scripts/enable-rls.sql` (duplicaba a mano lo que ya hacen las migraciones
0017/0031/0055, sin nada que avisara si divergían), `scripts/gen-eval-dataset.py`,
`rust/scripts/migrate_v3.sql` (migración v2.x→v0.3.0, el proyecto va por
0.24), `rust/tests/datasets/beam_prepare.py` y `brain_qa_es.jsonl` (52 KB, solo
lo nombraba el script anterior), `rust/examples/bench_handlers.rs`. Cada uno
verificado con `grep -rn` sobre todo el repo antes de borrarlo.

## [0.24.0] — 2026-08-15 (Cargo `0.24.0` · npm `0.24.0` · PyPI `1.26.0`)

El grafo crece solo, y lo que escribe un modelo entra en cuarentena. Esa frase es
la versión entera; todo lo demás es lo que hizo falta para que fuera cierta y
segura. Dos revisiones adversarias sobre el propio trabajo encontraron seis
agujeros, uno de ellos el día antes de la primera prueba entre dos máquinas.

### El ciclo REM era lo único periódico, y casi no llegaba a correr

Su primera consolidación ocurría **cuatro horas después de arrancar**, porque el
bucle consumía el primer tick del `interval` —que en tokio resuelve al instante—
y esperaba al segundo. Bajo stdio, donde un proceso rara vez vive tanto, **no
corría jamás**, y cada reinicio de la máquina ponía el contador a cero. Medido en
el grafo vivo: **96 de 322 entidades** habían sido escaneadas alguna vez, 226
esperando, a 5 por ciclo — una semana de cola.

Ahora el primer ciclo llega a los cinco minutos (`CUBA_REM_FIRST_DELAY_SECS`), el
lote sube a 20 mientras haya 50 o más esperando, y dentro corren tres cosas que ya
existían y solo se ejecutaban a mano: **auto-extracción, detección de comunidades
y conteo de duplicados**. Adelantarlo no daña nada: el decay es
`EXP(-0.693 · transcurrido / 86400)`, decae por tiempo pasado y no por número de
ejecuciones.

### Lo que un modelo escribe entra cuarentenado, siempre

`auto_extract` es lo único que convierte una conversación en hechos, y **no tenía
disparador**: dependía de que una IA se acordara de teclearlo, y el SKILL
instalado ni lo menciona.

Antes de esto, `brain_observations.trust` tenía `DEFAULT 'trusted'` y **las 1902
filas eran `trusted`: la cuarentena existía y no se había usado ni una vez**. Sin
marcar, lo que un modelo inventara sería indistinguible de lo que escribió una
persona, alimentaría la búsqueda, y llegaría al grafo — porque el escaneo de
relaciones solo lee filas confiables. Cuarentenado, `faro` lo retiene en sus ocho
ramas hasta que `cuba_eco promote` lo asciende, y `jornada start` dice cuántas
esperan revisión, solo cuando hay alguna.

**Y no escribe entidades ni aristas.** No hay forma de marcarlas:
`brain_entities` no tiene columna de confianza y el `CHECK` de
`brain_relations.provenance` no admite un valor de cuarentena — y ningún
consumidor leería ninguno de los dos. Una marca que nadie lee es el mismo agujero
con otro nombre.

### La cuarentena no sobrevivía al viaje *(encontrado un día antes de la prueba)*

El bundle lleva el `trust` con el que se escribió cada fila, y el import **lo
tiraba**: recalculaba mirando solo si el texto parecía una credencial. Lo que la
auto-extracción escribe es cuarentena **porque lo escribió un modelo**, no porque
parezca peligroso — así que **un solo sync habría convertido cada alucinación de
la máquina A en memoria confiable y buscable en la B**. Ahora el resultado es el
más restrictivo de los dos.

### El reranker: el catálogo prometía uno que estaba apagado

`constants.rs` decía a cada modelo que el rerank se auto-activa con solo tener el
ONNX. No era cierto: hacía falta además `CUBA_MODE=completo`. En una instalación
limpia el reranker estaba apagado **y el modelo creía que estaba encendido**, así
que nunca lo pedía — y es la mayor mejora de calidad que este proyecto ha medido,
**+93% nDCG**.

Encenderlo era la mitad fácil. La condición obvia habría roto **todos los binarios
publicados de Linux y Windows**, que llevan CUDA: `gpu::status().degraded` solo
comprueba que el fichero del proveedor exista y que haya driver. Dónde corre de
verdad lo decide `wants_gpu(Workload::Reranker)`, que lee `CUBA_RERANK_DEVICE`.
Con ese knob en `cpu`, `degraded` es falso, el default se habría encendido, y cada
búsqueda costaría **106,9 s contra un presupuesto de 20** — y el timeout ni
siquiera lo recupera: `spawn_blocking` no se cancela y retiene el mutex de la
sesión mientras la siguiente búsqueda hace cola.

`doctor` distingue ahora las tres razones por las que puede estar apagado, que se
leían igual: no hay modelo, no hay GPU real, o **el plan de recursos lo desconectó
por RAM en el instante del arranque** — decisión que dura toda la vida del proceso
y que solo aparecía en una línea de log. Visto ocurrir con 373 MB libres.

### Dos búsquedas que informaban de éxito dando menos

- **Sin modelo de embeddings**, `vector_search` devolvía `Ok(vec![])`: la rama
  vectorial no aportaba nada y la respuesta salía sin marca. Una instalación nueva
  parecía sana y buscaba solo por léxico. El propio `doctor` ya lo llamaba «la
  búsqueda vectorial devuelve vacío en silencio».
- **`enrich_graphrag` repetía la misma entidad** tantas veces como resultados suyos
  hubiera —tres entradas idénticas en una búsqueda medida— y hacía una consulta por
  resultado. Ahora deduplica y resuelve todos los vecinos en una sola consulta,
  conservando el tope de 5 **por entidad**.

### El ciclo, visible

Era el motor del sistema y no se veía por ningún lado. `brain_rem_cycles` guarda
una fila por ciclo, y el panel separa lo que antes era idéntico: **nunca corrió**,
**corrió sin nada que hacer**, y **corrió con su LLM rindiéndose**.

### El juez del dedupe decidía con 450 caracteres

`dedupe --judge` moría a mitad con `end byte index 90 is not a char boundary; it
is inside 'é'` — cortaba por bytes una razón escrita en español. Había decidido ya
24 de 31 pares; todos perdidos.

Peor que la caída: **la fusión dependía del orden**. Cada veredicto producía su
grupo y se aplicaban en secuencia, así que cuando el juez dijo A≡B, A≡C y B≠C —lo
que hizo— B y C acababan dentro de A, que es justo lo que había rechazado.

Y se equivocaba. Veía **tres observaciones cortadas a 150 caracteres**: 450
caracteres para juzgar 161 memorias. Por eso declaró la misma entidad a
`Mapupita-Web` y `Mapupitta-Web` — dos proyectos distintos, sin un día de
solapamiento, en comunidades de Leiden separadas, con pagerank 0,798 contra 0,0 y
un coseno máximo de 0,681. Y por eso **3 de 22 veredictos cambiaban** entre dos
corridas sobre los mismos datos.

Ahora ve seis observaciones de 300 caracteres repartidas por toda la línea
temporal, más las señales objetivas que sí acertaron: fechas, sesiones, proyecto,
y el coseno **medio** entre ambos conjuntos. Y usa la confianza que el modelo ya
devolvía **y el código descartaba**, sin una sola llamada extra. Vuelto a correr,
llama distintos a los dos pares que antes habría fusionado, y explica por qué.

Para lo que el juez frena a propósito y una persona sí ha verificado:
`dedupe --merge A --into B`, con dry-run por defecto.

### El perfil `lean` estaba elegido a ojo

Llevaba `cuba_receta` (puesto 9, 32 llamadas) y **dejaba fuera `cuba_decreto`**
(puesto 3, 114). Reconstruido con 33 días de uso real: las diez más llamadas más
las dos meta — **12 anunciadas, el 93,5% del uso**, y el catálogo baja de 31.838 a
15.604 caracteres.

### Y el CI miraba 25 de 62 ficheros de test

Treinta y siete nunca corrían allí mientras el job informaba en verde sobre los
commits que los añadían. **Tercera vez que esta clase muerde**: la puerta local
tenía el mismo bug a 30 de 52. Ahora descubre `tests/*.rs`, con cuatro exclusiones
razonadas. Pasar a un glob heredó los ficheros **sin el entorno que la puerta
construye alrededor**: tres de ellos exigen un segundo nodo y se niegan a saltarse,
porque un test de dos nodos que pasa en uno no prueba nada. El job lo provisiona.

### Lo que esta versión NO arregla

El conteo de duplicados del ciclo es O(n²) — 140 ms con 325 entidades, ~12 s a
3.000. Corre una vez cada cuatro horas y el grafo crece a una entidad al día.
Los episodios y errores **no transportan `trust`** en el bundle: es una brecha del
formato, no del import, y no afecta a la auto-extracción, que escribe
observaciones. Y el `eval` smoke de la puerta imprimió cinco `different vector
dimensions 384 and 1024` con el modelo cargado, que **no se ha conseguido
reproducir** fuera de ella con tres entornos distintos: el nDCG de esa corrida es
solo léxico y queda como hallazgo abierto.

## [0.23.0] — 2026-08-13 (Cargo `0.23.0` · npm `0.23.0` · PyPI `1.25.0`)

Cuatro auditorías adversarias contra la 0.22.0, corridas horas después de
publicarla. Encontraron una pérdida de datos, tres fugas de contenido **entre
proyectos**, un demonio que tira trabajo al morir, dos agentes que podían
compartir sesión, y una puerta que elegía qué mirar.

### La 0.22.0 salió con un bug de pérdida de datos

Al importar, si el nombre de una entidad ya existía aquí con otro identificador,
el remapeo se aplicaba a las observaciones, a los episodios y a los dos extremos
de cada relación — **y no a los hechos**. El hecho sobrevive como texto y se lee
bien; lo que pierde es `subject_entity_id`, que es justo para lo que esa columna
existe: buscar hechos por entidad, `find_similar_entities`, recorrer el grafo.

Silencioso, y solo visible si después vas a buscar hechos por entidad.

### Un proyecto podía leer el texto de otro, por tres caminos

Antes de nada: **con la instalación por defecto RLS está inerte**. `.env.example`
conecta como `cuba`, que es `SUPERUSER` con `bypassrls`. Esto muerde a quien
corrió `create-app-role.sql` y conecta como `cuba_app`, que es el modo
recomendado y el que empuja `doctor`. Quien mida «cero filas filtradas» como
`cuba` no ha medido nada.

- **Los resúmenes de compactación eran la peor, y no necesitaban ninguna
  sincronización.** El servidor listaba los últimos veinte de **todos** los
  proyectos como recursos MCP —repartiendo sus identificadores— y luego servía
  `summary_md` con `WHERE id = $1` y nada más. `summary_md` es el registro
  destilado de una sesión entera.
- **`brain_sync_conflicts` guarda texto de observaciones literal** y no tenía
  política, así que `cuba_sync conflicts` devolvía exactamente lo que la política
  de `brain_observations` negaba por la puerta principal.
- **La caché del juez contestaba antes de que nadie comprobara nada**: buscaba
  por dos identificadores y devolvía el razonamiento del modelo sobre ambas
  observaciones; la lectura protegida por RLS corría **después**.

Migración 0055: los tres, más cuatro tablas de contenido que ya filtraban en
código (`brain_procedures`, `brain_wm`, `brain_observation_chunks`,
`brain_embedding_stats`) y que ahora también lo hacen por debajo.

**Lo que a propósito NO lleva política**, escrito dentro de la migración:
`brain_audit_log` no puede —su cadena de hash es lineal sobre todas las filas, y
esconder una hace que `cuba_archivo verify` **denuncie manipulación donde no la
hay**—; `brain_peer_notices` la escribe un par remoto sin proyecto aquí; y el
único lector de `brain_handler_failures` corre con el ámbito vacío.

### Una credencial pegada a una palabra pasaba la puerta de escritura

El escáner parte por espacios, quita los caracteres **no alfanuméricos** del
principio y mira si lo que queda empieza por el prefijo. `"ghp_..."` se cazaba;
`Authorization:ghp_...` no, porque empieza por letra y no se recorta nada. Y ese
mismo escáner es el que sostiene `refuse_secrets`: **no era solo el redactor
dejando salir una credencial, era la puerta dejándola entrar**.

Ahora busca el prefijo dentro de cada tramo, lo que cierra gratis un segundo
agujero que un test documentaba como límite conocido: el JSON compacto.

### Dos IA en el mismo proyecto

Ya funcionaba lo básico —misma base, lo que una escribe la otra lo ve en la
consulta siguiente—. Faltaba que se enteraran sin ahogarse:

- **`cuba_faro` marca qué resultados son nuevos** desde que abriste sesión, con
  un array de índices: **11 tokens**. La forma obvia —`created_at` en cada fila—
  cuesta 230 medidos con el mismo `cl100k_base` que usa el presupuesto, en la
  herramienta más llamada del servidor. Un UUID cuesta lo mismo que una fecha, así
  que listarlos habría sido igual de caro. **Sin sesión no aparece el campo**, ni
  vacío: ese es el caso mayoritario y tiene que costar cero.
- **El punto de referencia no costó ni una consulta**: la que lee los objetivos de
  la sesión ya corría en cada búsqueda, y ahora trae también `started_at`.
- **El canal de recados ya existía**: `cuba_centinela` con `on_session_start`
  guarda el mensaje, `jornada start` lo entrega y `max_fires` lo cierra solo.
  Cero herramienta nueva, **cero tokens de catálogo**. La 0056 le añade lo único
  que le faltaba: de quién viene, y un tope.
- **Dos clientes sin `Mcp-Client-Id` compartían sesión.** Toda instancia de Claude
  Code manda el mismo `clientInfo.name`, así que caían en la misma clave y
  heredaban la sesión de la otra — pizarra incluida. Una identidad adivinada ya no
  registra sesión: perderla es honesto, heredar la ajena no.
- **Y la pizarra se fugaba**: un agente sin sesión **leía y borraba** la memoria de
  trabajo de los demás. El filtro dejaba pasar todo con `session_id` nulo, así que
  `clear` sin etiqueta era `DELETE FROM brain_wm` entero.

### El demonio tiraba trabajo al morir

`serve` salía con `exit(1)` **sin drenar**; la rama stdio sí drenaba. Lo que se
pierde es el embedding que cada escritura encola: la fila guardada, el vector
nunca calculado, la búsqueda dejando de encontrarla. El contrato que debía
impedirlo nombraba dos comandos a mano y `serve` no era uno.

### La puerta elegía qué mirar

Enumeraba **30 de 52** ficheros de test a mano, y los quince de integración
escritos para la sincronización y el panel **nunca habían corrido en ella** —
mientras informaba en verde sobre los commits que los añadían. Ahora descubre
`tests/*.rs`: **44 ficheros**.

Eso destapó de inmediato un test que nunca había corrido y no podía pasar: pedía
50 candidatos y daba por hecho que el suyo entraba, afirmaba una relación con
entidades que no creaba, y sobre todo **dependía de lo que contestara un LLM
local**, que bajo la puerta pierde la carrera por la CPU. Reintenta tres veces,
porque una aserción inestable en una puerta enseña a reintentarla.

### Quitado

- **`observability` y `anthropic-api`**: ninguna aparecía en la matriz de release,
  así que en todo binario publicado `record_handler` compilaba a nada, el
  exportador de Prometheus no existía y `ANTHROPIC_API_KEY` no hacía nada —
  mientras el README documentaba `CUBA_METRICS_PORT` como si funcionara. El panel
  ya da lo que daban, por el mismo endpoint autenticado y sin un segundo puerto
  que restringir.
- **219 líneas más sin un solo llamante**: `temporal_query`, `entity_linking`,
  `mi_tagging`. Con `cas.rs` van **592 borradas** en dos versiones.

### Y tres carreras entre tests, que son la misma forma

Una variable de proceso con dos dueños. `CUBA_SYNC_DIR` hacía fallar al
exportador; `CUBA_PANEL` hacía que un test que afirma que el panel **no** se
sirve recibiera un 200. Un contrato lo vigila ahora, y cuenta guardias
**tomados**, no declarados: la primera versión miraba la declaración y un
sabotaje que borró todos los `.lock()` pasó en verde.

### Y la puerta podía tumbar la base viva

Descubierto corriendo la puerta de esta misma versión: abortó con código 2 **sin
un solo test en rojo**, y la base `brain` pasó por recovery.

Provisionaba sus bases desechables con `docker exec cuba-memorys-db psql`. Lo
que dispara el reinicio **no** es que ese proceso salga con código distinto de
cero —medido: `docker exec <c> false` no provoca nada—, sino que **quede
huérfano**: entonces lo adopta el postmaster, y un hijo adoptado que muere mal se
lee como un backend caído. Pasa en cuanto el `docker exec` se interrumpe: matar
la puerta, reiniciar la máquina, un `trap EXIT` solapado con la corrida
siguiente.

Medido contra un pg18 desechable: huérfano con salida 2 → `untracked child
process ... exited with exit code 2`, `terminating any other active server
processes` y recovery completo; el mismo huérfano con salida 0 → una línea de
log y nada más.

Ahora llega por TCP con el `psql` del anfitrión, que nada puede adoptar, y se
niega a arrancar con el motivo escrito si no lo encuentra. De paso deja de estar
atada al nombre del contenedor: con `docker exec` fijo, apuntar `DATABASE_URL` a
otro sitio provisionaba el servidor equivocado.

`backup-db.sh` y `restore-db.sh` **no se pueden mover**: el `pg_dump` del
anfitrión suele ser más viejo que el servidor (16.14 contra 18.3) y se niega a
volcarlo. El riesgo queda escrito donde alguien lo lee — no matar un backup en
marcha.

### Dos documentos que prometían lo que nadie cumple

- `.env.example` seguía ofreciendo `ANTHROPIC_API_KEY` después de que esta misma
  versión borrara la feature que la leía. El contrato solo vigilaba una
  dirección —que lo que el código lee esté documentado— y una variable que nadie
  lee le es invisible. La dirección inversa es ahora otro contrato.
- Cerrarla destapó que **`RUST_LOG` no estaba documentada en ningún sitio**: la
  lee `EnvFilter::try_from_default_env()`, así que el escáner que busca
  `env::var` nunca la vio, y el lector que la buscaba en el README tampoco.

### Lo que esta versión NO arregla

**7 de las 12 migraciones de la 0.22 abortan si se reaplican** (8 sentencias:
`ADD CONSTRAINT` y `CREATE TRIGGER`, que no admiten `IF NOT EXISTS`; 13 si se
cuenta cada una de las 6 tablas que recorre el bucle de 0045, en vez de la
sentencia dinámica una sola vez). Arreglarlo exige editarlas, y eso rompe el
checksum de sqlx en toda base que ya las tiene. Solo muerde en reparación
manual o restauración sin `_sqlx_migrations`, y falla ruidosamente. Queda
anotado.

## [0.22.0] — 2026-08-13 (Cargo `0.22.0` · npm `0.22.0` · PyPI `1.24.0`)

Dos máquinas comparten su memoria y convergen sin perder nada, un panel para
verlo, y doce migraciones que existen porque la versión anterior se quedaba
callada cuando algo se perdía.

### Lo que había antes no era sincronización

Había un canal: `cuba_sync export/import` sobre un repositorio git. Movía datos
en una dirección y perdía cosas en la otra. Medido, no leído:

- **Un borrado se revertía.** A borra una observación y reexporta; B importa el
  bundle sin ella y **la conserva** (el import no ejecutaba un solo `DELETE`);
  B exporta el suyo y A **la recupera**. Cuatro pasos, dos bases, cero avisos.
- **`merge` era idéntico a `skip`.** Los dos iban a `ON CONFLICT DO NOTHING`, y
  el gancho de git que el proyecto instala usa `merge` por defecto: ganaba quien
  llegó primero, una corrección posterior no llegaba nunca, y el import
  informaba éxito.
- **Un nombre de entidad repetido tumbaba el bundle entero.** `UNIQUE (name)`
  contra `ON CONFLICT (id)`: dos «Postgres» creados por separado y no entraba
  ni una fila.
- **22 de 28 tablas no salían jamás**, entre ellas `brain_facts` con 990 filas.
- **El ciclo no convergía en trabajo**: `access_count` se mueve con el uso, así
  que cada export producía un hash nuevo y las dos máquinas se reimportaban
  para siempre bundles que insertaban cero filas.

### Ahora

- **Lápidas** (0045, 0049): un borrado viaja, y **solo borra las filas que
  nombra** — nunca por cascada. Una lápida de una entidad con 3 hijos en A no se
  lleva los 332 de B: se retiene y se reporta. Por encima del 10% del corpus y
  de 25 filas, para y pide confirmación.
- **Identidad de nodo en la base** (0046), no en el entorno: `HOSTNAME` no está
  exportado y el unit traía `CUBA_NODE_NAME` comentado, así que `origin_node`
  se escribía vacío — 240 filas NULL en el corpus vivo.
- **Reloj por columna** (0047): `BEFORE UPDATE OF content, observation_type,
  trust, evidence, tags`. El decaimiento toca 1097 de 1880 filas cada 4 h y
  **no** mueve el reloj, por construcción y no por convención.
- **Historial al importar** (0048): `overwrite` ya no borra una corrección sin
  rastro; lo pisado va a `previous_versions` con tope de 20, y el embedding se
  invalida cuando cambia el texto que describía.
- **Contadores que ambos lados incrementan** se fusionan con `GREATEST`, no con
  «gana el último»: la suma sería más fiel pero **no es idempotente**, e
  importar dos veces duplicaría el número.
- **Hechos contradictorios**: dos máquinas que discrepan ya no dejan dos
  verdades vigentes. Decide `observed_at`, no el orden de llegada — si no,
  quien sincronizara último decidiría qué es verdad.
- **Conflictos** (0052): lo que el import descartaba se contaba y se olvidaba.
  Ahora se guardan **las dos versiones** y se cierran con
  `cuba_sync resolve --keep ours|theirs|both`.

### Entre máquinas, por HTTP

- **`CUBA_PEER_TOKEN`**: un segundo token que alcanza **solo** los verbos de
  sincronización. Forzado dentro de `dispatch` y no en el borde, porque
  `cuba_call` recibe el nombre de la herramienta como argumento y pasaría
  `{"tool": "cuba_forget"}` por debajo de cualquier lista blanca exterior. El
  demonio se niega a arrancar si es igual al `CUBA_HTTP_TOKEN` del túnel.
- **`pull`**: entrega el bundle dentro de la respuesta, paginado, **sin escribir
  un byte**. No podía ser `export`, que escribe y poda: servir por ahí sería
  darle a un token de solo lectura una forma de borrarte el bundle.
- **`fetch`**: la mitad local. Pagina, aterriza, importa con las mismas
  validaciones, y **guarda el hash** para no volver a importar lo mismo.
- **`notify`**: lo único que un par puede escribir, y no es memoria — ni
  entidad, ni observación, ni vector. Una campana que aparece al abrir sesión.
- **Tiempo real** (0054): `pg_notify` en el reloj de 0047, con carga útil de
  solo identificadores (el tope de NOTIFY son 8000 bytes, duros). El demonio
  escucha en su propia conexión, amortigua 500 ms y avisa al par. `NOTIFY` no es
  duradero, así que el `fetch` sigue siendo el canal; esto solo quita la espera.
- **`cuba_faro` dice cuán vieja es su vista** cuando hay pares, y solo entonces.

### El panel

`GET /panel` sirve una página compilada dentro del binario que **no lleva
datos**: todo lo pide por `POST /mcp` con el mismo token bearer que cualquier
cliente MCP. Estado, diagnóstico completo, quién está conectado, llamadas
recientes y problemas con su acción al lado.

Los datos llegan por métodos `admin/*` y **no** por herramientas nuevas: el
catálogo viaja en el contexto de todos los modelos en cada petición, y un panel
de administración no es algo que un modelo deba invocar. Dos interruptores
apagados por defecto (`CUBA_PANEL`, `CUBA_PANEL_PUBLIC`), y el segundo existe
porque el túnel de Cloudflare apunta a `127.0.0.1`: mirar la dirección del
cliente no distingue nada.

### Medido

- **Import**: 1,41 s → 0,68 s con 322 entidades, 1880 observaciones, 990 hechos
  y 301 relaciones, mismas filas en las cuatro tablas. De ~5500 idas y vueltas a
  unas diez, con `jsonb_to_recordset` — `UNNEST` no sirve porque `tags`,
  `actors` y `artifacts` son `text[]` y un array no se despliega junto a
  escalares.
- **Timeouts**: `statement_timeout`, `lock_timeout` e
  `idle_in_transaction_session_timeout` estaban los tres en 0. Un sync que
  chocaba con otro colgaba para siempre; ahora se rinde a los 10 s y dice por qué.
- **RLS**: con el ámbito fijado, un `INSERT` de otro proyecto **se rechaza**;
  con el ámbito vacío entra. El agujero nunca fue la política — era que el
  camino de sync no declaraba ámbito.

### Quitado

- **`cas.rs` y `transport.rs`**, 373 líneas y 9 tests: cero llamantes desde que
  nacieron, diez versiones menores atrás. Su detección de divergencias la hace
  ahora la tabla de conflictos. Lo que se va de verdad es la deduplicación por
  contenido, y se va por forma antes que por tamaño: el `pull` pagina ficheros y
  `cas` trocea contenido, así que habría que reescribirla igual.

### Lo que esta versión NO hace

- **No sincroniza `brain_audit_log`**: mezclar dos cadenas de hash invalida la
  del destino, y `cuba_archivo verify` dejaría de significar algo.
- **No promete «tiempo real» como eslogan**: promete un número medido de
  caducidad y que la herramienta lo diga.
- **No prueba tres máquinas**. El cursor por par escala, pero solo se midió con
  dos y no se afirma lo que no se midió.
- **`observability` sigue muerta en el binario que se publica**: la feature no
  está en ninguna fila de la matriz de release, así que `record_handler`
  compila a nada. El anillo del panel va fuera de ese `#[cfg]` a propósito.
  (Ya no es cierto desde la 0.23.0, que borró la feature entera en vez de
  arreglarla — ver «Quitado» en esa sección.)

## [0.21.0] — 2026-08-03 (Cargo `0.21.0` · npm `0.21.0` · PyPI `1.23.0`)

Licencia permisiva, dos controles de seguridad que existían pero no ejercían, y
cuatro defectos encontrados midiendo en vez de suponer.

### Apache-2.0: ahora cualquiera puede usarlo, también dentro de una empresa

AGPL era casi con seguridad el techo de adopción. Para un servidor MCP que corre
en la máquina del usuario, el gatillo de red del §13 es discutible en la práctica,
pero sobra para que el departamento legal de cualquier empresa mediana lo bloquee
de entrada — y el público objetivo son ingenieros dentro de manufactureras, que
son justo quienes no pueden instalar software AGPL.

Apache-2.0 y no MIT porque **concede derechos de patente de forma explícita**, que
es la cláusula que revisión legal busca de verdad.

Se hizo ahora porque era verificable: `git log` muestra un único autor en toda la
historia, así que no hay copyright de terceros del que pedir permiso. Después del
primer PR externo habría hecho falta localizar a cada contribuidor.

- **CONTRIBUTING.md** lleva DCO (`git commit -s`): sin CLA, sin cesión de copyright.
  No se añadió CLA a propósito — su utilidad sería reservar la opción de doble
  licencia, y eso es exactamente lo que Apache-2.0 acaba de entregar.
- **SECURITY.md** es nuevo, y su mitad importante es *"Not protected"*.

### RLS y la auditoría append-only no ejercían ningún control

Las migraciones 0016 y 0017 estaban bien escritas y no protegían nada. El runtime
conectaba como `cuba`, y `SELECT rolsuper, rolbypassrls` sobre la base viva
devolvía `t | t`. De ese hecho salían dos consecuencias:

- PostgreSQL deja pasar a superusuarios y roles `BYPASSRLS` por encima de
  row-level security **siempre**. `FORCE ROW LEVEL SECURITY` cubre al propietario
  de la tabla, no al superusuario. El aislamiento entre proyectos dependía solo
  del `WHERE` de cada handler: la segunda muralla no existía.
- El trigger de auditoría evalúa `pg_has_role(current_user,'cuba_admin','MEMBER')`,
  y un superusuario es miembro implícito de **todo** rol. La condición era siempre
  cierta, el `RAISE EXCEPTION` inalcanzable, y `brain_audit_log` una tabla normal
  con `UPDATE` y `DELETE` libres.

La migración 0041 crea `cuba_app` como `NOSUPERUSER NOBYPASSRLS` y hace `REVOKE
UPDATE, DELETE` sobre el log — un privilegio, no solo un trigger. Toda ruta de
fallo conserva la conexión admin en vez de dejar al usuario fuera de su memoria.

### La rectificación GDPR se descartaba en silencio

`brain_audit_block_mutation()` terminaba en `RETURN OLD` y colgaba tanto del
trigger `BEFORE UPDATE` como del `BEFORE DELETE`. Para `DELETE` es correcto. Para
`UPDATE` no: **devolver OLD no cancela la operación**, la deja proceder escribiendo
los valores viejos. PostgreSQL informa `UPDATE 1` y quien llama cree que funcionó.

El caso que rompe es justo el que la migración 0016 documenta como razón de ser
del rol `cuba_admin`: *"emergency rectification (e.g. GDPR cascade)"*. Un operador
redactando un dato personal recibía `UPDATE 1` y ninguna redacción.

### Credencial pública y puerto abierto

`const PG_PASSWORD = "memorys2026"` compilada en el binario y publicada en GitHub,
npm y PyPI, con el contenedor publicando en `0.0.0.0`. Docker escribe sus propias
reglas en la cadena `DOCKER` de iptables, que **puentean UFW**, así que el firewall
del sistema nunca protegió esto.

Ahora: bind a `127.0.0.1` (`CUBA_PG_BIND` para quien necesite lo contrario) y
credencial generada por instalación en `~/.cache/cuba-memorys/pgpass` con modo
0600. Un contenedor preexistente conserva la suya en vez de dejar al usuario sin
acceso.

### Rendimiento: lo medido, incluidas dos correcciones propias

- **Build con GPU: 0,356 s contra 20,669 s de mediana. 58×.** La causa es el
  reranker, no el embebedor: éste es INT8 y el proveedor CUDA no registra kernel
  para `MatMulInteger`, así que corre en CPU haya GPU o no. `scripts/build-gpu.sh`
  fija la feature y el cgroup; `doctor` avisa en amarillo si hay GPU sin usar.
- **Doble embedding eliminado.** Cada observación se embebía dos veces: una en el
  dedup y otra en el insert. En un lote de 100, cien inferencias tiradas.
- **El dedup comparaba vectores de distribuciones distintas** — `embed_passage()`
  contra una columna llena con `embed_passage_contextual()`. El umbral de 0,92
  nunca midió lo que decía medir.
- **`random_page_cost` y `ANALYZE`.** Ningún `last_analyze` existía: el planificador
  creía 20 filas donde hay 1740. Corregido, el índice HNSW pasa a usarse (5,6× en
  ese paso). Se informó como 111×; el número honesto es 5,6× y son ~13 ms de una
  consulta, así que **nunca fue el cuello de botella**.
- **Una consulta redundante por observación** en `batch_add`: ~100 por lote.
- **halfvec**: columna e índice nuevos, rellenados por el ciclo REM. 4,0 kB → 2,0
  kB por fila. Nada lee todavía de ahí: conmutar la lectura es una decisión de
  recall que exige medirla, no una de almacenamiento.
- **El semáforo del reranker mentía**: anunciaba 2 concurrentes mientras un `Mutex`
  los serializaba, y el segundo esperaba dentro de `spawn_blocking` reteniendo un
  hilo. Ahora 1, que es lo que el `Mutex` permite de verdad.

### Integridad y cadena de suministro

- **Cadena de auditoría con clave.** Con `CUBA_AUDIT_KEY` pasa a HMAC-SHA256. Las
  filas anteriores siguen verificando bajo SHA-256, para que activar la clave no
  marque como manipulado todo el historial. **Sube el listón, no cierra el modelo**:
  una clave en el mismo host cae ante el mismo root que alcanza la base.
- **Descargas de modelos verificadas** con SHA-256; un desajuste borra el fichero y
  aborta. Antes se ejecutaban 1,1 GB de pesos sin comprobar nada.
- **`cargo deny` corre en CI.** El fichero de política existía desde hacía releases
  y no lo ejecutaba ningún workflow, así que no se aplicaba a ninguna de las 433
  dependencias. Migrado al esquema `version = 2`.
- **La clave de caché de embeddings no incluía el modelo**: cambiar de modelo servía
  vectores del anterior durante todo el TTL.

### Evaluación

El evaluador declara su propio efecto mínimo detectable, y con n=60 era 0,245 nDCG
— mayor que cualquier mejora pendiente. Con las 221 preguntas baja a **0,110**, 2,2×
más sensible. Además reporta ahora latencia p50/p95: antes daba tokens medios y
máximos, así que un cambio que redujera tokens duplicando el p95 habría parecido
una victoria limpia.

### Migraciones

- **0040** — el trigger de auditoría devuelve NEW en UPDATE.
- **0041** — rol `cuba_app` con privilegios acotados.
- **0042** — columna `halfvec` dimensionada desde el `atttypmod` vivo, más su índice.

318 tests en verde, clippy sin avisos.



### Corregido

- **`packaging/cuba-memorys.service` se quedó atrás en 0.20.0.** La unidad que se
  distribuye seguía siendo la de 0.19.0, así que quien la copiara no obtenía nada
  de lo que anuncia esta versión. Tres problemas concretos: `Restart=always`
  habría relanzado el daemon al instante tras un apagado por inactividad,
  anulándolo por completo; faltaban todas las variables nuevas
  (`CUBA_*_DEVICE`, `CUBA_GPU_MEM_LIMIT_MB`, `CUBA_RERANK_CHUNK`), de modo que la
  colocación por modelo no se aplicaba; y traía `CUBA_NODE_NAME=leandro-pop-dev`
  hardcodeado — el nombre de una máquina concreta en un paquete público, cuando
  el código ya cae al hostname si la variable no está.
- **Añadido `packaging/cuba-memorys.socket`.** El README de 0.20.0 documenta la
  activación por socket pero el paquete no incluía la unidad que hace falta.
  `Restart=on-failure`, `MemoryHigh`/`MemoryMax` según el pico medido (~3,9 GB), y
  `CUBA_IDLE_SHUTDOWN_SECS` comentado con la advertencia de que sin el `.socket`
  habilitado nada vuelve a levantar el daemon.

## [0.20.0] — 2026-07-29 (Cargo `0.20.0` · npm `0.20.0` · PyPI `1.22.0`)

El daemon dejó de ser algo que corre siempre. En la GPU de 6 GB donde se midió
esto pasó de retener **5228 MiB de VRAM desde el arranque** —el 93% de la
tarjeta— a **1470 MiB mientras busca y 0 en reposo**. Ese 93% era la razón por la
que otros programas de GPU dejaban de arrancar: el driver devolvía
`NV_ERR_NO_MEMORY` al crear un canal, que es exactamente donde falla un juego o
una terminal acelerada.

Dos columnas, porque no todo viene gratis: la primera es lo que trae el código
publicado; la segunda añade una variable de entorno y el reranker refusionado que
documenta el README.

| | antes | **defaults 0.20.0** | + `CUBA_RERANK_CHUNK=4` y artefacto fusionado |
|---|---|---|---|
| VRAM buscando | 5228 MiB | **2950 MiB** | **1460 MiB** |
| VRAM en reposo | 5228 MiB | **0** — el proceso no existe | 0 |
| Arranque hasta responder | 11,1 s | **0,027 s** | 0,027 s |
| Búsqueda en caliente | 5,90 s | 5,25 s | **1,70 s** |
| Embedding de una query | 52,3 ms | **35,8 ms** | 35,8 ms |

Ninguna función se quitó.

### Colocación por modelo, no por proceso

`gpu::configure()` registraba CUDA para las tres sesiones ONNX. El README decía
que sin `--features cuda` «todos los modelos corren en CPU», dando a entender que
con el feature los tres irían a la GPU. **Solo el reranker fue nunca.**

- **El embedder no puede usar CUDA.** Viene cuantizado dinámicamente a INT8: 96
  `DynamicQuantizeLinear` alimentando 144 `MatMulInteger`, y el provider CUDA no
  registra kernel para ninguno de los dos (verificado contra el `.so` instalado y
  documentado por Optimum: *«nodes such as MatMulInteger and
  DynamicQuantizeLinear … cannot be consumed by the CUDA execution provider»*).
  ONNX Runtime los particionaba a CPU de todos modos. Registrar CUDA solo
  reservaba un arena en el que el modelo nunca computó: **374 MiB retenidos
  mientras los 544 MB de pesos vivían en RAM del host**, midiendo la sesión
  aislada.
- **El NLI tiene el problema opuesto.** Es FP32 y ahí se queda: mDeBERTa está
  documentado aguas arriba como no compatible con FP16, y la build INT8 devuelve
  entailments falsas con confianza (ya estaba anotado en `nli.rs`). Se invoca
  poco y tolera latencia, así que en CPU cuesta 150-400 ms medidos y libera más
  de un gigabyte de VRAM.
- **Corolario:** cuantizar el reranker a INT8 sería contraproducente — lo
  expulsaría de la GPU igual que al embedder. Su FP16 es la representación
  correcta.

Nuevas `CUBA_EMBED_DEVICE` / `CUBA_RERANK_DEVICE` / `CUBA_NLI_DEVICE` (`cpu` ·
`gpu` · `cpu`) para medir una colocación sin recompilar. `doctor` ahora informa
cuál corre dónde, porque tener GPU no dice nada sobre qué sesiones la usan.

### El arena de CUDA dejó de duplicarse

`ArenaExtendStrategy::NextPowerOfTwo` es el default de ONNX Runtime y reserva en
potencias de dos en lugar de lo que la sesión pidió. Así 1,65 GB de pesos se
convertían en 5+ GB. Ahora se fija `SameAsRequested` con un tope explícito
(`CUBA_GPU_MEM_LIMIT_MB`, 2048 por defecto). **El tope es por sesión**, que es
sostenible solo porque exactamente un modelo pide CUDA.

`CUBA_RERANK_CHUNK` (16) expone el otro extremo: bajo `fixed_shape` cada lote se
rellena a 512 tokens, así que es la palanca principal sobre el arena — 16 → 2938
MiB, 4 → 2364 MiB. Los scores no cambian: una búsqueda `verbose` a 16 y a 4
volvió byte a byte idéntica.

### El reranker carga en su primer lote

Calentar solo el embedder cuesta 0,026 s; calentar los dos, 11 s — el
cross-encoder son 1,08 GB y su warm-up corre un lote real de 50 candidatos. Bajo
activación por socket el daemon arranca muchas más veces de las que rerankea, y
buena parte de esos arranques solo atienden un `save`. `CUBA_WARM_RERANKER=1`
restaura la precarga.

### Un daemon que no corre cuando nadie pregunta

`CUBA_IDLE_SHUTDOWN_SECS` apaga el daemon tras ese tiempo sin peticiones de
ningún cliente. `serve` adopta el socket que systemd pasa como fd 3
(`LISTEN_FDS`), de modo que una unidad `.socket` retiene el puerto mientras el
daemon no corre y ningún cliente ve una conexión rechazada.

Se apaga por el camino normal —`serve` retorna, el drenaje de fondo vacía las
escrituras de embeddings en vuelo, `sqlx` cierra su pool— y no llamando a
`process::exit`, que se saltaba ese drenaje y perdía esas escrituras en silencio.
El README documenta el par de unidades systemd.

### Hilos, pool y arranque

- **`CUBA_EMBED_INTRA_THREADS`.** Estaba fijo en 2, de cuando se esperaba que
  CUDA hiciera el trabajo. Nunca lo hizo, así que esos hilos *son* el embedder.
  Medido en 12 hilos por query: 1 → 94,8 ms · 2 → 52,3 ms · **4 → 35,8 ms** · 6 →
  68,1 ms · 12 → 155,4 ms. Pasado medio núcleo lógico la sincronización cuesta
  más de lo que la paralelización aporta.
- **`with_intra_threads` del reranker baja a 2 en GPU.** Ahí los GEMM corren en
  kernels CUDA y esos hilos solo mueven tensores.
- **`with_memory_pattern(false)`** en embedder y NLI: el planificador de memoria
  de ONNX Runtime solo rinde con shapes estáticos, y la longitud de entrada varía
  en cada llamada.
- **`with_intra_op_spinning(false)`** en el NLI: los veredictos llegan de uno en
  uno, con minutos entre medias.
- **Pool de Postgres 10 → 4.** Era el default de sqlx, nunca ajustado; un proceso
  atiende ahora a todos los clientes en lugar de uno por ventana.
- **`worker_threads = 4`** en tokio (era uno por núcleo, 12) — el trabajo real va
  a `spawn_blocking` y el default hacía competir al runtime con los pools
  intra-op de ONNX.
- **`fixed_shape()`** se decide por la colocación real y no por el feature de
  compilación, para que apuntar el reranker a CPU no lo deje rellenando todo a
  512 tokens.

### Corregido

- **`scripts/backup-db.sh` y `scripts/restore-db.sh` estaban rotos desde
  `e96df5d`.** Aquel commit de estilo quitaba comentarios y trató `${#OLD[@]}` y
  `$#` como el inicio de uno, comiéndose el resto de la línea: `if ((${` y
  `if [[ $`. El gate de merge fallaba con error de sintaxis de bash, y el script
  de restauración —el camino de recuperación ante desastre— no arrancaba. Ambas
  líneas restauradas a su forma original.

## [0.19.0] — 2026-07-29 (Cargo `0.19.0` · npm `0.19.0` · PyPI `1.21.0`)

Un servidor para todos los clientes, y el reranker de v0.18.0 aplicándose de
verdad en la máquina donde estaba instalado.

### `serve`: un proceso en lugar de uno por ventana

stdio da a cada cliente su propio proceso, y a cada proceso su propia copia de
los modelos. Con embeddings + reranker + NLI eso son ~6 GB **por ventana de
editor**; tres sesiones abiertas se comían 18,5 GB de un portátil de 16 GB, y el
escritorio entero acababa en swap.

`cuba-memorys serve` carga los modelos una vez y atiende a todos por HTTP en
loopback — que es además la forma que fijó la [especificación MCP del
2026-07-28](https://blog.modelcontextprotocol.io/posts/2026-07-28/): núcleo sin
estado, sin handshake de sesión, cada petición se describe a sí misma.

- **Las sesiones se aíslan por cliente.** `session.rs` guardaba la sesión activa
  en un `static` global; compartido, un `jornada start` en una ventana se
  convertía en la sesión activa de las demás. Ahora resuelve contra un
  task-local con la identidad del cliente (`Mcp-Client-Id`), y el global sigue
  siendo el camino de stdio y de los subcomandos. Fuera de una petición —el
  ciclo REM, una tarea de fondo— el daemon no responde ninguna sesión en vez de
  responder la de otro.
- **El ciclo REM corre una vez.** Bajo stdio cada ventana consolidaba la misma
  base en paralelo.
- **Un panic ya no se lleva a todos por delante.** El perfil release pasa a
  `panic = "unwind"` y el transporte contiene cada petición, porque con un
  proceso compartido un fallo en el handler de un cliente abortaba el servidor
  de todos. `outbound()` devolvía `.expect()` — con `panic = "abort"`, eso era
  el proceso entero al primer mensaje iniciado por el servidor sobre HTTP.
- **stdio ya no deja procesos huérfanos.** Si los modelos tardan más que el
  timeout de conexión del cliente (30 s), el cliente se rinde pero *no* cierra
  nuestro stdin: el proceso se quedaba vivo sujetando cada modelo que había
  cargado, uno por intento, hasta que la máquina no daba más. Ahora sale si no
  llega ningún handshake en `CUBA_HANDSHAKE_TIMEOUT_SECS` (60 s; `0` desactiva).

### El reranker: lo que faltaba después de v0.18.0

v0.18.0 dejó el camino GPU listo. Lo que no comprobaba nadie es que **el binario
instalado se hubiera compilado con `--features cuda`** — el de esta máquina no,
así que el cross-encoder corría en CPU, se pasaba de su presupuesto de 20 s en
cada consulta y `faro` descartaba los scores. Se pagaba la inferencia completa
para devolver el orden de RRF. Medido con `examples/rerank_bench` (50
candidatos, longitudes mixtas como las reales):

| configuración | media | ¿dentro del presupuesto? |
|---|---|---|
| CPU, 2 hilos (lo que había) | 106,9 s | no — 5,3× por encima |
| CPU, 6 hilos (núcleos físicos) | 61,0 s | no |
| **GPU (`--features cuda`)** | **4,1 s** | **sí** |

El ranking de GPU y el de CPU son idéntico candidato a candidato; solo difieren
en la quinta cifra decimal del score.

Tres defectos reales aparecieron por el camino:

- **Reordenar una lista vacía cargaba el modelo entero.** La comprobación de
  `enabled()` iba antes que la de lista vacía, así que `rerank(q, &[])` pagaba
  una carga perezosa de 1,1 GB para devolver el vector vacío que ya sabía
  devolver. Era también lo que colgaba dos tests de la suite.
- **`enabled()` se llamaba desde una tarea async.** Puede cargar el modelo, así
  que bloqueaba el executor: en el daemon, la primera búsqueda de un cliente
  congelaba a todos los demás. Ahora va al pool de bloqueo junto con la
  inferencia que controla.
- **`with_intra_threads(2)` estaba fijo** para un XLM-RoBERTa-large. Ahora son
  los núcleos físicos (`CUBA_RERANK_INTRA_THREADS` lo fuerza): 1,74× en CPU, y
  medido que pasarse a los hilos SMT empeora (12 hilos → 16,5 s vs 10,8 s con 6).
- **Los lotes se agrupan por longitud** (`CUBA_RERANK_LENGTH_BUCKETING`) cuando
  el padding no es fijo, para no gastar cómputo en relleno. Los scores no
  cambian: la máscara de atención ya anula las posiciones rellenadas.

`examples/rerank_bench` mide todo esto en cualquier máquina y dice si el
reranker cabe en su presupuesto o si se está tirando el trabajo.

## [0.18.0] — 2026-07-28 (Cargo `0.18.0` · npm `0.18.0` · PyPI `1.20.0`)

Dos cosas que llevaban releases documentadas como pendientes: el reranker que
medía +92% y estaba apagado, y un codegraph que se corrompía solo al segundo
build.

### El reranker en GPU: +93% nDCG por 1,1 s de latencia

El cross-encoder costaba ~15 s por consulta en CPU, así que nunca se activaba.
El crate ya tenía la feature `cuda`, `gpu::configure()` cableado en los tres
modelos y el provider CUDA descargado — **nunca se había compilado con ello**.

Compilar con `--features cuda` no bastaba. Tres cosas separaban el camino GPU
de uno usable:

- **El modelo se cargaba dentro del handler de la primera búsqueda**, que tiene
  30 s. Cargar más la primera inferencia se los pasaba de largo, así que la
  primera búsqueda tras cada arranque moría con `-32603`. Ahora el servidor
  precalienta el reranker en una tarea de fondo al arrancar, fuera de todo
  handler.
- **Precalentar con un pasaje corto no servía de nada**: ONNX Runtime compila
  kernels CUDA *por forma de entrada*, y el camino real son 50 candidatos.
  Precalentar con la forma real saca ese coste de la ruta de consulta.
- **El tokenizador rellenaba con `BatchLongest`**, así que cada consulta
  generaba formas de tensor nuevas y volvía a pagar la compilación. Rellenar a
  512 fijo con lotes de tamaño constante bajó el precalentamiento de **42,8 s a
  8,9 s**. Es una ganancia exclusiva de GPU — en CPU un 512 constante solo hace
  cada lote más grande —, así que por defecto se activa únicamente en builds con
  `cuda`/`directml`, y `CUBA_RERANK_FIXED_SHAPE` lo fuerza en cualquier sentido.

Medido sobre 60 preguntas contra el corpus real, k=10:

| métrica | sin rerank | con rerank | cambio |
|---|---|---|---|
| nDCG@10 | 0,3039 | **0,5873** | **+93%** |
| MRR | 0,2366 | **0,4914** | +108% |
| R@10 | 0,4346 | **0,6961** | +60% |
| tokens/respuesta | 5252 | **4026** | **−23%** |

La ganancia (+0,283) supera el efecto mínimo detectable que el propio evaluador
calcula (0,245), así que no es ruido. La mediana por consulta pasa de 4,23 s a
5,35 s; ese mismo trabajo costaba ~15 s en CPU.

### codegraph: el segundo build ya no corrompe el grafo

Tres defectos que solo aparecen al reconstruir un repo que ha cambiado — que es
justo el único caso que importa en una herramienta que sigue código vivo:

- **El contenido de la observación llevaba dentro el rango de líneas del símbolo,
  y el dedup comparaba esa cadena exacta.** Añade un comentario encima de una
  función y su rango se desplaza: el `WHERE NOT EXISTS` no encuentra nada, entra
  una fila nueva y la vieja se queda para siempre. **Cada edición multiplicaba las
  filas.** La identidad ahora es `` {tipo} `{nombre}` in {fichero}: `` —sin números
  de línea— comparada con `left(content, n)` para no depender de escapes de `LIKE`,
  y un símbolo que se mueve actualiza su fila en lugar de duplicarla.
- **El recorrido usaba `path.is_dir()`, que sigue enlaces simbólicos.** Un enlace
  a un directorio ancestro y el recorrido no termina nunca. Ahora usa
  `entry.file_type()`, que no los sigue, y registra el salto para que un árbol
  enlazado se vea omitido en vez de desaparecer en silencio.
- **La persistencia eran cientos de sentencias sueltas contra el pool.** Un fallo
  a media faena dejaba el grafo a medio escribir sin forma de saberlo. Las tres
  pasadas comparten ahora una transacción y confirman juntas.

306 tests en verde, clippy sin avisos.

## [0.17.1] — 2026-07-28 (Cargo `0.17.1` · npm `0.17.1` · PyPI `1.19.1`)

Correcciones encontradas ejecutando el ciclo REM de v0.17.0 contra el corpus real
de producción, no contra una base de prueba. El escaneo funcionaba en los tests
porque el test llama a `scan_entity_relations()` directamente con una entidad de
tres notas; sobre entidades reales el prompt lleva hasta 12 observaciones más 60
nombres del grafo, y ahí se rompía.

- **El escaneo de relaciones heredaba el presupuesto del handler MCP.** El ciclo
  REM no corre dentro de un handler y no tiene por qué respetar sus 30 s, pero
  `extraction_budget()` le imponía el 60% de ese límite: 18 s para un prompt
  varias veces más largo que el de `auto_extract`. Ahora el escaneo tiene su
  propio presupuesto (`CUBA_REM_SCAN_TIMEOUT_SECS`, por defecto 90 s).
- **Los dos timeouts no estaban alineados.** `ClaudeCodeJudge` lleva su propio
  corte interno de 30 s, así que el presupuesto externo nunca llegaba a aplicarse
  y el CLI moría antes. `resolve_offline_llm_within()` ajusta ahora el timeout del
  backend al presupuesto de quien lo llama, de modo que manda un solo número.
- **Un fallo aislado abortaba el lote completo.** El bucle hacía `break` al primer
  error, así que una entidad lenta se llevaba por delante a las cuatro restantes
  del ciclo. Ahora tolera fallos sueltos y solo abandona tras dos consecutivos;
  el recuento se reporta en `failed`.

Medido en producción antes y después del arreglo: **4 de 5 entidades escaneadas
con 1 fallo → 5 de 5 con 0 fallos**. Acumulado sobre el corpus real: relaciones
213 → 228 (15 de ellas `provenance='inferred'`), entidades aisladas 148 → 141,
48 observaciones largas por fin fragmentadas en chunks.

302 tests en verde, clippy sin avisos.

## [0.17.0] — 2026-07-28 (Cargo `0.17.0` · npm `0.17.0` · PyPI `1.19.0`)

v0.16.0 prometió que el grafo crecía solo. No crecía: la capacidad estaba ahí y
nunca llegaba a ejecutarse. Esta versión la hace funcionar de verdad y aplica lo
mismo al corpus que ya existía.

### La extracción automática nunca se había ejecutado

Consultando producción por origen: 1571 observaciones `source='agent'`, 74
`source='user'` y **cero** `source='inference'`. Ese último número es la prueba:
`source='inference'` es lo único que escribe `auto_extract`, y `auto_extract` es la
única ruta que crea relaciones automáticamente. Cero filas en 1645 observaciones
significa que nunca corrió.

La causa era una puerta cerrada: `auto_extract` devolvía `degraded` salvo que el
cliente anunciara `capabilities.sampling`. **Ningún cliente real lo anuncia** —
verificado en vivo contra Claude Code 2.1.220, que respondió *"client did not
advertise MCP sampling capability"*. La función estaba fuera de alcance desde todos
los clientes en uso.

- **`auto_extract` ahora escala igual que `cuba_juez`**: sampling si el cliente lo
  ofrece, si no un CLI local, si no la API. Ese escalonado ya existía en
  `resolve_llm_judge()` y llevaba releases funcionando; `auto_extract` simplemente
  no lo llamaba. La rama de sampling se mantiene aparte porque
  `MCPSamplingJudge::run_prompt` corta en 256 tokens y la extracción necesita 1024.
- **La respuesta ahora incluye `backend`**, para que se pueda ver qué LLM contestó
  en lugar de deducirlo.

### Dos fallos que solo aparecen bajo el protocolo real

Ambos pasaban desapercibidos llamando al handler directamente, que es como estaba
escrito el test:

- **El handler tiene 30 s y el CLI tardaba ~20 s**, así que el primer intento
  end-to-end moría con `-32603 Handler timed out`. El LLM recibe ahora una fracción
  fija del presupuesto (60%) y agotarla degrada a una respuesta normal en vez de
  tumbar la llamada entera.
- **`claude --print` cargaba toda la configuración MCP del usuario**, cuba-memorys
  incluido: un servidor lanzando un cliente que relanza el servidor.
  `--strict-mcp-config` lo corta — **9,48 s → 3,98 s (−58%)** y adiós recursión.
  `cuba_juez` comparte backend y hereda la mejora.

### El ciclo REM cablea también lo que ya estaba escrito

Arreglar `auto_extract` solo sirve para memorias nuevas. El corpus existente seguía
igual: 148 de 279 entidades (53%) con grado 0 y cero relaciones inferidas.

- **Nueva tarea REM**: cada ciclo toma un lote de entidades aisladas que tienen
  observaciones y pregunta al LLM qué conectan sus propias notas.
- Al prompt se le pasan **los nombres ya presentes en el grafo**, ordenados por
  grado, para que reutilice `PostgreSQL` en lugar de acuñar `Postgres` y fragmentar
  el grafo. Se le dice explícitamente que una lista vacía es respuesta válida,
  porque un modelo presionado a encontrar enlaces se los inventa.
- Las entidades se sellan con `relations_scanned_at` para no pagar dos veces por un
  escaneo que legítimamente no encontró nada. Pero un sello a secas congelaría el
  grafo, así que la cola devuelve también las entidades cuya observación más nueva
  es posterior al sello.
- El lote está acotado: `CUBA_REM_RELATION_BATCH` (por defecto 5, `0` lo desactiva).
  Cada entidad cuesta una llamada de CLI de ~4 s, así que un ciclo gasta ~20 s cada
  cuatro horas en vez de vaciar la cola de golpe.

### Migración

- **0039**: `brain_entities.relations_scanned_at` + índice parcial para la cola.
  Aditiva; se aplica sola al arrancar.

### Medido

Contra una base aislada, por JSON-RPC y sin sampling: 3 hechos extraídos y 3
relaciones escritas (`cuba-memorys --uses--> Rust`, `--uses--> PostgreSQL`,
`--depends_on--> pgvector`), todas con `source='inference'` y
`provenance='inferred'`. El escaneo retroactivo tomó una entidad huérfana con tres
notas y la dejó como `--depends_on--> PostgreSQL` y `--uses--> Rust`, sellada y
fuera de la cola; añadirle una nota posterior la devolvió a la cola.

302 tests en verde, clippy sin avisos.

## [0.16.0] — 2026-07-28 (Cargo `0.16.0` · npm `0.16.0` · PyPI `1.18.0`)

Esta versión sale de una comparación con el estado del arte (mem0, Zep/Graphiti,
Letta, cognee, HippoRAG 2, el survey de memoria de agentes arXiv 2602.06052) y de
medir el corpus real en vez de suponer.

### El grafo ahora crece solo

Medido antes del cambio: **148 de 279 entidades (53%) sin una sola arista**, grado
medio 1,53, 213 relaciones para 1645 observaciones. Siete algoritmos de grafo ya
implementados —PageRank personalizado, Louvain, k-core, closeness/harmonic,
betweenness, Adamic-Adar, activación por propagación estilo HippoRAG— corrían sobre
un sustrato que la mitad de las veces no existía.

La causa: nada creaba relaciones por su cuenta. `auto_extract` extraía hechos pero
nunca aristas, y `link` (NPMI) es un comando manual que nadie ejecuta.

- **`auto_extract` ahora pide hechos Y relaciones** en la misma llamada de sampling
  (mismo coste, $0), y las escribe con `provenance='inferred'`. Los extremos se
  auto-crean, así que una arista que menciona una tecnología vista por primera vez
  en esa conversación igual aterriza. El parser acepta tanto la forma nueva como el
  array plano anterior.
- **El ciclo REM pasó de 3 a 6 tareas**: además de decay, decay de episodios y
  PageRank, ahora hace autolink NPMI, backfill de embeddings y chunking. Esto es el
  *sleep-time compute* que Letta nombró en 2025 — cuba-memorys tenía el daemon desde
  antes, al 30% de su capacidad.
- **`cuba-memorys rem`** ejecuta un ciclo bajo demanda en vez de esperar 4 horas.
- **43 observaciones (2,6%) no tenían embedding** y eran permanentemente invisibles
  a la búsqueda vectorial, sin nada que las reprocesara. El ciclo REM las rellena,
  con tope por ciclo (`CUBA_REM_BACKFILL_LIMIT`, 100 por defecto).

Hallazgo honesto: el autolink NPMI creó **0** aristas, y es correcto — 46 de sus 47
pares candidatos ya tenían relación. La co-ocurrencia está saturada en este corpus;
lo que densificará el grafo es la extracción por LLM, no NPMI.

### Cuarentena contra envenenamiento de memoria

La memoria persistente tiene una clase de ataque que la inyección de prompt no
tiene: la escritura y su efecto están separados en el tiempo. MINJA (arXiv
2601.05504) planta memoria envenenada con turnos de usuario normales —sin
privilegios, sin acceso al almacén— y reporta **>95% de éxito**; la instrucción
dispara semanas después. El hash-chain CFR-21 prueba *a posteriori* que algo se
alteró; no impide que una escritura legítimamente autenticada meta un hecho hostil.

Siguiendo SMSR (arXiv 2606.12703): separar memoria candidata no confiable de la
memoria confiable, con promoción mediada.

- Migración 0037 añade `brain_observations.trust`, por defecto `trusted`, así que
  todo lo existente se comporta igual.
- `cuba_ingesta auto_extract` acepta `untrusted: true` para texto que no controlás.
  `CUBA_QUARANTINE_INFERENCE=1` aplica la política a toda extracción por LLM.
- Lo cuarentenado se almacena y es inspeccionable, pero **no se recupera**: ni por
  `cuba_faro` ni por BM25, y queda fuera de la calibración OOD para que el texto no
  confiable tampoco pueda mover el umbral de abstención.
- `cuba_eco` gana la mediación: `pending` lista lo retenido, `promote` lo hace
  recuperable, `quarantine` lo retira. Ambas transiciones quedan en la cadena de
  auditoría.

### Chunking: el final de los textos largos deja de ser invisible

El embebedor trunca a 512 tokens. Todo lo que pasa de ~1800 caracteres nunca
llegaba al modelo. Medido: **48 observaciones (2,9%) con ~29.700 caracteres
invisibles** a la búsqueda vectorial — y son las más densas (post-mortems, lecciones
detalladas, decisiones de arquitectura).

- Migración 0038 añade `brain_observation_chunks` con solapamiento. La columna
  vectorial se crea leyendo la dimensión que ya usa la base, no una fija: esta
  instalación corre bge-m3 a 1024 mientras las migraciones declaran 384.
- La búsqueda vectorial une los aciertos directos con los aciertos por chunk y
  deduplica a la observación padre.
- Medido: para una consulta que apunta al carácter 3047 de una observación de 3221,
  la similitud vía documento truncado es **0,6566** y vía el chunk que lo cubre,
  **1,0000**.

### BEAM, y los dos bugs que destapó en minutos

LOCOMO está saturado (16-26k tokens entran en cualquier ventana moderna) y Zep
documentó que cambiar el prompt del juez mueve su accuracy dos dígitos. BEAM (ICLR
2026) es el estándar al que se movió el campo. Se añade `beam_prepare.py`, que
convierte un shard de BEAM al JSONL del harness y mapea `source_chat_ids` a las
observaciones ingeridas — permitiendo puntuar el retrieval por id, sin juez.

Medido en BEAM-100K (3 conversaciones, 42 preguntas): nDCG@10 0,283 / 0,444 / 0,250
y recall@10 0,322 / 0,566 / 0,267, bastante por debajo de los números LOCOMO de este
repo (0,484 / 0,610), tal como predicen las propias líneas base del paper.

### Correcciones

- **`calibrate --apply --json` descartaba `--apply` en silencio**: la rama JSON
  retornaba antes de persistir. Toda calibración automatizada imprimía un umbral y
  no guardaba nada.
- **Con abstención activada y sin umbral calibrado, el gate OOD rechazaba el 100% de
  las consultas respondibles** (recall 0,0000, tasa de falsa abstención 1,0). La
  causa medida: los embeddings del corpus están a distancia Mahalanobis p50 18,7 de
  su propio centro mientras las consultas están a p50 51,7 —los pasajes se embeben
  con prefijo contextual y las consultas no—, así que el corte teórico χ² (~21) cae
  por debajo de toda consulta real. Tras `calibrate --apply` (umbral conformal
  58,35): recall 0,5660, falsa abstención **0,0**.
- **Dos tests comparaban contra la constante `EMBEDDING_DIM`** en vez de
  `embedding_dim()`, así que pasaban en CI (variable sin definir) y fallaban en
  cualquier máquina configurada con otro modelo.

## [0.15.0] — 2026-07-17 (Cargo `0.15.0` · npm `0.15.0` · PyPI `1.17.0`)

### Nuevo

- **El servidor arranca aunque PostgreSQL no responda.** Antes moría con
  `exit(1)` antes de hablar el protocolo — el motivo real por el que el
  quality-check de Glama nunca pasaba: levanta el contenedor sin base de datos
  real. Ahora arranca en modo degradado (`tools/list` es estático, cada tool
  falla con su error real en vez de tirar todo el proceso).
- **`cuba-memorys hook install`** conecta git a la memoria: `post-commit`
  exporta, `post-checkout` importa, y un merge driver propio fusiona
  observaciones/relaciones/entidades por id en vez de dejar conflict markers.
  `--with-codegraph` además re-indexa el grafo de código en cada commit.
  `hook uninstall` revierte exactamente lo que `install` agregó.
- **Proveniencia en las relaciones (`extracted` / `predicted` / `inferred`).**
  `cuba_puente predict` ahora puede persistir sus sugerencias Adamic-Adar como
  relaciones reales (`persist: true`, `relation_type` configurable) en vez de
  solo devolverlas; `traverse` expone la proveniencia de cada arista.
- **`cuba-memorys codegraph build`**: parsea Rust y Python con tree-sitter
  (determinístico, sin LLM) y lo integra en el MISMO grafo que ya usan
  `cuba_faro`/`cuba_puente` — funciones/clases se vuelven entidades buscables,
  las llamadas/imports resueltos se vuelven relaciones con
  `provenance='extracted'`. Una llamada solo se resuelve cuando su nombre
  coincide con exactamente un símbolo del lote parseado; ambiguas se
  descartan en vez de adivinar.

### Correcciones

- **El binario Linux x64 se compila contra musl, no contra glibc.** Exigía
  glibc ≥ 2.39 (compilado en el runner de GitHub), inexistente en Debian 12,
  Ubuntu 22.04 o RHEL 9 — así rompió el primer intento de build de Glama.
  Ahora es estático (`static-pie`), sin dependencia de la glibc del sistema.
- **22 bugs encontrados en una auto-auditoría** de todo lo de arriba (cada
  hallazgo verificado adversarialmente antes de arreglarse), entre ellos: una
  fuga de `app.current_project` entre conexiones recicladas del pool (podía
  filtrar el scope de un proyecto a una request de otro), `--conflict
  overwrite` de `cuba_sync` completamente no-funcional (se comportaba como
  `skip`), `export()` nunca borraba archivos de filas eliminadas de la DB
  (podían resucitar en el próximo `import`), y funciones anidadas mal
  atribuidas en el parser de código (arreglado en Rust y Python).

## [0.14.1] — 2026-07-15 (Cargo `0.14.1` · npm `0.14.1` · PyPI `1.16.1`)

### Correcciones

- **`models runtime --gpu` instala de verdad la GPU.** Antes solo extraía la
  librería principal del runtime, nunca los execution providers de CUDA
  (`libonnxruntime_providers_cuda.so` + `_providers_shared.so`), así que aun con
  una instalación GPU limpia el proveedor CUDA no se registraba y todo caía a CPU
  en silencio. Además el comando saltaba la descarga si ya existía cualquier
  runtime, con lo que pasar de CPU a GPU era un no-op. Ahora `--gpu` siempre
  re-descarga y extrae la principal más los providers (TensorRT excluido: necesita
  librerías extra que no distribuimos), y lista cada archivo extraído.

- **`doctor` reporta el estado real de la GPU.** `gpu::active_provider()` devolvía
  un `"cuda"` fijo sin comprobar nada, así que `doctor` mostraba `[ok] gpu` incluso
  corriendo en CPU. Ahora `gpu::status()` verifica que el provider CUDA esté junto
  al runtime y que exista una GPU NVIDIA, y `doctor` avisa con `warn` accionable
  cuando una build con GPU degrada a CPU.

- **Las migraciones vuelven a ser inmutables.** El barrido de comentarios de
  `e96df5d` había tocado 33 archivos de migración ya publicados. sqlx valida el
  checksum SHA-384 de cada migración aplicada en cada arranque, así que cambiar su
  contenido rompía el arranque contra cualquier base creada antes de 0.14 (toda
  instalación real) con `migration N was previously applied but has been modified`.
  Restaurados los 33 archivos a su contenido original; una migración publicada es
  inmutable y el estándar de "código sin comentarios" no le aplica.

## [0.14.0] — 2026-07-15 (Cargo `0.14.0` · npm `0.14.0` · PyPI `1.16.0`)

### Modos de funcionamiento: `CUBA_MODE=local | red | completo`

Un preset que configura la base de datos, los modelos y la red saliente a la vez, en
vez de alinear a mano una docena de variables:

- **local** (default) — Postgres en Docker local, modelos locales, sin red saliente.
- **red** — Postgres gestionado compartido (TLS): dos máquinas con una sola memoria,
  procedencia por nodo (`origin_node` / `CUBA_NODE_NAME`), sincronización en tiempo real.
- **completo** — todo: reranker (GPU si hay) + `cuba_docs`. Máxima capacidad.

`doctor` reporta el modo activo como primer check. Las env vars individuales siguen
ganando sobre el preset.

### El reranker se enciende, y es la mayor mejora del proyecto

Medido: base RRF nDCG **0.2758 → con reranker 0.5300**, **+92%** (bootstrap pareado
[+0.207, +0.302], mejora 86 de 191 queries). Estaba inerte: nada apuntaba el binario al
modelo (ahora cae al caché y `doctor` lo reporta) y puntuaba los 50 candidatos de uno en
uno (ahora una pasada batcheada). En CPU sigue siendo pesado, así que `faro` lo acota con
un timeout (`CUBA_RERANK_TIMEOUT_SECS`, 20 s) y cae a RRF si se pasa. En GPU es instantáneo.

### GPU: auto-detección CUDA → DirectML → CPU

Las tres sesiones ONNX (embeddings, NLI, reranker) registran los execution providers
compilados y caen a CPU si el runtime no los soporta. Los binarios de release traen CUDA
(NVIDIA) y, en Windows, DirectML (cualquier GPU). `cuba-memorys models runtime --gpu`.

### Onboarding cross-platform

- `cuba-memorys models <embed|nli|reranker|runtime|all>` — descarga modelos y runtime en
  cualquier OS, reemplaza los tres `.sh` (borrados) que no corrían en Windows.
- Procedencia: columna `origin_node`, se rellena sola desde `CUBA_NODE_NAME` o el hostname.

### Código sin comentarios

Nuevo estándar: código limpio y auto-explicativo, el porqué en los mensajes de commit.
Se quitaron los comentarios inline de todo el proyecto (~4600 líneas); build, clippy y
tests en verde.

## [0.13.1] — 2026-07-14 (Cargo `0.13.1` · npm `0.13.1` · PyPI `1.15.1`)

Two things v0.13.0 shipped that nobody could use, found by installing it instead of
trusting the green checkmark.

### `cuba_docs` shipped to nobody

It was gated behind a Cargo feature that is **off by default** — and the published
binaries *are* the default build. So the tool existed for one entire release and could
not be invoked by a single person who installed from npm or PyPI. CI never compiled the
feature either, so nothing anywhere was checking it.

The feature now ships **compiled in** and switched off at runtime. With `CUBA_DOCS`
unset the tool is not advertised, the dispatcher refuses it, and the server makes no
outbound request of any kind — the guarantee is unchanged, but it is now a guarantee
an agent can see, rather than a comment promising one. Set `CUBA_DOCS=1` to enable it.

- Release binaries and the Python wheel build with `--features docs`.
- CI compiles, clippies and tests **both** configurations. A feature CI never builds is
  a feature nobody is checking.
- A test asserts the tool is absent from the catalogue when the switch is off.

### The npm recovery instruction sent you in a circle

With `npm config set ignore-scripts true` — a reasonable hardening, and common — the
postinstall that downloads the binary never runs. `bin.js` correctly refused to run a
stale binary off the PATH and told you to fix it with:

    npm rebuild cuba-memorys --foreground-scripts

**which cannot work, because `npm rebuild` obeys `ignore-scripts` too.** The command it
prints now is the one that was verified on a machine with the setting on:

    npm rebuild cuba-memorys --ignore-scripts=false --foreground-scripts

A recovery instruction that fails is worse than none: it costs the reader the time to
try it before they start doubting the message instead of the setting.

## [0.13.0] — 2026-07-14 (Cargo `0.13.0` · npm `0.13.0` · PyPI `1.15.0`)

### `verify` decides for itself now — locally, in Spanish, in 50 ms

v0.11.2 fixed `cuba_faro mode=verify` by handing its evidence to an LLM judge. That was
correct and it was slow: ~20 s per claim through the `claude` CLI, and with no CLI and
no MCP client that supports sampling, verification degraded to `unknown` — which is to
say, to nothing.

Entailment is a classification problem, so it is now classified: **mDeBERTa-v3-base-xnli**
(100 languages, 87.1% on XNLI) runs on the ONNX runtime this project already links.

```
claim: "cuba-memorys está escrito en Java"   (FALSE)
  v0.11.0 (cosine) ....... 0.61  ← scored HIGHER than the true claim
  v0.11.2 (LLM judge) .... contradicted, ~20 s
  now (local NLI) ........ contradicted, ~50 ms, offline, free

claim: "cuba-memorys está escrito en Rust"   (TRUE)
  now .................... verified, 0.995
```

A full 10-evidence `verify` went from blowing the 30 s handler timeout to **5 s**.

- New `NliJudge`, preferred in `CUBA_JUDGE=auto` when a model is installed. It takes
  `judge_claim` (entailment) and leaves `judge` (contradiction between two memories) to
  the LLM — that taxonomy includes `supersedes`, which means *the same fact, updated*,
  and needs a sense of time a 3-way classifier does not have. Reporting a port migration
  as a contradiction would be a regression, so the NLI does not answer what it cannot.
- `./rust/scripts/download_nli.sh` fetches the model. `cuba-memorys doctor` reports it.
- Undecided claims do **not** escalate to an LLM by default. An undecided NLI already
  returns `unknown`, which counts for neither side — abstaining is *already* safe.
  Escalation buys recall, not safety, and it costs 12 s per evidence. Opt in with
  `CUBA_NLI_ESCALATE=1`.

### Three things that looked obviously right and were measurably wrong

Recorded because the next person to improve this will reach for the same three.

1. **The quantized model (323 MB) confirms false claims.** It read evidence saying the
   reranker "is disabled by default" as SUPPORTING the claim that it is *enabled*, at
   0.62 confidence. The fp32 export says `contradicts` at 0.995. DeBERTa-v3's
   disentangled attention does not survive int8 — and it bought nothing: **48 ms per
   verdict quantized, 53 ms at full precision.** It was paying in accuracy for a speedup
   that does not exist on CPU. Ships fp32 (1.1 GB).

2. **Decomposing evidence into sentences makes it worse.** XNLI premises are single
   sentences and stored memories are paragraphs, so cutting them up looked necessary. It
   got **three of five real cases wrong, and it CONTRADICTED true claims** — because NLI
   is trained on *scenes*, where two predicates about one subject are alternatives ("a
   man is playing guitar" genuinely contradicts "…playing piano"). A knowledge base is
   not a scene: "cuba-memorys uses PostgreSQL" and "cuba-memorys is written in Rust" are
   both true, and shown in isolation the model rates them a **contradiction at 0.993**.
   Every clause about a *different attribute of the same entity* became a vote against
   the claim. The premise is now scored whole.

3. **An argmax over the 3-way head is not a verdict.** The model scores `entailment`
   at 0.693 for "the reranker is a bi-encoder" against evidence saying it is a
   cross-encoder — jargon whose mutual exclusivity it cannot know. An argmax publishes
   that as `supports`: a confirmed false claim, the exact bug this subsystem exists to
   kill. Each class must now clear its own floor, and the floors are **asymmetric on
   purpose** — entailment needs 0.80, contradiction 0.60. Confirming a false memory and
   doubting a true one are not errors of equal cost. Every genuine entailment measured
   scored ≥0.95; spurious ones live in the 0.6s.

### Fixed

- **Every CLI invocation was silently using hash embeddings.** `setup` downloads
  onnxruntime to `~/.cache/cuba-memorys/onnxruntime/` and writes `ORT_DYLIB_PATH` into
  the MCP client's config — so the *server* found the library and nothing else did.
  `search`, `dedupe` and `reembed` run from a plain shell saw no `ORT_DYLIB_PATH`, found
  no system library, and fell back to hash vectors: **the same query answered from the
  CLI and from the MCP was hitting two different vector spaces.** `locate_onnxruntime()`
  now looks in its own cache directory, where `dlopen` cannot guess to look.

## [0.12.0] — 2026-07-13 (Cargo `0.12.0` · npm `0.12.0` · PyPI `1.14.0`)

The benchmark could not measure what this project claimed to have measured, and the
graph was quietly broken.

### ⚠ Two published findings are withdrawn

The evaluation had **ten queries**. At n=10 the 95% interval on nDCG is roughly
±0.12, and the smallest detectable effect is ~0.25. Two conclusions this project
published rested inside that noise:

- ~~**"The cross-encoder reranker earns nothing"**~~ — **it had never run.** Three
  bugs in series, each hiding the next:

  1. `faro` wrapped the call in `if let Ok(..)`, so the error was **dropped** and the
     RRF ranking returned untouched. The same silent-degradation pattern that let the
     vector branch die unnoticed. *This is why the output was "bit for bit identical"
     to not reranking:* not because reranking changed nothing, but because it never
     happened.
  2. It fed the model **`token_type_ids`**. bge-reranker-v2-m3 is XLM-RoBERTa, which
     has no segment embeddings — that is a BERT input. Every inference threw
     `Invalid input name: token_type_ids`.
  3. It read the logits as **`f32`**. The checkpoint emits `f16` (needs ort's `half`
     feature). This one only surfaced once (2) was fixed.

  Two of these were architectural mismatches, obvious from a single error message.
  **Nobody saw the message, because bug 1 ate it.** A feature cannot earn anything
  when its results are thrown away. Fixed and being measured properly.

- **"Associative retrieval degrades all four metrics"** — the conclusion holds, the
  evidence did not. −0.03 at n=10 is a quarter of the error bar. But the correct test
  for two configurations over the *same* queries is a **paired** one, and under a
  paired bootstrap on the new dataset the interval is **[−0.051, −0.018]** and never
  touches zero: it improves **0** queries and hurts **23**. The decision to disable it
  was right; the reasoning was not. *The power was never in more data — it was in
  using the right test.*

### The real numbers

The system's nDCG is not 0.894. On 221 id-scored queries it is **0.50** [95% CI
0.44–0.56]. It did not get worse; it was never 0.894.

`compact` saves **28% of tokens** (not 40%) at **exactly identical nDCG** — identical
to four decimal places, because a response format cannot change *which* documents rank,
only how they are printed. That the old benchmark measured a quality cost for
truncation was itself an artefact: truncating the text removed the marker substrings it
was grading on.

The **+21.2 nDCG for bge-m3 is withdrawn.** The direction is almost certainly right;
the magnitude came from the broken benchmark and re-establishing it would mean
re-embedding the corpus twice.

### Fixed — the benchmark itself

- **Relevance was judged by substring match.** A result counted as correct if its
  text merely *contained* a marker word, so every observation mentioning "postgres"
  scored as a right answer to any question about postgres — whether it answered
  anything or not. That measures keyword presence, not retrieval, and it biases the
  whole benchmark toward the lexical branch and against the vector one. Ground truth
  is now a set of observation **ids** per query (TREC-style qrels).
- **nDCG normalized against what was RETRIEVED, not what EXISTS.** With 5 relevant
  documents in the corpus and 2 found, the "ideal" ranking was taken to be those 2 —
  so a system that missed 60% of the answer scored a **perfect 1.0**. The ideal is
  now built from `min(total_relevant, k)`, so documents you failed to retrieve count
  against you. This makes the numbers go **down**, which is the expected direction
  when you stop grading on a curve you drew yourself.
- **R@10 = 3.125 shipped in the README.** Recall is a proportion. The denominator
  was the count of *marker strings*, not of relevant *documents*.
- **Every metric now carries a bootstrap 95% interval** (Efron 1979, deterministic
  resampling) and the run reports its **minimum detectable effect**. A benchmark that
  cannot see a 5-point change should not be used to claim a 3-point regression.

### Added

- **`cuba-memorys dedupe`** — entities that are the same thing under different names.
  `cuba_alma create` inserts with `ON CONFLICT (name)`: a different string is a
  different entity, so one project fragments into `Mapupita-Web`, `Mapupitta-Web`
  (typo), `Mapupita Web`, `mapupita`… On the live brain: **266 entities, 158 (59%)
  with not a single relation** — for PageRank and multi-hop retrieval they do not
  exist.

  The infrastructure to fix this was already present and dead: `brain_entity_aliases`
  has a schema, indexes, and a `resolve_entity()` that matches exactly and fuzzily.
  Zero rows; nothing called the function. Merging now writes the old name there, so
  nothing is lost.

  **What decides a merge is not the embedding centroid.** That was the obvious idea
  and it is wrong: `M-Codes Reference Guide` and `G-Codes Reference Guide` sit at
  **0.811 cosine** between centroids. On a corpus about one domain, centroid
  similarity measures the domain, not the entity — trusting a 0.80 threshold would
  have merged two different CNC guides irreversibly. So `--apply` merges only what is
  *provable* (identical after normalizing case and separators), and everything else
  is shown, or judged one by one with `--judge`.

  (The LLM judge, asked whether `Mapupitta-Web` and `Mapupita-Web` were the same
  entity, first answered *"different — there are separate memory records for each"*.
  That is the bug offered as proof there is no bug. The prompt now disarms that
  argument explicitly, and a test pins it.)

- **`reranker_degraded` in the search response.** You asked for reranking, the
  cross-encoder threw on every pair, and you got the RRF order back looking exactly
  like a reranked one. Same reason `degraded` exists for the vector branch: an agent
  handed a silently un-reranked top-10 will simply trust it.

### Fixed — the CLI was eating your flags

- **`search "x" --format verbose` searched, literally, for «x --format verbose».**
  Unknown flags fell into the catch-all and were **concatenated onto the query**. It
  returned nothing, with no hint why.
- **`save "x" --importancia 0.9` stored «x --importancia 0.9» AS THE MEMORY CONTENT.**
  Same catch-all. This one corrupts data.
- Both, plus `delete`, now reject unknown `--flags` with a usage error. Same family as
  the `--batch 64` that `reembed` silently ignored: an argument a tool pretends not to
  see is an argument that lies about what it did.

### Added — build limits

`.cargo/config.toml` (3 jobs) and a `quick` profile (`lto = "thin"`, 16 codegen units).
The release profile's fat LTO with `codegen-units = 1` peaked past 8 GB in a single
unit and froze a 14.9 GB laptop running zram. Use `--profile quick` to iterate;
`--release` only to measure and ship.

## [0.11.2] — 2026-07-13 (Cargo `0.11.2` · npm `0.11.2` · PyPI `1.13.2`)

The anti-hallucination feature was hallucinating. Found by pointing the demo at it.

### ⚠ Breaking — `cuba_faro mode=verify` now calls an LLM judge

Verification escalates its evidence to a judge and derives confidence from the
verdicts. It costs a model call (free via MCP sampling — your client's model — or a
local `claude` CLI) and takes a few seconds. With no judge available it answers
`unknown` instead of inventing a verdict. Response gains `interpretation`,
`judged_by`, and a per-evidence `verdict`/`reason`.

### Fixed

- **`verify` scored false claims HIGHER than true ones.** Confidence came from
  cosine similarity to the retrieved evidence — and similarity measures what a text
  is *about*, not what it *asserts*. "cuba-memorys is written in Rust" and "…in
  Java" are nearly the same vector: same subject, same shape, one word apart.
  Measured on the live 1,461-observation corpus:

  | claim | before | after |
  |---|---|---|
  | "usa RRF con k=60" (true) | 0.59 | **0.83 · verified** |
  | "está escrito en Java" (false) | **0.61** | **0.00 · contradicted** |
  | "la mejor paella lleva azafrán" (unrelated) | 0.45, 10 "evidence" items | **0.00 · unknown**, none |

  No threshold could have fixed it — true claims landed at 0.43–0.57 similarity and
  false ones at 0.55–0.59, completely overlapping. Entailment is a different
  question from similarity and needs something that reads. Evidence below a
  similarity floor is now discarded (retrieval always returns its top-K; that is
  right for search and wrong for verification), and what survives goes to a judge.
  Verdicts are weighted by similarity, so similarity decides how much a verdict
  counts — never what the verdict is. "Unrelated" contributes to neither side: being
  on-topic is not support.

- **`cuba_juez` with the `claude_cli` backend never worked.** `claude --print
  --output-format json` returns a report *about* the call, with the model's answer
  as a string field inside it. The parser took the first `{` and last `}` — that
  envelope — found no `verdict`, and fell back to "unknown". Since v0.8. The
  heuristic quietly did all the work while the logs showed a model being called.

- **Setting `ONNX_MODEL_PATH` without `ORT_DYLIB_PATH` hung the server.** `ort` loads
  the runtime dynamically; when it cannot find the library it does not error, the
  process just stops answering — after starting, connecting, migrating and
  announcing itself ready. It logs an ERROR and degrades to lexical search now.

- **`compact` reported `"i": null`** on most results. Only the vector branch failed
  to select `importance`, and a semantic hit usually wins the fusion — so the field
  looked broken exactly where it mattered.

- Judge verdicts are fetched **concurrently**. Serially, a three-evidence verify cost
  over a minute of wall clock and would have been unusable however correct it was.

### Changed

- **README rewritten** for someone arriving new, not for someone who followed the
  version history. Every number in it is checked against the code by a test or was
  measured — the old one claimed 25 tools (there are 28), pinned installs to
  versions two releases stale, and documented none of the 13 CLI commands.
- **The demo no longer writes to your database.** It defaulted `DATABASE_URL` to the
  real brain on `:5488`, so recording the README GIF created entities in a live
  memory store and ran PageRank over it. It now starts a throwaway Postgres and
  destroys it on exit, and ignores your embedding config rather than inheriting it.

## [0.11.1] — 2026-07-13 (Cargo `0.11.1` · npm `0.11.1` · PyPI `1.13.1`)

Two bugs found by *using* v0.11.0 rather than testing it — both in the same family
as the ones v0.11.0 set out to kill.

### Fixed

- **Every new memory was stamped with the wrong model name.** `embeddings::onnx`
  exposed a `pub const CURRENT_MODEL = "multilingual-e5-small"` beside a
  `current_model()` that reads `CUBA_EMBED_MODEL`. The split was perverse: every
  site that **wrote** an embedding used the constant, every site that **compared**
  one used the function. So on a bge-m3 corpus, each new observation got a correct
  1024-d bge-m3 vector labelled with a 384-d model that had not run in months —
  permanently stale to `doctor`, whose warning count could only grow, and to
  `zafra reembed`, which could never converge: it re-encoded the row, and the next
  write re-mislabelled it.

  The vectors were always fine (measured, not assumed: cross-label cosine on
  same-entity pairs sits inside the range of within-label cosine — one vector
  space, not two). Only the name lied. But that name is what tells you, after the
  next model change, which rows still need re-encoding. `CURRENT_MODEL` is private
  now, so the compiler forbids the mistake — and it immediately found a fifth site:
  a smoke test asserting the constant's value, which had pinned the bug in place.

  Only affects setups that override `CUBA_EMBED_MODEL`; on the default model the
  label was accidentally correct.

- **`reembed`'s smallest unit of work was "everything".** One observation missing a
  vector, and the only cure on offer was to recompute all 1,461 — overwriting 1,460
  good vectors to fill one empty. It now re-encodes the stale set by default (no
  vector, or tagged with another model), which is right in both real cases without
  a flag: changing models makes every row qualify; a single failed embedding makes
  exactly one. `--all` still forces the full pass.

- **`reembed --batch 64` was silently ignored** — only `--batch=64` parsed, and the
  space-separated form fell into a catch-all that dropped it. Both forms work now,
  and an unrecognised argument is an error instead of a shrug.

## [0.11.0] — 2026-07-13 (Cargo `0.11.0` · npm `0.11.0` · PyPI `1.13.0`)

The fourth memory, and every optimization measured on a real corpus instead of
assumed. Several long-standing features turned out not to work at all; they are
fixed or cut, and the negative results are recorded rather than buried.

### ⚠ Breaking — `cuba_faro` now answers in `compact` by default

The default response shape changed from `verbose` to `compact`: abbreviated keys
(`e` entity, `c` content, `t` type, `i` importance, `s` score) and no per-branch
score breakdown. It costs **40% fewer tokens at identical nDCG** — the truncation
point was swept and set at its measured knee — and an agent reasoning over
memories does not need `bm25_score` to do it.

**If you parse the response**, this breaks you. Pass `"format": "verbose"` to get
the old shape back, unchanged:

```json
{ "query": "...", "format": "verbose" }
```

Agents reading the JSON are fine — the tool description documents the short keys.
Scripts and tests that index `entity_name` / `content` / `*_score` are not, and
must ask for `verbose`. Both shapes are now pinned by an integration test, so
neither can drift again.

### Added

- **Procedural memory** — `cuba_receta` (migration `0033`): how things are *done*
  here, not just what is true. Ranked by the **Wilson lower bound** of the success
  rate, so a recipe with a track record beats a lucky first try (1-of-1 scores
  0.21; 47-of-50 scores 0.84). Reinforced by outcome, not by access — the
  ACT-R distinction between declarative and procedural memory. `cuba-memorys
  skills <dir>` exports them as Claude Code Skills, which load lazily.
- **Progressive tool loading** — `cuba_tools` + `cuba_call`, and
  `CUBA_TOOL_PROFILE=lean`. The catalogue shrinks 67% (25,060 → 8,413 chars)
  while **every tool stays callable**: schemas are deferred, not deleted.
- **Calibrated abstention** — `cuba-memorys calibrate`. The OOD gate now detects
  out-of-distribution queries (100%) without rejecting answerable ones (0% false
  abstentions). Persisted in `brain_calibration` (`0032`).
- **RBAC** — `brain_principals` × `brain_grants` (`0031`), enforced by a
  RESTRICTIVE RLS policy. Zero regression: with no principals defined, nothing
  is denied.
- **New subcommands** — `doctor` (health check), `calibrate`, `recall` (session
  context for a `SessionStart` hook), `skills`, `reembed`, `link`, `setup`,
  `search` / `save` / `delete` / `export` / `dashboard`.
- **Graph auto-linking** — `cuba-memorys link`, scored by normalized pointwise
  mutual information so a ubiquitous entity earns no edges from being ubiquitous.
- **Model-agnostic embeddings** — e5-small (384-d) or bge-m3 (1024-d) by config.
  Measured on a real 1,443-observation corpus: **nDCG@10 0.682 → 0.894**.

### Fixed

- **Hybrid search could silently become lexical search.** A failing vector branch
  was discarded by an `if let Ok(..)` — no log, no flag, no symptom. Now it logs
  at ERROR, sets `degraded: true` in the response, and the server **refuses to
  start** when the model's dimension disagrees with the column.
- **`setup check` reported "all consistent" while a stale project-level
  `.mcp.json` spawned 384-d servers against a 1024-d column.** It now audits
  project configs too, and treats an absent `CUBA_EMBEDDING_DIM` as the 384-d
  value it actually is — so it can disagree with one that sets 1024.
- **Retrieval was not deterministic.** Fusion happened in a `HashMap` and sorted
  by score with no tie-break; Rust randomizes iteration order per process, so
  three identical eval runs scored 0.7389 / 0.7344 / 0.7389. Every optimization
  number previously recorded rested on that. Now tie-broken by id: 5/5 identical.
- **The token budget counted text it then threw away**, spending a 5,000-token
  budget to return 798. Shape first, then budget. Compact truncation swept and
  set at its measured knee (1200 chars): **40% fewer tokens, identical nDCG**.
- **The OOD threshold rejected 100% of answerable queries.** The covariance was
  fitted from 500 samples in 384 dimensions with a fixed ridge mislabelled
  "Ledoit-Wolf". Now real Ledoit-Wolf shrinkage plus a conformal threshold.
- **The eval panicked on an empty result list** (`relevances[..1]` on a
  zero-length slice) — it never fired only because nothing ever abstained.
- **The LLM judge shipped credentials to a third party.** Observation text is now
  redacted (Postgres URLs, provider tokens, JWTs) and length-capped.
- **`doctor` could not see a stale process** — Linux appends `" (deleted)"` to
  the exe name, and the filter dropped exactly the processes it existed to find.
- **`cuba-memorys --version` connected to your database and ran migrations.**
  Argument parsing had a catch-all that fell through to the MCP server, so the one
  command a person runs *because they do not yet trust what they installed* was the
  one that quietly reshaped their schema. `--version` is now inert — it prints and
  exits, with a test that pins it by pointing `DATABASE_URL` at a closed port.
- **`--help` did not exist**, for the same reason, which is why nothing ever
  documented the 13 subcommands. And a typo (`doctro`) launched the server on a
  stdio socket nobody was speaking to — indistinguishable from a hang. An
  unrecognised argument is now a usage error (exit 2). The server is what you get
  with *no* arguments, which is how MCP clients launch it.
- **npm could silently run a different version than the one you installed.**
  `bin.js` fell back to any `cuba-memorys` on the `PATH` when the postinstall
  binary was missing — and postinstall does not run under `--ignore-scripts`,
  standard practice in hardened CI. Installing 0.11.0 and getting an 0.6.0 left
  over from an old pip install is not a fallback; here it is a *migration* run by
  the wrong binary. The `PATH` binary must now prove its version matches, or the
  launcher refuses and says why.
- A test now pins `Cargo.toml` and `package.json` to the same version. npm's
  postinstall downloads from `releases/download/v{package.json.version}/`, an asset
  the release workflow only builds for the *Cargo* version — nothing connected
  those two numbers, and a drift would have 404'd every install.
- Zero `unwrap()` in production code; zero clippy warnings.

### Changed

- `cuba_faro` defaults to `compact` (40% cheaper, same quality).
- The eval reports **token cost beside every quality metric**, and tracks false
  abstentions — abstention accuracy alone is trivially maximized by answering
  nothing.

### Removed / not adopted

- **The cross-encoder reranker does not earn its place.** Its integration added
  `score × 0.0001` to fusion scores separated by 0.00016 — arithmetically
  incapable of reordering anything. Fixed the wiring, measured it properly, and
  it still bought nothing for 0.33 s/query and 1.1 GB. Off by default, with the
  negative result documented in the module.
- **Associative multi-hop retrieval degrades every metric** (nDCG 0.734 → 0.705,
  MRR 0.833 → 0.660, recall 2.31 → 1.88). The previous "+10 points recall" claim
  predates the determinism fix. Stays opt-in and off.

## [0.10.0] — 2026-06-04 (Cargo `0.10.0` · npm `0.10.0` · PyPI `1.12.0`)

Knowledge-graph memory plane: bitemporal facts, graph metrics, retrieval benchmarks,
and MCP unified search view — built on the v0.9 hybrid `cuba_faro` stack (not replaced).

### Added
- **Bitemporal core** (`core::bitemporal`, migration `0018`): `brain_facts` +
  `brain_fact_supersedes`; writes mirror observations on `cuba_cronica` add/batch_add
  and `cuba_ingesta` (via batch). **Default on**; disable with `CUBA_BITEMPORAL=0`.
- **Entity linking & temporal query** (`core::entity_linking`, `core::temporal_query`,
  migrations `0019`–`0020`).
- **Graph metrics** (migration `0022`): `brain_node_metrics` with PageRank, energy,
  betweenness; `cuba_zafra` `pagerank` persists ranks then refreshes energy scores.
- **Communities** (migration `0023`): Leiden detection + `detect_and_persist`;
  `cuba_zafra` action `communities`; `cuba_vigia` health metric persists tags.
- **Spreading activation** (`graph::activation`): multi-hop propagation; enriches
  `cuba_puente` `predict` alongside Adamic-Adar.
- **Eval harness** (`eval/`): nDCG@k, MRR, P@k, R@k over live `cuba_faro` hybrid;
  JSONL dataset loader + builtin smoke set; JSON reporters.
- **MCP memory v2 view** (migration `0024`): `v_unified_memory_search` joins facts via
  `brain_entities` (never `fact_id = node_id`).
- **Compatibility views** (migration `0025`): `v_observations_compat`.
- **Calibration alignment** (migration `0021`), scripts: `backup-db.sh`, `restore-db.sh`,
  `merge-gate.sh`, `mcp_live_session_test.py`.

### Changed
- `cuba_faro` remains production hybrid search (RRF + BM25 + vector + optional rerank).
- PageRank REM cycle also upserts `brain_node_metrics.pagerank_score`.

### Notes
- Optional Cargo features `bitemporal`, `graph-energy`, `eval-benchmarks` are markers;
  modules ship in the default library build.
- Run `./scripts/merge-gate.sh` before merging to `main`.

---

## [0.9.3] — 2026-05-04 (Cargo `0.9.3` · npm `0.9.3` · PyPI `1.11.3`)

Final piece of the v0.9.x roadmap. The cross-encoder reranker is now a
real bge-reranker-v2-m3 ONNX forward pass, not the heuristic baseline
that v0.9.2 shipped as scaffolding.

### Added
- **Real bge-reranker-v2-m3 ONNX forward pass** (`search::rerank`).
  Mirrors the `embeddings::onnx` loader pattern: lazy-init `Session`
  behind a `Mutex`, `tokio::task::spawn_blocking` for inference, and a
  semaphore capping concurrent calls at 2 to match
  `with_intra_threads(2)` (Little's Law — prevents threadpool
  starvation under load). Tokenizer encodes the (query, candidate)
  sentence pair with `[CLS]/[SEP]` segments and 512-token truncation.
  Output handled for both `[batch, 1]` regression heads and
  `[batch, 2]` binary classification heads (logit difference). Sigmoid
  to [0, 1] before sorting.
- Activation: drop a directory containing `model.onnx` (or
  `model_quantized.onnx`) plus `tokenizer.json` and point
  `CUBA_RERANKER_PATH` at it. Identity fallback otherwise — production
  behavior unchanged when the asset is absent.
- `cuba_faro` keeps the same `rerank: bool` arg surface from v0.9.2;
  no client-side change required.
- Expected gain: +12-25% nDCG@10 (Xiao 2023, BGE-Reranker paper).

### Changed
- Replaced the v0.9.2 heuristic body (token overlap + length penalty)
  with the real cross-encoder forward pass. Heuristic-only callers
  see unchanged behavior because the env var gates activation.

### Notes
- Adds zero new Rust deps — `ort 2.0.0-rc.12` and `tokenizers 0.21`
  were already present for `embeddings::onnx`.
- bge-reranker-v2-m3 quantized ONNX is ~280 MB; download from
  https://huggingface.co/BAAI/bge-reranker-v2-m3 (or use
  `huggingface-cli download BAAI/bge-reranker-v2-m3 --local-dir
  models/bge-reranker-v2-m3`). The asset is NOT bundled in the
  release artifact — operators provide it explicitly.

---

## [0.9.2] — 2026-05-04 (Cargo `0.9.2` · npm `0.9.2` · PyPI `1.11.2`)

Closes the v0.9.x roadmap with the deferred MCP correlator + reranker
scaffolding. No breaking changes.

### Added
- **MCP request/response correlator** (`protocol.rs` major refactor).
  The reader/writer split into three concurrent tasks: a single-owner
  stdout writer task draining an `mpsc::UnboundedSender<Value>`, a
  `PENDING` map of `oneshot::Sender<Value>` keyed by server-initiated
  request id, and per-request handler tasks. This enables:
  - **Real `MCPSamplingJudge`** — `protocol::request_sampling()` issues
    a `sampling/createMessage` to the connected client and awaits the
    reply on a oneshot. 30s timeout matches `HANDLER_TIMEOUT`. When the
    client did not advertise `sampling`, fails fast with an actionable
    message and the resolver auto-falls back to CLI / API / heuristic.
  - **`notifications/progress`** — `protocol::notify_progress()` emits
    standard MCP progress events. Wired into `cuba_zafra reembed`
    (~5% increments) so re-embedding 500+ observations is no longer
    silent.
  - **`notifications/cancelled`** — per-request `CancelToken`
    registered by `tools/call`, signaled by the cancellation
    notification. Handlers race against the token via `tokio::select!`
    and return a clean error instead of running to completion.
- **Cross-encoder reranker scaffold** (`search::rerank`). Activated by
  `CUBA_RERANKER_PATH` env var pointing to a bge-reranker-v2-m3 ONNX.
  When unset, identity fallback preserves upstream RRF order. New
  `cuba_faro` argument `rerank` for explicit override. Pipeline:
  top-50 RRF → rerank → MMR → top-K. Heuristic body included as a
  baseline that exercises the integration path; full model forward
  documented as a one-file follow-up to drop in.
- New `cuba_faro` arg surface: `rerank` (boolean).
- New helper `protocol::register_cancel_token` /
  `protocol::unregister_cancel_token` exported for any future handler
  that wants explicit cancellation hooks.

### Changed
- `JsonRpcResponse` / `JsonRpcError` structs removed — every outbound
  envelope is built ad-hoc with `serde_json::json!()` and pushed to the
  `OUTBOUND` channel. Narrower surface, no temptation to construct
  envelopes from places that should not.
- `cuba_zafra reembed` accepts `_meta.progressToken` to correlate
  progress with the MCP `tools/call` request id.

### Notes
- Real bge-reranker forward pass (ONNX session + tokenizer) is the
  only piece marked as TODO in the rerank module. The integration
  point, env var, schema arg, and identity fallback are all live.
  When the asset is bundled, swap the ~30-line heuristic body for the
  real inference and `enabled()`-true path becomes production.

---

## [0.9.1] — 2026-05-04 (Cargo `0.9.1` · npm `0.9.1` · PyPI `1.11.1`)

Production hardening + MCP spec usage. Closes the v0.9.x roadmap with
PRs #8–#11 plus the deferred infrastructure pieces from #10/#11.

### Added
- **PR #8** — `graph::closeness` (Bavelas 1950 + Boldi-Vigna 2014 harmonic)
  and `graph::kcore` (Seidman 1983, Batagelj-Zaversnik 2003 with
  running-max for correct k-core numbers). Exposed via new
  `cuba_vigia metric=structural` action.
- **PR #9** — working memory (`cuba_pizarra` + `brain_wm` table with
  GENERATED `expires_at` from `ttl_seconds`), Allen interval algebra
  (`cognitive::allen`, all 13 relations in O(1)), ADWIN drift detector
  (`cognitive::adwin`, Bifet-Gavaldà SDM 2007 with Hoeffding bound +
  Bonferroni correction), MI tagging (`cognitive::mi_tagging`,
  Brown JMLR 2012).
- **PR #10** — Tamper-evident audit log (`cuba_archivo` + `brain_audit_log`
  with SHA-256 hash chain, append-only PostgreSQL trigger, `cuba_admin`
  bypass role). Spotlighting prompt-injection defense in
  `cognitive::judge::build_prompt` (Hines 2024 — per-call nonce markers).
  Brier score + Expected Calibration Error in `cuba_calibrar metrics`
  (Brier 1950 / Naeini AAAI 2015) with reliability diagram.
- **Optional Prometheus `/metrics` endpoint** behind feature flag
  `observability` (`metrics 0.24` + `metrics-exporter-prometheus 0.16`).
  Default bind `127.0.0.1:9090` (env `CUBA_METRICS_PORT`/`CUBA_METRICS_BIND`).
  Pre-registered metrics: `cuba_handler_duration_seconds`,
  `cuba_handler_calls_total`, `cuba_judge_calls_total`,
  `cuba_judge_timeout_total`.
- **PostgreSQL Row-Level Security** per project (migration 0017).
  `tenant_isolation` policy across the six scoped tables. Defense in
  depth — the handler-side WHERE clause stays as the primary gate, RLS
  catches direct DB connections that bypass handlers. Sentinel `*` =
  bypass, NULL = back-compat.
- **PR #11** — MCP `resources/list` + `resources/read` with the
  `cuba://` URI scheme: `cuba://entity/<name>`,
  `cuba://project/<name>`, `cuba://snapshot/<id>`. Server now advertises
  the `resources` capability during initialize. Client capability
  detection captures `capabilities.sampling` so future Sampling calls
  prefer it (today it errors out with a clear migration message — full
  loop correlator scheduled for v0.10).
- New backend `MCPSamplingJudge` in `cognitive::judge` (auto-preferred
  when client supports sampling).
- New deps: `sha2 0.10`, `hex 0.4`. Optional: `metrics 0.24`,
  `metrics-exporter-prometheus 0.16`.

### Changed
- `cuba_juez` resolver order: `mcp_sampling` → `claude_cli` → `anthropic_api` → `heuristic`.
- `cuba_calibrar` JSON Schema gains `metrics` action.
- 25 MCP tools (was 24): `cuba_archivo` joins as the audit handler.
- Smoke test count bumped to 25.
- 4 new migrations (0014 source_trust, 0015 working_memory,
  0016 audit_log, 0017 rls_policies). Total: 17.

### Notes
- 3 RUSTSEC advisories from upstream transitive deps remain open
  (rustls-webpki via sqlx, rand via tokenizers/reqwest). All upstream;
  no remediation in this scope.
- MCP Sampling backend currently fails fast — wiring requires the
  request/response correlator refactor planned for v0.10. Auto-fallback
  keeps production unaffected.

---

## [0.9.0] — 2026-05-04 (Cargo `0.9.0` · npm `0.9.0` · PyPI `1.11.0`)

Search & Retrieval upgrades + Cognitive layer refinements + sqlx-migrate
foundation. Zero breaking changes — every new feature is opt-in via
`cuba_faro` arguments or activates automatically with safe defaults.

### Added
- **PR #5 sqlx-migrate** — replaced ad-hoc `*_MIGRATION` constants with
  versioned files in `rust/migrations/` (14 migrations, 0001 → 0014). The
  bootstrap is transparent for legacy v0.7/v0.8 DBs because every
  migration is idempotent (`DO $$ ... IF NOT EXISTS ... END $$`).
- **PR #6 Phase 1** — three new search modules:
  - `search::bm25` — BM25-flavored sparse retrieval via PostgreSQL
    `ts_rank_cd` (Robertson-Walker SIGIR 1994 baseline).
  - `search::mmr` — Maximal Marginal Relevance diversification with
    Jaccard token-set similarity (Carbonell-Goldstein SIGIR 1998).
  - `search::ood` — Out-of-distribution detection via Mahalanobis
    distance with ridge-regularized Σ⁻¹ (Lee NeurIPS 2018).
  - `search::budget` — exact `tiktoken-rs` cl100k_base counting (replaces
    the "len/4 chars per token" heuristic that mis-counted Spanish 30%).
- **PR #6 Phase 2-3** — `cuba_faro` exposes 5 new arguments:
  `enable_bm25` (default `true`), `diversify`, `mmr_lambda`,
  `abstain_ood`, `ood_threshold`. Output adds `bm25_score` to the score
  breakdown alongside `text_score`/`vector_score`. `verify` mode now
  bumps `hnsw.ef_search` to 200 transactionally for recall@10≈0.99.
- **PR #7 Phase 1** — `cognitive::prediction_error::adaptive_thresholds_conformal`
  uses empirical quantiles instead of z-score — distribution-free
  (Vovk-Gammerman-Shafer 2005, Angelopoulos-Bates 2023). Cosine
  similarities are anisotropic skewed-right (Ethayarajh EMNLP 2019), so
  z-score over-fires REINFORCE; conformal does not.
- **PR #7 Phase 2** — testing effect (Karpicke-Roediger Science 2008):
  `cuba_zafra decay` now scales halflife by `(1 + ln(1 + access_count))`,
  so a memory accessed 50× decays ≈4× slower than one accessed 0×.
- **PR #7 Phase 3** — Hebbian Δt-aware burst suppression in
  `cognitive::hebbian::boost_on_access`. The boost is multiplied by
  `(1 - exp(-Δt/τ))` with τ=600s. Re-access in the same second yields
  factor 0 (anti-saturation), Δt > 1h yields ≈1 (full boost).
- **PR #7 Phase 4** — Robbins-Monro stochastic learning rate in
  `cuba_eco`'s Oja positive/negative: `η = 0.05 / sqrt(1 + access_count/100)`.
  Convergence O(1/√t) bounds importance volatility on heavily-fed
  observations.
- **PR #7 Phase 5** — source credibility tracking. Migration
  `0014_source_trust.up.sql` adds `brain_source_trust(source, alpha,
  beta, updated_at)` pre-seeded with five standard sources. Each
  `cuba_calibrar resolve` updates the Beta(α, β) posterior of every
  source supporting the verified claim (Yin-Han-Yu IEEE TKDE 2008). New
  `cuba_calibrar trust` action returns posteriors with credible-interval
  width.
- New deps (lib): `sqlx` feature `migrate`, `tiktoken-rs 0.7`,
  `nalgebra 0.33` (no LAPACK), `async-trait 0.1`.
- 22 new tests (97 total: 84 unit + 13 smoke + 2 integration ignored).

### Changed
- `cuba_faro` JSON Schema in `constants.rs::tool_definitions()` extended
  with v0.9 args. Description updated to advertise MMR / OOD / tiktoken.
- `cuba_calibrar` JSON Schema gains the `trust` action.
- `cuba_zafra` `decay` response includes `testing_effect` annotation
  with the Karpicke-Roediger citation.
- `db.rs` shrunk from 310 → 100 lines (sqlx-migrate replaces nine
  hand-rolled migration constants).
- Smoke test `test_handler_dispatch_coverage` keeps the same 23-tool
  list (no new MCP tools added in v0.9 — all upgrades are arg
  extensions or new actions on existing handlers).

### Fixed
- 6 pre-existing clippy 1.94 warnings cleaned: `vec![...]` → array
  literals in `graph/pagerank.rs::tests`; `assert!(CONST > 0)` →
  `const _: () = assert!(...)` in `tests/smoke_test.rs` and
  `pagerank.rs::tests::test_pagerank_convergence_constants`.

### Notes for upgraders
- Existing v0.7 / v0.8 DBs auto-migrate on first boot. The
  `_sqlx_migrations` table is created automatically and populated with
  the 14 historical migrations on the first run; subsequent boots
  apply only deltas.
- Legacy `embeddings/onnx.rs` heuristic `count_tokens` is now
  superseded by `search::budget::count_tokens`.

---

## [0.8.0] — 2026-05-04 (Cargo `0.8.0` · npm `0.8.0` · PyPI `1.10.0`)

Engram-Cloud-inspired additions: 4 new tools + audit of all 19 v0.7
handlers for project scoping. Zero breaking changes — every filter is
opt-in via `cuba_jornada start --project NAME`.

### Added
- **`cuba_proyecto`** (PR #1) — per-project isolation via `project_id
  UUID NULL` on six core tables, `brain_projects` registry, six actions
  (`list / current / switch / stats / rename / merge`). NULL = global =
  back-compat. Kill-switch `CUBA_PROJECT_FILTER=off`.
- **`cuba_pre_compact`** (PR #2) — survives `/compact`. `snapshot`
  persists session state (recent obs, decisions, unresolved errors,
  pending embeddings, goals) into `brain_compaction_snapshots`;
  `restore` returns the latest snapshot for the active session.
  `cuba_jornada current` now returns `compaction_hint: bool`.
- **`cuba_sync`** (PR #3) — git-friendly export/import with
  `export | import | diff | status`. Layout: `manifest.json`,
  `entities/<slug>.json` (each with embedded observations),
  `episodes/<yyyy-mm>/<id>.json`, `decisions/<id>.json`,
  `errors/<id>.json`, `relations.json`, optional `embeddings.bin.zst`.
  Idempotent via `ON CONFLICT DO NOTHING` + `brain_sync_state`. Schema
  versioning with hash-derived dedup. Path traversal protection.
- **`cuba_juez`** (PR #4) — LLM-judge for ambiguous (cosine 0.6-0.8)
  contradictions. Trait `ContradictionJudge` with three backends:
  `ClaudeCodeJudge` (subprocess to `claude` CLI, $0 with subscription),
  `AnthropicApiJudge` (feature flag `anthropic-api`), `HeuristicJudge`
  (fallback wrapping the bilingual negation marker check).
  Permanent cache via `brain_judgments(observation_a, observation_b)`
  UNIQUE index.
- WRITE audit: `cronica`, `alma`, `alarma`, `puente`, `decreto` populate
  `project_id` from current session.
- READ audit: `faro`, `vigia`, `expediente`, `contradiccion`,
  `reflexion`, `hipotesis`, `decreto`, `puente`, `alma`, `cronica` apply
  `($N::uuid IS NULL OR project_id = $N OR project_id IS NULL)` filter.

### Changed
- 23 MCP tools (was 19). 75 tests (was 68). 0 clippy warnings.

---

## [0.7.0] — Earlier 2026

10 algorithmic improvements + 19 bug fixes + comprehensive audit
(condensed): PageRank α=0.3 blend (preserves Hebbian/BCM learned
importance), hybrid verify (trigram + embedding fusion), ONNX
concurrency semaphore (Little's Law), sigmoid entropy routing
(Jaynes 1957 MaxEnt), word-level session boost, weighted Hebbian
neighbor diffusion (Collins-Loftus 1975), exponential coverage
saturation, O(n) entropy. Fixed: hash embeddings corrupting DB,
centrality normalization, cache LRU, jornada race condition, six MCP
schemas. Removed `blake3` dependency. 68 tests, 0 clippy warnings,
0 tech debt.

[0.9.0]: https://github.com/LeandroPG19/cuba-memorys/releases/tag/v0.9.0
[0.8.0]: https://github.com/LeandroPG19/cuba-memorys/releases/tag/v0.8.0
[0.7.0]: https://github.com/LeandroPG19/cuba-memorys/releases/tag/v0.7.0
