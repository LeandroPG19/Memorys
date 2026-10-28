# 0.32 B8 — La proyección a FalkorDB/Neo4j: ni inyección, ni fallos mudos

Especificación (rol especificador, Swarm Forge). Rama `feat/0.32-analisis-y-sota`, HEAD `196f9d7`.
No se compiló ni se corrió nada. Todo lo que sigue sale de leer `rust/src/graph_db.rs` entero y sus
llamantes, de `git log -S`, y de la documentación de FalkorDB y de su cliente Rust (Context7,
`/falkordb/docs`, `/falkordb/falkordb-rs`). Lo que no se pudo verificar lo dice con esas palabras.

## 0. Resumen en cinco líneas

1. `reconcile_stub` **ya no existe**: lo quitó `c53f473` (0.32 B6). Esa mitad de B8 está hecha; no se repite.
2. Hay **cinco funciones, siete plantillas** que arman Cypher con texto del usuario (`graph_db.rs:426-481`
   y `:498-509`), todas con `escape_cypher_str` (`:139-141`) y `format!`. El escapado es probablemente
   correcto para `'` y `\`, pero no hay una sola prueba de que no se pueda salir de la cadena, y la
   lectura tiene un agravante que convierte el riesgo en explotable (§1.2).
3. Los parámetros de Cypher **no son una barrera en el cable** para FalkorDB (§2.1): el valor viaja
   como literal en el prefijo `CYPHER p='…'` de la misma cadena. Sirven para que el cuerpo de la
   consulta sea una constante y el escapado viva en **una** función con prueba exhaustiva.
4. Los fallos de escritura se descartan en **seis** `let _ =` de los handlers (no en `graph_db.rs`,
   donde las dos `let _ =` son inocuas), y `reconcile` cuenta éxitos pero no fallos, informa `ok: true`
   con filas sin proyectar y tiene un bucle sin fin si falla una lectura (§1.3).
5. Sin dependencias nuevas: ni `redis` ni `falkordb` (§2.4). No hace falta socket/osv.

## 1. Hallazgos (ruta:línea, verificados en el árbol actual)

### 1.1 Cypher armado con datos del usuario

Todos pasan por `escape_cypher_str` (`graph_db.rs:139-141`: sólo `\` → `\\` y `'` → `\'`) y `format!`:

| Sitio | Ruta:línea | Dato del usuario | Tipo |
|---|---|---|---|
| `project_artifact` | `graph_db.rs:426-432` | `path` | escritura (`MERGE`) |
| `project_entity_full` | `graph_db.rs:438-453` | `name`, `entity_type`, `project_id` (hoy siempre `None`: sólo lo llama `project_entity`, `:434-436`; el brazo `Some` no tiene llamante) | escritura |
| `project_relation_full` | `graph_db.rs:455-471` | `from`, `to`, `rel_type`; `strength` se interpola como `f64` tras `clamp(0,1)` | escritura |
| `unproject_relation` | `graph_db.rs:473-481` | `from`, `to`, `rel_type` | escritura (`DELETE rel`) |
| `falkor_neighbors` | `graph_db.rs:491-528` (`:498-509`, dos plantillas) | `start` | **lectura** |

`version` (`i64`, `:429`) no inyecta. `strength` es seguro salvo `NaN`: `f64::NAN.clamp(0.0, 1.0)`
devuelve `NaN` y se interpolaría como `NaN` (error de sintaxis, no inyección; hoy los llamantes pasan
`1.0` o un `float8` de Postgres).

Origen de los datos, que es lo que hace de esto un tema de seguridad y no de estilo:
- `name` / `entity_type` llegan de `cuba_alma create` (`handlers/alma.rs:75`) y de `cuba_ingesta`
  (auto-extracción de entidades a partir del texto de observaciones: contenido que el agente leyó de
  la web o de un fichero). Un nombre de entidad es texto que puede controlar un tercero.
- La **lectura** usa nombres que ya están en Postgres: `faro.rs:815-822` (`mentioned_entities` y los
  nombres de los resultados → `ppr_around_seeds` → `falkor_neighbors`), `puente.rs:151`
  (`start_entity`, argumento del cliente), `hipotesis.rs:57` (`effect`), `graph_cli.rs:29,66`.
  Un nombre guardado una vez se interpola en Cypher cada vez que alguien busca con `faro`.

### 1.2 H2 — el agravante: la lectura "de sólo lectura" cae a una escritura

`falkor_ro_query` (`graph_db.rs:319-333`) llama `GRAPH.RO_QUERY` y, **ante cualquier error**, repite la
misma consulta con `GRAPH.QUERY` (`:325` si el texto del error contiene `unknown` o `RO_QUERY`; `:329`
incondicional en el otro camino). `falkor_exec` antepone `"{cmd} error:"` al mensaje (`:309-310`), de
modo que el texto siempre contiene `RO_QUERY`.

