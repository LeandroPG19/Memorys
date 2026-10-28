# 0.32 C4-resto — lo que todavía traga fallos de lectura

Especificador (Swarm Forge). Sin código, sin tests, sin cargo: todo sale de leer el árbol en la
rama `feat/0.32-analisis-y-sota` (HEAD `196f9d7`) y de `git log`. Nada se corrió.

Política (la de C4, ya aplicada a faro, contexto, jornada, calibrar, pre_compact, alarma y
`alma get`): una lectura secundaria que falla **no se convierte en «vacío» ni en 0**. La respuesta
sigue donde puede, **dice qué señal no pudo leer** (nombre estable para un programa y una razón en
palabras con lo que cuesta esa ausencia) y una respuesta sana no lleva ninguna clave nueva.
Patrón exacto a reusar: `rust/src/degraded.rs` (`LostSignals::note` + `LostSignals::annotate` →
`signals_degraded: true`, `signals_lost: [...]`, `signals_degraded_reason`), y los tests
`rust/tests/v032_the_rest_say_what_they_could_not_read.rs` y
`rust/tests/v032_a_search_says_what_it_could_not_read.rs` (base de scratch por test con
`common::in_a_scratch_database`, `break_it` con la cuenta dueña, `must_say_it` / `must_be_silent`,
`as_a_client` para correr en paralelo).

## 0. Qué ya está hecho (no se especifica)

Commits `3e740b4` y `9a266e9` (rojo), `2f99e4e` (verde) y `e0a715c`: faro (`session_context`,
`annotate_evidence`, `written_since`), contexto, pre_compact, jornada, alarma, `alma get`,
calibrar, `centinela::check_triggers`. `git log -- rust/src/{admin,protocol,graph_db}.rs` no
muestra ningún commit de C4: esos tres ficheros están intactos.

## 1. Sitios pendientes (ruta:línea, lo que se pierde hoy)

Contador de apoyo (regex multilínea `.await` seguido de `unwrap_or*`/`.ok()`): `admin.rs` 12,
`protocol.rs` 11, `graph_db.rs` 5, `faro.rs` 6. Abajo está lo que de verdad es una lectura de base
de datos que cambia lo que el lector cree.

### A. `rust/src/admin.rs` — el panel de estado (cuatro métodos `admin/*`)

Todas devuelven `Ok` con un 0 o una lista vacía cuando la base no se pudo leer. El panel
(`rust/src/panel/index.html`) lo pinta como hecho: «Migraciones 0», «Observaciones 0», «el ciclo
REM nunca ha corrido en esta base» y, la peor, «Nada que revisar: ningún chequeo en rojo, ningún
conflicto abierto, nada en cuarentena» cuando ninguna de esas lecturas funcionó.

| Línea | Método | Lectura | Hoy, si falla | Señal |
|---|---|---|---|---|
| 79-82 | status | `count(*) FROM _sqlx_migrations` | `migrations_applied: 0` | `migrations_applied` |
| 83-88 | status | `pg_size_pretty(pg_database_size(..))` | `database_size: null` (el panel pone «—», igual que «no aplica») | `database_size` |
| 91-99 | status | los cuatro `count(*)` del corpus | `corpus: {0,0,0,0}` | `corpus` |
| 177-185 | status (vía `rem_status`) | `brain_rem_cycles` | `last: null, stale: true` → «el ciclo REM nunca ha corrido» | `rem_cycles` |
| 188-198 | clients | `brain_sessions` activas | `sessions: []` | `sessions` |
| 200-206 | clients | `brain_sync_peers` | `peers: []` | `peers` |
| 225-233 | traffic | `brain_handler_failures` | `failures: []` (el panel cae a «recent», que parece sano) | `handler_failures` |
| 258-267 | problems | `brain_sync_conflicts` abiertos | `conflicts: []` | `sync_conflicts` |
| 269-276 | problems | cuarentena (3 tablas) | `quarantined: 0` | `quarantined` |
| 278-282 | problems | `brain_errors` sin resolver | `unresolved_errors: 0` | `unresolved_errors` |
| 293 | problems | `sync::pending_notices` | `peer_notices: []` | `peer_notices` |

No entran (sección 6): `admin.rs:76` y `253` (`DATABASE_URL` es una variable de entorno, no una
lectura) y `admin.rs:89` (`db::node_id`).

### B. `rust/src/handlers/faro.rs`

Nombrados en el CHANGELOG:

| Línea | Qué | Hoy, si falla | Señal |
|---|---|---|---|
| 1078-1087 `mentioned_entities` (llamadas en 816 y 901) | `SELECT name FROM brain_entities` | `Err(_) => Vec::new()`: sin entidades mencionadas, `factoid_effective_scope` (901-905) **cambia la estrategia de la búsqueda** y el multi-hop (816) pierde sus semillas. Sin traza alguna, ni log. | `mentioned_entities` |
| 1316-1332 `verify_claim`, rama semántica | consulta vectorial de evidencia | `unwrap_or_default()` → menos evidencia → confianza más baja y, con cero filas, `interpretation: "Nothing in memory relates to this claim. No grounding — treat as unverified."` (1453-1457): una afirmación que puede ser falsa. Sin log. | `semantic_evidence` |
| 1333-1336 | `embed(claim)` falla | `warn!` y `vec![]` (mismo efecto, sólo log) | `semantic_evidence` (misma señal) |

