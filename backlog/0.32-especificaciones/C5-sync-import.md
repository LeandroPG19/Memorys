# 0.32 C5-resto: descomponer `sync::import` — especificación (especificador)

Rol: especificador. No escribe código ni tests; no compiló ni corrió cargo. Lo medido aquí es `lizard 1.24.1` sobre el árbol de `feat/0.32-analisis-y-sota` (HEAD `196f9d7`) y lectura del código. El tag de pieza del plan es **C5** (`backlog/0.32-analisis-y-sota.md` §3.4 y tabla de P2, línea «C5 … `sync::import` (CC 141)»).

## 0. Resumen

- La función es `async fn import` en `rust/src/handlers/sync.rs:1826-2767` (942 líneas, 893 NLOC). Lizard 1.24.1 la mide en **CC 139** (el plan dice 141: deriva de versión de lizard, igual que `extract_runtime`, ver §3.4 del plan). La línea base `scripts/lizard-baseline.txt:116` dice `src/handlers/sync.rs::import#1 139`.
- **No está hecha**: `git log -- rust/src/handlers/sync.rs` no tiene ningún refactor de `import`; los commits 45947cb y 42a798a tocaron `faro.rs`, `cronica.rs` y la línea base de `hybrid_search`. Su último cambio fue 8a63255.
- Tiene **dos llamantes**: `handle` (sync.rs:45, la herramienta `cuba_sync action=import` y `sync import` de la CLI/hooks de git) y `fetch` (sync.rs:1183, el camino del peer LAN). Un cambio de comportamiento en ella lo ven los dos.
- Hay mucho test de `import` ya (≈35 ficheros `#[ignore]` que necesitan `DATABASE_URL`), pero **ninguno fija un conteo exacto**: todos los `rows_inserted` se afirman con `>= 1` o `> 0`, y `quarantined` con `>= 2`. Un refactor que cambie lo que se cuenta pasaría todos. Además **no hay ningún test** de: importación de `artifacts.json`, restauración de `embeddings.bin.zst` con éxito, `projects.json`, las políticas `skip`/`overwrite` sobre entidades, procedimientos y relaciones, ni el error de `conflict` inválido / manifest ausente / `schema_version` demasiado nueva.
- **Hallazgo de proceso (importante para el endurecedor)**: la mutación del diff corre `cargo mutants … -- --lib` (`scripts/quality-gate.sh:451`) y `crap-gate.sh` mide `llvm-cov --lib`. Los tests de `import` están todos en `rust/tests/` y marcados `#[ignore]`: **el paso de mutación no los ve**. Todo lo que se mueva de `import` y toque SQL tendrá mutantes que nadie mata (precedente: 2d001b9, «asked in --lib, where the mutation step can see it»). Ver §6, R8: el diseño de la descomposición tiene que separar lo puro (que se prueba en `--lib`) de lo que ejecuta SQL.

## 1. Mapa de `import` por responsabilidades

CC por etapa medido recortando el rango de líneas a una función de prueba en el scratchpad y pasando `lizard` (no compila nada). «n/m» = lizard perdió el recorte (el tokenizador se confunde con ese fragmento; para S0 y S5a se estima a ojo ~6 y ~8).

Estado compartido que cruza etapas (es lo que obliga a un struct y no a funciones sueltas): `tx` (la **única** transacción), `root`, `manifest`, `overwrite: bool`, `tombstones` (`Applied`: `buried: HashSet<Uuid>`, `deleted`, `withheld`), `remapped: HashMap<Uuid,Uuid>` (lo llena la etapa de entidades, lo leen observaciones, episodios, hechos y relaciones), y los acumuladores `inserted`, `diverged: Vec<Uuid>`, `superseded_facts`, `quarantined`, `quarantine_reasons`, `embeddings_restored`.

