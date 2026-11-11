# Después de la 0.31 · tareas que la lista de cierre manda para más tarde

Escrita el 10-nov-2026 a las 19:48, sobre `feat/ci-rapido`. La lista de cierre (`backlog/ci-rapido-cierre.md`) manda estas cosas «para después de la 0.31» y hasta hoy no existían en ningún backlog: eran una frase en la bitácora, en una spec o en el plan. Aquí cada una es una tarea con su casilla. Lo que debe estar hecho **antes** de publicar está en `backlog/lista-del-release-0.31.md`; esto empieza cuando la 0.31 sale (o cuando se dice expresamente que una tarea se adelanta).

**Cómo se usa.** Una casilla se marca `[x]` solo con el comando y el número de ese momento, en una frase fechada debajo de la tarea (`constancia-en-tiempo-real.md`). Cada tarea dice: qué es y por qué importa, **De** (de dónde sale, con referencia), **Depende de**, **Tamaño** (solo si una fuente lo da; si no, «no consta») y **Decide**.

**Abreviaturas.** `B:n` = `backlog/ci-rapido.md`, línea n. `P:n` = `~/.claude/plans/necesito-que-entres-en-shimmying-sun.md`, línea n. `<X>.spec:n` = `.cursor/handoffs/ci-rapido-<X>.spec.md`, línea n (esa carpeta no se versiona). `inv. N` = punto N de `backlog/ci-rapido-cierre.md`. Tamaño = minutos de agente / minutos de máquina, como en la lista de cierre. Los números de línea de código son los de hoy; los de specs y plan, los comprobados hoy (algunos difieren en unas líneas de los que anotó el inventario, que se indican).

**Qué no está aquí a propósito.** Lo que ya tiene tarea o fichero propio: las 15 casillas de `lista-del-release-0.31.md` (incluido el punto 7, que decide entre subir sqlx y renovar las excepciones) y las tajadas CI-E que siguen abiertas dentro del lote (CI-E2, E3, E7b, etc.: inv. 12, 13, 16).

## A. Producto (cambian lo que ve o hace el binario o su código)

Toda tarea de producto pasa por el six-pack completo (`swarm-forge.md`); el producto no entra por el atajo del lote del juez (inv. 89a).

- [ ] **1. La conexión LISTEN de `protocol.rs` sin ámbito RLS.** La conexión que escucha las notificaciones de sincronización no lleva ámbito RLS. Es el hueco que quedó aparte del arreglo de T-RLS y toca el aislamiento entre proyectos.
  - **De:** B:865 («fuera del lote, como tarea de producto para después de la 0.31: la conexión LISTEN de `protocol.rs` no lleva ámbito RLS»); inv. 35a. Contexto del arreglo vecino: `CHANGELOG.md:15` (T-RLS). Hoy la tarea de escucha sale de `rust/src/protocol.rs:440-444` (`sync_listener`).
  - **Depende de:** nada escrito. **Tamaño:** no consta (el inventario solo da 5 min para abrirla). **Decide:** nadie hace falta; va por el six-pack.
- [ ] **2. La reserva de hilos de `rerank.rs` deja de seguir `wants_gpu`.** `rust/src/search/rerank.rs:51` reserva 2 hilos por omisión si `wants_gpu(Reranker)` aunque el reranker corra en CPU; por eso la fila REL del juez necesita un pool propio y D-F3m («el valor que usa la serie») quedó «desviado en REL». La propuesta es que la reserva siga la colocación real, como `resources::plan`, y no `wants_gpu`.
  - **De:** F3.4.spec:114-115 («Propuesta, fuera de F3 (toca `src/`; para después de la 0.31)… Va al backlog como tarea de producto; no bloquea nada»; cita `rerank.rs:52-54` y `resources::plan` `:240-244`); B:855 (la causa, 19.ª corrida); inv. 35b. D-F3m en F3.4.spec:36.
  - **Depende de:** nada. **Tamaño:** no consta. **Decide:** nadie hace falta.