Consecuencia: una consulta con una cláusula de escritura inyectada **falla** en `RO_QUERY` (eso es lo
que `RO_QUERY` garantiza) y el código la **reintenta con `GRAPH.QUERY`, que sí la ejecuta**. La
protección de sólo lectura del servidor queda anulada por el cliente. No depende de que el escapado
falle hoy: depende de que algún día falle, y entonces la lectura borra. (La versión mínima de
FalkorDB con `GRAPH.RO_QUERY` **no se verificó**; por eso la salida se acota en vez de eliminarse, R4.)

### 1.3 `let _ =` y errores descartados

| Ruta:línea | Qué se descarta |
|---|---|
| `handlers/alma.rs:75` | `project_entity` de `cuba_alma create` |
| `handlers/puente.rs:73` | `project_relation_full` (ida) |
| `handlers/puente.rs:90` | `project_relation_full` (vuelta, bidireccional) |
| `handlers/puente.rs:127` | `unproject_relation` de `delete` |
| `handlers/artefacto.rs:259` | `project_artifact` (put, rama de actualización) |
| `handlers/artefacto.rs:301` | `project_artifact` (put, rama de creación) |
| `graph_db.rs:315` | `let _ = falkor_exec(...)?` — **inocuo**: el `?` ya propaga; sólo tira la respuesta |
| `graph_db.rs:367` | `let _ = probe_reachable().map_err(record_err)` — **inocuo**: `record_err` ya anotó el error |

Lo que ya hace `project_cypher` (`graph_db.rs:384-424`): anota `LAST_ERROR` y devuelve `Err`. El
problema es lo que pasa después:
- **No hay contador de fallos.** `PROJECTED` (`:14`) sólo cuenta éxitos (`projected_ops`).
- **`clear_err()` borra la evidencia.** Se llama tras cada escritura buena (`:399`, `:410`) y tras cada
  `PING` bueno (`:351`, es decir cada vez que alguien pide `status`): un fallo seguido de un éxito, o de
  un `graph status`, deja `last_error: null`. Un grafo que perdió escrituras se ve sano.
- Los handlers no dicen nada al cliente ni escriben un `warn`: el fallo sólo existe en `LAST_ERROR`.
- **`reconcile` descarta el motivo**: `.is_ok()` en `graph_db.rs:808, 847-850, 878` cuenta los éxitos y
  tira el `Err`; devuelve `"ok": true` (`:898`) aunque `projected_* < postgres_*`, publica
  `graph.reconciled` (`:888`) igual, y `graph_cli.rs:22-24` imprime y sale con 0.
- **`reconcile` traga errores de Postgres**: `.unwrap_or_default()` en `:802, 833, 845, 872`
  (los 4 de `graph_db` que cuenta B4). Si falla la lectura de una página, el bucle ve una página vacía
  y termina con `ok: true`.
- **Bucle sin fin (defecto nuevo, no está en el backlog)**: las relaciones se paginan con dos
  consultas separadas (`:822-833` y `:838-845`). Si la segunda falla y `unwrap_or_default()` devuelve
  `[]`, el `zip` (`:846`) no itera, `last_id` no avanza, `page.len() == 250` no sale del bucle (`:855`)
  y la misma página se relee para siempre. Aunque no falle, dos consultas sin transacción pueden
  desalinearse si entra una relación entre ambas, y el `zip` empareja ids con filas que no son suyas.

### 1.4 Cliente RESP escrito a mano

`graph_db.rs:143-249, 296-357`: `resp_bulk`, `resp_array`, `parse_resp`, `read_resp`, `falkor_exec`.
Lo que está bien: el comando viaja como **bulk strings** con longitud en bytes (`s.len()` es bytes,
correcto para UTF-8), así que el texto del usuario no puede romper el encuadre RESP; sólo la capa
Cypher es el riesgo. Lo que no:
- `TcpStream::connect(hostport)` (`:298`, `:341`) **sin tiempo límite de conexión** y bloqueante dentro
  de funciones `async` (`project_cypher` llama a `falkor_query` directamente, `:394`): una IP que no
  contesta deja un worker de tokio parado lo que tarde el sistema operativo (minutos) en el camino de
  `cuba_alma create`. Los timeouts de lectura/escritura (8 s, `:300-301`) sí están. Contradice la regla
  de Rust/async (timeout en toda operación de red, nunca bloquear en async).
- `parse_resp`, rama `*` (`:210`): `Vec::with_capacity(n as usize)` con `n` que viene del servidor. Un
  servidor que responde `*4611686018427387904\r\n` provoca `capacity overflow` (pánico en un worker). El
  servidor es el que el operador configuró, pero la URL puede apuntar a la LAN.
- `read_resp` (`:229-249`) re-parsea el buffer entero en cada lectura (cuadrático hasta 8 MB). No es un
  fallo de seguridad; no entra.

### 1.5 La rama Neo4j