| Etapa | Líneas | Hace | Lee de estado | Escribe en estado | CC hoy |
|---|---|---|---|---|---|
| S0 preámbulo | 1827-1857 | `resolve_dir`; falta `manifest.json` → error; parsea manifest; `schema_version > SCHEMA_VERSION+1` → error; `validate_bundle`; política `skip`/`merge`→`overwrite=false`, `overwrite`→true, otra → error | args | `root`, `manifest`, `overwrite` | n/m (~6) |
| S1 tx + candado + ámbito | 1858-1872 | `pool.begin`, `take_sync_lock`, `set_config('app.current_project', pid, true)` si el manifest declara proyecto | manifest | `tx`, `scoped` | 5 |
| S2 tombstones + guarda de vectores | 1873-1901 | `apply_tombstones` (borra y/o retiene); si `with_embeddings`: rechaza dimensión distinta y modelo distinto | manifest, tx | `tombstones` | 6 |
| S3 huella e idempotencia | 1902-1918 | `recompute_digest`, `tampered = huella != manifest_hash`; si la huella ya está en `brain_sync_state` → **devuelve `skipped` con la tx sin commit (rollback, tombstones incluidos)** | tx | `on_disk`, `tampered` | 4 |
| S4 proyectos | 1926-1955 | `projects.json` dedup por id; INSERT … ON CONFLICT (NOTHING / UPDATE si overwrite); cuenta `rows_affected` | overwrite | `inserted` | 7 |
| S5a entidades: ficheros + remapeo | 1956-2002 | lee `entities/*.json`; salta las enterradas; busca nombres que ya existen con otro id → `remapped`; dedup por id resultante | tombstones.buried | `remapped`, filas | n/m (~8) |
| S5b entidades: escritura | 2003-2028 | INSERT … ON CONFLICT (GREATEST de importance/access_count; overwrite además pisa nombre, tipo, proyecto, fecha); cuenta `rows_affected` | overwrite | `inserted` | 4 |
| S5c observaciones: filas | 2030-2065 | por entidad, por observación: salta enterradas y repetidas; `trust_for_imported` → razón de cuarentena; arma la fila JSON con `entity_id` remapeado | buried, remapped | `reasons` local, filas | 6 |
| S5d observaciones: escritura | 2067-2166 | si `!overwrite`: INSERT en `brain_sync_conflicts` de las que difieren (conflictos **abiertos**) → `diverged` + evento CRDT; luego INSERT de observaciones (overwrite: archiva la versión previa, anula `embedding`/`embedding_half` si cambia el texto, `WHERE content distinct`); cuenta las que **aterrizaron** (RETURNING) y, de ésas, las puestas en cuarentena | overwrite, manifest_hash | `diverged`, `inserted`, `quarantined`, `quarantine_reasons` | 9 |
| S6 episodios | 2169-2242 | lee `episodes/<mes>/*.json` (dos niveles), dedup, `trust_for_imported`, `entity_id` remapeado; INSERT; cuenta landed + cuarentena | remapped, overwrite | `inserted`, `quarantined`… | **19** |
| S7 errores | 2244-2314 | `errors/*.json`, dedup, trust sobre `error_message\nsolution`; INSERT; cuenta landed + cuarentena | overwrite | idem | **15** |
| S8 hechos | 2316-2411 | `facts.json` sin enterrados, `subject_entity_id` remapeado; INSERT con subselects (`SELECT id FROM brain_entities/brain_projects` → NULL si no existe) y `brain_layer_by_name`; ON CONFLICT NOTHING; luego dos UPDATE de supersesión sobre los que aterrizaron y son vigentes | buried, remapped | `inserted`, `superseded_facts` | 10 |
| S9 artefactos | 2413-2479 | por artefacto: lee fila local `(version, crdt_actor, crdt_counter)`; decide con `pick_lww` (Concurrent → mayor `version`); si toma: UPSERT con `WHERE` de versión/reloj; **`inserted += 1` sea cual sea el resultado del UPSERT**; evento `merged` | tx | `inserted` | 10 |
| S10 procedimientos | 2481-2550 | `procedures.json` sin enterrados, dedup; INSERT con subselect de proyecto; ON CONFLICT (GREATEST de contadores; overwrite además pisa pasos y textos); cuenta `rows_affected` | buried, overwrite | `inserted` | 8 |
| S11 confianza de fuentes | 2552-2577 | `source_trust.json` dedup por fuente; UPSERT con GREATEST de alfa/beta (igual en las dos políticas) | — | `inserted` | 6 |
| S12 relaciones | 2579-2630 | `relations.json`; extremos remapeados; dedup por (from,to,tipo) **después** del remapeo; UPSERT (merge: GREATEST de strength; overwrite: además bidirectional, proyecto, fecha, provenance); cuenta `rows_affected` | remapped, overwrite | `inserted` | 9 |
| S13 vectores | 2632-2670 | si `with_embeddings` y hay blob: descomprime, calcula tamaño de registro, si no divide → `tracing::warn` y sigue; si sí, un UPDATE por observación (`embedding = $1, embedding_half = NULL`) | manifest | `embeddings_restored` | 9 |
| S14 marca y commit | 2672-2685 | si `tombstones.withheld` vacío: INSERT en `brain_sync_state` (huella, proyecto, `inserted`, ruta); `tx.commit()` | tombstones, inserted | — | 4 |
| S15 avisos del peer | 2687-2695 | **fuera de la tx, con `pool`**: cierra `brain_peer_notices` de esa huella; si falla, 0 (error tragado) | on_disk | `notices_closed` | 1 |
| S16 notas, resultado, evento | 2697-2766 | arma `quarantine_note`, `divergence_note` (20 ids), el JSON de salida (17 claves) y publica `sync.import` | todo | — | 4 |

Suma de las medidas ≈ 139: las etapas son separables sin que la complejidad se esconda en el hilo.

## 2. Orden de escritura: es contrato

El orden S4 → S5 → S6 → S7 → S8 → S9 → S10 → S11 → S12 → S13 no es estético:

- `facts.subject_entity_id`, `facts.project_id` y `procedures.project_id` se escriben con **subselect** (`(SELECT id FROM brain_entities WHERE id = u.…)`): si la entidad o el proyecto aún no existe, **no falla, queda en NULL**. Mover S8 o S10 antes de S4/S5 corrompe sin error.
- S5d/S6/S12 tienen FK a entidades; S13 hace UPDATE sobre filas que S5d tiene que haber escrito.
- `remapped` se llena en S5a y lo leen S5c, S6, S8, S12: si no hay directorio `entities/`, sigue vacío (hoy también).
- S2 (tombstones) va **antes** de S3 (idempotencia) y S3 antes de cualquier escritura: la segunda importación idéntica devuelve `skipped` con la tx descartada (rollback), de modo que un tombstone aplicado en esa tx **no** persiste. Hoy es observable: no se puede «optimizar» moviendo S3 después de las escrituras.
- S14 (marca) va dentro de la tx y **antes** del commit; S15 y S16 después. El evento `sync.import` se publica **después** de S15.

## 3. Lo que hoy está fijado por tests (E) y lo que no (N)

Todos los tests existentes son `#[ignore]` en `rust/tests/` salvo lo indicado, llaman a `handlers::dispatch(..., "cuba_sync", …)` y se serializan con `pg_advisory_xact_lock(0x0CBA_A0D1_7106_0027)` porque `CUBA_SYNC_DIR` es de proceso. Los de §3.1 son los que **ya** actúan de red.

### 3.1 Fijado (E)

| Comportamiento | Test |
|---|---|
| Validación del bundle antes de tocar la base (relación a la nada, listas cerradas) | `v022_bundle_is_validated_first` |
| El import espera el candado y el export no corre en paralelo; un sync atascado falla con motivo | `v022_sync_takes_a_lock`, `v025_a_stuck_sync_fails_instead_of_hanging` |
| Entidad inventada dos veces con id distinto: remapeo por nombre, observaciones y hechos la siguen | `v022_same_name_different_id`, `v028_two_node_convergence::a_fact_keeps_its_entity_link…` |
| `merge` dice qué no tocó (`diverged`, `divergence_note`) y deja el conflicto **abierto** | `v022_merge_reports_what_it_drops`, `v026_a_conflict_is_something_you_can_see` |
| GREATEST de contadores (entidades, relaciones) | `v023_counters_merge` |
| `overwrite` archiva la versión sustituida | `v023_history_is_kept` |
| Tombstones: borrar viaja, no arrastra hijos desconocidos, no aborta con tabla sin `id`, mismas tablas que escriben; retenidos se reintentan y no marcan la huella | `v023_tombstones`, `v024_tombstone_lists_agree`, `v028_two_node_convergence::a_withheld_tombstone…` |
| Guarda de vectores: modelo/dimensión distintos se rechazan | `v024_embedding_guard` |
| Hechos con capa por nombre; dos máquinas en desacuerdo no dejan dos verdades vigentes; procedimientos viajan | `v024_facts_and_procedures_travel` |
| Bundle editado a mano ya no se descarta; segunda importación idéntica → `skipped` | `v024_hand_edited_bundle` |
| Misma fila dos veces en un lote no aborta | `v025_a_repeated_key_in_one_batch` |
| Bundle de proyecto: la base es el guardia (`scope_enforced`) | `v025_import_declares_its_scope` |
| Cuarentena: credenciales en observaciones, episodios y errores | `v021_import_quarantine_all_kinds`, y 3 tests `--lib` en sync.rs:3110-3170 (`trust_for_imported`) |
| Ida y vuelta completa con hechos, procedimientos y `source_trust`; corrupción deja la base intacta y sin marca; filas promovidas siguen promovidas | `v028_two_node_convergence` |
| Peer: el aviso se silencia al tomar lo ofrecido | `v027_the_bell_closes_itself`, `v025_two_machines_converge` |
| Un merge/rebase de git importa antes de exportar | `v045_a_merge_reaches_the_database` |