- [ ] **3. H-E22-2: `doctor` no cuenta los episodios sin vector.** Después de CI-E22 los episodios sin vector se recuperan con `reembed`, pero `doctor` no los cuenta y no avisa de que existen.
  - **De:** B:1241 («Anotados aparte, prioridad baja: H-E22-2 (`doctor` no cuenta episodios sin vector)…»). La spec es `ci-rapido-E22.spec.md`.
  - **Depende de:** CI-E22 integrada y con su juez verde (inv. 8). **Tamaño:** no consta. **Decide:** nadie. Prioridad baja.
- [ ] **4. H-E22-3: el backfill de trozos también escribe hash sin modelo.** El backfill de REM tenía el defecto H-E22-1 (guardaba el vector del fallback de hash etiquetado como del modelo real), corregido en CI-E22 (D-E22f: el backfill solo corre con modelo cargado); el backfill de trozos tiene el mismo defecto y queda fuera de CI-E22.
  - **De:** B:1241 (H-E22-1 y «H-E22-3 (el backfill de trozos también escribe hash sin modelo)»).
  - **Depende de:** CI-E22 (misma regla). **Tamaño:** no consta. **Decide:** nadie. Prioridad baja.
- [ ] **5. H-E17-1: si el servidor arranca con la base caída, nunca migra cuando la base vuelve.** Con CI-E17 `main` abre el pool una vez y, con la base caída, arranca con un solo aviso; pero esa rama no vuelve a intentar las migraciones al reconectar.
  - **De:** B:1241 («H-E17-1 (si arranca con la base caída nunca migra cuando vuelve)»); la spec es `ci-rapido-E17.spec.md`.
  - **Depende de:** CI-E17 integrada (inv. 9). **Tamaño:** no consta. **Decide:** nadie para el arreglo; si cambia qué mensaje o comportamiento gana con la base caída, sí Leandro (inv. 9 lo marca como decisión).
- [ ] **6. Subir sqlx a 0.9.** Retira RUSTSEC-2023-0071 (`rsa`, por `sqlx-mysql`) y RUSTSEC-2026-0459 (`libsqlite3-sys`, por `sqlx-sqlite`) del lockfile y de `scripts/advisory-exceptions.txt`. No retira RUSTSEC-2024-0436 (`paste`, por `tokenizers` y `nalgebra`). — **ADELANTADA 11-nov: Leandro decidió subirla ahora («si está la actualización la corres»): es CI-E36 del lote «CI rápido»; sqlx-core 0.9.0 corrige además el `Pool::close()` que rodeó CI-E32.**
  - **De:** decisión de Leandro del 10-nov 18:18 (B:1219 (2): «sqlx: “Tras la 0.31, tarea propia”»); inv. 86; `CHANGELOG.md:71` (la subida se probó y se revirtió: `sqlx` 0.9 con `pgvector` 0.4.2 renombra las features de TLS —`runtime-tokio-rustls` desaparece—, deja 24 errores de compilación solo en la librería, 18 de ellos «dynamic SQL strings should be audited» → `SqlSafeStr`, 11 en `handlers/sync.rs` y 7 en `db.rs`, y cambia `MIGRATOR.run_direct`, el camino de las migraciones con SHA-384 congelado). Relacionada en `CHANGELOG.md:28`: `block-buffer` 0.10.4 (GHSA-qwgh-2vcv-g2f7) queda planeado para la 0.32 con dueño Leandro y revisión el 2026-11-30.
  - **Depende de:** antes de tocar la dependencia, `socket` (`depscore`) y `osv` (`nucleo.md`; inv. 86). Es migración de producto: la compatibilidad con el binario anterior (expand → contract) se revisa antes. **Tamaño:** no consta (el CHANGELOG la llama «a slice of its own»). **Decide:** ya decidido (tarea propia tras la 0.31); si la 0.31 no ha salido el 30-nov, ver la tarea 18.