`project_cypher` (`:403-423`) hace `POST {url}` con `{"query": cypher}`, sin cabecera de autenticación.
Nada la lee: las lecturas exigen `backend() == Falkor` (`:492`, `:618`). Sólo escribe. El cuerpo
`{"query": …}` no coincide con la forma de la API HTTP de Neo4j que yo recuerdo (`statement` +
`parameters`) y Neo4j exige autenticación por defecto; **no lo pude verificar** (Context7 no devolvió
el esquema del cuerpo de la Query API). Nadie la ha podido probar contra un Neo4j real. Es una decisión
de producto (§8), no de B8.

### 1.6 `reconcile_stub`

`git log -S"reconcile_stub"`: introducido en `e0dc719` (0.25.0), eliminado en `c53f473` (0.32 B6,
`CHANGELOG.md:38`). `grep -rn reconcile_stub` sobre el árbol (sin `target/`): sólo CHANGELOG y la tabla
del backlog. **Nada que hacer**, salvo corregir la línea B8 de `backlog/0.32-analisis-y-sota.md:127`,
que aún lo nombra (lo hace el orquestador, no esta pieza).

## 2. Decisión: la forma más pequeña correcta

### 2.1 Parámetros `$p`: qué compran y qué no (verificado en la documentación)

La documentación de FalkorDB (`GRAPH.QUERY`): `GRAPH.QUERY g "CYPHER name='Alice' MATCH (n {name:$name}) RETURN n"`.
Los parámetros **no** son un campo aparte del protocolo: son asignaciones **dentro de la misma cadena de
consulta**, en el prefijo `CYPHER`. El cliente oficial de Rust (`falkordb-rs`, `with_param`) lo hace así:
codifica el valor como literal Cypher y lo escapa (`"O'Reilly"` → `'O\'Reilly'`). Con FalkorDB no existe
la separación dato/código de un `$1` de Postgres: seguirá habiendo **un literal que se serializa a
mano**. (Cómo escapa la barra invertida el cliente oficial no lo comprobé.)

Lo que sí compran, y por eso se adoptan:
1. El **cuerpo** de la consulta pasa a ser una constante (`&'static str`): ningún dato de usuario se
   concatena en él. El tipo lo impone: un `String` armado con `format!` no compila como `&'static str`.
2. El escapado deja de estar repetido en siete plantillas y vive en **una** función (`cypher_literal`),
   con una prueba exhaustiva (§4.1). Un escapado olvidado en un sitio nuevo deja de ser posible.
3. FalkorDB cachea el plan por el texto del cuerpo; con cuerpo constante se reutiliza (la propia
   documentación lo recomienda para consultas repetidas).

Para **Neo4j** los parámetros sí son un objeto JSON aparte del texto (`parameters`): ahí no hay
escapado alguno. Se envían así; el nombre del campo de la sentencia no está verificado (§1.5) y B8 no
lo toca, sólo añade `parameters`.

### 2.2 `cypher_literal`: contrato (no código)

Para un `&str` devuelve `'…'` donde el interior sólo contiene caracteres seguros y secuencias de
escape de openCypher: `\\`, `\'`, `\"`, `\n`, `\r`, `\t` y `\uXXXX` para el resto de los caracteres de
control (U+0000–U+001F, U+007F), más U+0085, U+2028 y U+2029. Razón de escapar también `"` y los
saltos: el prefijo `CYPHER` lo lee un analizador propio de FalkorDB antes del analizador de Cypher
(no verificado cómo trata comillas dobles ni saltos de línea); con este contrato el literal no contiene
ninguna comilla sin escapar ni ningún salto de línea crudo, **sea cual sea** ese analizador. Que el
servidor entienda `\uXXXX` dentro del prefijo **no se verificó** (prueba Q9, contra un FalkorDB real; si
falla, el contrato cae a rechazar el valor con error en vez de enviarlo, nunca a enviarlo sin escapar).
Los números se serializan con `{}` de `i64` y de `f64` finito; un `f64` no finito es error de
serialización, no texto.

Forma de la API (orientativa, el implementador decide nombres): `project_cypher(body: &'static str,
params: &[(&'static str, Param)])`, con `Param::{Str(&str), Int(i64), Float(f64)}`; los nombres de
parámetro también son `&'static str`. Falkor: `CYPHER a='…' b=1 BODY` como **un** argumento bulk.
Neo4j: `{"query": BODY, "parameters": {…}}`.

### 2.3 Qué hacer con los errores

- Un fallo de proyección **no** hace fallar la operación del usuario: Postgres es la verdad y el
  `INSERT` ya se confirmó; devolver error por un grafo caído convertiría una proyección opcional en un
  punto único de fallo. (Decisión técnica mía; se revierte cambiando una línea por handler.)