### 3.2 No fijado (N): hay que fijarlo ANTES de mover una línea

Los verifiqué buscando la clave o el efecto en `rust/tests` y `rust/src`; «sin referencia» = cero coincidencias. Donde un test toca el área pero no la afirmación, lo digo.

- **N1 conteos exactos**: `rows_inserted`, `quarantined`, `facts_superseded`, `diverged` sólo con `>=`/`>`. Además la contabilidad es **asimétrica** a propósito o por accidente: S4/S5b/S10/S11/S12 suman `rows_affected` (un `DO UPDATE` cuenta como «insertada»), S5d/S6/S7/S8 suman las filas que `RETURNING` devuelve (un `DO NOTHING` no cuenta), S9 suma 1 por artefacto tomado aunque el `WHERE` del UPSERT no actualice nada. Quien «unifique» la cuenta cambia `rows_inserted` y el contador de `brain_sync_state`.
- **N2 forma del resultado**: ningún test fija las 17 claves ni el texto de `quarantine_note`, `divergence_note` (tope de 20 ids), `scope_note`; ni el payload del evento `sync.import`; ni `peer_notices_closed` (sin referencia en tests).
- **N3 errores de la puerta**: sin `manifest.json`, política de conflicto inválida, `schema_version > SCHEMA_VERSION+1` (`SCHEMA_VERSION = 2` en `sync/chunk.rs:6`): sin referencia. Y que no se escribe nada.
- **N4 `projects.json`**: sin referencia en tests. Dedup, `DO NOTHING` vs `DO UPDATE` por política.
- **N5 políticas sobre entidades, episodios, errores, procedimientos y relaciones**: `overwrite` sólo se ejercita en observaciones (v021 lo usa con episodios/errores pero para cuarentena, no para el SET); `skip` ≡ `merge` (ambos `overwrite=false`) sin test; las ramas `UPDATE SET …` de entidades/procedimientos/relaciones por política no están afirmadas columna a columna.
- **N6 observaciones, escritura**: que `overwrite` anula `embedding` y `embedding_half` sólo si cambia el texto (v023_history toca `embedding` pero no lo afirma); que con el mismo texto el `WHERE` no actualiza y la fila no cuenta; que **la cuarentena se cuenta sólo para filas que aterrizaron** (una fila existente no cuenta).
- **N7 artefactos**: **cero** cobertura de integración de `artifacts.json` (v045_a_git_merge… sólo mezcla ficheros con el merge driver). `pick_lww` tiene tests unitarios en `crdt`, pero la decisión *dentro de import* (None → toma; TakeIncoming/KeepLocal; Concurrent → `version` mayor) y el `inserted += 1` incondicional no.
- **N8 vectores, camino feliz**: `embeddings_restored` sin referencia en tests. Tampoco el camino `rec_size` inválido (blob de tamaño que no divide) → `warn` y sigue sin contar. v024_embedding_guard cubre el rechazo previo, no la restauración.
- **N9 idempotencia más fina**: que la segunda importación idéntica no persiste el efecto de los tombstones de esa tx (rollback por drop); que `already` se evalúa con la huella **recalculada en disco** (`on_disk`) y no con `manifest.manifest_hash` (por eso editar a mano re-importa): eso último lo cubre `v024_hand_edited_bundle`; lo no cubierto es el rollback de los tombstones en el camino `skipped`.
- **N10 atomicidad real a mitad de la tx**: v028 `a_corrupted_bundle_file…` falla en `validate_bundle`, **antes** de abrir la tx; no hay un test donde falle una etapa tardía después de escribir las tempranas. Es *la* red contra la avería clásica de una extracción: una etapa que escribe por `pool` en lugar de por `&mut *tx` compila y escapa de la transacción, del candado y del `set_config(…, true)`.
- **N11 orden de etapas**: nada rompe si se adelantan hechos o procedimientos a proyectos/entidades (subselect → NULL silencioso). v028 cubre «hecho conserva su vínculo» sólo para el caso con remapeo.
- **N12 lo que NO se consulta de `buried`**: sólo entidades, observaciones, hechos y procedimientos filtran por `buried`; proyectos, episodios, errores, relaciones, artefactos y `source_trust` no. Está fijado por ausencia, no por test.
- **N13 `edited_since_export` (`tampered`)**: se calcula y se informa pero la importación **continúa**. Cubierto por v024_hand_edited; se mantiene.
- **N14 cierre de avisos tras el commit**: si el UPDATE de `brain_peer_notices` falla, el import **no** falla (devuelve 0). Imposible de provocar sin trigger; ver escenario G14.

## 4. Gherkin: comportamientos a fijar primero (tajada T0)

Dónde viven (recomendación del especificador, decide el implementador):
- **Integración** `rust/tests/v046_sync_import_characterisation.rs`, `#[ignore]` como sus hermanos, con `own_the_sync_dir` y `CUBA_SYNC_DIR`, base desechable (convención de `AGENTS.md`, «Immutable fixtures»). Cada escenario llama a `dispatch(pool, "cuba_sync", {"action":"import", …})`.
- **`--lib`** (módulo `tests` de `sync.rs`, no `#[ignore]`) para lo **puro** que cada tajada extraiga (§5), para que la mutación del diff lo vea. Su rojo es el que no compila (la función aún no existe) y el orquestador pega ese error como línea roja, igual que en `hybrid_search`.
- Los valores esperados de los escenarios con números se **calculan a mano desde el bundle** y se comprueban contra el código actual **antes** de congelarlos. Si el código actual hace algo que parece un bug, **se fija como está** (ver §6, R9) y se anota; no se arregla en C5.