Mismo flujo `verify_claim`, no nombrados pero del mismo defecto (decisión mía: entran, son la
misma función):

| Línea | Qué | Hoy, si falla | Señal |
|---|---|---|---|
| 1412-1421 | `INSERT INTO brain_verify_log … .ok()` | la verificación no queda registrada: `calibrar` nunca la verá y el agente no se entera | `verify_log` |
| 1423-1432 | calibración histórica `.ok().flatten()` | falta `calibrated_accuracy` (1478), que se lee igual que «sin historial todavía» | `calibration_history` |

Hermanos del mismo patrón encontrados al leer `faro.rs`, NO en la lista del CHANGELOG (tajada B2,
opcional, sección 5):

| Línea | Qué | Hoy | Señal |
|---|---|---|---|
| 149 (`annotate_sync_freshness`) | `brain_sync_peers`, `Err(_) => return` | sin aviso de «esta máquina lleva N h sin sincronizar»: se lee como «al día» | `peer_freshness` |
| 1636 (`text_search`) | episodios, búsqueda léxica | `unwrap_or_else` con `tracing::error!` cuyo texto admite «nothing in the response says so» | `episodes_lexical` |
| 1783 (`vector_search`) | episodios, búsqueda vectorial | ídem, «carries observations only» | `episodes_vector` |
| 1152 (`entity_factoid_observations`) | `Err → debug!` + vacío | los resultados «entity_factoid» desaparecen con log nivel debug | `entity_factoid` |
| 1193 (`observations_for_entities`) | `Err → debug!` + vacío | la expansión PPR del multi-hop | `ppr_observations` |
| 1844, 1858, 1887 (`associative_expand`) | `warn!` + `return`/`continue` | con `associative: true` la expansión se salta en silencio | `associative_expansion` |
| 2059 (`enrich_graphrag`) | `warn!` cuyo texto dice «reads exactly like these entities having no neighbours» + `[]` | `graphrag_context: []` | `graphrag_neighbours` |

### C. `rust/src/protocol.rs`

C1. El ciclo REM (`run_rem_consolidation_locked`, 746-945). Cada paso que falla hace `warn!` y
devuelve 0, y el ciclo termina con `error: None`: la fila de `brain_rem_cycles` y el panel dicen
«ciclo limpio». El propio log miente a veces (`info!("episode power-law decay applied")` con 0
tras un fallo).

| Línea | Paso | Hoy | Señal (paso) |
|---|---|---|---|
| 853-862 | decaimiento de episodios `.unwrap_or(0)` | 0 y un `info!` de éxito | `episode_decay` |
| 1030-1031 | `information_schema` de `embedding_half` `.ok().flatten()` | «no hay columna», salta sin una palabra | `halfvec_backfill` |
| 1042-1051 | relleno halfvec (ya hace `warn!`) | 0 | `halfvec_backfill` |
| 1064-1084 | estadísticas del planificador, dos `.ok().flatten()` | «no hace falta analizar», sin log | `planner_stats` |
| 1121-1123 | cola del escaneo de relaciones `.unwrap_or(0)` | lote pequeño por defecto, sin log | `relation_scan_queue` |
| 1135-1143 | listado de entidades sin escanear | `warn!` + informe vacío (el ciclo parece sin trabajo) | `relation_scan_listing` |
| 1174-1186 | listado de observaciones por extraer | ídem | `extraction_listing` |
| 1241-1250 `rem_count_duplicate_candidates` | conteo de duplicados `.unwrap_or(0)` | **se persiste 0 en `brain_rem_cycles.duplicate_candidates`** y el panel muestra «Duplicados 0»: un hecho falso grabado | `duplicate_count` |
| 1262-1287 | candidatos y aplicación de autolink | `warn!` + 0 | `autolink` |
| 1291-1321 | relleno de chunks y de embeddings | `warn!` + 0 / informe por defecto | `chunk_backfill`, `embedding_backfill` |
| 906-917 | detección de comunidades | `warn!` + `communities: 0` | `community_detection` |

C2. Recursos MCP (`resources/list`, `resources/read`), mismo fichero:

| Línea | Qué | Hoy, si falla | Señal |
|---|---|---|---|
| 1342-1347 | entidades de `resources/list` | la lista omite todas las entidades | `resource_entities` |
| 1355-1361 | proyectos | ídem | `resource_projects` |
| 1376-1383 | snapshots | ídem | `resource_snapshots` |
| 1448-1456 `read_resource entity/…` | observaciones de la entidad | la entidad existe y responde `"observations": []`: un hecho falso | `entity_observations` |