- Pero **se cuenta y se dice**, en tres sitios:
  1. `status_summary()` gana `failed_ops` (contador monótono, igual que `projected_ops`). `last_error`
     deja de borrarse por un `PING` bueno o por una escritura buena posterior: sólo lo borra un
     `reconcile` que termina con cero fallos. (Cambia `clear_err()` en `:351`, `:399`, `:410`.)
  2. Cada fallo deja un `tracing::warn!` con qué se iba a proyectar (tipo de operación, **no** el
     contenido completo: nombre de entidad truncado) y el error.
  3. La respuesta del handler lleva `graph_projection_failed: true` y `graph_projection_error`
     **sólo cuando falló** (misma convención que C4: una llamada sana no lleva la clave). Con
     `backend() == Off` no pasa nada, ni clave ni coste: `project_cypher` sale `Ok` en la primera línea
     (`:385`) y eso no cambia.
- `reconcile`: sale `ok: false` si hubo **cualquier** fallo de lectura de Postgres o de escritura al
  grafo; añade `failed_entities`, `failed_relations`, `failed_artifacts` y `first_error` (una cadena, no
  la lista); `projected_*` pasa a significar "escritos con éxito" y se cumple
  `projected + failed == postgres` por categoría. Una lectura de Postgres que falla **aborta** el
  barrido de esa categoría con `ok: false`, no termina en silencio. Las relaciones se paginan con
  **una** consulta que trae `r.id` junto con el resto (se elimina la segunda y su `zip`).
  `graph.reconciled` se publica con los tres contadores de éxito y los tres de fallo. Las claves
  existentes (`projected_*`, `postgres_*`, `paged`, `backend`) no cambian de nombre.
- `graph reconcile` (CLI, `graph_cli.rs:20-25`): sale con código 1 cuando `ok` es `false` (hoy sale 0
  con `"ok": false`, tanto con el grafo apagado como con fallos). Un operador o un script que lo lanza
  necesita el código. Se revierte con una línea. Revisar `rust/tests/cli_contract.rs` (`graph` aparece
  en `:56` y `:192`) antes de tocarlo.

### 2.4 Cliente RESP: se queda; sin dependencias nuevas

Sustituirlo por `redis` o `falkordb` añadiría un árbol de dependencias (y su cadena de suministro) para
dos comandos (`PING`, `GRAPH.QUERY`/`GRAPH.RO_QUERY`) y no resolvería nada: el cliente oficial hace el
mismo escapado de literal. Alternativa rechazada con ese motivo. Lo que sí se corrige, sin
dependencias: tiempo límite de conexión, el tope de `with_capacity` y la lectura que cae a escritura.
Si el implementador cree necesaria una dependencia, **para y lo dice**: socket (`depscore`) y osv antes.

## 3. Reglas para el implementador (comportamiento observable)

- **R1.** Ningún valor del usuario entra en el texto del cuerpo de una consulta; entra por
  `cypher_literal` (Falkor) o `parameters` (Neo4j).
- **R2.** `escape_cypher_str` desaparece (o queda privada y sólo la usa `cypher_literal`); su prueba
  `cypher_escape_quotes` (`graph_db.rs:915-918`) se sustituye por la de §4.1.
- **R3.** Los seis `let _ =` de los handlers desaparecen; los dos de `graph_db.rs` (`:315`, `:367`) se
  dejan salvo que clippy diga otra cosa.
- **R4.** `falkor_ro_query` sólo repite con `GRAPH.QUERY` si el servidor dijo que no conoce el comando
  (`unknown command`); cualquier otro error se devuelve tal cual. Una lectura **nunca** se reenvía a un
  comando que escribe tras un error de sintaxis o de "no es sólo lectura".
- **R5.** Conexión con tiempo límite (2 s, mismo orden que el `PING`) y, para las escrituras, fuera del
  hilo del runtime (`spawn_blocking`) o con el tiempo acotado; se prueba que un servidor que acepta y
  no contesta no tarda más que el tope de lectura. (Tajada D, opcional.)
- **R6.** `parse_resp` no reserva más de lo que el buffer puede contener: un `*N` con `N` mayor que los
  bytes restantes es `Err`, no pánico.

## 4. Gherkin

Fondo común de los escenarios de proyección: «un servidor RESP falso escucha en `127.0.0.1:0` y
registra los argumentos de cada comando que recibe; `MEMORY_INDUSTRY_GRAPH_DB=falkor` y
`MEMORY_INDUSTRY_GRAPH_URL=redis://127.0.0.1:<ese puerto>`».

