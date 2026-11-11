# CI rápido · lista de cierre (se va tachando)

**Foto de las 18:02 del 10-nov-2026**, sobre `feat/ci-rapido` en `397e162` (veredicto del día sobre `5bec0b6`).

**Orden de Leandro que la origina:** «que no falte ninguna tarea por más mínima que sea para cumplirlas todas hoy… y dejar todo listo y 100 % habilitado».

**Cómo se usa.** El estado vivo de cada punto se anota AQUÍ: la casilla y una frase fechada con el commit o la medida. La bitácora sigue en `backlog/ci-rapido.md`. Un punto se marca `[x]` solo con el comando y el número de ese momento; mientras tanto lleva «EN CURSO» con lo que falta.

**De dónde sale.** Inventario de solo lectura del 10-nov: el backlog entero (1212 líneas), el plan (426), las 26 specs y los 26 traspasos `ci-rapido-*` contrastados con el backlog y con HEAD `397e162`, los ficheros de excepciones, la metodología de escala y el estado de la máquina. Quedaban 96 puntos abiertos; 41 son huérfanos (están en el plan o en las specs y el backlog no los daba ni por hechos ni por abiertos). El objetivo literal (`todo` < 15 min por omisión) estaba cumplido sobre `5bec0b6`, pero «listo» según `verificacion.md` todavía no: faltaba la segunda vuelta sobre el SHA final, el endurecedor y el qa no habían corrido sobre nada del lote, y `feat/ci-rapido` no estaba en `release/0.31.0`.

**Abreviaturas.**
- `B:n` = `backlog/ci-rapido.md`, línea n.
- `P:n` = `~/.claude/plans/necesito-que-entres-en-shimmying-sun.md`, línea n.
- `<X>.spec:n` = `.cursor/handoffs/ci-rapido-<X>.spec.md`, línea n (la carpeta no se versiona).
- `CR` = `~/.cache/memory-industry-gate/ci-rapido`.
- **(H)** = huérfano.
- Tamaño = minutos de agente / minutos de máquina («en exclusiva» = `pesado` sin `-p`).

## Trabajo en curso en el momento de la foto (el backlog acababa a las 17:14)

- `Memorys-ci-e16`: pasada 1 de CI-E16, 6 ficheros sin commit; rojo de compilación visto con cargo a las 17:51 (`CR/mi-cargo-202611101751.out`: rc=101, 21 errores E0425/E0433, los esperados).
- `Memorys-ci-e12`: 11 ficheros sin commit, los de CI-E19 (`fake_host`, `discovery`, `pair`, `proxy`, `doctor`, v022, v027, v029×3) más `gate_dag_contract.rs`.
- `Memorys-ci-e26`: 6 ficheros sin commit, entre ellos `gate_dag_contract.rs`, `gate-cluster.sh` y `validar-handoff.sh`.
- `~/.local/bin/pesado` cambiado a las 17:43 con el arreglo de P-1 (punto 80).
- `pesado` lo tenía Mapupita (turno compartido 1, `verde_264t13` desde 17:44) con 2 tiques en cola. Disco: 562 GB libres, bajo el umbral de 600.

## ¿HECHO? — el backlog lo abre y después lo cierra (no repetir)

- CI-E20 (B:1161) → dentro de CI-E15 (B:1176).
- CI-E13, E14, E15, E21 y E24 → B:1141, 1152, 1184, 1192, 1202. De CI-E18 solo faltan sus sabotajes (38c).
- CI-J17 «confirmar que no queda `juez/juez`» (B:998) → B:1019; el resto fue a la tanda 5 (B:1040) y al medidor (B:1021).
- CI-K1 «aplazada» (B:925) → hecha (B:934; `rust-toolchain.toml` existe).
- K7b y la excepción RUSTSEC-2025-0119 (B:963) → borrada (B:971; hoy hay 3 excepciones y no está).
- T-DBURL (B:901) → integrado (B:975, 983).
- Las 14 líneas `pendiente` de `gate-latency.txt` (eran 13) → 0 (B:1202).
- Líneas de techos de `v017_rerank_gpu`, `nli_cost` y `v046` (B:855) → B:1190, 1207.
- «local_gate_contract 55,5 s por explicar» (B:853) → VM degradada y PATH (B:965, 989).
- Excepción `demo.sh` sin dueño (B:622) → CI-J13 D1.
- `seed: miss` (B:323) → F4d (B:563).
- Poda de corridas D-F3i (B:875) → CI-E15. La otra mitad, los `.tmp` de la semilla, no (punto 15).
- `registrar` pierde la línea y `__pycache__` (B:470, 477) → hechos en `gate_history.py:826` y `gate-dag.py:72`.
- U4 y «la vuelta docs no repite contratos» (B:668, 938) → CI-K8 (B:952).
- Horario de la auditoría (B:869) → domingo 02:00 (B:901).
- Puntero a la metodología (B:1003) → `AGENTS.md` (B:1011).
- A1, C1 y C3 del perfil → CI-E10, CI-E9 y CI-E11.
- H1–H10 del arquitecto → tanda 5, E7 y J18; H7 quedó para «la lista del release» (punto 61).
- «CI-E8 no funciona» y «CI-E10 no baja» → corregidos (B:1082, 1143).
- CI-J2: A1–A11 y P1–P5 están todos en HEAD.
- CI-K5 = CI-E13; CI-K3b = CI-E1.
- Camino crítico «P→DC 228,6 s» (B:1086) → superado: DC mide 23–34 s.

## (A) Código o tests del repo por hacer

- [ ] **1.** **CI-E16** · carga perezosa del embedder (producto). B:1147, 1159, 1212; E16.spec. — **EN CURSO 10-nov 18:17:** pasada 2 commiteada 6935b8d; verde con cargo a las 18:08 (compila a la primera; los 5 tests nuevos de onnx pasan, 25 de 25); fusionada en feat/ci-rapido (f4f54d5). Falta: el juez, los sabotajes y la medida A/B. — **NOTA 10-nov 18:40:** JUEZ VERDE sobre 87346f6 (18:30-18:39, 540 s): la fila E2E pasa de 113-150 s a 42,8 s. Integrada. Faltan solo los sabotajes (noche) y la medida A/B intercalada (noche).
  - **EN CURSO (10-nov 18:07):** pasada 1 en commit `bed6489` (`test(rojo)`, 18:04) y pasada 2 `6935b8d` (18:07) en `feat/ci-e16`. Falta: verde con cargo, integrar, sabotajes y medida.
  - Lo que tocaba: pasada 2 en `onnx.rs`, `db.rs:653`, los nueve llamantes de `cronica`, `faro`, `zafra`, `doctor` y `reembed_cli` (las líneas de la spec siguen valiendo) y CHANGELOG; 19 sabotajes; medida ABBA; un `todo`.
  - Depende de CI-E10 (ya en la rama).
  - La spec tiene 20 escenarios + 3 criterios y 19 sabotajes; B:1159 dice 22 y 20.
  - Tamaño: 60–90 / rojo y verde 15, sabotajes 60–70, medida 15 en exclusiva más dos builds de release.
- [ ] **2.** **CI-E26** · reparto en turno de 12 CPUs (unos 170 s). B:1193, 1199, 1212. — **NOTA 10-nov 18:40:** juez verde sobre 87346f6. Medido en turno de 12 CPUs: B1 espera 264 s (antes 338-411), pero ahora por cpu, no por jobs (M usa 6 núcleos), y B3 con 6 jobs y compañía tarda 299 s (antes 169-216 sola): T 565 → 539 s, casi todo por E2E. Queda abierto como punto nuevo: en un turno de 12, que B1 entre antes que M (simular antes de tocar).
  - **EN CURSO (10-nov 18:05):** commits `2cc9606` (`test(rojo)`) y `d2292cc`, fusionados en `feat/ci-rapido` (`ebbc710`). Falta el juez.
  - Cambia la clave de la tabla: pide una serie de siembra. Tamaño: 45–60 / 30–40 compartida.
  - [ ] **2a.** El backlog solo habla de jobs. No recoge que el semáforo `models` ya muerde: DBG espera 84–91 s y REL 226 s en el veredicto de 12 CPUs. Comprobar que el arreglo lo cubre o anotarlo.
  - [ ] **2b.** **(H)** S1 de E11, «DBG sin `models`» (`scripts/gate-dag.txt:41`; E11.spec §11).