- [ ] **7. T6: el crate `search-core`.** Segundo paso de «mutación barata»: sacar la búsqueda a un crate propio. El primer paso (semilla de mutación por contenido) lo cubre F4; el A/B de sccache seguía pendiente.
  - **De:** B:952 («T6 mutación: paso 1 cubierto por la semilla por contenido (F4) y el A/B de sccache pendiente; paso 2 (crate `search-core`) es refactor de producto → Después de la 0.31»); inv. 36b.
  - **Depende de:** nada escrito. **Tamaño:** no consta. **Decide:** Leandro, si el refactor merece el riesgo; la fuente lo llama refactor de producto.

## B. Juez y CI

Cada una dice el nivel y los segundos que añade antes de implementarse (`ci-rapido.md`, «El CI escala con el proyecto»; regla de Swarm Forge).

- [ ] **8. CI-E5: regresión por punto de cambio.** Detectar la regresión de cada paso por la serie de sus tiempos (detección de punto de cambio) y dejar el 20 % como alarma secundaria. Importa porque un 20 % fijo no ve una regresión del 15 % en un paso estable.
  - **De:** B:1009 (lista de guardas «abiertas como tareas para después de la 0.31»: «CI-E5 regresión por punto de cambio»), B:1025 («Quedan fuera por ahora, con motivo: CI-E5 (punto de cambio: necesita historial, hay 2 corridas)»); `docs/ci/metodologia-de-escala.md:205` (local) con la decisión del 6-nov de mantener el 20 % como alarma secundaria; inv. 14.
  - **Depende de:** historial: **10 corridas por modo** (inv. 14); a las 09:43 del 10-nov había 2 corridas completas (B:1025). `unidades.jsonl` ya se escribe desde CI-E28 (inv. 11, a falta de verlo en una corrida real). **Tamaño:** no consta. **Decide:** nadie.
- [ ] **9. CI-K3c: los 59 ficheros de D2 a base desechable.** Los 59 ficheros de D2 corren cada uno en su proceso sobre la `brain_gate` compartida; solo se pueden agrupar en pocos binarios si pasan antes a base desechable (`common::in_a_scratch_database`), sin escribir el entorno del proceso y sin `session::set`. Importa porque es lo que baja el número de binarios (y de enlaces) de D2.
  - **De:** K3.spec:460-463 («Es un cambio de cada test, no de forma, y hasta entonces cada uno sigue con su proceso»); inv. 35c.
  - **Depende de:** nada escrito. **Tamaño:** no consta (es un cambio por test). **Decide:** nadie. Cualquier agrupación se mide con `cargo --timings` antes y después; sin esa medida no se afirma el ahorro.
- [ ] **10. Tres mejoras pequeñas anotadas como «futuras».** Cada una con su motivo de haberse descartado:
  - [ ] **10a. K6e: un `OnceLock` por guion** para que el contrato de residuos corra igual con un solo test filtrado. Hoy «no se cambia (un OnceLock por guion es más código; se anota como mejora futura)». **De:** K6.spec:18.
  - [ ] **10b. Q2-e: mapa de cobertura por línea o región** en la mutación (subconjuntos más pequeños, solo cambia tiempo). «Descartada en F6; se reconsidera con el estudio en la mano» (frágil en el `{` de un cuerpo reemplazado). **De:** F6.spec:124. Depende de que el estudio de Q2 (inv. 38i) exista.
  - [ ] **10c. Q1-f: subir el techo de 6 o el `--timeout 90`** de la mutación. El techo existe por los *timeout* en saturación; «fuera de F6 (F5)». **De:** F6.spec:119.
  - Inventario: inv. 35d, que cita K6.spec:19, F6.spec:126 y F6.spec:131 (hoy en K6.spec:18, F6.spec:124 y F6.spec:119). **Tamaño:** no consta. **Decide:** nadie; son deuda.