```gherkin
# language: es
Característica: un nombre de entidad es un dato, nunca parte de la consulta

  Esquema del escenario: el valor no puede salir de su literal
    Dado un nombre de entidad "<nombre>"
    Cuando se proyecta la entidad al grafo
    Entonces el servidor recibe exactamente un comando GRAPH.QUERY
    Y el cuerpo de la consulta es idéntico al de una entidad llamada "x"
    Y el valor aparece sólo dentro de un literal entre comillas simples en el prefijo CYPHER
    Y ese literal, leído con las reglas de cadena de openCypher, termina en su última comilla
    Y decodificado devuelve exactamente "<nombre>"

    Ejemplos:
      | nombre                                   | por qué                         |
      | O'Reilly                                 | comilla simple                  |
      | a"b                                      | comilla doble                   |
      | C:\temp\new                              | barras invertidas               |
      | \'                                       | barra y comilla juntas          |
      | \\'                                      | barra doble y comilla           |
      | línea1<LF>línea2                         | salto de línea real             |
      | a<CR><LF>b                               | CRLF                            |
      | '}) DETACH DELETE (n) //                 | el ataque de la tarea           |
      | x'}) MATCH (m) DETACH DELETE m //        | variante con MATCH              |
      | a<NUL>b                                  | NUL (Postgres lo rechaza; la función no puede suponerlo) |
      | a<U+2028>b                               | separador de línea Unicode      |
      | 😀 ñ 日本                                | UTF-8 multibyte                 |
      | ' OR 1=1 --                              | estilo SQL                      |

  Escenario: ninguna de las plantillas concatena datos
    Dado cada una de las funciones que escriben o leen el grafo
    Cuando se llama con el nombre "'}) DETACH DELETE (n) //" en cada argumento de texto
    Entonces el cuerpo recibido por el servidor es el mismo que con el nombre "x"
    Y no contiene la subcadena "DETACH"

  Escenario: la relación y el artefacto también
    Dado una relación de "a'b" a "c\d" con tipo "r'}) //"
    Y un artefacto con ruta "dir/it's<LF>.md"
    Cuando se proyectan
    Entonces los cuerpos son constantes y los textos van como literales íntegros

  Escenario: la lectura no se convierte en escritura
    Dado un servidor falso que responde error a GRAPH.RO_QUERY con "ERR syntax error"
    Cuando se recorre el grafo desde "'}) DETACH DELETE (n) //"
    Entonces el servidor no recibe ningún comando GRAPH.QUERY
    Y el recorrido devuelve un error que contiene el texto del servidor

  Escenario: un servidor viejo sin RO_QUERY sigue funcionando
    Dado un servidor falso que responde "ERR unknown command 'GRAPH.RO_QUERY'"
    Cuando se lee el vecindario de "A"
    Entonces se repite como GRAPH.QUERY, una sola vez, con el mismo cuerpo constante

Característica: un fallo de escritura al grafo se cuenta y se informa

  Escenario: el grafo rechaza una escritura
    Dado un servidor falso que responde "-ERR boom" a GRAPH.QUERY
    Y el grafo configurado como falkor
    Cuando se crea la entidad "A" con cuba_alma
    Entonces la entidad existe en Postgres
    Y la respuesta lleva graph_projection_failed true y un graph_projection_error que contiene "boom"
    Y el estado del grafo dice failed_ops 1 y last_error con "boom"
    Y projected_ops sigue en 0

  Escenario: un éxito posterior no borra el fallo
    Dado una escritura fallida seguida de otra correcta
    Cuando se pide el estado del grafo (dos veces: la segunda con PING bueno)
    Entonces failed_ops sigue en 1 y last_error sigue presente
    Y projected_ops es 1

  Escenario: el grafo no contesta
    Dado un oyente en 127.0.0.1:0 que acepta y cierra sin responder
    Cuando se crea una relación con cuba_puente
    Entonces la relación existe en Postgres y la respuesta lleva graph_projection_failed true
    Y la llamada vuelve en menos de 10 segundos

  Escenario: una llamada sana no lleva ruido
    Dado el grafo apagado, o un servidor que acepta
    Cuando se crea una entidad
    Entonces la respuesta no contiene ninguna clave que empiece por graph_projection

  Escenario: borrar una relación falla en el grafo
    Dado una relación existente y un servidor que rechaza GRAPH.QUERY
    Cuando se borra con cuba_puente delete
    Entonces la fila de Postgres desaparece y la respuesta dice graph_projection_failed true

Característica: reconcile dice la verdad

  Escenario: todo se proyecta
    Dado 3 entidades, 2 relaciones y 1 artefacto en la base desechable y un servidor que acepta
    Cuando se ejecuta reconcile
    Entonces ok es true y projected + failed == postgres en las tres categorías
    Y failed_* son 0 y first_error es null

  Escenario: el grafo rechaza algunas filas
    Dado un servidor que responde error a las consultas de relaciones
    Cuando se ejecuta reconcile
    Entonces ok es false, failed_relations es 2, projected_relations es 0
    Y first_error contiene el texto del servidor
    Y el evento graph.reconciled lleva los contadores de fallo

  Escenario: Postgres falla a mitad del barrido
    Dado que la lectura de brain_relations falla (tabla renombrada en la base desechable)
    Cuando se ejecuta reconcile
    Entonces ok es false con un error que nombra la categoría
    Y el barrido termina (no hay bucle sin fin)

  Escenario: la CLI avisa con el código de salida
    Dado un reconcile con ok false
    Cuando se ejecuta "memory-industry graph reconcile"
    Entonces el proceso sale con código distinto de 0 y la salida sigue siendo el JSON

Característica: el cliente RESP aguanta a un servidor hostil

  Escenario: respuesta con una longitud imposible
    Dado un servidor que responde "*4611686018427387904\r\n"
    Cuando se proyecta una entidad
    Entonces el resultado es un error, no un pánico

  Escenario: conexión sin tiempo
    Dado una dirección que no contesta
    Cuando se proyecta una entidad
    Entonces falla dentro del tope de conexión y no deja un worker bloqueado
```