- [x] **3.** **CI-E12** · seis tests de `gate_dag_contract` con muro de tiempo. B:1102, 1141, 1207. Es lo que permite bajar los techos subidos hoy (punto 55). 30–45 / 3. — **HECHO 10-nov 18:40:** juez verde sobre 87346f6; gate_dag_contract 17,3 s con 99 tests (techo 23).
  - **EN CURSO (10-nov 18:05):** commit `8cfb106`, fusionado en `feat/ci-rapido` (`57ae0b9`). Falta el juez.
- [x] **4.** **CI-E19** · esperas fijas fuera del detector. B:1155. La spec añade D-E11j.3 (`start_paused`). 45 / 5. — **HECHO 10-nov 18:40:** juez verde sobre 87346f6 (el test g15 arreglado en c57ab09 pasa). Sus 9 sabotajes de producto van en la cadena de la noche.
  - **EN CURSO (10-nov 18:05):** commit `8cfb106`, fusionado en `feat/ci-rapido` (`57ae0b9`). Falta el juez.
- [ ] **5.** **(H)** `v09` y `v040` exigen `ready` y verían `pending` con un embebido de más de 2 s. B:1064 lo declara como riesgo, sin tarea ni guarda, y no estaba en la lista de CI-E19. 20–30 / 2.
- [x] **6.** **CI-E23** · el log del clúster va al directorio de la corrida. B:1178. 20–30 / 1. — **HECHO 10-nov 18:40:** juez verde sobre 87346f6.
  - **EN CURSO (10-nov 18:05):** commits `2cc9606`/`d2292cc`, fusionados (`ebbc710`). Falta el juez.
- [x] **7.** **CI-J19** · `validar-handoff.sh` (`filas_dag`) usa el lector único de la tabla. B:1123. 20 / 1. — **HECHO 10-nov 18:40:** juez verde sobre 87346f6.
  - **EN CURSO (10-nov 18:05):** commits `2cc9606`/`d2292cc`, fusionados (`ebbc710`). Falta el juez.
- [ ] **8.** **CI-E22** · bug de producto: un episodio sin vector no lo recupera `reembed` (H1 de E16). B:1160. O `reembed` cubre `brain_episodes`, o el mensaje de `main.rs:84-90` deja de prometerlo. 30–60 / 5. — **EN CURSO 10-nov 18:37:** CI-E22 especificada (ci-rapido-E22.spec.md, 11 escenarios, 18 sabotajes, sin migración: brain_episodes.embedding existe desde 0002): reembed (cuba_zafra y la CLI) y el backfill cubren los episodios con la misma regla que las observaciones; faro sí busca episodios por vector y filtra los que no lo tienen, así que el arreglo no puede ser solo de texto. Pasada 1 en curso en feat/ci-e22. — **EN CURSO 10-nov 18:49:** pasada 1 f3e5bef (rojo de compilación visto: 9 errores) y pasada 2 05bd51a (compila a la primera; 7 tests de lib en verde, incluido el del mensaje del drenaje); fusionada (bf619b7). Falta: los 4 tests de v048 con base (pasada completa en curso), su rojo devolviendo el producto (noche) y el juez. — **NOTA 10-nov 18:54:** pasada completa sobre bf619b7 (18:48-18:53): los 4 tests de v048 con base pasan (group_scratch_db 44, recuento OK). Falta el juez y el rojo de comportamiento devolviendo el producto (noche).
- [ ] **9.** **CI-E17** · dos pools al arrancar (`main.rs:354-369`, `protocol.rs:434`). B:1161. Va después de medir E16; hay que decidir qué mensaje gana con la base caída. 45 / 5. — **EN CURSO 10-nov 18:37:** CI-E17 especificada (ci-rapido-E17.spec.md): main abre el pool una vez y lo pasa a run_mcp; con la base caída un solo aviso («starting without PostgreSQL…»); va en serie tras CI-E22 (las dos tocan main.rs). — **EN CURSO 10-nov 19:00:** CI-E17: pasada 1 a493651 (rojo visto con cargo: «the listener counted 2 connection attempts, expected 1»), pasada 2 69eb669 (verde: los dos tests de v048 pasan), fusionada 85af826. Falta el juez (E2E) y sus sabotajes.
- [ ] **10.** **CI-E25** · `os.getuid` y `os.kill(pid, 0)` de CI-E15 son solo POSIX. B:1186. Depende de la medida en Windows (punto 84). 20 / —.
  - **EN CURSO (10-nov 18:05):** commits `2cc9606`/`d2292cc`, fusionados (`ebbc710`). Falta el juez. Ver también CI-E25b en «Añadidos después de la foto».
- [ ] **11.** **(H)** `unidades.jsonl` y `flaky.jsonl` no se escriben nunca. Es el huérfano que más bloquea. 30–45 / 2. — **EN CURSO 10-nov 18:49:** CI-E28 (b4fb912/0ed9bb4, fusionada 538f250): el planificador llama a gate_history.units para cada fila cuyo log trae binarios de test con su tiempo; falta verlo escribir unidades.jsonl en una corrida real del juez.
  - `gate_history.units()` solo la llama `registrar`, una CLI sin llamadores (`scripts/gate_history.py:578-598`, `:793-834`).
  - Tras 12 corridas del 10-nov no existe el fichero en `~/.cache/ci-tiempos/memory-industry/20261110/`.
  - B:1025 decía «nace con CI-E4» y no nació.
  - Bloquea CI-E2 (punto 12), la marca de intermitente y D-F5e.
- [ ] **12.** **CI-E2** · coste por test y cuota de los 10 más caros. B:1009, 1025. Depende del punto 11. Lleva la decisión de los números en el recibo (punto 92). 60 / 2.
- [ ] **13.** **CI-E3 y L1–L3** · segundos por autotest con techo; paralelizar los casos del autotest de `merge-gate.sh` (62 → 71 s tras CI-E15). B:1009, 1025, 1076. Los datos ya están medidos. 60–90 / 3.
- [ ] **14.** **CI-E5** · regresión por punto de cambio. B:1009, 1025. Necesita historial (10 corridas por modo). Después.
- [ ] **15.** **(H)** `mutants_seed_prune` no borra los `<clave>.tmp-<pid>` de un pid muerto (`scripts/mutants-gate.sh:690-693`). Anotado en B:875 («después de F6»), nunca abierto. 20–40 / 1. — **EN CURSO 10-nov 18:49:** CI-E28: mutants_seed_prune borra los <clave>.tmp-<pid> de pid muerto del mismo usuario; autotest verde; falta el juez.
- [ ] **16.** **CI-E7b** · J de DBG y B3 desde barridos fiables. B:1031, 1212; E7.spec:205-227. Depende del punto 39. 45 / lo de los barridos.
  - [ ] **16a.** E7b.2 (`--modo serie|dag`) no existe y con el DAG por omisión conviene cerrarla como «no se hace».
  - [ ] **16b.** **(H)** Quitar el prefijo `CARGO_BUILD_JOBS` de D-F1b bajo cargo-mutants (F6.spec:176).