```gherkin
Feature: sync import — comportamiento que la descomposición no puede mover

  Background:
    Given una base desechable con las migraciones aplicadas
    And un directorio de bundle propio asignado a CUBA_SYNC_DIR
    And el modelo local produce vectores de la misma dimensión y huella que declara el manifest

  # N3
  Scenario: G1 — un directorio sin manifest.json se rechaza y no escribe nada
    Given un directorio sin manifest.json
    When se hace import
    Then falla con un error que nombra "no manifest.json at" y el directorio
    And brain_sync_state y todas las tablas están como antes

  Scenario: G2 — una política de conflicto desconocida se rechaza después de validar el bundle y antes de abrir la tx
    Given un bundle válido
    When se hace import con conflict "union"
    Then falla con "invalid conflict policy: union"
    And no hay fila nueva en brain_sync_state ni en ninguna tabla

  Scenario: G3 — un bundle de schema_version demasiado nueva se rechaza
    Given un manifest con schema_version igual a SCHEMA_VERSION + 2
    When se hace import
    Then falla con "is too new (this build supports 2)"
    But un manifest con schema_version igual a SCHEMA_VERSION + 1 sí se importa

  # N5: skip == merge
  Scenario: G4 — "skip" y "merge" se comportan igual
    Given un bundle con una entidad, una observación con texto distinto al local y un procedimiento ya existente
    When se importa con "skip" sobre una copia del estado y con "merge" sobre otra copia idéntica
    Then los dos resultados (sin from ni manifest_hash) y los dos volcados de tablas son idénticos

  # N4
  Scenario: G5 — projects.json: dedup por id y política por conflicto
    Given un projects.json con el mismo id dos veces (nombres "uno" y "dos") y otro proyecto ya existente con nombre distinto
    When se importa con "merge"
    Then el proyecto nuevo existe una sola vez, el existente conserva su nombre y rows_inserted cuenta exactamente lo que el código de hoy cuenta
    When se importa otro bundle con el nombre cambiado de ese proyecto y conflict "overwrite"
    Then el proyecto existente toma el nombre y la created_at del bundle

  # N5: entidades, procedimientos, relaciones por política
  Scenario Outline: G6 — qué pisa y qué conserva cada política en <tabla>
    Given una fila local de <tabla> y una del bundle con el mismo id (relaciones: misma terna) y TODAS las columnas distintas, con contadores locales mayores en unas y menores en otras
    When se importa con "<politica>"
    Then cada columna queda como la tabla de abajo, columna a columna
    Examples:
      | tabla       | politica  | pisa                                                                         | conserva / GREATEST                         |
      | entidades   | merge     | nada                                                                         | importance y access_count = GREATEST        |
      | entidades   | overwrite | name, entity_type, project_id, created_at                                    | importance y access_count = GREATEST        |
      | procedim.   | merge     | nada                                                                         | success_count y failure_count = GREATEST    |
      | procedim.   | overwrite | name, steps, trigger_context, preconditions, verification, updated_at        | contadores = GREATEST                       |
      | relaciones  | merge     | nada                                                                         | strength = GREATEST                         |
      | relaciones  | overwrite | bidirectional, project_id, created_at, provenance                            | strength = GREATEST                         |

  # N6
  Scenario: G7 — overwrite anula el vector sólo si cambia el texto
    Given dos observaciones locales con vector, A y B
    And un bundle con A con otro texto y B con el mismo texto
    When se importa con "overwrite"
    Then A tiene embedding y embedding_half NULL y B conserva los dos
    And B no está en rows_inserted porque el WHERE no la actualizó

  Scenario: G8 — la cuarentena se cuenta sólo para filas que aterrizaron
    Given una observación local con id X, y un bundle con X (con una credencial en el texto) y con Y nueva (también con credencial)
    When se importa con "merge"
    Then quarantined es 1, quarantine_reasons suma 1 y sólo Y quedó con trust "quarantined"
    And X sigue con su texto y su trust locales y aparece en diverged

  # N7
  Scenario Outline: G9 — un artefacto entrante frente a uno local
    Given un artefacto local (path P, proyecto Q) con reloj <local> y version <vl>
    And el bundle trae el mismo (P, Q) con reloj <entrante> y version <ve>
    When se importa
    Then el contenido final es el <gana>
    And rows_inserted <cuenta>
    Examples:
      | local         | entrante      | vl | ve | gana     | cuenta                        |
      | (actor,5)     | (actor,9)     | 1  | 1  | entrante | +1                            |
      | (actor,9)     | (actor,5)     | 1  | 9  | local    | +0 (no se toma)               |
      | (a,3)         | (b,3)         | 2  | 3  | entrante | +1 (concurrentes, version)    |
      | (a,3)         | (b,3)         | 3  | 2  | local    | +0                            |
      | ninguno       | (a,1)         | -  | 1  | entrante | +1                            |
    # Fijar también: el UPSERT con WHERE que no actualiza suma 1 igualmente cuando
    # se "toma" y la fila local gana el WHERE (comportamiento de hoy, ver R9).

  # N8
  Scenario: G10 — los vectores del bundle se restauran y se cuentan
    Given un bundle con with_embeddings true, un blob válido para dos observaciones del bundle y dimensión y modelo iguales a los locales
    When se importa
    Then embeddings_restored es 2, esas observaciones tienen embedding y embedding_half NULL

  Scenario: G11 — un blob cuyo tamaño no divide en registros se salta con un aviso y no cuenta
    Given un bundle con with_embeddings true y un blob descomprimido de longitud que no es múltiplo del registro
    When se importa
    Then embeddings_restored es 0, no falla y las filas de texto sí se importaron

  # N9, N10
  Scenario: G12 — la segunda importación idéntica no persiste lo que sus tombstones habrían hecho
    Given un bundle con un tombstone de la entidad E y la entidad E ya borrada por la primera importación
    And E recreada localmente después con el mismo id
    When se importa el mismo directorio otra vez, sin cambios en disco
    Then el resultado es skipped true con "these exact files were already imported"
    And E sigue existiendo (el borrado de esa tx se descartó)

  Scenario: G13 — si una etapa tardía falla, no queda nada de las tempranas ni marca de sync
    Given una base desechable con un trigger que lanza una excepción al insertar una relación con un tipo marcador
    And un bundle válido con un proyecto, entidades, observaciones, un episodio, un hecho y una relación con ese tipo marcador
    When se importa
    Then la llamada falla
    And no existe ninguna de esas filas, ni el proyecto, ni la marca en brain_sync_state
    And la segunda llamada, con el trigger retirado, importa todo y devuelve rows_inserted igual al esperado de G15

  Scenario: G14 — cerrar los avisos del peer no puede hacer fallar un import ya confirmado
    Given un import confirmado y un aviso abierto de esa huella
    Then peer_notices_closed es 1 y el aviso tiene resolved_at
    # El camino de error tragado (unwrap_or(0)) no es alcanzable sin romper la
    # conexión: se documenta como no probado por construcción y se vigila por
    # lectura del código; el sabotaje S15b del §7 sólo cubre la condición del UPDATE.

  # N1, N2, N11
  Scenario: G15 — un bundle con todos los tipos de fichero produce exactamente este resultado
    Given un bundle con: 1 proyecto; 3 entidades (una con nombre que ya existe local con otro id); 4 observaciones
      (una enterrada por tombstone, una repetida en dos ficheros, una con credencial, una cuyo id ya existe con texto distinto);
      2 episodios (uno con credencial); 2 errores; 3 hechos (dos del mismo sujeto y predicado con observed_at distinto,
      uno con subject_entity_id de la entidad remapeada); 2 artefactos; 2 procedimientos; 2 source_trust; 3 relaciones
      (dos que colisionan tras el remapeo); y vectores para 2 observaciones
    When se importa con "merge" sobre una base en la que ya existe lo mínimo para las colisiones
    Then el JSON de salida, sin from ni manifest_hash, tiene las 17 claves y estos valores calculados a mano:
      action, rows_inserted, diverged, edited_since_export, facts_superseded, tombstones_applied,
      tombstones_withheld, divergence_note, embeddings_restored, quarantined, quarantine_reasons,
      quarantine_note, peer_notices_closed, scope_enforced, scope_note
    And el hecho con subject_entity_id apunta a la entidad local (la remapeada), no a la del bundle
    And ninguna relación cuelga de un id de entidad del bundle que fue remapeado
    And brain_sync_state tiene una fila con rows_inserted igual al del resultado, el proyecto del manifest y la ruta del directorio

  Scenario: G16 — los textos de las notas y su tope
    Given un import con 25 observaciones divergentes y 3 filas en cuarentena
    Then quarantine_note empieza por "3 imported rows look like they carry a credential"
    And divergence_note empieza por "25 observations already existed here" y lista exactamente 20 ids
    And con 0 divergentes y 0 en cuarentena ambas notas son null

  Scenario: G17 — el evento de bus lleva los cinco campos
    Given un suscriptor de crate::events::subscribe()
    When se importa el bundle de G15
    Then recibe un evento "sync.import" con manifest_hash, rows_inserted, diverged, quarantined y peer_notices_closed
    And lo recibe después de que peer_notices_closed se haya calculado

  Scenario: G18 — lo que `buried` no filtra (se fija como está)
    Given un tombstone con el id de un episodio, de un error y de una relación presentes en el bundle
    When se importa
    Then esas tres filas se importan igual (sólo entidades, observaciones, hechos y procedimientos respetan buried)
    # Es un hallazgo, no un deseo. Si se quiere cambiar, es otra tarea.
```