### 4.1 Propiedad sin dependencias

El repo no tiene `proptest` (`rust/Cargo.toml`) y no se añade. La propiedad se prueba enumerando
**todas** las cadenas de hasta 3 caracteres sobre un alfabeto hostil de unos 18 símbolos
(`a ' " \ LF CR TAB NUL U+007F U+0085 U+2028 } ) ( / ; é 😀`: unas 6 000 cadenas) más los 13 ejemplos de
la tabla. El oráculo es un **lector de literales independiente**, escrito en la prueba (no reutiliza el
escapador): recorre la salida de `cypher_literal`, exige que empiece por `'`, que cada `\` introduzca un
escape válido, que no haya `'` ni salto de línea crudo hasta el último carácter, que el último carácter
sea la `'` de cierre y que lo decodificado sea igual a la entrada. Va en `#[cfg(test)]` de `graph_db.rs`:
el paso de mutación del diff corre sus pruebas con `--lib` (`CHANGELOG.md:42`) y una prueba que sólo
viva en `tests/` no la vería.

## 5. Procedimiento QA (con sabotajes), sin FalkorDB real

### 5.0 Servidor falso (sin puerto fijo, sin puerto sin oyente)

- Un oyente `TcpListener::bind("127.0.0.1:0")`; el puerto sale de `local_addr()`. Nunca un puerto fijo
  ni uno donde no escucha nadie (regla `ci-rapido.md`: un rechazo "a propósito" debe ser instantáneo y
  determinista; un puerto sin oyente depende de la máquina).
- Por conexión: lee el comando RESP (`*N` + N bulks), lo guarda en un `Arc<Mutex<Vec<Vec<String>>>>` y
  responde según el modo del escenario: `+PONG` al `PING`; para `GRAPH.QUERY`/`GRAPH.RO_QUERY`,
  respuesta válida (`*3`: cabecera, filas, estadísticas), `-ERR boom`, cierre sin respuesta, o
  `*4611686018427387904\r\n`. Un modo "acepta y no contesta" mantiene el socket abierto hasta que el
  test acaba (para el tope de lectura).
- Los tests de `graph_db.rs` (`--lib`) toman `crate::session::GLOBAL_STATE_GUARD` y `ScopedEnv`
  (`envs.rs:62-87`) para fijar `MEMORY_INDUSTRY_GRAPH_DB`/`_URL`, como ya hace
  `the_graph_config_hangs_off_the_home…` (`graph_db.rs:1005-1009`). Los tests de handlers
  (alma/puente/artefacto) van en `rust/tests/` con una base desechable y **su propio** guardián del
  entorno, como `v040_write_ahead.rs:38-90`; **leer antes** `smoke_test.rs:405-480`, que escanea cada
  fichero de `tests/` que escribe variables de entorno y exige guardas concretas. `ScopedEnv` es
  `#[cfg(test)]` de la biblioteca y no se ve desde `tests/`.
- El cliente se conecta a `127.0.0.1` literal (`redis://127.0.0.1:<p>`), así que el oyente va en IPv4,
  no en `localhost`.

### 5.1 Pasos

| # | Qué se hace | Qué debe pasar | Cubre |
|---|---|---|---|
| Q1 | Propiedad de §4.1 sobre `cypher_literal` | las ~6 000 cadenas y las 13 de la tabla pasan | escapado |
| Q2 | Servidor falso; `project_entity` con cada nombre hostil | un `GRAPH.QUERY`; cuerpo idéntico al de `"x"`; el literal decodifica al original | no inyección (escritura) |
| Q3 | Igual con `project_relation_full`, `unproject_relation`, `project_artifact` | cuerpos constantes; sin `DETACH` en el cuerpo | las plantillas de escritura |
| Q4 | `falkor_neighbors` / `walk_falkor` con `'}) DETACH DELETE (n) //` y servidor que da `-ERR` a `RO_QUERY` | 0 comandos `GRAPH.QUERY` | §1.2 (R4) |
| Q5 | Servidor que dice `unknown command 'GRAPH.RO_QUERY'` | exactamente un reintento con `GRAPH.QUERY` y cuerpo constante | compatibilidad |
| Q6 | `-ERR boom` a `GRAPH.QUERY` + `cuba_alma create`, `cuba_puente create` (ida y bidireccional) y `delete`, `cuba_artefacto put` (ambas ramas) | fila en Postgres; `graph_projection_failed`; `failed_ops` sube; `last_error` | los 6 `let _ =` |
| Q7 | Fallo, luego éxito, luego `status_summary()` dos veces | `failed_ops` y `last_error` siguen | `clear_err` |
| Q8 | `reconcile` sobre base desechable con 3/2/1 filas: sano; con error del servidor; con la tabla de relaciones rota | `ok`, contadores y `first_error` según §2.3; el caso roto **termina** | reconcile |
| Q9 | **Manual, una vez, antes de cerrar la pieza (no entra al juez):** contenedor `docker run -d --name memorys-falkor-qa-20261028 -p 127.0.0.1::6379 falkordb/falkordb`, `MEMORY_INDUSTRY_GRAPH_NAME=qa_b8`; proyectar los 13 nombres hostiles y leerlos de vuelta con `graph tokens` | cada nombre vuelve idéntico; `MATCH (n) RETURN count(n)` coincide con el número de nombres distintos; ningún nodo desaparece | **lo no verificado**: que el servidor lea `\uXXXX`, `\"` y los saltos escapados dentro del prefijo `CYPHER` |
| Q10 | `graph reconcile` con grafo apagado y con fallo | código de salida 1 y JSON en stdout | CLI |
| Q11 | Servidor con `*4611686018427387904` y oyente que no contesta | error, no pánico; vuelve dentro del tope | R5, R6 |