- [ ] **17.** **CI-E1b y CI-E1c** (B:1037): solo anotados. E1c pide que `test-times.sh --count` dé `ignored`. Unos 60 / 2.
- [ ] **18.** **(H)** E1 §9, tercer seguimiento: rechazar una línea `plain/…` que cambia sin tocar las fuentes de su binario (E1.spec:383). 45–60 / 1.
- [ ] **19.** Un primer argumento desconocido de `run-all-tests.sh` arranca el juez entero (`:1855-1866`). B:1037; sin identificador y fuera de B:1212. 20–30 / 1. — **EN CURSO 10-nov 18:49:** CI-E28: un argumento desconocido de run-all-tests.sh sale 2 antes del candado; falta el juez.
- [ ] **20.** **(H)** `rust/build.rs` con `rerun-if-changed=migrations`. P:68 lo marcaba ✅; B:113-115 lo vio en la práctica y no abrió tarea; no existe. Es producto: hay que decidir si entra en la 0.31. 15 / una compilación.
- [ ] **21.** **(H)** Rama inalcanzable en `scripts/eval_seed.py` (deuda del mejorador, B:263). 5 / 0. — **EN CURSO 10-nov 18:49:** CI-E28: quitada la rama inalcanzable de seed() en eval_seed.py.
- [ ] **22.** ¿Debe el juez rechazar un `TMPDIR` que no existe? B:485 («deuda»); `gate-lock.sh:540-544` sigue igual. Es además la excepción del punto 57. 20 / 1. — **EN CURSO 10-nov 18:49:** CI-E28: un TMPDIR definido que no es directorio se rechaza con exit 2 (no se crea); la excepción NOT_HANDED_DOWN se queda (TMPDIR se sigue exportando), con su razón actualizada; falta el juez.
- [ ] **23.** **(H)** `run-all-tests.sh:1966-1973` sale con 1 (rojo de producto) por falta de disco; es entorno y debería ser 2 (K4.spec:310). 20 / 1. — **EN CURSO 10-nov 18:49:** CI-E28: disco bajo sale 2 (entorno), no 1; falta el juez.
- [ ] **24.** **(H)** El mejorador no ha pasado por el Python del juez. Fijar la línea base: 10. Partirlas: 2–4 h / 2. — **EN CURSO 10-nov 18:49:** CI-E28: lizard-baseline.txt gana las 25 funciones de gate-dag.py y gate_usl.py sobre CC 8 (la línea base dice la verdad; partirlas sigue siendo trabajo del mejorador). Anotado: --update-lizard-baseline solo mide rust/src y borraría estas líneas; scripts/mcp_live_session_test.py::main tiene CC 44 y no está.
  - 25 funciones sobre CC 8 (13 en `gate-dag.py`, 12 en `gate_usl.py`: `simular` 19, `admission` 15, `parse_row` 15, `plan_problem` 14…).
  - `scripts/lizard-baseline.txt` no tiene ninguna línea `scripts/gate*`.
  - Según el lector de E1–E9, `quality-gate` saldría con 2 si su diff las incluye (`quality-gate.sh:3999`).
- [ ] **25.** **(H)** Deuda «para el mejorador» sin tarea. 60–90 / 2.
  - [ ] **25a.** El camino crítico está en tres sitios (`gate-dag.py:844`, `gate_history.py:718`, `gate_usl.py:697`; D-E7g).
  - [ ] **25b.** `planned_pool` (`gate-dag.py:562`) copia la regla de `resources.rs` (B:1133).
  - [ ] **25c.** La pasada del mejorador de F2a/F2b nunca se hizo (B:478).
- [ ] **26.** **(H)** `pasos.jsonl` no lleva `jobs` por paso (D-F3s). Menor. 15 / 1. — **EN CURSO 10-nov 18:49:** CI-E28: pasos.jsonl lleva jobs (los que el paso recibió, o null); falta el juez.
- [ ] **27.** **(H)** Hueco en `validar-handoff.sh:567-571`: una nota del implementador con `commit: none` pasa sin coste ni `written`/`frozen` (D-E6e). Así pasaron E13, E14, E15, E18 y E21. 40 / 1. — **EN CURSO 10-nov 18:49:** CI-E28: una nota del implementador a otro rol que nombra tests lleva segundos:. Los cinco traspasos de hoy (E13, E14, E15, E18, E21) dejan de pasar hasta que tengan su línea con los segundos medidos: se miden en la cadena de la noche.
- [ ] **28.** **R-A1-3** · stdout atascado: `runtime.shutdown_timeout` (B:1055; E10.spec:387). Sin identificador y fuera de B:1212. Prioridad baja. 30 / 2.
- [ ] **29.** **M0 y A2** del segundo perfil del camino crítico. B:1047, 1049. Fuera de B:1212. 30 + 45 / 5.
  - [ ] **29a.** M0: hora por sección dentro de cada fila (`$EPOCHREALTIME`).
  - [ ] **29b.** A2: carriles dentro de E2E (−17 a −19 s); espera a la medida de M0.
- [ ] **30.** **(H)** S2 de E11: enseñar a `gate-audit.sh` los ficheros de cargo entre lectores (E11.spec:488). Depende del punto 41. 20 / 1.
- [ ] **31.** S3 de E11: la sonda `claude -p` de D3 (hasta 60 s) sigue esperando a la red (B:1047). Prioridad baja.
- [ ] **32.** Tabla de clases versionada para `--compare-classes` (B:308). Hoy la referencia de F4 vive en `~/.cache/memory-industry-gate/f0/` (punto 74). 30 / 0.
- [ ] **33.** Tres comentarios falsos o viejos. 10 / 0. — **EN CURSO 10-nov 18:49:** CI-E28: corregidos test-time-ceilings.txt (v025: 10,32 → 1,29 s) y local_gate_contract.rs ≈2076; common/mod.rs ≈254 ya estaba bien.
  - [ ] **33a.** **(H)** `scripts/test-time-ceilings.txt:87` dice «pending» para v025 y el número existe (1,29 s, B:1138). Toca `scripts/`: va con el próximo commit que ya pida `todo`.
  - [ ] **33b.** `rust/tests/local_gate_contract.rs:2076-2079` («against the live cluster»).
  - [ ] **33c.** `rust/tests/common/mod.rs:254-256` (F2.spec:495).
- [ ] **34.** **(H)** «12 GB por copia» en `mutants-gate.sh:257-263` es la cifra de Windows (D-F4k; `docs/gate.md:165`). Medir en 20 / 3, o descartarlo por escrito.
- [ ] **35.** Tareas de producto para después de la 0.31 que no existen en ningún backlog (5 cada una para abrirlas).
  - [ ] **35a.** La conexión LISTEN de `protocol.rs` sin ámbito RLS (B:865).
  - [ ] **35b.** **(H)** La reserva de `rerank.rs:51` sigue con `wants_gpu` (F3.4.spec:114; B:855).
  - [ ] **35c.** **(H)** CI-K3c: los 59 ficheros de D2 a base desechable (K3.spec:463).
  - [ ] **35d.** K6e (OnceLock por guion, K6.spec:19), Q2-e y Q1-f (F6.spec:126, 131).
- [ ] **36.** **T7 y T6**, después de la 0.31.
  - [ ] **36a.** T7, flujos de GitHub (B:950). La spec de K7 le añade: el bloque muerto de OpenSSL en `publish.yml:200-242`, los pasos `--features docs` duplicados en `ci.yml:94-101`, y `dtolnay/rust-toolchain@stable` con el fichero de toolchain presente.
  - [ ] **36b.** T6, crate `search-core` (B:952).

## (B) Verificaciones y medidas por correr