Los escenarios G6, G9 y G15 son tablas: el implementador las parte en tantos tests como filas, cada uno con su mensaje de fallo. `rust/tests/` ya trae helpers repetidos (`call`, `own_the_sync_dir`) y se copian como los hermanos hacen; no hay módulo `common` para esto y no se crea uno.

## 5. Candidatas a extracción (propuesta, el implementador puede ajustar nombres y firmas)

Dos reglas de diseño que salen de medir, no de gusto:

1. **Separar lo puro de lo que ejecuta SQL.** Lo puro (armar filas, deduplicar, remapear, notas, decisión LWW, decodificar el blob) se prueba en `--lib` y lo ve la mutación. El SQL queda en funciones finas (un `sqlx::query` + contar) cubiertas por los tests `--ignored`.
2. **Un solo `&mut Transaction` y un solo `Tally`.** Cada etapa recibe `tx: &mut sqlx::Transaction<'_, sqlx::Postgres>` (como `apply_tombstones`) y usa `&mut **tx`/`&mut *tx`; **ninguna** función de etapa recibe el `PgPool`. S15 es la única que usa `pool`, y es posterior al commit.

Piezas (CC objetivo ≤ 8 cada una, medidas con lizard antes de cerrar; las marcadas † tienen hoy etapa > 8 y exigen partirse):