Q9 es una prueba contra un servicio real y por eso **no** es un test del repo (el repo prohíbe
`#[ignore]` y los saltos). Se corre a mano, se pega el resultado en el handoff de QA y, si falla, la
pieza no cierra. Hasta que se corra, el contrato sobre `\uXXXX` de §2.2 es una **suposición declarada**.
(El contenedor lleva nombre con fecha por la guardia de Docker; el puerto lo asigna el sistema.)

### 5.2 Sabotajes (cada uno debe poner un test en rojo; si no, ese test no prueba nada)

| # | Sabotaje en producción | Test que debe ponerse rojo |
|---|---|---|
| S1 | `cypher_literal` deja de escapar `'` (o `\`) | Q1 y Q2 (la fila `'}) DETACH DELETE (n) //`) |
| S2 | `cypher_literal` deja de escapar LF o U+2028 | Q1 (oráculo: salto crudo) |
| S3 | Una plantilla vuelve a `format!("… '{n}' …")` con `escape` | Q2/Q3: el cuerpo ya no es igual al de `"x"` |
| S4 | Se reintroduce el reintento a `GRAPH.QUERY` en cualquier error de `RO_QUERY` | Q4 |
| S5 | Volver a `let _ =` en `alma.rs:75` (y uno por handler, uno a uno) | Q6, uno por cada handler |
| S6 | `project_cypher` deja de incrementar `failed_ops` | Q6/Q7 |
| S7 | `clear_err()` vuelve tras un éxito o un `PING` | Q7 |
| S8 | `reconcile` vuelve a `.is_ok()` sin contar fallos / `ok: true` fijo | Q8 |
| S9 | Se restaura `.unwrap_or_default()` en la lectura de relaciones | Q8 (caso de la tabla rota: el test lleva un tope de iteraciones y falla, no cuelga) |
| S10 | Se restaura la segunda consulta de ids + `zip` | revisión de diff (un test de intercalado de verdad exige insertar entre las dos consultas; con una sola consulta deja de existir el problema) |
| S11 | `Vec::with_capacity(n as usize)` sin tope | Q11 |
| S12 | Se quita el tiempo límite de conexión | Q11 (el oyente que no contesta supera el tope) |
| S13 | El código de salida de la CLI vuelve a 0 | Q10 |

Los tests de §4 se escriben **antes** (pasada 1 del implementador); el orquestador los corre, ve el rojo
contra el código actual, pega la línea en el handoff y commitea `test(rojo)`. Después va el código, sin
tocar los tests.

## 6. Tajadas, orden y riesgos

A y B tocan `graph_db.rs` y van **en serie**; C depende de la API de B; D es independiente de C pero
también toca `graph_db.rs` (en serie con A y B). Un implementador por tajada.

| Tajada | Contenido | Ficheros | Tests (rojo primero) |
|---|---|---|---|
| **A: la consulta no admite datos** | `cypher_literal`, cuerpos constantes `&'static str`, las plantillas, R1, R2, R4; Neo4j con `parameters`; servidor falso en `--lib` | `graph_db.rs` | Q1–Q5; S1–S4 |
| **B: los fallos se cuentan** | `failed_ops`, `last_error` que no se borra, `warn`, `reconcile` (una consulta de relaciones, errores de Postgres, contadores, `ok`), evento, CLI con código 1 | `graph_db.rs`, `graph_cli.rs` | Q7, Q8, Q10; S6–S10, S13 |
| **C: los handlers lo dicen** | los 6 `let _ =`; `graph_projection_failed`/`_error` en las respuestas de `alma create`, `puente create/delete`, `artefacto put` | `handlers/{alma,puente,artefacto}.rs` | Q6; S5 |
| **D: cliente RESP (opcional)** | R5, R6: tiempo de conexión, `spawn_blocking` o equivalente, tope de reserva | `graph_db.rs` | Q11; S11, S12 |

Si hay que cortar, D es lo que se aplaza (no es inyección); A es lo único que no se puede aplazar. B y
C juntas son «se cuenta y se informa».

### Riesgos

- **El servidor entiende o no `\uXXXX` en el prefijo `CYPHER`** (no verificado). Mitigado por Q9 manual y por el contrato de rechazo (§2.2).
- **Forma de `reconcile`**: `doctor.rs:1252` y `http.rs:3083` leen `status_summary()`, no `reconcile`; las claves nuevas se **añaden** (`failed_ops` en el estado; `failed_*`, `first_error` en el informe), ninguna existente cambia de nombre.
- **Código de salida 1 de `graph reconcile`**: cambia lo que ve un script. Revisar `cli_contract.rs`. Se revierte con una línea.
- **Claves nuevas en las respuestas de los handlers**, sólo en fallo; ningún cliente existente las lee. `v040_write_ahead.rs` fuerza el grafo apagado y debe seguir verde sin cambios.
- **Entorno global en tests**: `set_var` está guardado por `smoke_test.rs:405-480`; un test nuevo en `tests/` que no cumpla sus marcadores rompe esa guarda, no la prueba nueva.
- **CRAP**: `reconcile` ya es larga; al tocarla su CRAP no puede subir frente a la base de `HEAD`. Partirla (una función por categoría) entra en B porque el cambio la obliga a tocarla.

## 7. Lo que NO entra

- Reemplazar el cliente RESP por una biblioteca (§2.4).
- Quitar o poner tras una *feature* de Cargo la proyección entera, o decidir el futuro de Neo4j: es C4 del backlog (`0.32-analisis-y-sota.md:302`) y una decisión de producto.
- Señalar `degraded` en las **lecturas** del grafo (`faro.rs:821-826`, `puente.rs:151`, `hipotesis.rs:57`, hoy `if let Ok(..)`: un grafo caído se ve igual que un grafo sin vecinos). Es el mismo defecto que C4 en otro sitio; B8 sólo garantiza que esas lecturas no puedan escribir (R4). Queda anotado para el resto de C4.
- Cola de reintentos o proyección diferida: el remedio de un grafo que perdió escrituras es `graph reconcile`, que ahora informa de verdad.
- Cablear la rama `project_id: Some` de `project_entity_full` (no tiene llamante): si estorba a la plantilla constante se elimina, no se cablea.
- Corregir `reconcile_stub` (ya no existe) y la línea B8 de la tabla del backlog (la edita el orquestador).
- TLS o autenticación hacia FalkorDB/Neo4j (`redis://` sin `AUTH`): hallazgo aparte, fuera de alcance.

## 8. Pregunta para el humano (una sola)

**¿Se mantiene la rama Neo4j?** Hoy sólo escribe, sin autenticación, con un cuerpo que no puedo
confirmar que Neo4j acepte, y nada la lee. Recomendado: B8 la deja segura por construcción
(parámetros JSON) sin ampliar nada, y C4 decide si se quita. Si prefieres quitarla ahora, la tajada A se
encoge (una rama menos) y el valor `neo4j` de `MEMORY_INDUSTRY_GRAPH_DB` pasa a rechazarse con un
mensaje en `doctor`. Mientras tanto la pieza avanza con la recomendada.

## 9. Handoff (para que lo escriba el orquestador en `.cursor/handoffs/`)

El encargo limitaba la escritura a este fichero, así que el YAML queda aquí, listo para copiar:

```yaml
from: especificador
to: implementador
type: note
task: "0.32 B8: la proyeccion a FalkorDB/Neo4j sin concatenar datos en Cypher (cypher_literal + cuerpos constantes, RO_QUERY sin caer a escritura), fallos contados e informados (failed_ops, last_error persistente, graph_projection_failed, reconcile veraz); spec en backlog/0.32-especificaciones/B8-graph-proyeccion.md (tajadas A y B en serie; luego C; D opcional)"
commit: none
evidence: "none (solo lectura: Read de rust/src/graph_db.rs entero, grep/sed de handlers/{alma,puente,artefacto,faro,hipotesis}.rs, graph_cli.rs, http.rs, doctor.rs, tests/; git log -S reconcile_stub; Context7 /falkordb/docs y /falkordb/falkordb-rs; no se compilo ni se corrio ningun test)"
```

## Decisión de Leandro (2026-10-28)

La rama Neo4j/FalkorDB **se mantiene y se completa**: la idea es que el grafo quede implementado al 100%, se instale con el proyecto
y la gente lo use a fondo. No se quita nada. Entran las tajadas A, B y C y también la opcional D; además hay una tajada nueva de
instalación (el grafo se provisiona solo, como Postgres) que se especifica aparte en `G-grafo-instalable.md`.
Cómo se revierte: cada tajada es un commit; la instalación automática va tras un interruptor y no cambia el modo por defecto sin grafo.