Parece del patrón y no lo es: `protocol.rs:1371-1373` y `1507-1510`
(`current_project_id().ok().flatten()`): `project::current_project_id` (`project.rs:23-28`) no lee
la base ni devuelve nunca `Err`. Sección 6.

### D. `rust/src/graph_db.rs` — `reconcile` (802, 833, 845, 872)

`reconcile` pagina Postgres y proyecta al grafo; sólo lo llama `graph_cli reconcile`.

- **802, 833, 872** (`fetch_all(..).unwrap_or_default()`): una página que no se pudo leer es una
  página vacía → `if page.is_empty() { break }` → el bucle termina y el informe sale
  **`ok: true`** con `postgres_entities`/`postgres_relations`/`postgres_artifacts` más bajos. Un
  informe de reconciliación que sale limpio y truncado es lo peor que puede decir una herramienta
  cuyo único trabajo es comparar.
- **845**: segunda consulta, de ids de relaciones. Si falla, el `zip` con `ids` vacío no avanza
  `last_id`; con una página llena (250) el bucle repite la misma página, vuelve a proyectar y
  suma contadores sin fin. No es sólo silencio: es un bucle. (Que haya dos consultas con el mismo
  `LIMIT` puede además divergir entre ellas; fusionarlas en una sola que devuelva `r.id` es del
  implementador y elimina la lectura 845. El comportamiento exigido es el mismo.)
- `graph_cli.rs:19-25` imprime el informe y sale 0 siempre.

## 2. Decisiones de diseño (mías; cómo se revierte cada una)

1. **Un valor perdido es `null`, no 0 ni `[]`** en `admin/*` (los consumidores son el panel y
   humanos; `0` y `[]` son afirmaciones). Revertir: `lost.note` con el default.
2. **Admin sigue devolviendo `Ok`**, con las claves `signals_degraded`, `signals_lost`,
   `signals_degraded_reason` (`LostSignals::annotate`), y el panel muestra la razón. No falla con
   error: el panel es la herramienta con la que se diagnostica una base mala, y en blanco no sirve.
   (Contraste con `calibrar`, donde la lectura es la acción entera y falla.)
3. **REM**: el rastro persistente va en una columna nueva `steps_lost text[] NOT NULL DEFAULT '{}'`
   de `brain_rem_cycles` (migración `0066_rem_cycle_steps_lost`, expand puro: una columna con valor
   por defecto, compatible con el binario anterior; con `.down.sql`). No se reutiliza `error`: eso
   haría pasar por «ciclo fallido» (`status: fail` en el panel) un ciclo que hizo casi todo.
   Revertir: `down.sql` y quitar la lectura. Hasta la 0060 las migraciones están congeladas
   (`migrations_contract.rs`, `FROZEN_THROUGH = 60`); la 0066 queda por encima y no toca el digest.
   En el rojo, comprobar que ningún test fija el número de la última migración.
4. **`resources/list`** lleva la degradación en `result._meta` (las mismas tres claves), porque el
   resultado de un método de protocolo no es un cuerpo JSON de herramienta. **`resources/read
   entity/…`** la lleva en el cuerpo JSON (el `text`), como las herramientas.
5. **`reconcile`** falla en lo que es: una lectura que es la acción entera. Informe con
   `ok: false`, `incomplete: true`, `error` con la tabla y la causa, y los contadores hasta donde
   llegó; `graph_cli reconcile` sale con código distinto de 0 **sólo** con `incomplete: true` (los
   `ok: false` de «backend off» y «no alcanzable» no cambian de código).
6. **Verify**: «el modelo ONNX no está cargado» NO es una lectura que falló (es una instalación
   sin modelo; ya lo dice el check `embedding` de `doctor`). Sólo cuenta como señal perdida que
   `embed(claim)` o la consulta fallen estando el modelo cargado.
7. Los nombres de señal de arriba son contrato: un programa los compara por igualdad.

## 3. Gherkin por tajada

Convención de «sano»: una respuesta sin ningún fallo es **idéntica a la de hoy** y no lleva
`signals_degraded`, `signals_lost` ni `signals_degraded_reason` (`must_be_silent`).

### Tajada A — admin

