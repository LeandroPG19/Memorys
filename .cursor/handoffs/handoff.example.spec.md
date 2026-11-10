# Ejemplo de spec (plantilla)

La spec que nombra `handoff.example.yml`. Una spec real lleva además el procedimiento QA con sus sabotajes y sus
decisiones con cómo se revierten.

```gherkin
Característica: lo que se entrega, en una línea

  Escenario: el caso que debe pasar
    Dado el estado de partida
    Cuando ocurre lo que se pide
    Entonces se observa el resultado

  Escenario: el caso que debe rechazarse
    Dado el mismo estado de partida
    Cuando ocurre lo que no se permite
    Entonces se rechaza diciendo qué falta
```

## Coste de cada escenario

Una fila por escenario, con su nombre exacto. Nivel: `pura`, `guion`, `servicio` o `e2e`, del más barato al más caro.
Segundos: lo que añade a la suite. Carril: una fila de `scripts/gate-dag.txt`. El porqué es obligatorio cuando el nivel
no es `pura`.

| Escenario | Nivel | Segundos | Carril | Por qué no el barato |
|---|---|---|---|---|
| el caso que debe pasar | pura | 0.01 | DBG | |
| el caso que debe rechazarse | guion | 0.06 | DBG | La guarda solo existe en bash; una función pura sería una segunda implementación. |