- [ ] **37.** **Dos vueltas seguidas del `todo` sobre el SHA final** (`verificacion.md`; P:420; F3.spec:803). El par verde es de `0bd4421`; después cambió `scripts/` (`e6a68f9`, `a28d946`) y sobre `5bec0b6` hay una sola corrida (565 s). El SHA vuelve a cambiar con E12, E16 y E26: va al final. 5 / 2×10 compartida.
- [ ] **38.** **Blindaje de esta noche** (B:1114, 1212). — **EN CURSO 10-nov 19:00:** cadena de la noche escrita y revisada (noche/: noche.sh, sabotajes.sh, ab.sh, medidas.py, 56 parches que aplican limpios con git apply --check, 13 filas sin parche con su porqué). Bloque 1 exclusivo a las ~19:45; bloque 2 tras las 3 h de Mapupita.
  - [ ] **38a.** 16 sabotajes de CI-E10: la fase `c` de `CR/medir-e10.sh` no ha corrido nunca. 60–70 compartida.
  - [ ] **38b.** **(H)** S-A1-4, S-B2-3 y la mitad E2E de S-A1-1 no los cubre el guion. 15 / 10.
  - [ ] **38c.** 4 sabotajes de CI-E18 «razonados, no corridos» (B:1157). 10 / 5.
  - [ ] **38d.** **(H)** 3 sabotajes de CI-E21 (`ci-rapido-E21.yml`), fuera de la lista. 15 / 5.
  - [ ] **38e.** **(H)** Sabotajes reales de E11 (S6b, S8, S14, S15) y el escenario E11-16. 15 / 20–30.
  - [ ] **38f.** **(H)** Sabotajes de CI-E1 en el árbol real (corridas A y B, S-R4, S-C1–5); hay que rederivarlos tras E11. 20 / 15–20.
  - [ ] **38g.** Antes y después de CI-E10 con A/B intercalados (B:1143). 5 / 15–20 en exclusiva.
  - [ ] **38h.** ABBA serie/DAG (B:1005; F3.spec:854). 10 / unos 50 en exclusiva.
    - **(H)** La etiqueta `juez-0.31-serie` de P:359 no existe.
    - `CR/campana-final.sh:184` no calcula el IC95 por bootstrap ni el n de P:418.
  - [ ] **38i.** Humo de quality-gate (`--study full`, 8–15) y estudio Q2 (`QG_BASE=v0.30.0 … --study legacy`, 3,5–4,5 h en exclusiva, tope de 6 h). B:895, 1005.
    - B:1212 omite el estudio.
    - La spec pide más de 600 GB libres (562 en la foto).
    - Si la referencia `legacy` sale roja, es trabajo de endurecedor antes de publicar.
    - Q2 sigue sin adoptar: el modo por omisión es `full`.
- [ ] **39.** **Barridos USL.**
  - [ ] **39a.** `debug` con la máquina quieta: unos 29 en exclusiva (B:1066).
  - [ ] **39b.** **(H)** `release` nunca se barrió y ahora hace falta (B3 con 0 % de holgura; `campana-final.sh:125` solo llama a `debug`): 45–60 en exclusiva.
- [ ] **40.** **Six-pack final** (B:253, 278, 863): solo consta el arquitecto (B:1027).
  - [ ] **40a.** Endurecedor: `quality-gate` (CRAP y mutación del diff) no ha corrido sobre nada del lote. `in-diff` da 0 apariciones en el backlog.
  - [ ] **40b.** Los 12 sabotajes a mano de bash de `ci-rapido-J-arquitecto.yml` no constan corridos, y 3 de sus anclas quedaron viejas. Re-anclar 15; correrlos 10–15 con `nice`.
  - [ ] **40c.** La nota de B:907 sigue viva: `quality-gate.sh:3534` excluye `setup.rs log` y v047 ya lo juzga.
  - [ ] **40d.** qa: los procedimientos §5 de cada spec.
- [ ] **41.** **(H)** **`gate-audit.sh --run` sobre el juez real** no se ha corrido nunca, y el DAG ya es el valor por omisión (F3.spec:780-801; F2.spec:451; E11.spec §7.5). 30–45 / 25–50 en exclusiva.
  - Candidato conocido: `~/.cache/memory-industry/{audit_key,pgpass_app}` creados a la vez por dos filas en una máquina limpia (B:1062).
  - También confirma que NLI no conecta a Postgres, que hace falta para C2 (punto 87).
- [ ] **42.** **(H)** **Corpus de rojos de referencia** (11 del plan + 3 de F3): mismo paso rojo, subcadena y código en serie y en DAG. P:354-357, 414; F3.spec:825. 0 apariciones en el backlog. 60–90 / 60–90 compartida.
- [ ] **43.** **(H)** **Inventario `--list` global**, juez viejo (F0) ↔ serie ↔ DAG, en los dos sentidos. P:413; F3.spec:769. Solo hay inventarios por tajada. Sale de logs: 30–40 / 0.
- [ ] **44.** **(H)** Pruebas de precisión de F3 §6 pasos 10–11.
  - [ ] **44a.** κ por binario ≤ 1,35 (F3.spec:846): en su lugar se subieron techos. 20–30 / 0, desde logs.
  - [ ] **44b.** sha256 del release igual en serie y en DAG: 10, con la ABBA (38h).
  - [ ] **44c.** F1.4, sha256 igual con `-j3` y `-jN` (P:292; J-F1.spec:421): 5 / 10–15 en exclusiva.
- [ ] **45.** **(H)** Sabotajes manuales en el árbol real. 15 / 10–20.
  - [ ] **45a.** `touch main.rs` antes de B3g y `cp /bin/true` sobre el binario (J-F1.spec:489); hay que re-anclarlos al sha cruzado de F3.4.
  - [ ] **45b.** `GATE_PG_BINDIR=/nonexistent` y la raíz de clústeres en disco (F2.spec:442).
  - [ ] **45c.** R3: NLI como `normal` con M viva (F3.spec:738). Sin objeto si se aprueba C2 (punto 87).
- [ ] **46.** **CI-K2** · criterios de cierre de `ci-minuto-uno` (segunda orden con 0 crates propios; juez tras 2 h parado en menos de 30 s). B:923. `campana-final.sh:151` hace `touch lib.rs` antes de cada juez, así que no puede medirlos. 10 / 5 más la espera.
- [ ] **47.** **(H)** Medidas «de campaña» que no tienen fase en `campana-final.sh`. Caben en una cadena exclusiva de 45–60, más 15 compartida.
  - [ ] **47a.** K3: `--timings` antes y después (K3.spec:399; B:936 «el ahorro no se afirma hasta medirlo»). 10–15 en exclusiva. Necesita `scratch/antes-k3` (punto 70).
  - [ ] **47b.** K4-M3: A/B del incremental (K4.spec:83). 20–30 en exclusiva.
  - [ ] **47c.** K7: compilación en frío con y sin, las cifras 237 → 136 s (B:963). 10–15 en exclusiva.
  - [ ] **47d.** K8: `--list` con y sin docs (K7.spec:462). 3–5.
  - [ ] **47e.** K4-M8/M9: rojo sembrado en P en menos de 30 s, en serie y en DAG (K4.spec:339). 2–3.
  - [ ] **47f.** K7b: `tokens =` de `nli_probe` antes y después (K7.spec:229). 6–10 compartida.
  - [ ] **47g.** K7 §4.4: `cargo check` de `--no-default-features --features cuda`, de `docs,cuda,directml` y `cargo xwin check` (K7.spec:428). 5–10.
- [ ] **48.** **(H)** F2: dos comprobaciones que necesitan un contenedor de la imagen vivo, así que van antes del punto 76. 25 / 5.
  - [ ] **48a.** La sonda de `wal_level` y `FILE_COPY` (F2.spec:379), o cerrarla por escrito (62i).
  - [ ] **48b.** La intercalación entre glibc 2.36 y 2.43 (F2.spec:427).
- [ ] **49.** **(H)** Dos decisiones de CI-E7 con el disparador ya cumplido y sin revisar (B:1031). Los picos por fila existen (M 15,2 GiB, B3 5,0). Decidir: 10 / 0. Si se barre: 30–40 / 40–80 en exclusiva.
  - [ ] **49a.** El `CARGO_BUILD_JOBS` de `pesado` como del operador.
  - [ ] **49b.** El barrido de concurrencia.
- [ ] **50.** **(H)** Línea base de CI-E1 sin el cruce de tres fuentes (E1.spec §7.3): salió de una corrida con rojos y `lib` da 1220 frente a 1198 sin explicar. 15 / 0.
- [ ] **51.** Techos. 15 / 0, desde logs.
  - [ ] **51a.** Apretar con k = 1,3 «tras la tanda» (B:1149).
  - [ ] **51b.** Volver a fijar `v016` y `v017_relation_scan` con más corridas (texto de sus líneas).