```gherkin
Característica: el panel de estado dice qué no pudo leer

  Escenario: una base sana no cambia nada
    Dado una base migrada con datos en todas las tablas
    Cuando pido admin/status, admin/clients, admin/traffic y admin/problems
    Entonces ninguna respuesta lleva signals_degraded, signals_lost ni signals_degraded_reason
    Y los números son los de hoy

  Esquema del escenario: una tabla que no se puede leer se nombra y su valor no es 0 ni vacío
    Dado una base migrada y <rotura> hecha con la cuenta dueña
    Cuando pido <metodo>
    Entonces la respuesta es Ok y sigue trayendo lo demás
    Y signals_degraded es true y signals_lost es exactamente [<señal>]
    Y <campo> es null (no 0 ni [])
    Y signals_degraded_reason dice en palabras qué significa esa ausencia y menciona `memory-industry doctor`

    Ejemplos:
      | metodo         | rotura                                                                      | señal              | campo                                  |
      | admin/status   | quitar la tabla _sqlx_migrations                                            | migrations_applied | migrations_applied                     |
      | admin/status   | renombrar la tabla brain_relations                                          | corpus             | corpus.observations (y las otras tres) |
      | admin/status   | quitar la tabla brain_rem_cycles                                            | rem_cycles         | rem.last, rem.recent y rem.stale       |
      | admin/clients  | renombrar la columna brain_sessions.session_name                            | sessions           | sessions                               |
      | admin/clients  | quitar la tabla brain_sync_peers                                            | peers              | peers                                  |
      | admin/traffic  | quitar la tabla brain_handler_failures                                      | handler_failures   | failures                               |
      | admin/problems | quitar la tabla brain_sync_conflicts                                        | sync_conflicts     | conflicts                              |
      | admin/problems | renombrar la columna brain_errors.trust                                     | quarantined        | quarantined                            |
      | admin/problems | renombrar la columna brain_errors.resolved                                  | unresolved_errors  | unresolved_errors                      |
      | admin/problems | quitar la tabla brain_peer_notices                                          | peer_notices       | peer_notices                           |

  Escenario: la base entera no responde
    Dado una base migrada y su pool cerrado
    Cuando pido admin/status, admin/clients, admin/traffic y admin/problems
    Entonces ninguna entra en pánico y cada una responde con signals_lost que incluye todas sus
      señales (status incluye database_size y migrations_applied)
    Y ningún número es 0 ni ninguna lista es []
    (Si `doctor::run_checks` entra en pánico con el pool cerrado, es un hallazgo aparte: se
     anota, no se arregla dentro de esta tajada.)

  Escenario: el panel no dice «al día», «nunca ha corrido» ni «nada que revisar» sobre lo que no leyó
    Dado que admin/status trae signals_lost con rem_cycles
    Cuando el panel pinta el estado del REM
    Entonces el pill es «atención» y la nota dice que no se pudo leer brain_rem_cycles
    Y no dice «el ciclo REM nunca ha corrido» ni «al día»
    Dado que admin/problems trae signals_lost no vacío
    Cuando el panel pinta los problemas
    Entonces muestra la razón y no el texto «Nada que revisar»
    Y cuando admin/status trae signals_lost el panel muestra la razón en la cabecera
```

Tests: `rust/tests/v032_the_status_panel_says_what_it_could_not_read.rs`, con la estructura de
`v032_the_rest_say_what_they_could_not_read.rs`. El último escenario (panel) no se prueba con regex
sobre el HTML (`verificacion.md`): lo cubre el recorrido QA con Playwright de la sección 4.
`v035_the_rem_cycle_leaves_a_trace.rs` y `v026_the_panel_*` no deben cambiar.

### Tajada B1 — faro: lo nombrado y el mismo flujo de verify

```gherkin
Característica: faro no cambia de estrategia ni de veredicto en silencio

  Escenario: una búsqueda sana no cambia
    Dado un corpus con una entidad de nombre largo
    Cuando hago faro hybrid sobre su nombre
    Entonces no hay claves de degradación y la estrategia es la de hoy

  Escenario: las entidades mencionadas no se pudieron leer
    Dado que la lectura de nombres de brain_entities falla
    Cuando se llama mentioned_entities con la consulta
    Entonces la lectura perdida se anota como mentioned_entities
    Y la respuesta de hybrid_search lleva signals_lost con mentioned_entities
    Y signals_degraded_reason dice que la consulta se trató como si no mencionara ninguna entidad
      y que eso cambia el alcance y las semillas del multi-hop

  Escenario: la evidencia semántica de una verificación no se pudo leer
    Dado el modelo cargado y la consulta semántica de evidencia falla
    Cuando hago faro mode=verify
    Entonces la respuesta lleva signals_lost con semantic_evidence
    Y la razón dice que «nada relacionado» puede ser falso: faltó la mitad semántica
    Y la `interpretation` «Nothing in memory relates to this claim» no sale sola, sin la señal

  Escenario: el modelo no está cargado
    Dado que no hay modelo ONNX
    Cuando hago faro mode=verify
    Entonces la respuesta no lleva signals_degraded (es una instalación, no una lectura fallida)

  Escenario: la verificación no quedó registrada
    Dado que el INSERT en brain_verify_log falla
    Cuando hago faro mode=verify
    Entonces la respuesta sigue trayendo confianza y evidencia
    Y signals_lost incluye verify_log y la razón dice que calibrar no verá esta verificación

  Escenario: el historial de calibración no se pudo leer
    Dado que la lectura de calibración en brain_verify_log falla
    Cuando hago faro mode=verify
    Entonces signals_lost incluye calibration_history
    Y la razón dice que la falta de calibrated_accuracy no significa «sin historial»
```