- `struct Tally { inserted, diverged, remapped, superseded_facts, quarantined, quarantine_reasons, embeddings_restored }` con `fn landed(&mut self, ids: &[Uuid], reasons: &HashMap<Uuid, &'static str>)` (lo hacen hoy tres veces S5d, S6, S7; CC ~3). Puro, test `--lib`.
- Selectores de cláusula `ON CONFLICT`: una constante o `fn on_conflict_<tabla>(overwrite: bool) -> &'static str` que devuelve **el mismo literal** que hoy va dentro del `format!`. Quita un `if` por etapa. Los literales no se tocan (ni espacios ni `\` de continuación).
- `Opening` (S0-S3): `enum Opening { Skipped(Value), Ready(ReadyImport) }`. `Skipped` devuelve **sin commit** (drop de la tx). Puro lo que se pueda: `parse_policy(&str) -> Result<bool>`, `check_schema_version(&Manifest) -> Result<()>`, `embedding_guard(&Manifest, local_dim, local_model) -> Result<()>` († S0 y S2 > 8 si se dejan enteras).
- Lectores de ficheros (fs, probables en `--lib` con `scratch()` que ya existe en el módulo de tests): `read_entity_files(dir, &buried)`, `read_episode_files(root)` († S6 = 19), `read_error_files(dir)` († S7 = 15). Un helper `json_files(dir) -> Result<Vec<PathBuf>>` que filtra `extension == json` lo comparten los tres (3 llamantes, no es una capa). **No se ordena** la lectura (hoy el orden de `read_dir` es el del sistema; ordenar sería cambio de comportamiento).
- Constructores de filas, puros: `remap_by_name(files, existing) -> (Vec<Value>, remapped)`, `observation_rows(files, &remapped, &buried) -> (Vec<Value>, reasons)`, `episode_rows`, `error_rows`, `fact_rows` + `current_claims(facts, landed)` (el filtro `arrived ∧ is_current.unwrap_or(true)`), `procedure_rows`, `relation_rows(rels, &remapped)` (dedup tras remapeo), `project_batch`, `trust_batch`. Test `--lib` con tablas, uno por rama.
- Decisión LWW: `fn takes_incoming(local: Option<(i64, Option<String>, Option<i64>)>, a: &ArtifactRow) -> bool` (hoy un `match` anidado de CC ~6 dentro de S9). Puro.
- `decode_embedding_blob(raw: &[u8], dim: usize) -> Option<Vec<(Uuid, Vec<f32>)>>` (None = tamaño inválido → warn). Puro; `embedding_record_size` ya existe y tiene test.
- Escritores SQL finos, uno por tabla: `write_projects`, `write_entities`, `write_observations`† (partir en `record_divergences` + `upsert_observations`), `write_episodes`, `write_errors`, `write_facts`† (partir: `insert_facts` + `close_superseded`), `write_artifacts`† (el bucle y el UPSERT), `write_procedures`, `write_source_trust`, `write_relations`, `restore_embeddings`.
- Cierre: `record_sync_state`, `close_peer_notices(pool, hash)`, `quarantine_note(n)`, `divergence_note(&[Uuid])`, `import_result(...)` (el JSON; puro), `publish_import_event(...)`.
- Hilo: `import` = `open_import` → `write_rows` (etapas S4-S13 **en este orden**, agrupadas en dos o tres funciones de ≤ 8 llamadas con `?`) → `close_import`. Medido: cada `?` suma 1 a la CC de lizard (S14 son 2 `?` + 1 `if` = CC 4), así que un hilo con 14 llamadas con `?` seguiría en ~15: hay que agruparlas.

Nombres a evitar (colisionan con funciones del fichero y desplazarían los ordinales `#n` de la línea base): `handle, may_delete, declared_project_id, prune_stale_files, prune_stale_episode_files, record, finish, write_bundle_file, publish_if_changed, embedding_record_size, resolve, trust_for_imported, export, export_into, publish_embeddings, publish_manifest, conflicts, resolve_conflict, peer_token_or_refuse, ask_peer, announce_to_peers, fetch, drain_peer, notify, pending_notices, bundle_files, pull, pull_from_staging, recompute_digest, take_sync_lock, tombstone_key, apply_tombstones, validate_bundle, diff, status, scratch, write_json`.

## 6. Orden de las tajadas y riesgos

Todas tocan el mismo fichero (`rust/src/handlers/sync.rs`): **en serie, un implementador**. Cada tajada = dos pasadas (tests rojos y commit `test(rojo)`, luego código con tests congelados) y termina con los tests `--ignored` de §3.1 más los de T0 en verde y el volcado antes/después sin diferencias (§7).

| # | Tajada | Qué se mueve | Riesgo principal | Red |
|---|---|---|---|---|
| T0 | Caracterización | **ningún código de producción**; sólo tests de G1-G18 (verdes sobre el código actual; su «rojo» es el sabotaje de §7, no la ausencia de código) | pinear un comportamiento que parece un bug: se fija y se anota | §7 sabotajes S1-S16 sobre código actual |
| T1 | Cierre puro + `Tally` | S16 y `Tally::landed`; S15 como función fina | texto de notas y 17 claves | G14, G15, G16, G17 |
| T2 | Apertura | S0-S3 → `Opening` con `Skipped` sin commit | perder el rollback del `skipped`; reordenar guarda de vectores y tombstones | G1-G3, G12, v024_* |
| T3 | Lectores y filas puras | S5a, S5c, S6, S7 (lectura + filas), S8 filas, S10, S11, S12 filas | el orden no determinista de `read_dir`; `seen` por tipo | G15, G18, v022_same_name, v025_a_repeated_key |
| T4 | Escritores SQL | S4, S5b, S5d, S6/S7 escritura, S10-S12 escritura con los selectores `ON CONFLICT` | literales SQL alterados; contar distinto | G4-G8, G13, G15 (conteos exactos) |
| T5 | Hechos, artefactos, vectores | S8 supersesión, S9, S13 | NULL silencioso por subselect; `inserted += 1` incondicional | G9-G11, v024_facts_* |
| T6 | Hilo + línea base | `import` agrupa; `scripts/lizard-baseline.txt` | línea base de dos sentidos (§6.1) | juez completo |

### 6.1 Objetivo medible y línea base

- **Aceptación del plan (C5)**: CC por función ≤ 30 en `import` y en todas las que salgan de él. Aquí, a 139 → tabla de abajo; criterio duro: `lizard -l rust rust/src/handlers/sync.rs` no muestra ninguna función **nueva** por encima de 30 ni `import` por encima de 30.
- **Objetivo de higiene del repo (lo que el endurecedor acabará exigiendo, como pasó con `multi_hop_context` en 42a798a)**: techo `LIZARD_CC_MAX=8` del `quality-gate.sh`: toda función **nueva** ≤ 8, y `import` ≤ 10 (meta ≤ 8, que borraría su línea). Una función nueva por encima de 8 que no esté en la línea base es rojo.
- **Entradas de `scripts/lizard-baseline.txt` que cambian** (sólo `src/handlers/sync.rs`):
  - `src/handlers/sync.rs::import#1 139` → el número que mida lizard al final (o la línea **se borra** si queda ≤ 8: una entrada de una función ya no medida por encima del techo también es rojo).
  - Ninguna otra entrada de `sync.rs` cambia mientras no haya funciones nuevas con nombre de otra ya existente (§5) y no se toquen `apply_tombstones`, `validate_bundle` (36), `export_into` (59) ni `fetch`; ésas quedan fuera de C5 (el plan sólo nombra `hybrid_search` e `import`; `validate_bundle` y `export_into` por encima de 30 son otra pieza y lo dejo señalado, no decidido).
  - Una función nueva que se quede entre 9 y 30 **se añade** con su comentario de motivo y dueño en la línea de encima (formato de `lizard-baseline.txt`, p. ej. «Owner: mejorador 0.32»).
- **Regla de dos sentidos** (leída en `quality-gate.sh`, `run_lizard`): dentro del diff es rojo (a) una función no listada por encima del techo, (b) una listada que **empeoró**, (c) una listada que **mejoró** y la línea no la siguió, (d) una entrada cuyo nombre ya no se mide sobre el techo. Por tanto el mismo commit que baja `import` debe regenerar la línea (`--update-lizard-baseline`, sin tocar a mano) y el mejorador no puede dejarla en 139 «por si acaso».
- **Deriva de versión de lizard**: el plan dice 141 y esta máquina 139 (1.24.1); el dueño con otra versión puede leer ±2. Se regenera la línea con el lizard de la máquina del juez; si dos versiones discrepan, se anota, como en 42a798a.
- **CRAP/cobertura**: `crap-gate.sh` es un piso global de 15 % de líneas con `llvm-cov --lib`. Todo lo que sólo prueban los tests `--ignored` cuenta 0 de cobertura en ese número y en cualquier CRAP por función que use la misma corrida; las extracciones puras con tests `--lib` lo mejoran. No verifiqué el comportamiento exacto de `cargo-crap` en este repo (no abrí su configuración): el mejorador lo mide en su corrida.

### 6.2 Riesgos

- **R1 Migración de datos**: ninguna. No cambia el esquema, ni se añade migración, ni se toca `0001-0060` (congeladas por SHA-384). **Los literales SQL no cambian ni en un espacio**; el `expand → contract` no aplica. Si una tajada pide una columna, para y devuelve una nota.
- **R2 Orden de escritura**: §2. Test de red: G15 y `v028_a_fact_keeps_its_entity_link`.
- **R3 Transacciones**: una sola tx; todas las etapas por `&mut *tx`; `set_config(…, true)` y `SET LOCAL` de `take_sync_lock` sólo valen dentro de esa tx. Sabotaje S1c/G13 es la red. `Skipped` hace drop de la tx (rollback), no commit.
- **R4 Candado de proceso**: los tests de integración son mutuamente excluyentes por `own_the_sync_dir` y por `take_sync_lock`; T0 usa lo mismo. Un test `--lib` que llame a `import(pool, Some(dir), …)` directamente evita `CUBA_SYNC_DIR` global, pero no el candado de la base: no puede correr a la vez que los de integración sobre la misma base.
- **R5 No determinismo ya existente**: orden de `read_dir`, orden del `RETURNING` de `diverged` y, por tanto, los 20 ids de la nota. Los tests afirman conjuntos y conteos, no listas ordenadas; y la refactorización **no** ordena nada «de paso».
- **R6 Dos llamantes**: `fetch` (peer) usa `import` con el directorio de staging; v025/v027/v028 son su red; no cambiar la firma `import(pool, dir_arg, conflict, confirm, deletes_allowed) -> Result<Value>`.
- **R7 Estado visible**: `rows_inserted` se guarda en `brain_sync_state` y lo lee el panel (`src/panel/index.html`) y `hooks_cli`; cambiar la contabilidad cambia lo que un operario ve.
- **R8 La mutación no ve los tests de import** (hallazgo): `--lib` sólo. Para el endurecedor hay dos caminos y lo decide él, con el criterio de este especificador: (a) lo **puro** con tests `--lib` y mutantes muertos; (b) las funciones finas de SQL se excluyen de `MUTANTS_EXCLUDE_RE` con una entrada específica (`sync.rs.*replace write_… with`), con motivo, `Owner:` y `Expires: YYYY-MM-DD` en la misma línea de comentario, como exigen `exclusion_expiry_problems` y `unused_exclusions` (una exclusión que no casa con ningún mutante es roja). La alternativa de tests `--lib` no-`#[ignore]` con base real **no tiene precedente en el repo** (el test de `protocol.rs:2031` es `#[ignore]`) y habría que cablear `DATABASE_URL` en `cargo mutants` y `llvm-cov`: no se recomienda.
- **R9 Comportamientos que parecen bugs y se fijan como están** (no se arreglan en C5; cada uno, si se quiere cambiar, es tarea aparte con su Gherkin): (1) `inserted += 1` por artefacto «tomado» aunque el UPSERT no actualice; (2) `rows_affected` cuenta `DO UPDATE` como insertada en proyectos, entidades, procedimientos, `source_trust` y relaciones; (3) `skip` ≡ `merge`; (4) `buried` sólo filtra cuatro tipos; (5) error de cierre de avisos tragado (`unwrap_or(0)`); (6) blob de vectores de tamaño inválido sólo avisa; (7) `tampered` informa pero no detiene; (8) `.expect(...)` en la restauración de vectores (el mensaje dice por qué no puede fallar).
- **R10 Sin comentarios que sobran**: `AGENTS.md` del repo manda: los comentarios del crate son carga, no se quitan; lo que se mueve se mueve **con** su comentario.

## 7. Procedimiento QA (lo corre el orquestador; los agentes no compilan)

### 7.1 Antes de mover código (cierra T0)
1. Con el árbol limpio y la base desechable: correr `v046_sync_import_characterisation` (`--ignored`) y los 35 tests de §3.1: todo verde sobre el código **actual**.
2. Por cada escenario nuevo, aplicar el sabotaje de 7.3 que le toca sobre producción (en una copia o con `git checkout -- rust/src` al acabar), correr **sólo** ese test y pegar la línea roja en el handoff. Un test que sigue verde con su sabotaje no prueba nada y se rehace.
3. Volcado de referencia (como C5, **desechable, no se commitea**): una base clonada, el bundle de G15 más uno `overwrite` sobre estado poblado y uno `skip`, y guardar (a) el JSON de salida sin `from`/`manifest_hash`, (b) `SELECT * ORDER BY id` de las once tablas tocadas (`brain_projects, brain_entities, brain_observations, brain_sync_conflicts, brain_episodes, brain_errors, brain_facts, brain_artifacts, brain_procedures, brain_source_trust, brain_relations`, y `brain_sync_state`) con las columnas `NOW()` (`updated_at` por `COALESCE`, `detected_at`, `superseded_at` dentro de `previous_versions`) excluidas.

### 7.2 Tras cada tajada
1. Rojo de la pasada 1 pegado; verde de la pasada 2 con `tests: frozen` y `paths:` acotado a `rust/src/handlers/sync.rs` y el fichero de tests de la tajada (`./scripts/validar-handoff.sh`).
2. Los tests de §3.1 + T0 en `--ignored`, `cargo test --lib`, clippy `-D warnings`.
3. El volcado de 7.1.3 repetido: **0 diferencias**. Si sale una, no se «ajusta el volcado»: se reabre la tajada.
4. `lizard -l rust rust/src/handlers/sync.rs`: ninguna función nueva > 8 (o > 30 como mínimo); `import` y los escritores < lo anterior.
5. T6: `./scripts/quality-gate.sh --update-lizard-baseline`, ver el diff de la línea base (sólo `sync.rs`), y el `quality-gate` sobre el diff completo, y el juez (`./scripts/merge-gate.sh`) una vez sobre el SHA final.

### 7.3 Sabotajes por etapa (sobre producción, deben dar rojo en el test nombrado; a revertir)

| Etapa | Sabotaje | Rojo esperado |
|---|---|---|
| S0 | quitar `if manifest.schema_version > SCHEMA_VERSION + 1` | G3 |
| S0 | `"skip"` pasa a `overwrite = true` | G4 |
| S0 | validar la política **después** de abrir la tx | G2 (sigue verde si falla antes de escribir: añadir al test una aserción sobre `brain_sync_state` y sobre el candado) |
| S1 | `set_config(…, true)` → `false` | `v025_import_declares_its_scope` |
| S1 | una etapa tardía (S12) ejecuta por `pool` en vez de `&mut *tx` | G13 |
| S2 | mover `apply_tombstones` tras S5 | `v023_tombstones` |
| S2 | quitar la guarda de modelo | `v024_embedding_guard` |
| S3 | mover la comprobación `already` tras las escrituras | G12 (y duplicaría filas con `merge`) |
| S3 | `already` usa `manifest.manifest_hash` en lugar de `on_disk` | `v024_hand_edited_bundle` |
| S4 | `DO NOTHING` ↔ `DO UPDATE` | G5 |
| S5a | quitar `remapped.insert` | `v022_same_name_different_id`, G15 |
| S5a | quitar `seen_entities` | `v025_a_repeated_key_in_one_batch` |
| S5b | `GREATEST` → `EXCLUDED` | `v023_counters_merge`, G6 |
| S5b | rama overwrite = rama merge | G6 (entidades/overwrite) |
| S5c | no filtrar `buried` en observaciones | `v023_tombstones` |
| S5d | `!overwrite` → `overwrite` en el bloque de conflictos | `v026_*`, G8 |
| S5d | quitar `embedding = CASE …` | G7 |
| S5d | quitar el `WHERE content IS DISTINCT` del overwrite | G7 |
| S5d/S6/S7 | contar cuarentena también si no aterrizó | G8 |
| S6/S7 | quitar `resolve(&remapped, …)` de `entity_id` | G15 (episodio con entidad remapeada) |
| S7 | trust sobre `error_message` sólo | `v021_import_quarantine_all_kinds` |
| S8 | quitar la segunda sentencia de supersesión (`self_closed`) | `v024_facts_and_procedures_travel` |
| S8 | `unwrap_or(true)` → `unwrap_or(false)` | G15 |
| S8 | adelantar S8 antes de S5 | `v028_…a_fact_keeps_its_entity_link`, G15 |
| S9 | invertir `KeepLocal`/`TakeIncoming` | G9 |
| S9 | `inserted += 1` fuera del `if take` | G9 (+0) |
| S10 | `GREATEST` → `EXCLUDED` en `success_count` | G6 |
| S11 | quitar `GREATEST` de alfa | `v028_…a_full_round_trip` |
| S12 | dedup antes del remapeo (no después) | G15 (relaciones que colisionan) |
| S12 | quitar `resolve` de `to_entity` | G15 |
| S13 | quitar `embedding_half = NULL` | G10 |
| S13 | quitar el `filter(|size| raw.len() % size == 0)` | G11 |
| S14 | `if tombstones.withheld.is_empty()` → siempre insertar | `v028_…withheld_tombstone` |
| S14 | `inserted as i32` → `0` | G15 (fila de `brain_sync_state`) |
| S15 | quitar `resolved_at IS NULL` | G14 |
| S15b | cerrar avisos **antes** del commit | G13 (con aviso previo) |
| S16 | `take(20)` → `take(21)` | G16 |
| S16 | quitar un campo del evento | G17 |
| S16 | cambiar una clave del JSON | G15 |

## 8. Handoff propuesto al implementador

No lo escribo (la instrucción fue no tocar nada más que este fichero). El orquestador lo crea en `.cursor/handoffs/0.32-c5-sync-import.yml` con la forma exacta de `handoff.example.yml`:

```yaml
from: especificador
to: implementador
type: git_handoff
task: "0.32 C5-resto, T0: tests de caracterización de sync::import (G1-G18); sin tocar producción. Ver backlog/0.32-especificaciones/C5-sync-import.md"
commit: none
evidence: "lizard -l rust rust/src/handlers/sync.rs (import CC 139, 893 NLOC); lectura de rust/tests y rust/src; sin cargo"
```

Las tajadas T1-T6 llevan su handoff propio (`commit` = el `test(rojo)` anterior, `tests: written` y luego `frozen`, `paths:` = `rust/src/handlers/sync.rs` y el fichero de tests de la tajada).

## 9. Preguntas abiertas al humano

Ninguna que bloquee. Decidido aquí, con cómo revertirlo:
- Fijar R9 como está (en vez de corregir de paso): se revierte abriendo una tarea con su Gherkin; el código de G6/G9/G15 cambia entonces con ella.
- Tratar el techo de 8 del `quality-gate` como objetivo de higiene y el 30 del plan como aceptación dura: si se quiere sólo el 30, se acepta una línea base con varias entradas entre 9 y 30, con comentario y dueño.
- Mutación: lo puro en `--lib` y el SQL fino excluido con dueño y caducidad (R8). Se revierte quitando la exclusión y cableando `DATABASE_URL` a `cargo mutants`.