- [ ] **52.** Sin reproducir ni causa.
  - [ ] **52a.** `gate-cluster.sh --self-test` rc=1 con carga (B:1076, 1090).
  - [ ] **52b.** El directorio temporal perdido de F6c.3 (B:999, 1027).
  - [ ] **52c.** La causa raíz de CI-E21 (B:1178; depende del punto 6).
  - [ ] **52d.** El `cgroup.kill` real con `comm` de CI-E15, solo ejercitado con fixture (B:1176).
- [ ] **53.** Menores de E10. 10 / 0.
  - [ ] **53a.** La cuenta de la fila E2E contra los 180,7 s del 8-nov (E10.spec §6).
  - [ ] **53b.** El «antes» de v09 (5,04 s frente a los 9,64 de la spec) sin explicar.

## (C) Excepciones con dueño y caducidad

- [ ] **54.** **`scripts/advisory-exceptions.txt`** (espejos en `rust/.cargo/audit.toml` y `rust/deny.toml:53-56`): tres líneas de `orquestador-0.31` que caducan el **2026-11-30**; el día siguiente el juez da rojo en P. Se retiran subiendo sqlx (punto 86) o renovando las tres en los tres ficheros.
  - [ ] **54a.** RUSTSEC-2023-0071 (`rsa`, por `sqlx-mysql`).
  - [ ] **54b.** RUSTSEC-2024-0436 (`paste`, por `tokenizers` y `nalgebra`).
  - [ ] **54c.** RUSTSEC-2026-0459 (`libsqlite3-sys`, por `sqlx-sqlite`).
- [ ] **55.** **`scripts/test-time-ceilings.txt`**: 22 líneas (10 de windows, 12 de linux), todas de `orquestador-0.31` y con caducidad **2026-11-30**.
  - [ ] **55a.** Linux: las subidas del 10-nov (`local_gate_contract` 142, `v017_rerank_gpu` 127, `v046` 16, `v017_relation_scan` 30, `v016` 40 como cota) son subidas de presupuesto; las baja CI-E12 (punto 3).
  - [ ] **55b.** Windows: siguen sobre dos pasadas (puente 52/399; `local_gate_contract` 58/217 con rojo probable por suelo). Las rehace la medida en Windows (punto 84).
- [ ] **56.** **`scripts/handoffs-sin-coste.txt`**: 14 líneas, caducidad **2026-12-31**, dueño `orquestador`. 15 / 2.
  - Nada se pone rojo al caducar: el contrato compara con el literal, no con hoy (`local_gate_contract.rs:1869-1875`).
  - Para retirarlas: borrar las 14 y bajar `HANDOFFS_SIN_COSTE` a 0 en una pasada de tests.
- [ ] **57.** Dos excepciones dentro de `rust/tests/local_gate_contract.rs`: `orquestador`, **2027-02-28**.
  - [ ] **57a.** `:3757` `NOT_HANDED_DOWN`: `TMPDIR`. Se retira con el punto 22.
  - [ ] **57b.** `:4036` `MODEL_NAMES_ELSEWHERE`: `demo.sh`.
- [ ] **58.** Sin fecha, a revisar: `scripts/gate-latency.txt` (14 líneas, 0 `pendiente`, nada que caduque) y `scripts/lizard-baseline.txt` (punto 24). No existe fichero de excepciones de mutantes; la exclusión vive en `quality-gate.sh:296` y `:3534` (40c).

## (D) Documentación, backlog, Notion y memoria

- [ ] **59.** **Notion**: la última sincronización anotada es del 7-nov 12:19 (B:841). Pendiente todo lo posterior (B:857, 1003, 1009, 1212). 60–90 con conector (punto 91).
  - [ ] **59a.** Tareas: CI-F2, F3, F5, F6 y F7; J12–J19; K1–K9; E1–E26; T-DBURL.
  - [ ] **59b.** Las decisiones de Leandro del 9 y 10-nov; bugs y lecciones; el «Registro de mejoras y tiempos»; la metodología de escala.
  - [ ] **59c.** Las filas de Decisiones D-F2a/g/l (F2.spec:424) y D-F4m…v (F4 d.4 paso 5).
  - [ ] **59d.** La auditoría backlog ↔ Notion antes de decir «terminado» (P:422).
- [ ] **60.** **Backlog**. 15 / 0.
  - [ ] **60a.** La constancia de todo lo de «Trabajo en curso» (17:14–18:02) y de P-1. Tras la foto existe `a0dea6c` (`docs(backlog)`: la orden de cerrar todo hoy, tres tajadas entregadas, `pesado`); comprobar que no falta nada.
  - [ ] **60b.** La tabla del lote está vieja (B:38 dice `wal_level=minimal` y el código usa `replica`).
- [x] **61.** **(H)** **«La lista del release» no existe.** B:1027 y B:1039 mandan ahí H7 y la medida en Windows; no hay tal fichero. Debe recoger los puntos 10, 54, 55, 84, 85, 86, 88 y 89. 15 / 0. — **HECHO 10-nov 18:23:** creada backlog/lista-del-release-0.31.md (15 puntos con casilla, quién, fuente y comando).
- [ ] **62.** **Cierres por escrito**: decisiones cuyo dato ya existe y solo falta anotar. 30 / 0. — **EN CURSO 10-nov 18:23:** 11 cierres escritos en la bitácora (entrada de las 18:19); sigue abierta la sonda de wal_level (48a, opcional) y quedan para Leandro: adoptar juez-cuda (ahorro tope 5-10 s) y T2.
  - [ ] **62a.** F3.5, lanzador por binario (B:863): la condición no se cumple con lo medido (162 s de holgura).
  - [ ] **62b.** **(H)** F5b `juez-cuda`: `recomendar` dice «yes (B3 slack 0.0 s)» desde las corridas de las 15:27 (`CR/campana-final-20261110-152701/usl-recomendar-final.log:18`) y B:1128 sigue con «no». Los `--timings` dicen que ganaría como mucho 7–8 s; lo razonable es anotarlo y cerrarla.
  - [ ] **62c.** **(H)** T2: la unidad `cuba-memorys` cuesta 100,8 s de 167,9 (60 %), así que cumple el criterio de K7.spec:403 para llevárselo a Leandro (92b); el ahorro de reloj sería casi nulo.
  - [ ] **62d.** **(H)** A2 de F3 (diagnóstico con factor 2,0: ya no aplica; F3.spec:802).
  - [ ] **62e.** **(H)** O4 de F4 (línea base sobre el 10 % del paso: sigue descartada; F4.spec:86).
  - [ ] **62f.** **(H)** Monte Carlo (menos de 20 corridas) y `nice` por criticidad (F3.spec:159).
  - [ ] **62g.** **(H)** `RUST_TEST_THREADS` y el `wait` sin plazo de `on_exit` (J14.spec:32, 55).
  - [ ] **62h.** **(H)** R5 de E11 (shmem 0,21 GiB con cuatro clústeres en tmpfs).
  - [ ] **62i.** **(H)** `wal_level` (se queda `replica`; ver 48a y 60b).
  - [ ] **62j.** La spec de E10 nombra `lock_timeout_sql` y el código `lock_timeout_millis` (B:1100).
  - [ ] **62k.** B:863 («el resto como tareas del backlog de Mapupita») contradice D-F7d (F7.spec:137).
- [x] **63.** **`docs/gate.md`** (lo lee el juez: va en un commit antes del veredicto final). 20 / 0. — **HECHO 10-nov 18:23:** docs/gate.md: hora de la auditoría decidida, los dos bins con test = false, juez de Windows una vez por release con la comprobación de Python de CI-E25b.
  - [ ] **63a.** `:263` dice que el temporizador está «pending the hours», y ya está decidido (domingo 02:00).
  - [ ] **63b.** **(H)** No dice que los dos bins llevan `test = false` ni que el aviso de cargo sigue (D-F5y; F5.spec:160).
  - [ ] **63c.** **(H)** Ni `gate.md` ni `AGENTS.md` dicen que el juez de Windows corre una vez por release, antes de `release.sh` (P:377 «así queda»).