Seams (los crea el implementador; no cambian comportamiento): `mentioned_entities` recibe
`&mut LostSignals`; la consulta semántica de verify sale a una función que recibe el embedding ya
calculado (`semantic_evidence_rows(pool, emb, project_id)`) para probarla sin modelo. Motivo: no
hay rotura de tabla que aísle `mentioned_entities` de la búsqueda (`text_search` también lee
`brain_entities`, con `?`): es lo que dice el CHANGELOG. Se prueban en `--lib` (dentro de
`faro.rs`, con pool cerrado o columna renombrada). El flujo `verify` por el handler usa roturas
aisladas: renombrar `brain_verify_log.entity_name` rompe sólo el INSERT; renombrar
`brain_verify_log.outcome` rompe sólo la calibración; renombrar `brain_observations.embedding`
rompe sólo la rama semántica (por eso el seam). Para la respuesta de verify se usa
`LostSignals::annotate` con su propio texto de costes (el de `annotate_lost_signals` habla del
ranking y no aplica). Un claim sin evidencia alcanza el INSERT y la calibración, así que basta un
corpus vacío para esos dos.

### Tajada B2 — faro: los hermanos (opcional, sección 5)

```gherkin
  Escenario: el pie de sincronización no se pudo leer
    Dado que brain_sync_peers no se puede leer
    Cuando hago cualquier faro
    Entonces signals_lost incluye peer_freshness y la razón dice que la ausencia de aviso de
      sincronización no significa que los pares estén al día

  Escenario: los episodios no se pudieron buscar
    Dado que brain_episodes.search_vector no se puede leer
    Cuando hago faro hybrid con scope all
    Entonces las observaciones siguen en el ranking
    Y signals_lost incluye episodes_lexical y la razón dice que ningún episodio pudo salir
    (episodes_vector: mismo comportamiento; sin modelo ONNX no hay prueba, sección 6)

  Esquema del escenario: las expansiones del ranking y del contexto
    Dado que <lectura> falla
    Cuando hago faro con <condicion>
    Entonces signals_lost incluye <señal> y la razón lo explica
    Ejemplos:
      | lectura                                          | condicion                    | señal                 |
      | observaciones del factoid de entidad             | consulta factoid con entidad | entity_factoid        |
      | observaciones de la expansión PPR                | consulta multi-hop           | ppr_observations      |
      | semilla, activación u observaciones asociativas  | associative: true            | associative_expansion |
      | vecinos del graphrag                             | resultados con entidades     | graphrag_neighbours   |
```

### Tajada C1 — REM

```gherkin
Característica: un ciclo REM que se saltó un paso lo dice en su fila

  Escenario: un ciclo sano
    Dado una base sana
    Cuando corre un ciclo REM
    Entonces la fila de brain_rem_cycles tiene steps_lost vacío
    Y el panel no lo marca

  Esquema del escenario: un paso que no se pudo leer o escribir se nombra
    Dado que el paso <paso> falla
    Cuando el ciclo termina
    Entonces el ciclo no se marca fallido (error es null)
    Y la fila lleva steps_lost con <paso>
    Y admin/status muestra ese paso como «no se pudo» y no como un 0 limpio
    Ejemplos:
      | paso                  | cómo se provoca                                                           |
      | episode_decay         | renombrar brain_episodes.importance (sólo lo usa este paso); ciclo entero |
      | duplicate_count       | rem_count_duplicate_candidates con pool cerrado                           |
      | autolink              | pool cerrado sobre el paso privado (test en --lib de protocol.rs)         |
      | embedding_backfill    | ídem                                                                      |
      | chunk_backfill        | ídem                                                                      |
      | halfvec_backfill      | ídem                                                                      |
      | planner_stats         | ídem                                                                      |
      | relation_scan_queue   | ídem                                                                      |
      | relation_scan_listing | ídem                                                                      |
      | extraction_listing    | ídem                                                                      |
      | community_detection   | ídem                                                                      |

  Escenario: el conteo de duplicados no queda grabado como un 0 sin marca
    Dado que el conteo de duplicados falla
    Cuando corre el ciclo
    Entonces steps_lost incluye duplicate_count
    (lo que valga la columna duplicate_candidates lo decide el implementador; lo que no puede
     pasar es un 0 sin marca)

  Escenario: el panel
    Dado una fila con steps_lost no vacío
    Cuando el panel pinta el REM
    Entonces el pill es «atención» y lista los pasos que se saltó
    Y no dice «al día»
```

Notas: la firma pública `rem_count_duplicate_candidates(&PgPool) -> i64` la usa
`rust/tests/v031_rem_wiring.rs:194,214`. Si cambia, ese test cambia **a propósito y con aviso en
el handoff** (devolver `Result<i64>` o recibir `&mut LostSignals`); si no se quiere tocar, se añade
una función hermana. Los pasos privados se prueban en `--lib` dentro de `protocol.rs`, como ya
hace `a_cycle_that_dies_mid_flight_leaves_a_row_carrying_its_own_error` (línea 2040).

### Tajada C2 — recursos MCP