- [ ] **11. El mejorador sobre el Python del juez.** `scripts/gate-dag.py` y `scripts/gate_usl.py` tienen 25 funciones sobre CC 8 (13 y 12: `simular` 19, `admission` 15, `parse_row` 15, `plan_problem` 14…). CI-E28 ya fijó la línea base verdadera (`scripts/lizard-baseline.txt` gana esas 25); queda partirlas.
  - **De:** inv. 24 (con su nota de CI-E28 del 10-nov 18:49: `--update-lizard-baseline` solo mide `rust/src` y borraría esas líneas; `scripts/mcp_live_session_test.py::main` tiene CC 44 y no está en la línea base). Según el inventario, `quality-gate` saldría con 2 si su diff las incluye (`quality-gate.sh:3999`).
  - **Depende de:** nada. **Tamaño:** 2–4 h / 2 (inv. 24). **Decide:** nadie. Regla del repo: no se parte una función de un solo trabajo solo para bajar el número (`swarm-forge.md`, «Mejorador»); lo que no se parta se anota por qué.
- [ ] **12. Deuda del mejorador en el planificador: el camino crítico en tres sitios y `planned_pool`.**
  - [ ] **12a.** El camino crítico existe en tres sitios: `gate-dag.py` (`ranks`, orden de arranque), `gate_history.py` (`camino_critico`) y `gate_usl.py` (`holguras`). La spec de E7 decidió dejar `holguras` y `camino_critico` porque miden otra cosa (holgura y cota inferior) y garantizar con la propiedad P5 que dan la misma longitud: queda decidir si se unifican. **De:** E7.spec:62 (D-E7g); inv. 25a (que cita `gate-dag.py:844`, `gate_history.py:718`, `gate_usl.py:697`; hoy `:860`, `:724`, `:697`).
  - [ ] **12b.** `planned_pool` (`scripts/gate-dag.py:578`) copia en Python la regla `half_cores.min(MAX_*)` de `rust/src/resources.rs`: dos fuentes de la misma regla. **De:** B:1133 («Acoplamiento anotado…»); inv. 25b.
  - [ ] **12c.** La pasada del mejorador de F2a/F2b nunca se hizo. **De:** B:478 («F2a's improver/architect pass goes with F2b»); inv. 25c.
  - **Depende de:** la tarea 11 si tocan las mismas funciones (ficheros compartidos van en serie). **Tamaño:** 60–90 / 2 (inv. 25). **Decide:** nadie.
- [ ] **13. Squawk: linter de migraciones con trinquete.** Las migraciones sqlx en Postgres se lintean con Squawk con un trinquete; las migraciones ≤ 0060 están congeladas. Complementa expand → contract, que ya es política.
  - **De:** decisión de Leandro del 10-nov 18:18 (B:1219 (4): «Squawk, herramientas por hash y la batería de defectos sembrados, tareas para después de la 0.31»); P:73 («Squawk (linter de migraciones) con trinquete»); inv. 95c.
  - **Depende de:** `socket` y `osv` antes de añadir la herramienta (`nucleo.md`); fijarla por hash (tarea 14). **Tamaño:** no consta. **Decide:** ya decidido.
- [ ] **14. Cadena de suministro del juez: herramientas fijadas por hash, `global-min-publish-age` y control positivo del escáner.** Importa porque MemoryIndustry publica en npm y PyPI, así que la cadena de suministro es crítica aquí. `deny.toml` ya tiene `[sources]`.
  - **De:** B:1219 (4); P:72; inv. 95d.
  - **Depende de:** nada escrito; encaja con la tarea 13. **Tamaño:** no consta. **Decide:** ya decidido.
- [ ] **15. Batería de defectos sembrados con la regla de tres.** Una batería de 30–50 parches por clase para comprobar que el juez se pone rojo donde debe.
  - **De:** B:1219 (4); P:69 («Defectos sembrados + regla de tres: batería de 30–50 parches por clase»); inv. 95e, que dice «es el punto 42» (el corpus de rojos de referencia, que sigue abierto en la lista de cierre; si el 42 se hace esta noche, esta tarea es su ampliación, no otra distinta).
  - **Depende de:** inv. 42 y 43 (los dos «cambian el veredicto o lo dejan sin base»). **Tamaño:** no consta (inv. 42: 60–90 / 60–90 compartida para el corpus actual). **Decide:** ya decidido que va después de la 0.31.

## C. Publicación

- [ ] **16. T7: flujos de GitHub.** Los flujos de publicación y de CI usan `cargo install` de audit, deny y cross y `actions/cache` con clave solo de `Cargo.lock`. Se añaden restos que dejó CI-K7/K8/K1: el bloque muerto de OpenSSL en `publish.yml:200-242` (sin `openssl-sys` ya no hace nada), los pasos `--features docs` duplicados en `ci.yml:94-101` (los cubre K8) y `dtolnay/rust-toolchain@stable` con el fichero de toolchain presente (rustup usa la toolchain fijada).
  - **De:** B:950 («T7 flujos de GitHub… Después de la 0.31 (toca publish.yml)»); K7.spec:117 (K7d, «su limpieza va con T7, después de la 0.31») y K7.spec:460 (`ci.yml`, «van con T7»); K1.spec:7 (K1b, la acción `@stable`); inv. 36a, que atribuye las tres a la spec de K7.
  - **Relacionada con:** el publish en seco (`lista-del-release-0.31.md`, punto 12), donde `rust-toolchain.toml` entra por primera vez en `publish.yml`. **Tamaño:** no consta. **Decide:** nadie. Cualquier cambio en `publish.yml` pasa por un publish en seco antes de una publicación real.
- [ ] **17. NPM-OIDC: publicación de npm con trusted publishing (OIDC).** Publicar desde `publish.yml` sin un token de npm de larga vida. El `.exe` de Windows sigue sin firmar.
  - **De:** B:45 («Outside the batch (Leandro, 2026-11-06): NPM-OIDC… its own task, after 0.31»); B:344 (registrada en la tabla del lote y en Notion); inv. 92c.
  - **Depende de:** la 0.31 publicada; la configuración del paquete en npm es de su cuenta (Leandro). **Tamaño:** no consta. **Decide:** Leandro (su cuenta y su orden de publicar).
- [ ] **18. Si la 0.31 no ha salido el 30-nov: renovar las tres excepciones de avisos.** Es la rama alternativa de la tarea 6. `RUSTSEC-2023-0071`, `RUSTSEC-2024-0436` y `RUSTSEC-2026-0459` caducan el 2026-11-30 y desde el día siguiente el juez da rojo en su primer paso (`advisory-exceptions.sh`). Se renuevan **en los tres ficheros a la vez** (`scripts/advisory-exceptions.txt`, `rust/.cargo/audit.toml`, `rust/deny.toml`), con fecha y motivo, a lo sumo 180 días adelante.
  - **De:** decisión de Leandro del 10-nov (B:1219 (2): «si la 0.31 no ha salido el 30-nov, las tres excepciones se renuevan con fecha y motivo»; nota del 10-nov 19:22 en inv. 86); inv. 54; `lista-del-release-0.31.md`, punto 7 (que describe las dos opciones). Una de las tres, RUSTSEC-2024-0436, no la retira ninguna subida de sqlx.
  - **Depende de:** la fecha: solo se hace si el 30-nov no hay release. **Tamaño:** no consta (no hay estimación escrita para esta tarea). **Decide:** ya decidido; el agente la aplica con la orden de renovar.

## D. Herramientas de la máquina

- [ ] **19. Instalar las unidades `qg-auditoria-semanal.service` y `.timer` (domingo 02:00).** La auditoría semanal de `quality-gate` con la suite entera. No están instaladas (comprobado el 10-nov): lanzan el guion del clon principal, que hoy está en `feat/0.29-seguimientos`. Se instalan tras publicar. La primera corrida necesita `QG_AUDIT_BASE` (`scripts/qg-weekly-audit.sh:9-11`: «Without it, the HEAD of the last audit… The first audit has no last one, so it needs this») y será la que mida por primera vez el reloj de Q1.
  - **De:** B:901 (decisión de Leandro del 9-nov: «la auditoría de quality-gate con la suite entera es semanal, el domingo a las 02:00»), B:903 (F6r: «Instalación de las unidades… después de la 0.31»); `docs/gate.md`, «Weekly audit of the mutation judge»; `lista-del-release-0.31.md`, punto 15; inv. 81.
  - **Depende de:** la 0.31 publicada y el clon principal en la rama de integración; `QG_AUDIT_BASE` (la decide quien la instala). Es cambio de sistema (systemd de usuario): va a memoria con su porqué y cómo se revierte. **Tamaño:** 15 / la auditoría, en exclusiva (inv. 81; tope de 6 h y 36G, B:903). **Decide:** nadie para instalar; la primera corrida pide turno exclusivo en `pesado`.

## E. Decisiones de Leandro

Las que solo él puede tomar o autorizar. Ninguna se resuelve por el agente.

- [ ] **20. `memory-industry reembed --all` en su instalación, si alguna vez corrió REM sin modelo.** El defecto H-E22-1 (backfill de REM sin comprobar el modelo) guardó vectores del fallback de hash etiquetados como del modelo real. Si su instalación corrió REM sin modelo, su corpus tiene vectores de hash con etiqueta buena; lo arregla `reembed --all`, que **escribe en la base viva**.
  - **De:** B:1241 («Para decidir con Leandro tras desplegar: … `memory-industry reembed --all`, que escribe en la base viva y necesita su orden»); inv. 8.
  - **Depende de:** CI-E22 desplegada (corrige el defecto) y, si se quiere, la tarea 4. **Tamaño:** no consta. **Decide:** Leandro, con orden explícita (escribe en la base viva; `nucleo.md`).
- [ ] **21. H3 de E16: ¿se calienta el reranker de stdio en segundo plano al arrancar?** Hoy, con CI-E16, el embedder carga perezoso y en stdio no se calienta en fondo (el runtime espera sin límite al `spawn_blocking`); el reranker sí se calienta en fondo al arrancar (`rust/src/protocol.rs:476-490`). Mismo trade-off que E16. — **DECIDIDO 11-nov (AskUserQuestion): «Solo si se usa».** En stdio el reranker se calienta solo si la búsqueda rerankea por omisión (`mode::active().rerank_default()`) o si una variable lo pide; `serve` sigue igual. A cambio, la primera búsqueda con `rerank:true` paga la carga. Tajada propia tras la 0.31 (E31.spec, P1).
  - **De:** B:1160 («(H3) el reranker de stdio se calienta en fondo al arrancar (`protocol.rs:476-490`): mismo trade-off que E16, sería decisión de Leandro si la medida lo justifica»); inv. 88b.
  - **Depende de:** una medida que lo justifique (la de CI-E16, inv. 1: A/B intercalado en exclusiva). **Tamaño:** no consta. **Decide:** Leandro, solo si la medida lo justifica.

## Cuenta

| Grupo | Tareas | Casillas (con subcasillas) |
|---|---|---|
| A. Producto | 7 | 7 |
| B. Juez y CI | 8 | 14 |
| C. Publicación | 3 | 3 |
| D. Herramientas de la máquina | 1 | 1 |
| E. Decisiones de Leandro | 2 | 2 |
| **Total** | **21** | **27** |

## F. Para la 0.32, por orden de Leandro (11-nov-2026: «esos fallos que salieron quiero que en la v 32 se corrijan»)

Son los fallos que destaparon las pruebas del 11-nov (GPU en WSL y en Windows, ensayo de actualización, despliegue en esta PC, primer quality-gate) y que la 0.31 no corrige. Cada uno entra por el especificador; ninguno bloquea la 0.31.

**Producto**
- [ ] **F1. `nli::available()` ignora `CUBA_NLI_PATH`.** `rust/src/cognitive/nli.rs:87-104` decide si hay modelo mirando la ruta fija, y la carga sí respeta la variable. Efecto: `nli_probe` no corrió ni en CPU ni en GPU en WSL (W8) ni en Windows (V5n); **el NLI en GPU no está verificado en ningún sistema**. Cierre: `nli_probe` corre con `CUBA_NLI_PATH` en un HOME de prueba, y V5n/W8 pasan.
- [ ] **F2. El servicio tarda 2 min 45 s en contestar tras un reinicio.** Medido en el despliegue de esta PC (14:55:45 → 14:58:30): calienta el reranker en CPU antes de abrir el puerto (`bind < warm < announce < serve`). Decidir con Leandro qué se prefiere (abrir antes y decir «calentando» en `/health`, o calentar en segundo plano); hoy una actualización deja a todas las sesiones sin memoria ese rato.
- [ ] **F3. `doctor` abre la base antes de cualquier chequeo** (`doctor.rs:1762`): sin base no dice nada de GPU ni de runtime. Una orden que diga la colocación y el runtime sin base (o `doctor` degradando esos chequeos) habría evitado W5/V3.
- [ ] **F4. En `doctor`, `http_port` avisa «something is already listening» cuando quien escucha es el propio servicio.** Ruido en cada instalación con el servicio en marcha.
- [ ] **F5. El reintento del reembebido automático desde el ciclo REM (A1 de la revisión) no tiene prueba de extremo a extremo**; ni que `run_in_background` use el pool lateral y el codificador de fondo (A3). El ensayo Q7 lo cubre de forma indirecta (0 esperas de pool, latencia ×1,13).
- [ ] **F6. `principal_grants` no cubre `brain_observation_chunks` ni `brain_procedures`** y el código nunca fija `app.current_principal` (H-C3; anterior al lote).
- [ ] **F7. La importación en modo `merge` con texto divergente deja la fila y luego le escribe el vector del paquete**, hecho para otro texto (anterior al lote; hallazgo del implementador de CI-D5c-C).
- [ ] **F8. Un Linux sin `ca-certificates` no puede construir ningún cliente HTTP**, ni los de loopback (reqwest 0.13 con el almacén del sistema). Decidir si avisa y sigue con una lista empaquetada.

**Guiones de prueba y juez**
- [ ] **F9. `probar-gpu.sh`: una tercera corrida limpia en los dos sistemas** con las correcciones del 11-nov (opciones de `env`, toolchain, UTF-8, lector del informe, base desechable para `doctor`), y en WSL darle una base del juez (`GPU_QA_DATABASE_URL`) para W5.
- [ ] **F10. El `cargo-mutants` de pega de los autotests de `quality-gate.sh` y `mutants-gate.sh` es una lista a mano** de lo que el real rechaza: el modo «lab» nunca había corrido de verdad y murió en la primera corrida (`--copy-target` con `--gitignore`). Un paso barato que pase la línea real por `cargo mutants --list`.
- [ ] **F11. 23 funciones Python del juez, del e2e y del ayudante de QA por encima de CC 8** con línea en `lizard-baseline.txt` (revisar antes del 2027-02-28); `run_rem_consolidation_locked` sigue en 15.
- [ ] **F12. lizard 1.24 no cierra un literal `char` no ASCII en Rust** y mide de más (un CC 19 falso); guarda o nota en el gate.
- [ ] **F13. El juez de Windows:** el planificador puede no importarse con el Python de Windows (`fcntl`, `SIGHUP`: CI-E25b) y `run_counted` para en la primera sección sin línea de Windows, lo que obliga a varias corridas para reunir recuentos; un modo de medida que siga tras un recuento que falta.
- [ ] **F14. Una dev-dependency no puede pedir features que el build normal no pide** (`tokio-rustls/ring` partió el binario release en dos; lo cazó B3g, tarde): contrato barato sobre `cargo tree -e features` con y sin aristas `dev`.
- [ ] **F15. Una tajada que añade tests a `gpu::` mueve dos líneas base** (`lib` y `placement-table`): que el juez lo diga en `rapido`, no en la fila REL.
- [ ] **F16. `v046_migrations_apply_over_an_existing_schema` llegó al 93 % de su techo** en la corrida más lenta del veredicto (14,83 s de 16).