- [x] **64.** **`AGENTS.md`**. 10 / 0. — **HECHO 10-nov 18:23:** AGENTS.md: clústeres propios del juez (F2), fixture sembrado por MCP, coste en los traspasos (CI-E6), guardas hechas y abiertas, juez de Windows una vez por release.
  - [ ] **64a.** **(H)** Sin la parte de F2 (clústeres propios, PGDG; F2.spec:496; B:843).
  - [ ] **64b.** `:67` dice «a fixed snapshot restored» y lo real es un fixture sembrado por MCP (D-J3a).
  - [ ] **64c.** Sin la frase de CI-E6 en «Six-pack» (`docs/gate.md:585` sí la tiene).
  - [ ] **64d.** «missing guards (CI-E1 to CI-E8)» ya es falso.
- [x] **65.** **(H)** **CHANGELOG `[0.31.0]`**: no menciona T-RLS (el arreglo de seguridad del producto, B:746), ni `MEMORY_INDUSTRY_REQUIRE_DATABASE_URL` (T-DBURL; el README sí, `:367`), ni el juez en DAG. Sí están E10, K7, K7b y K8. 15 / 0. — **HECHO 10-nov 18:23:** CHANGELOG [0.31.0]: T-RLS (fbe6632), MEMORY_INDUSTRY_REQUIRE_DATABASE_URL (5314a6d) y el juez en DAG por omisión. Anotado que tampoco nombra F2, CI-K3 ni CI-E4/E6/E7a (son del juez, no del producto: no se añaden).
- [x] **66.** **Locales, sin versionar**. 20 / 0. — **HECHO 10-nov 18:23:** sección «Estado al 10-nov-2026» añadida a los dos ficheros locales, sin borrar nada.
  - [ ] **66a.** `docs/ci/metodologia-de-escala.md`: las tablas de `:185-209` siguen dando por «Missing» E1, E4, E6, E7 y E8, y por `pendiente` 13 líneas. Siguen sin medir: recuento y tabla por binario del árbol actual, `quality` entero, ahorro de K3 y K8, K9 en máquina sana y la mediana de 10 corridas. Queda una «Decision pending» para Leandro (93a).
  - [ ] **66b.** `docs/ci/plantilla-aplicada.md` es del 9-nov y dice «does not» para lo ya hecho (K4.spec:342 y K3.spec:447 mandan pegar ahí los números).
- [ ] **67.** **Traspasos**. 15–30 / 3 para los segundos.
  - [ ] **67a.** E13, E14, E15, E18 y E21 son notas con `commit: none`; tres dicen `segundos: no medido`. Reescribirlos como `git_handoff` con su commit rojo.
  - [ ] **67b.** No hay `.yml` de implementador de E1, E4, E6, E7, E9, E10, E11, J18, las tandas 5–9 ni E24; si existieron, se perdieron al cerrar árboles (la carpeta no se versiona).
  - [ ] **67c.** 14 specs no tienen `.yml` (F5, F6, F7, F3.4, J13, J14, J15, K1, K3, K4, K6, K7, K9, E7).
  - [ ] **67d.** CI-J llega a arquitecto→endurecedor y nadie lo consumió (ver 40b).
  - [ ] **67e.** La spec de T-DBURL ya no está en disco (su árbol `Memorys-dburl` se cerró).
- [ ] **68.** **(H)** **F7 no consta entregado** (F7.spec:148-152). 45–60 / 0.
  - [ ] **68a.** L2: entrada fechada con la matriz.
  - [ ] **68b.** L3: tarea CI-F7 en Notion.
  - [ ] **68c.** L4: recetas R1–R11 en la plantilla de CI y corrección de `wal_level` (Notion y `~/proyectos/mapupita/_ci-tiempos/PLANTILLA-ci-y-maquina-para-pegar.md`).
  - [ ] **68d.** L5: fila de Conocimiento de Mapupita, M1–M6, H1 (los `AGENTS.md` de Mapupita siguen diciendo «nativo 16.15 en :5575») y H3 (TASK-260.6).
  - [ ] **68e.** L6: `memory_receta`.
- [ ] **69.** **Memoria**: los decretos del diseño y del DAG por omisión sí existen (`e95550b5`, `f5740bf0`). 15 / 0.
  - [ ] **69a.** P-1 resuelto (la observación `03511fa4` dice «pendiente»).
  - [ ] **69b.** El decreto D-F2a/g/l (F2.spec:467).
  - [ ] **69c.** La receta del punto 68e.
  - [ ] **69d.** El cierre de jornada.

## (E) Limpieza de la máquina

- [ ] **70.** **Árboles del lote** (de 50 en total).
  - [ ] **70a.** `Memorys-ci-m10-antes` y `-m10-despues`: cerrar tras 38a y 38g.
  - [ ] **70b.** `Memorys-ci-todo10` (`ac33343`, superado; su rama no está fusionada).
  - [ ] **70c.** `Memorys-ci-antesk3` (`scratch/antes-k3`; se necesita para 47a y nunca se fusiona).
  - [ ] **70d.** e12, e16 y e26: al integrar.
  - [ ] **70e.** `Memorys-k7` (`c578fef`, ya dentro de `feat/ci-rapido`, limpio): se puede cerrar.
  - `Memorys-ci-medir` es el árbol del juez: se queda.
  - No son del lote y no se tocan: los 18 `feat/0.29-*`, los 13 `agent-*`, `Memorys-release-lf` y `wt-gif2` (prunable).
- [ ] **71.** **Ramas**: 71 `feat/ci-*` y 5 `fix/*` ya fusionadas en `feat/ci-rapido`; 2 sin fusionar a propósito (`feat/ci-rapido-todo10`, `scratch/antes-k3`). 5 / 0.
- [ ] **72.** **Stash**: tres `ci-j-pasada2*-wip-20261106` (código ya en commit; la guarda no deja hacer `drop`, B:252) y uno ajeno de `feat/0.28-seguimientos`. Ver punto 85. — **NOTA 10-nov 18:17:** autorizado por Leandro, pero la guarda bloquea ese borrado: pendiente de que lo corra él; el stash ajeno (feat/0.28-seguimientos) no se toca.
- [x] **73.** **`~/.cache/memory-industry-gate/tmp`**: 785 entradas, 5,5 GB. CI-E15 solo poda `memory-industry-dag.*` de más de 7 días; el resto es el borrado único del punto 85. — **HECHO 10-nov 18:17:** borradas 784 entradas (las 776 de más de 24 h, entre ellas mtime-exp.zTha, y ocho directorios de corridas de hoy ya terminadas, comprobando que su planner.pid no vive); quedan 2 (la corrida roja de las 15:27, 949 MB, evidencia de CI-E21, y la corrida viva); 5,6 GB → 0,9 GB. Autorizado por Leandro (AskUserQuestion: «Sí, bórralo todo»).
  - `mtime-exp.zTha`, 4,0 GB.
  - `memory-industry-dag.2dukuzwn`, 882 MB (corrida roja de las 15:27, conservada) y otras 14 `memory-industry-dag.*`, entre ellas la corrida parada de las 15:56 con pid muerto.
  - 578 entradas del 7-nov y 195 de octubre.
- [ ] **74.** **`CR`** (518 entradas, 77 MB: 107 `.sh`, 180 `.log`, 49 `.patch`, 44 carpetas), más `f0/` (61 MB) y `medidas/` (1,1 GB). No borrar todavía.
  - Los motivos de `test-time-ceilings.txt` y de `gate-latency.txt` citan por nombre `campana-final-20261110-*` y `veredicto-final-*`.
  - `f0/` guarda el `outcomes.json` de referencia de F4 (punto 32).