```gherkin
Característica: la lista de recursos no omite en silencio

  Escenario: sano
    Dado entidades, proyectos y snapshots legibles
    Cuando pido resources/list
    Entonces el resultado no trae _meta con degradación y trae las mismas entradas de hoy

  Esquema del escenario: una de las tres listas no se pudo leer
    Dado que <lista> no se puede leer
    Cuando pido resources/list
    Entonces los tres recursos fijos y las otras dos listas están
    Y result._meta.signals_lost es [<señal>] y la razón dice que faltan esas entradas, no que no existan
    Ejemplos:
      | lista     | rotura                                          | señal              |
      | entidades | renombrar brain_entities.access_count          | resource_entities  |
      | proyectos | quitar la tabla brain_projects                  | resource_projects  |
      | snapshots | quitar la tabla brain_compaction_snapshots      | resource_snapshots |

  Escenario: una entidad que existe pero cuyas observaciones no se pudieron leer
    Dado una entidad existente y brain_observations ilegible
    Cuando pido resources/read cuba://entity/<nombre>
    Entonces el cuerpo trae observations: null (no [])
    Y trae signals_lost con entity_observations y su razón
    Y una entidad que no existe sigue dando el error «entity not found»
```

### Tajada D — graph_db reconcile

```gherkin
Característica: reconcile no sale limpio si no pudo leer

  Escenario: sano
    Dado un backend falkor alcanzable (un oyente en 127.0.0.1:0 que acepta y responde +PONG)
      y unas entidades, relaciones y artefactos
    Cuando corre reconcile
    Entonces ok es true, no trae incomplete ni error, y los contadores son los de hoy

  Esquema del escenario: una página que no se puede leer
    Dado que <tabla> no se puede leer
    Cuando corre reconcile
    Entonces ok es false e incomplete es true
    Y error nombra <tabla> y la causa
    Y los contadores son los de lo que sí llegó a leer
    Y reconcile vuelve (no hay bucle)
    Ejemplos:
      | tabla           | rotura                         |
      | brain_entities  | quitar la tabla brain_entities |
      | brain_relations | quitar la tabla brain_relations|
      | brain_artifacts | quitar la tabla brain_artifacts|

  Escenario: la lectura de ids de relaciones (845) falla con más de una página
    Dado más de 250 relaciones y que la lectura auxiliar de ids falla
    Cuando corre reconcile
    Entonces termina con incomplete: true en vez de repetir la misma página sin fin
    (si el implementador fusiona las dos consultas, este escenario desaparece con la lectura; se
     pide entonces uno equivalente que pruebe que cada página se lee una sola vez)

  Escenario: el CLI
    Dado un informe con incomplete: true
    Cuando corre `memory-industry graph reconcile`
    Entonces imprime el informe y sale con un código distinto de 0
    Y con ok: false por backend off o por servidor no alcanzable el código no cambia
```

Aislamiento: el test arranca su propio oyente RESP en un puerto efímero (sin esperar a la red;
`ci-rapido.md`) y fija `MEMORY_INDUSTRY_GRAPH_DB=falkor` y `MEMORY_INDUSTRY_GRAPH_URL` por entorno
de proceso, así que va en su propio binario y en serie dentro de él (`envs::ScopedEnv` si es
público; si no, el implementador lo dice y se pide hacerlo público). El conteo de proyecciones
fallidas (`.is_ok()` en 808/849/878) no cambia: sección 6.

## 4. Procedimiento QA y sabotajes

Para cada tajada: (1) el orquestador corre los tests del rojo y pega la línea; (2) tras el verde,
el QA hace el recorrido de abajo contra un daemon real con una base de prueba (nunca la viva);
(3) sabotajes: se rompe UNA línea de producción, se corre el test acotado y se ve ROJO, se
restaura. Un sabotaje que deja el test verde invalida la tajada.

### Recorridos

A. admin y panel. Daemon con panel sobre una base clonada; con un token de administrador, POST a
`/mcp` con `{"jsonrpc":"2.0","id":1,"method":"admin/status"}`: sano → sin `signals_*`. Como dueño
con `psql`, renombrar `brain_relations`: repetir → `corpus` null y `signals_lost: ["corpus"]`;
restaurar. Abrir `/panel` con Playwright: con `brain_rem_cycles` renombrada el pill REM dice
«atención», no «sin ciclo» ni «al día»; con `brain_sync_conflicts`, `brain_errors.trust` y
`brain_peer_notices` rotas, Problemas muestra la razón y no «Nada que revisar».

B. faro. `memory_faro` sano: sin claves. Quitar `brain_verify_log` y `faro mode=verify "…"`: trae
`signals_lost` con `verify_log` y `calibration_history`. Renombrar `brain_sync_peers` (B2):
`peer_freshness`.

C. REM y recursos. `memory-industry rem` (`rem_cli`) con `brain_episodes.importance` renombrada: la
fila nueva lleva `steps_lost = {episode_decay}` y `error` nulo; `admin/status` la devuelve y el
panel la lista. Restaurar y correr otro: `steps_lost` vacío. `resources/list` con `brain_projects`
quitada → `_meta.signals_lost = ["resource_projects"]`.

D. reconcile. Oyente RESP falso (un `python3` con `socketserver` que responde `+PONG\r\n` a todo)
en un puerto efímero y `memory-industry graph set falkor` apuntando a él: `graph reconcile` sano →
`ok:true`, código 0. Quitar `brain_entities` → `ok:false`, `incomplete:true`, código ≠ 0, no se
cuelga.

### Sabotajes (línea de producción a romper → test que debe ponerse rojo)

| Tajada | Sabotaje | Test que se pone rojo |
|---|---|---|
| A | `admin.rs:82` volver a `.unwrap_or(0)` en vez de pasar por `lost` | caso `migrations_applied` |
| A | `admin.rs:99` volver a `.unwrap_or((0,0,0,0))` | caso `corpus` (espera null) |
| A | `rem_status` (185) volver a `unwrap_or_default()` | caso `rem_cycles` y el del panel «nunca ha corrido» |
| A | en cualquiera de los 11 sitios, devolver 0/[] aunque se anote la señal | el aserto «campo es null» de su caso |
| A | quitar la llamada a `annotate` de `problems` | caso `peer_notices` |
| A | anotar una señal con una lectura sana (marcar `peers` siempre) | `must_be_silent` del escenario sano |
| A | `panel/index.html`: quitar el chequeo de `signals_lost` en `renderProblems` | recorrido Playwright (se ve «Nada que revisar») |
| B1 | `mentioned_entities` vuelve a `Err(_) => return Vec::new()` | test `--lib` con pool cerrado y el de hybrid_search |
| B1 | rama semántica de verify vuelve a `.unwrap_or_default()` | `semantic_evidence_rows` con `embedding` renombrada |
| B1 | volver a `.ok()` tras el INSERT de `brain_verify_log` (1421) | caso `verify_log` |
| B1 | volver a `.ok().flatten()` en la calibración (1432) | caso `calibration_history` |
| B1 | emitir `signals_degraded` también sin modelo | escenario «modelo no cargado» |
| B2 | `annotate_sync_freshness`: volver a `Err(_) => return` | `peer_freshness` |
| B2 | 1636: quitar la anotación y dejar sólo el `tracing::error!` | `episodes_lexical` |
| C1 | 853-862 vuelve a `.unwrap_or(0)` | paso `episode_decay` |
| C1 | 1241-1250 vuelve a `.unwrap_or(0)` | paso `duplicate_count` |
| C1 | `record_rem_cycle` no escribe `steps_lost` | el test de la fila |
| C1 | marcar `error` cuando sólo se perdió un paso | «el ciclo no se marca fallido» |
| C2 | 1342-1347 / 1355-1361 / 1376-1383 vuelven a `unwrap_or_default()` | su caso del esquema |
| C2 | 1448-1456 vuelve a `unwrap_or_default()` | `entity_observations` (espera null, no []) |
| D | 802 / 833 / 872 vuelven a `unwrap_or_default()` | el caso de su tabla (hoy sale `ok:true`) |
| D | 845 vuelve a `unwrap_or_default()` | escenario de >250 relaciones (el test lleva `timeout`, porque cuelga) |
| D | `graph_cli` sale 0 con `incomplete` | el test del CLI |

## 5. Orden de las tajadas y ficheros compartidos

| Tajada | Ficheros de producción | Dependencia |
|---|---|---|
| A admin | `rust/src/admin.rs`, `rust/src/panel/index.html` | ninguna |
| B1 faro | `rust/src/handlers/faro.rs` | ninguna |
| D reconcile | `rust/src/graph_db.rs`, `rust/src/graph_cli.rs` | ninguna |
| C1 REM | `rust/src/protocol.rs`, `rust/migrations/0066_*` (up y down), `rust/src/admin.rs` (`rem_cycle_json`), `rust/src/panel/index.html` | **después de A** (mismo `admin.rs` y mismo HTML) |
| C2 recursos | `rust/src/protocol.rs` | **después de C1** (mismo fichero) |
| B2 faro hermanos | `rust/src/handlers/faro.rs` | **después de B1** (mismo fichero); opcional |

A, B1 y D van a la vez (tres implementadores, el tope): no comparten ficheros. C1 al cerrar A; C2
al cerrar C1; B2 al cerrar B1. Nunca `tester` junto a implementadores; un solo cargo pesado a la
vez, con `pesado --`.

Riesgos de puerta: `verify_claim` e `hybrid_search` están en la línea base de CRAP (`hybrid_search`
quedó en CC 24); cada señal nueva sale a una función chica (el seam `semantic_evidence_rows` ya lo
hace) y no debe subir ninguna función existente. Los tests nuevos mueven el conteo de la línea base
de dos sentidos. Cada fichero de test lleva el encabezado `// needs-a-server:` y debe quedar bajo el
tope de 10 s por binario (`test times` ya marcó tres binarios v032 en esta VM): un fichero por
tajada y tests en paralelo con `as_a_client`.

Handoff sugerido (no lo creo yo: el encargo me prohíbe tocar otra cosa; lo escribe el orquestador
en `.cursor/handoffs/0.32-c4-resto-<tajada>.yml`):