- [ ] **75.** **Compilación**.
  - [ ] **75a.** **(H)** `~/.cache/cargo-target/memory-industry`, 46 GB: T_dbg 19 GB y T_docs 17 GB llevan restos anteriores a K4, que pedía ≤ 8 cada uno (K4-M2, K4.spec:120); borrarlos libera unos 30 GB y cuesta una compilación en frío. Hay además 5,4 GB de `debug`, `release` y `llvm-cov-target` viejos, sin árbol. — **NOTA 10-nov 18:17:** los restos de T_dbg y T_docs (unos 30 GB) se borran antes de la última corrida de la noche, con el juez parado: cuesta una compilación en frío.
  - [ ] **75b.** `~/.cache/cargo-target/memory-industry-verif`, 15 GB.
  - [x] **75c.** `Memorys-ci/rust/target`, 2,0 GB (del 6-nov). — **HECHO 10-nov 18:17:** borrado (2,0 GB), junto con debug, release y llvm-cov-target de ~/.cache/cargo-target/memory-industry (5,4 GB sin árbol).
- [ ] **76.** **Contenedores** (D-F2m, B:843). Antes, el punto 48 si se hace. 10 / 0. — **EN CURSO 10-nov 18:17:** contenedor memory-industry-gate-20261028 retirado (autorizado por Leandro); su volumen anónimo 826ec86f… queda huérfano: la guarda bloquea el borrado de volúmenes, lo corre Leandro. memory-industry-gate-20261106 (:5490, Up) sigue: decisión pendiente (docs/gate.md lo usa de ejemplo de servidor externo).
  - [ ] **76a.** `memory-industry-gate-20261028` (:5489, `Exited (255)`) se retira; la condición de «dos verdes» está cumplida.
  - [ ] **76b.** `memory-industry-gate-20261106` (:5490, `Up`, `unless-stopped`) no lo usa ya el juez en Linux; decidir si se queda como servidor externo (`docs/gate.md:92` lo usa de ejemplo).
- [ ] **77.** **systemd de usuario**: ninguna unidad `mi-*` viva. Temporizadores: solo `cuidar-disco` y `respaldo-memoria`. `/tmp/memory-industry-gate-1000` está vacío. Nada que limpiar; volver a mirar al cerrar.
- [ ] **78.** `~/.cache/memory-industry-gate/mutants-seed`: 2 claves, 2,0 GB, correcto. Hay 13 copias `pesado.antes-*` en `~/.local/bin` y 5 de la batería en `~/herramientas`: las del 4–5 nov se pueden podar; las del 9–10 nov son la vuelta atrás documentada.

## (F) Herramientas fuera del repo

- [ ] **79.** **`pesado` sin probar en real**.
  - [ ] **79a.** Carga en los dos turnos y poda contra la puerta (B:1013).
  - [ ] **79b.** El adelantamiento contra la cola real y su guarda «razonada, sin test» (B:1080).
  - [ ] **79c.** El monitor de corridas solo lee el turno 1 (lo mantiene Mapupita; B:1013).
- [x] **80.** **P-1** · si muere el `pesado` que tiene el turno, su scope muere con él (B:1182, 1212).
  - **HECHO (10-nov 18:01):** batería `~/herramientas/pesado.test.sh` con 7 filas (k), 193 en verde; rojo en 2 con la versión anterior; `maquina-compartida.md` actualizada; scope `run-pesado-<pid>-<epoch>`.
  - Lo que había en la foto: el arreglo instalado a las 17:43 (`~/.local/bin/pesado:512-530`; copia `pesado.antes-scope-20261110`), sin fila en la batería ni regla.
  - Quedan fuera de esta casilla la memoria (69a) y el backlog (60a).
- [ ] **81.** **Unidades `qg-auditoria-semanal.service/.timer`** (domingo 02:00): no están instaladas (comprobado). B:903 las deja para después de la 0.31, porque lanzan el guion del clon principal. La primera corrida necesita `QG_AUDIT_BASE` (`scripts/qg-weekly-audit.sh:9-11`) y será la que mida por primera vez el reloj de Q1. 15 / la auditoría, en exclusiva.
- [ ] **82.** **(H)** `~/herramientas/cuidar-disco.sh` y `vigilar-disco.sh` no conocen `~/.cache/cargo-target/memory-industry*` ni `~/.cache/memory-industry-gate` (P:65; J-F1.spec:64; F3.spec:911; F6.spec:516). 20 / 0, o descartarlo por escrito; es cambio de sistema y va a memoria.
- [ ] **83.** **Reglas y guiones**.
  - [ ] **83a.** `~/.claude/agents/{especificador,implementador}.md` de WSL difieren de los de `/mnt/c/Users/emman/.claude/agents/` (deriva anotada en B:1039, sin arreglar).
  - [ ] **83b.** Los guiones de medida viven solo en la caché. A `campana-final.sh` le faltan fases para los puntos 39, 41, 42, 46 y 47 y para el IC95; `medir-e11.sh` marca rojo `cargo test --doc` con 0 tests (B:1098).

## (G) Solo Leandro

- [ ] **84.** **Medida del juez en Windows antes de publicar la 0.31** (B:1039, 1212): dos corridas, que lanza él.
  - [ ] **84a.** **(H)** Antes: recrear el contenedor `memory-industry-dbwin-*` con `-c cluster_name=memory-industry-gate`, o la guarda sale con 2 (J-F1.spec:514; el backlog no lo recoge).
  - [ ] **84b.** Primera corrida, de medida (saldrá roja en techos y recuentos).
  - [ ] **84c.** Escribir las líneas `plain/`, `contracts/` y `doc/windows/*` (hoy 0), rehacer los techos de windows (55b) y cerrar CI-E25 (punto 10) y CI-E25b.
  - [ ] **84d.** Segunda corrida, el veredicto.
- [ ] **85.** **Borrado único**.
  - [x] **85a.** `mtime-exp.zTha` (4,0 GB) y lo anterior al 8-nov de esa carpeta (B:1168, 1212). — **HECHO 10-nov 18:17:** hecho con el punto 73.
  - [ ] **85b.** El `drop` de los tres stashes del punto 72. — **NOTA 10-nov 18:17:** la guarda de comandos bloquea el borrado de stashes también para el orquestador: lo corre Leandro (comando en la lista para él).
- [ ] **86.** **Subir sqlx** para salir de RUSTSEC-2026-0459 (y de 2023-0071) antes del 30-nov, o aceptar la renovación (B:1121, 1212). Es dependencia de producto: socket y osv antes. — **NOTA 10-nov 19:22:** decidido por Leandro: subir sqlx va tras la 0.31 como tarea propia; si la 0.31 no ha salido el 30-nov, las tres excepciones se renuevan con fecha y motivo.
- [ ] **87.** Decisiones que cambian lo que se verifica (B:1047, 1135, 1212). — **NOTA 10-nov 19:22:** decidido por el agente (se informó a Leandro): NLI se queda exclusiva.
  - [ ] **87a.** **C2 · NLI deja de ser exclusiva** (−18 s): las corridas DAG que pedía E11 ya existen (NLI esperó 301,7 s y 147,7 s).
  - [ ] **87b.** **A3**: grupos del E2E en paralelo con bases clonadas.
- [ ] **88.** **(H)** Dos decisiones de E16 que no están en la lista de B:1212.
  - [ ] **88a.** **D-E16b**: un modelo configurado que no abre deja de arrancar (B:1159 «a revisar por Leandro»; E16.spec:92).
  - [ ] **88b.** **H3 de E16**: el reranker de stdio se calienta en fondo (`protocol.rs:476-490`).
- [ ] **89.** **Integrar y publicar** (P:375): `feat/ci-rapido` iba 500 commits por delante de `release/0.31.0` (`fbe6632`) en la foto, no tiene rama remota y no está integrada. Empujar y `release.sh v0.31.0` piden su orden. — **NOTA 10-nov 19:22:** decidido por Leandro: integrar feat/ci-rapido en release/0.31.0 y empujar las ramas, tras el veredicto final; no publica versión.
  - [ ] **89a.** **(H)** Decisión asociada: el producto del lote (K7, K7b, K8a, T-DBURL, CI-E10, CI-E16) entró directo en la integración, sin el six-pack sobre `release/0.31.0` que B:901 prometía «como T-RLS».
  - [ ] **89b.** Integrar en `release/0.31.0`, veredicto con dos vueltas y recibo sobre ese SHA (punto 37), y `release.sh` con su orden.
- [ ] **90.** **Publish en seco** (`cross` y maturin con `rust-toolchain.toml`, B:934). `cargo xwin` baja el SDK de MSVC (licencia; 47g).
- [ ] **91.** Acciones de su cuenta o de Windows.
  - [ ] **91a.** **Notion**: el conector no está conectado en la sesión; autenticarlo es suyo (punto 59).
  - [ ] **91b.** Sincronizar las reglas de Cursor desde Windows (B:1013, 1039).
- [ ] **92.** Preguntas de producto y de publicación.
  - [ ] **92a.** Los números de CI-E4 en el recibo, que se copian a cada tag (B:1033).
  - [ ] **92b.** T2: quitar o compartir el segundo bin (62c).
  - [ ] **92c.** NPM-OIDC: su tarea para después de la 0.31 (B:45, 344).
- [ ] **93.** Decisiones menores nunca lanzadas.
  - [ ] **93a.** No volver a correr un binario cuyas entradas no cambiaron (`metodologia-de-escala.md:209`).
  - [ ] **93b.** **(H)** Techos por máquina, con clave de host (K4.spec:303).
  - [ ] **93c.** **(H)** P1 de F2: la zona horaria del vivo (F2.spec:129).
  - [ ] **93d.** El rojo barato «juez real contra `:5488`» (J-F1.spec:515), que consulta el corpus vivo en lectura.
- [ ] **94.** **(H)** Propuesta a cargo-mutants de que su copia conserve el mtime (F4.spec:450): toca a un tercero.
- [ ] **95.** **(H)** Técnicas del §1 del plan marcadas ✅ o 🔧 sin ningún rastro en el backlog ni en el repo. Decide si se hacen o se descartan por escrito. — **NOTA 10-nov 19:22:** decidido por Leandro: hoy el modo rapido y el recibo de solo-docs (CI-E29: rapido barato + parcial; release.sh etiqueta el commit juzgado); Squawk, herramientas por hash y la batería de defectos, tareas para después de la 0.31. CI-E29 pasada 1 d992aed (43 escenarios; rojo visto en los tres autotests); pasada 2 en curso. — **NOTA 10-nov 19:42:** CI-E29 pasada 2 7af7c47 (los tres autotests en verde, también bajo las 33 variables del entorno del juez; contratos de Rust sin correr), fusionada 3dbacf4; documentos 1252471. Falta el juez (segunda corrida del bloque 1).
  - [ ] **95a.** Modo `rapido` de `como-el-ci.sh` (P:56; el guion solo tiene `todo|extra|quality|…`).
  - [ ] **95b.** Recibo atado a la huella del juez, con la lista blanca de solo-docs y el hook de parada (P:71).
  - [ ] **95c.** Squawk para las migraciones (P:79).
  - [ ] **95d.** Cadena de suministro: herramientas fijadas por sha, `global-min-publish-age` y control positivo del escáner (P:78; `deny.toml` sí tiene `[sources]`).
  - [ ] **95e.** Batería de defectos sembrados (P:75; es el punto 42).
- [ ] **96.** **(H)** Restos del plan sin cierre escrito.
  - [ ] **96a.** F0 corrida 2 (P:364 pedía dos corridas completas del juez viejo): B:330 dice que el contenedor viejo se quedaba para ella y nunca consta.
  - [ ] **96b.** Los cambios 11–18 de I0 (B:102: replay del LLM en Rust, reloj inyectado, Monte Carlo, aviso HNSW) solo se nombran una vez.

## Los huérfanos que más importan

- **Cambian el veredicto o lo dejan sin base**: 41 (auditoría strace nunca corrida con el DAG activo), 42 y 43 (corpus de rojos e inventario global), 37 (dos vueltas sobre el SHA final), 40 (endurecedor y qa), 89 (producto sin pasar por `release/0.31.0`).
- **Bloquean otras tareas**: 11 (`unidades.jsonl` no se escribe → CI-E2), 39 (barrido `release` → CI-E7b), 61 (no hay lista del release).
- **Caducan el 30-nov**: 54 y 55.
- **Lo ve el usuario final**: 65 (el CHANGELOG sin T-RLS ni T-DBURL), 20 (`build.rs`), 88 (D-E16b).
- **El resto**: 2, 5, 15, 16, 18, 21, 23–27, 30, 33–35, 38, 44, 45, 47–50, 62–64, 68, 75, 82, 84, 93–96.

## No determinado

- El estado real de Notion (sin conector): solo consta lo que el backlog dice haber subido.
- Qué contenían los diffs de e12, e16 y e26 en la foto: solo se miraron nombres de fichero. Que e26 llevara CI-E23 y CI-J19 se dedujo de ellos (confirmado después por los commits `2cc9606`/`d2292cc`).
- Si los árboles cerrados tenían traspasos de implementador, y la «deuda del revisor» de B:226 (no está en ningún fichero leído).
- Si alguien corrió `gate-audit.sh --run` o `quality-gate` fuera de lo que registra el backlog. No hay artefactos: ningún `study.tsv` ni `report.tsv`.
- El motivo exacto de las esperas de REL y DBG en el turno de 12 CPUs (el resumen del planificador no lo imprime).
- Si existen en memoria la receta R1–R11 y los decretos que piden F3.spec:875 y F3.4.spec:464.
- Si la sesión de Mapupita recibió H1 y H3 de F7.
- Los cuatro «no pude verificar» de E16; dos siguen abiertos (que el E2E exporte `ONNX_MODEL_PATH` al hijo; el truco de bytes de v045 en stdio).

## Lo que hubo que deducir porque no estaba escrito

- El repo no tiene `CLAUDE.md`; manda `AGENTS.md`. El backlog está en inglés hasta la línea ~800 y en español después, así que cada búsqueda necesita las dos lenguas.
- `docs/*` no se versiona (`.gitignore:78`, salvo `gate.md` y `despliegue-lan.md`), ni `.cursor/handoffs/`. La evidencia que citan los ficheros versionados vive en `~/.cache`, que se limpia a mano.
- El `grep` de esta máquina es ugrep y falla con contexto regex sobre líneas de 2500 caracteres; hace falta `grep -nF` o un ayudante en Python.
- «La lista del release» se nombra dos veces y no existe.
- `extra` es idéntico a `todo` en este repo (`scripts/como-el-ci.sh:5`), así que «las dos vueltas de extra» son dos `todo`.
- El recibo va por sha de HEAD (`~/.cache/cuba-gate/receipts/`): en la foto había recibo de `5bec0b6` y no de `397e162`, que solo añade backlog. Este repo no tiene lista blanca de solo-docs.

## Añadidos después de la foto

- [ ] **97.** **CI-E25b** · el planificador no se importa con un Python nativo de Windows.
  - `scripts/gate_history.py:29` hace `import fcntl` y `scripts/gate-dag.py:90` usa `signal.SIGHUP`; falta la lista de las demás llamadas solo-POSIX.
  - Se comprueba en la medida de Windows (punto 84) con `python3 -I -B scripts/gate-dag.py rows --table scripts/gate-dag.txt`.
  - Relacionado con CI-E25 (punto 10).
- [x] **98.** **CI-E27** · el remate del doble escalado de `gate_usl`. — **EN CURSO 10-nov 18:17:** CI-E27 commiteada eaa4651/f309e83 y fusionada (593d90f): efectivo(j) = share_jobs; con J_m ≥ 24 idéntico (81 combinaciones); falta el juez. — **HECHO 10-nov 18:40:** juez verde sobre 87346f6.
  - **EN CURSO (10-nov, tras la foto).**