```yaml
from: especificador
to: implementador
type: git_handoff
task: "0.32 C4-resto, tajada <A|B1|D|C1|C2|B2>, pasada 1 de 2 (sólo tests): ver backlog/0.32-especificaciones/C4-resto.md, Gherkin de esa tajada"
commit: none
evidence: "none: lectura de código y git log, nada corrido"
```

## 6. Qué NO entra y por qué

1. `admin.rs:76` y `253` (`DATABASE_URL` con `unwrap_or_default`): variable de entorno, no una
   lectura. Vacía, `doctor::run_checks` ya la reporta.
2. `admin.rs:89` (`db::node_id(..).ok()`): guarda el resultado en un `OnceCell` de proceso
   (`db.rs:351-360`); tras la primera lectura buena no vuelve a fallar, y antes de ella el único
   test posible depende del orden de los tests del binario (no determinista). Si falta
   `brain_node_identity`, ya fallan `migrations_applied` y `corpus`, y el panel pinta «—».
3. `protocol.rs:1371-1373` y `1507-1510` (`current_project_id().ok().flatten()`) y
   `faro.rs:1683` (`let _ = include_unscoped`): `current_project_id` no lee la base y nunca falla
   (`project.rs:23-28`): manejo de errores muerto, no un fallo tragado. Pasarlo a `?` como los demás
   handlers es higiene del mejorador, sin test posible en rojo.
4. `protocol.rs:793` (`serde_json::from_value(goals).unwrap_or_default()`): el valor se descarta
   (`_session_id, _` en 798); nunca llega a una respuesta ni a un número.
5. `protocol.rs:156`, `261`, `452`, `509-521` (`let _ = tx.send(..)`, `let _ = timeout(..)`):
   canales y cierres de un transporte que ya se fue; no hay a quién decírselo.
6. `protocol.rs:967-990` (`record_rem_cycle` sólo hace `warn!` si no pudo escribir su fila):
   best-effort legítimo; no hay dónde dejar el rastro de que el rastro no se pudo dejar. El panel
   ya lo cubre con `stale` (5 h sin ciclo).
7. `protocol.rs:11-30`, `289`, `1111`, `1179` (`.ok()` sobre `env::var`/`parse`): configuración con
   valor por defecto, no lecturas de base.
8. `graph_db.rs:808`, `849`, `878` (`project_*(..).is_ok()`): un elemento que no se proyectó queda
   a la vista en el propio informe (`projected_x < postgres_x`) y en `backend.last_error`
   (`record_err`). No está callado. Un contador `failed` es una mejora, no C4.
9. `graph_db.rs:349` y `415` (`as_str().unwrap_or_default()`, `r.text().await.unwrap_or_default()`):
   texto de diagnóstico dentro de un mensaje de error que ya se registra; el fallo se reporta
   aunque el texto salga vacío.
10. **No verificable aquí**: el `embed()` fallido en `verify` y la búsqueda vectorial de episodios
    (1783) sólo se alcanzan con el modelo ONNX cargado. Se implementan con la misma llamada a
    `note` y se declaran en el CHANGELOG «Not verified»; no se aparenta una prueba que no existe.
11. B2 (`faro.rs` 149, 1152, 1193, 1636, 1783, 1844-1887, 2059): no estaban en el encargo y es un
    trade-off de alcance. Recomiendo hacerlas: es el mismo defecto y dos lo admiten en su propio
    log; cuestan siete señales más y pasar un `LostSignals` por `text_search`/`vector_search`.
    Queda a decisión del orquestador/Leandro.
12. **Pendiente fuera de este encargo**: el mismo patrón sigue en `db.rs` (10), `handlers/eco.rs`
    (8), `handlers/cronica.rs` (5), `doctor.rs` (5), `handlers/zafra.rs` (4), `whoami.rs` (4),
    `vigia.rs` (4), `dedupe_cli.rs` (6) y varios menores (`sync/chunk.rs`, `sync.rs`, `ingesta.rs`,
    `receta.rs`, `puente.rs`, `contradiccion.rs`, `juez.rs`, `artefacto.rs`). No los audité uno a
    uno (hay best-effort legítimo entre ellos, p. ej. `doctor.rs` que es un diagnóstico): pide una
    pasada de especificación aparte, C4-resto-2, con el mismo criterio de esta hoja. El CHANGELOG
    dice que «el trabajo de C4 está hecho» para los handlers que el agente llama cada turno; esto
    no lo contradice.
13. Quitar o poner tras una *feature* de Cargo la proyección a FalkorDB (la otra mitad de C4):
    decisión de diseño de Leandro, fuera de este encargo.

## Decisión de Leandro (2026-10-28)

Entra la tajada **B2** (las siete lecturas hermanas de `faro`: peers, episodios, factoid, PPR, associative y graphrag) en este mismo lote,
después de B1. `db`, `eco`, `cronica`, `zafra`, `whoami`, `vigia` y `doctor` siguen fuera (C4-resto-2).
