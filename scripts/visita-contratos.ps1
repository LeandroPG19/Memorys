# Contratos de los guiones de la visita (tajada D de
# .cursor/handoffs/0.29.0-autoconexion.spec.md): la madre en modo lan, su aviso al
# iniciar sesion, el codigo de emparejamiento, el paquete CLIENTE y el empaquetador.
#
#   pwsh -NoProfile -File scripts/visita-contratos.ps1
#
# Sale 0 si todos pasan y 1 si alguno falla, con una linea por contrato. Corre en
# Linux con pwsh 7: los guiones se cargan con "punto" (solo definen sus funciones) y se
# prueban sus funciones puras; lo que solo existe en Windows (tareas, firewall,
# ventanas) no se toca. El empaquetador se corre de verdad sobre una copia del arbol
# en un directorio temporal, con un .exe falso, para no pisar dist-visita/.
#
# Necesita bash, python3 y sha256sum (lo mismo que el empaquetador). Si falta uno, el
# contrato que lo usa FALLA diciendo cual: aqui no se salta nada.
#
# Este fichero tambien es ASCII puro: los textos con acentos se arman con [char].
$ErrorActionPreference = 'Stop'

$script:DirScripts = $PSScriptRoot
$script:DirRaiz = Split-Path -Parent $PSScriptRoot
$script:Resultados = New-Object System.Collections.ArrayList
$script:o = [string][char]0xF3
$script:oRota = [string][char]0xC3 + [string][char]0xB3

function Get-RutaGuion { param([string]$Nombre) return (Join-Path $script:DirScripts $Nombre) }

# ---------------------------------------------------------------------------
# Afirmaciones
# ---------------------------------------------------------------------------

function ConvertTo-TextoPrueba {
    param($Valor)
    if ($null -eq $Valor) { return '<null>' }
    if ($Valor -is [System.Array] -or $Valor -is [System.Collections.IList]) {
        return ('@(' + ((@($Valor) | ForEach-Object { ConvertTo-TextoPrueba $_ }) -join ', ') + ')')
    }
    return ("'" + [string]$Valor + "'")
}

function Assert-Igual {
    param($Esperado, $Real, [string]$Que)
    $e = ConvertTo-TextoPrueba $Esperado
    $r = ConvertTo-TextoPrueba $Real
    if ($e -cne $r) { throw ('{0}: se esperaba {1} y salio {2}' -f $Que, $e, $r) }
}

function Assert-Cierto {
    param($Condicion, [string]$Que)
    if (-not $Condicion) { throw $Que }
}

function Assert-Contiene {
    param([string]$Texto, [string]$Aguja, [string]$Que)
    if ($null -eq $Texto -or $Texto.IndexOf($Aguja, [System.StringComparison]::Ordinal) -lt 0) {
        throw ('{0}: no aparece "{1}" en: {2}' -f $Que, $Aguja, $Texto)
    }
}

function Assert-NoContiene {
    param([string]$Texto, [string]$Aguja, [string]$Que)
    if ($null -ne $Texto -and $Texto.IndexOf($Aguja, [System.StringComparison]::OrdinalIgnoreCase) -ge 0) {
        throw ('{0}: aparece "{1}" en: {2}' -f $Que, $Aguja, $Texto)
    }
}

function Assert-Funcion {
    param([string[]]$Nombres, [string]$Guion)
    foreach ($n in $Nombres) {
        if ($null -eq (Get-Command -Name $n -CommandType Function -ErrorAction SilentlyContinue)) {
            throw ('falta la funcion {0} en {1}' -f $n, $Guion)
        }
    }
}

function Assert-Ascii {
    param([byte[]]$Bytes, [string]$Que)
    for ($i = 0; $i -lt $Bytes.Length; $i++) {
        if ($Bytes[$i] -gt 127) { throw ('{0}: byte fuera de ASCII (0x{1:X2}) en la posicion {2}' -f $Que, $Bytes[$i], $i) }
    }
}

function Assert-Crlf {
    param([byte[]]$Bytes, [string]$Que)
    if ($Bytes.Length -eq 0) { throw ('{0}: esta vacio' -f $Que) }
    for ($i = 0; $i -lt $Bytes.Length; $i++) {
        if ($Bytes[$i] -eq 10 -and ($i -eq 0 -or $Bytes[$i - 1] -ne 13)) { throw ('{0}: salto de linea LF suelto en la posicion {1}; cmd.exe necesita CRLF' -f $Que, $i) }
    }
}

function ConvertTo-RutaWin { param([string]$Ruta) if ($null -eq $Ruta) { return $null } return $Ruta.Replace('/', '\') }

# ---------------------------------------------------------------------------
# Ejecucion: cada contrato carga su guion en un ambito propio, para que las funciones
# de un guion no tapen las de otro. Los cuerpos ven esas funciones porque PowerShell
# resuelve los nombres por el ambito que llama.
# ---------------------------------------------------------------------------

function Test-Contrato {
    param([string]$Id, [string]$Guion, [scriptblock]$Cuerpo)
    $resultado = 'PASA'
    $detalle = ''
    try {
        & {
            param($RutaGuion, $Bloque)
            if ($RutaGuion) { . $RutaGuion }
            [void](& $Bloque)
        } $(if ($Guion) { Get-RutaGuion $Guion } else { $null }) $Cuerpo
    }
    catch {
        $resultado = 'FALLA'
        $detalle = $_.Exception.Message
    }
    [void]$script:Resultados.Add([pscustomobject]@{ Id = $Id; Resultado = $resultado; Detalle = $detalle })
    $color = 'Green'; if ($resultado -ne 'PASA') { $color = 'Red' }
    Write-Host ('[{0,-5}] {1}' -f $resultado, $Id) -ForegroundColor $color
    # El motivo, recortado: la salida entera del empaquetador no cabe en una linea.
    if ($detalle) {
        $corto = $detalle -replace '\s+', ' '
        if ($corto.Length -gt 400) { $corto = $corto.Substring(0, 400) + ' ...' }
        Write-Host ('        {0}' -f $corto)
    }
}

function New-DirTemporal {
    $d = Join-Path ([System.IO.Path]::GetTempPath()) ('visita-contrato-' + [guid]::NewGuid().ToString('N'))
    [void](New-Item -ItemType Directory -Path $d -Force)
    return $d
}

# ===========================================================================
# D2 . ensayo-madre.ps1: la madre en modo lan
# ===========================================================================

Test-Contrato 'D2 setup service en modo lan no lleva --addr' 'ensayo-madre.ps1' {
    Assert-Funcion @('Get-ArgumentosSetup') 'ensayo-madre.ps1'
    Assert-Igual @('setup', 'service', '--profile', 'lan') @(Get-ArgumentosSetup) 'el plan (sin aplicar)'
    Assert-Igual @('setup', 'service', '--profile', 'lan', '--apply', '--register') @(Get-ArgumentosSetup -Aplicar) 'la instalacion'
}

Test-Contrato 'D2 la direccion que debe quedar en el env file es lan:<puerto>' 'ensayo-madre.ps1' {
    Assert-Funcion @('Get-DireccionEsperada') 'ensayo-madre.ps1'
    Assert-Igual 'lan:8787' (Get-DireccionEsperada 8787) 'CUBA_HTTP_ADDR esperado'
}

Test-Contrato 'D2 el puerto por omision es 8787, el que escribe setup service sin --addr' 'ensayo-madre.ps1' {
    Assert-Igual 8787 $Puerto 'valor por omision de -Puerto'
}

Test-Contrato 'D2 la IP no se elige: se muestra la detectada sin preguntar' 'ensayo-madre.ps1' {
    function Read-Host { throw 'el instalador pregunto la IP (Read-Host); en modo lan solo se muestra' }
    function Write-Host { }
    foreach ($dhcp in @($false, $true)) {
        $elegida = [pscustomobject]@{ Ip = '192.168.1.30'; Alias = 'Ethernet'; Gateway = $true; Dhcp = $dhcp; Perfil = 'Private'; Metrica = 25; Excluida = $false }
        $otra = [pscustomobject]@{ Ip = '10.0.0.5'; Alias = 'Wi-Fi'; Gateway = $true; Dhcp = $true; Perfil = 'Private'; Metrica = 50; Excluida = $false }
        $humo = [pscustomobject]@{ Elegida = $elegida; Candidatas = @($elegida, $otra) }
        $ctx = [pscustomobject]@{ Ip = $null }
        Resolve-IpReal $ctx $humo
        Assert-Igual '192.168.1.30' $ctx.Ip ('IP mostrada con Dhcp={0}' -f $dhcp)
    }
}

Test-Contrato 'D2 hacen falta las dos reglas de firewall, TCP y UDP, en privada y dominio' 'ensayo-madre.ps1' {
    Assert-Funcion @('Get-ChecksReglas') 'ensayo-madre.ps1'
    $tcp = [pscustomobject]@{ Habilitada = 'True'; Direccion = 'Inbound'; Accion = 'Allow'; Protocolo = 'TCP'; Puerto = '8787'; Perfil = 'Domain, Private' }
    $udp = [pscustomobject]@{ Habilitada = 'True'; Direccion = 'Inbound'; Accion = 'Allow'; Protocolo = 'UDP'; Puerto = '8787'; Perfil = 'Domain, Private' }
    $udpPublica = [pscustomobject]@{ Habilitada = 'True'; Direccion = 'Inbound'; Accion = 'Allow'; Protocolo = 'UDP'; Puerto = '8787'; Perfil = 'Domain, Private, Public' }
    $udpOtroPuerto = [pscustomobject]@{ Habilitada = 'True'; Direccion = 'Inbound'; Accion = 'Allow'; Protocolo = 'UDP'; Puerto = '8797'; Perfil = 'Domain, Private' }
    $res = { param($checks, $id) @($checks | Where-Object { $_.id -eq $id } | ForEach-Object { $_.resultado }) }

    $c = @(Get-ChecksReglas @($tcp, $udp) 8787)
    Assert-Igual @('PASA') @(& $res $c 'regla_tcp') 'las dos reglas: TCP'
    Assert-Igual @('PASA') @(& $res $c 'regla_udp') 'las dos reglas: UDP'
    $c = @(Get-ChecksReglas @($tcp) 8787)
    Assert-Igual @('PASA') @(& $res $c 'regla_tcp') 'solo TCP: TCP'
    Assert-Igual @('FALLA') @(& $res $c 'regla_udp') 'solo TCP: falta la UDP del aviso'
    $c = @(Get-ChecksReglas @($tcp, $udpPublica) 8787)
    Assert-Igual @('FALLA') @(& $res $c 'regla_udp') 'una UDP abierta tambien en Publica'
    $c = @(Get-ChecksReglas @($tcp, $udpOtroPuerto) 8787)
    Assert-Igual @('FALLA') @(& $res $c 'regla_udp') 'una UDP en otro puerto'
}

Test-Contrato 'D2 la tarea "MemoryIndustry Madre aviso" corre aviso-madre.ps1 de la copia protegida al iniciar sesion, sin elevar' 'ensayo-madre.ps1' {
    Assert-Funcion @('Get-DefinicionTareaAviso') 'ensayo-madre.ps1'
    $t = Get-DefinicionTareaAviso -DirPaquete 'C:\ProgramData\MemoryIndustry\paquete' -Usuario 'MADRE\ana'
    Assert-Igual 'MemoryIndustry Madre aviso' $t.Nombre 'nombre de la tarea'
    Assert-Igual 'powershell.exe' $t.Programa 'programa (Windows PowerShell 5.1)'
    Assert-Igual 'AlIniciarSesion' $t.Disparador 'disparador'
    Assert-Igual 'MADRE\ana' $t.Usuario 'usuario'
    Assert-Igual 'Limited' $t.Nivel 'nivel de ejecucion'
    $a = ConvertTo-RutaWin ([string]$t.Argumentos)
    foreach ($x in @('-NoProfile', '-ExecutionPolicy Bypass', '-WindowStyle Hidden', '-File "C:\ProgramData\MemoryIndustry\paquete\aviso-madre.ps1"')) {
        Assert-Contiene $a $x 'argumentos de la tarea'
    }
}

Test-Contrato 'D2 Limpiar retira tambien la tarea del aviso de la madre' 'ensayo-madre.ps1' {
    Assert-Funcion @('Get-TareasPropias') 'ensayo-madre.ps1'
    $t = @(Get-TareasPropias)
    foreach ($n in @('memory-industry-ensayo-reinicio', 'memory-industry-ensayo-despues', 'MemoryIndustry Madre aviso')) {
        Assert-Cierto ($t -contains $n) ('Get-TareasPropias no incluye "{0}": {1}' -f $n, (ConvertTo-TextoPrueba $t))
    }
}

Test-Contrato 'D2 Despues acepta que la IP cambie en modo lan, pero no que la madre se quede sin IP' 'ensayo-madre.ps1' {
    $huella = 'a' * 64
    $antes = [pscustomobject]@{ direccion = 'lan:8787'; ip = '192.168.1.30'; puerto = 8787; token_sha256 = $huella; armado_en = '2026-10-10T08:00:00+00:00'; autologon = $false }
    $res = { param($checks) @($checks | Where-Object { $_.id -eq 'lan_direccion' }) }

    $actual = [pscustomobject]@{ direccion = 'lan:8787'; lan_direccion = '192.168.1.31:8787'; token_sha256 = $huella; errores_log = @(); health_ahora = $true; primera_sesion = $null }
    $c = @(& $res (Get-ChecksDespues $antes $null $actual 300))
    Assert-Igual 1 $c.Count 'checks lan_direccion con la IP cambiada'
    Assert-Igual 'PASA' $c[0].resultado 'la madre sigue a su IP nueva'
    Assert-Contiene ([string]$c[0].detalle) '192.168.1.31:8787' 'el detalle dice donde esta ahora'

    $actual = [pscustomobject]@{ direccion = 'lan:8787'; lan_direccion = $null; token_sha256 = $huella; errores_log = @(); health_ahora = $true; primera_sesion = $null }
    $c = @(& $res (Get-ChecksDespues $antes $null $actual 300))
    Assert-Igual 1 $c.Count 'checks lan_direccion sin IP'
    Assert-Igual 'FALLA' $c[0].resultado 'una madre sin IP privada no la ve ningun cliente'
}

# ===========================================================================
# D2 . aviso-madre.ps1 -Modo Aviso
# ===========================================================================

Test-Contrato 'D2 aviso: lee el bloque lan de /health' 'aviso-madre.ps1' {
    Assert-Funcion @('Read-SaludMadre') 'aviso-madre.ps1'
    $s = Read-SaludMadre '{"status":"ok","version":"0.29.0","lan":{"mode":"auto","address":"192.168.1.30:8787","since":"2026-10-10T08:00:00Z"}}'
    Assert-Igual 'auto' $s.Modo 'modo'
    Assert-Igual '192.168.1.30:8787' $s.Direccion 'direccion'
    Assert-Igual '192.168.1.30' $s.Ip 'ip'
    Assert-Igual 8787 $s.Puerto 'puerto'
    $s = Read-SaludMadre '{"status":"ok","lan":{"mode":"auto","address":null,"since":"2026-10-10T08:00:00Z"}}'
    Assert-Cierto ($null -ne $s) 'una madre que responde sin IP privada sigue siendo una respuesta'
    Assert-Igual $null $s.Ip 'ip sin direccion lan'
    $s = Read-SaludMadre '{"status":"ok"}'
    Assert-Cierto ($null -ne $s) 'una madre sin bloque lan sigue siendo una respuesta'
    Assert-Igual $null $s.Ip 'ip sin bloque lan'
    Assert-Igual $null (Read-SaludMadre '') 'texto vacio'
    Assert-Igual $null (Read-SaludMadre 'Bad Gateway') 'texto que no es JSON'
    Assert-Igual $null (Read-SaludMadre $null) 'sin respuesta'
}

Test-Contrato 'D2 aviso: activa, cambio de IP, sin red y sin respuesta' 'aviso-madre.ps1' {
    Assert-Funcion @('Read-SaludMadre', 'Get-DecisionAviso') 'aviso-madre.ps1'
    $log = 'C:\Users\ana\AppData\Local\MemoryIndustry\memory-industry.log'
    $s = Read-SaludMadre '{"status":"ok","lan":{"mode":"auto","address":"192.168.1.30:8787","since":"2026-10-10T08:00:00Z"}}'

    $d = Get-DecisionAviso -Salud $s -Ultima $null -Nombre 'MADRE' -Log $log
    Assert-Igual 'activa' $d.Tipo 'primera vez'
    Assert-Igual 'Madre activa: MADRE en 192.168.1.30' $d.Mensaje 'mensaje de la primera vez'

    $d = Get-DecisionAviso -Salud $s -Ultima '192.168.1.30' -Nombre 'MADRE' -Log $log
    Assert-Igual 'activa' $d.Tipo 'la misma IP que la ultima vez'

    $d = Get-DecisionAviso -Salud $s -Ultima '192.168.1.20' -Nombre 'MADRE' -Log $log
    Assert-Igual 'cambio' $d.Tipo 'IP distinta de la ultima vista'
    Assert-Contiene $d.Mensaje 'La IP cambio de 192.168.1.20 a 192.168.1.30.' 'mensaje del cambio'
    Assert-Contiene $d.Mensaje 'Los equipos la encuentran solos.' 'mensaje del cambio'
    Assert-Igual @('Fijar 192.168.1.30', 'Dejar automatica') $d.Botones 'botones del cambio'

    $sinRed = Read-SaludMadre '{"status":"ok","lan":{"mode":"auto","address":null,"since":"2026-10-10T08:00:00Z"}}'
    $d = Get-DecisionAviso -Salud $sinRed -Ultima '192.168.1.30' -Nombre 'MADRE' -Log $log
    Assert-Igual 'sin-red' $d.Tipo 'la madre responde en este equipo pero no tiene IP privada'

    $d = Get-DecisionAviso -Salud $null -Ultima '192.168.1.30' -Nombre 'MADRE' -Log $log
    Assert-Igual 'sin-respuesta' $d.Tipo 'la madre no responde'
    Assert-Contiene $d.Mensaje $log 'el mensaje lleva la ruta del log'
    Assert-Contiene $d.Mensaje 'doctor' 'el mensaje dice que corra doctor'

    foreach ($m in @($d.Mensaje)) { Assert-Ascii ([System.Text.Encoding]::UTF8.GetBytes($m)) 'mensaje del aviso' }
}

Test-Contrato 'D2 aviso: la ultima IP vista se guarda en %LOCALAPPDATA%\MemoryIndustry\aviso-madre.json' 'aviso-madre.ps1' {
    Assert-Funcion @('Get-RutaEstadoAviso', 'Save-UltimaVista', 'Read-UltimaVista') 'aviso-madre.ps1'
    Assert-Igual 'C:\Users\ana\AppData\Local\MemoryIndustry\aviso-madre.json' (ConvertTo-RutaWin (Get-RutaEstadoAviso 'C:\Users\ana\AppData\Local')) 'ruta del estado'
    $tmp = New-DirTemporal
    try {
        $ruta = Join-Path (Join-Path $tmp 'MemoryIndustry') 'aviso-madre.json'
        Assert-Igual $null (Read-UltimaVista $ruta) 'sin fichero todavia'
        Save-UltimaVista $ruta '192.168.1.30'
        Assert-Igual '192.168.1.30' (Read-UltimaVista $ruta) 'lo guardado se relee'
        Save-UltimaVista $ruta '192.168.1.31'
        Assert-Igual '192.168.1.31' (Read-UltimaVista $ruta) 'se guarda la ultima, no la primera'
        [System.IO.File]::WriteAllText($ruta, '{ roto')
        Assert-Igual $null (Read-UltimaVista $ruta) 'un fichero roto se lee como "nunca visto", sin error'
    }
    finally { Remove-Item -LiteralPath $tmp -Recurse -Force -ErrorAction SilentlyContinue }
}

Test-Contrato 'D2 aviso: "Fijar" usa mascara, puerta y DNS de la configuracion actual y avisa del DHCP' 'aviso-madre.ps1' {
    Assert-Funcion @('Get-PlanFijarIp') 'aviso-madre.ps1'
    $actual = [pscustomobject]@{ InterfaceIndex = 12; Ip = '192.168.1.30'; PrefixLength = 24; Puerta = '192.168.1.1'; Dns = @('192.168.1.1', '1.1.1.1') }
    $p = Get-PlanFijarIp -Actual $actual
    Assert-Igual $null $p.Error 'sin error'
    Assert-Igual 12 $p.InterfaceIndex 'interfaz'
    Assert-Igual '192.168.1.30' $p.IPAddress 'IP'
    Assert-Igual 24 $p.PrefixLength 'mascara'
    Assert-Igual '192.168.1.1' $p.DefaultGateway 'puerta de enlace'
    Assert-Igual @('192.168.1.1', '1.1.1.1') $p.Dns 'DNS'
    Assert-Contiene ([string]$p.Riesgo) 'DHCP' 'explica el riesgo con el DHCP'
    foreach ($falta in @('PrefixLength', 'Puerta')) {
        $incompleta = [pscustomobject]@{ InterfaceIndex = 12; Ip = '192.168.1.30'; PrefixLength = 24; Puerta = '192.168.1.1'; Dns = @('192.168.1.1') }
        $incompleta.$falta = $null
        $p = Get-PlanFijarIp -Actual $incompleta
        Assert-Cierto (-not [string]::IsNullOrEmpty([string]$p.Error)) ('sin {0} no se fija nada' -f $falta)
    }
}

# ===========================================================================
# D3 . aviso-madre.ps1 -Modo Codigo (GENERAR-CODIGO.cmd)
# ===========================================================================

Test-Contrato 'D3 codigo: lee el codigo y la huella de la salida de account pair' 'aviso-madre.ps1' {
    Assert-Funcion @('Get-CodigoDeSalida') 'aviso-madre.ps1'
    $salida = "Pairing code issued. It is shown only now and is stored only as a key.`n`nOn the client, run:`n`n  memory-industry bridge pair K7QM-3WXP-9HNR`n`nHost fingerprint: 5F3A-91C2-77D0-AB14 (the client prints the same one; compare them)`nvalid until 2026-10-11 10:00 (24 hours, single use)`n"
    foreach ($t in @($salida, $salida.Replace("`n", "`r`n"))) {
        $c = Get-CodigoDeSalida $t
        Assert-Igual 'K7QM-3WXP-9HNR' $c.Codigo 'codigo'
        Assert-Igual '5F3A-91C2-77D0-AB14' $c.Huella 'huella'
    }
    Assert-Igual $null (Get-CodigoDeSalida 'Error: account "ana" not found') 'una salida sin codigo'
    Assert-Igual $null (Get-CodigoDeSalida '') 'salida vacia'
}

Test-Contrato 'D3 codigo: crea la cuenta si falta y luego pide el codigo' 'aviso-madre.ps1' {
    Assert-Funcion @('Get-OrdenesCodigo', 'Test-CuentaYaExiste', 'Test-NombreCuenta') 'aviso-madre.ps1'
    $o = @(Get-OrdenesCodigo 'Ana')
    Assert-Igual 2 $o.Count 'numero de ordenes'
    Assert-Igual @('account', 'add', 'ana') $o[0] 'primero account add'
    Assert-Igual @('account', 'pair', 'ana') $o[1] 'despues account pair'
    Assert-Igual $true (Test-CuentaYaExiste 'Error: account "ana" already exists') 'la cuenta ya existia: se sigue'
    Assert-Igual $false (Test-CuentaYaExiste 'Error: connecting to database') 'otro error: se para'
    foreach ($bien in @('ana', 'Ana', 'ana.perez_2-b', ('a' * 32))) { Assert-Igual $true (Test-NombreCuenta $bien) ('nombre valido "{0}"' -f $bien) }
    foreach ($mal in @('', ' ', 'ana perez', '-ana', ('a' * 33), ('jos' + [char]0xE9))) { Assert-Igual $false (Test-NombreCuenta $mal) ('nombre invalido "{0}"' -f $mal) }
}

Test-Contrato 'D3 codigo: la pantalla muestra codigo, persona, nombre e IP de la madre y que hacer en el equipo nuevo' 'aviso-madre.ps1' {
    Assert-Funcion @('Format-PantallaCodigo') 'aviso-madre.ps1'
    $t = [string](Format-PantallaCodigo -Codigo 'K7QM-3WXP-9HNR' -Persona 'ana' -Madre 'MADRE' -Ip '192.168.1.30' -Huella '5F3A-91C2-77D0-AB14')
    foreach ($x in @('K7QM-3WXP-9HNR', 'ana', 'MADRE', '192.168.1.30', '5F3A-91C2-77D0-AB14', 'CONECTAR-A-LA-MADRE.cmd', '24 horas')) {
        Assert-Contiene $t $x 'pantalla del codigo'
    }
    Assert-Ascii ([System.Text.Encoding]::UTF8.GetBytes($t)) 'pantalla del codigo'
}

Test-Contrato 'D3 GENERAR-CODIGO.cmd lanza aviso-madre.ps1 -Modo Codigo' $null {
    $ruta = Get-RutaGuion 'GENERAR-CODIGO.cmd'
    $b = [System.IO.File]::ReadAllBytes($ruta)
    Assert-Ascii $b 'GENERAR-CODIGO.cmd'
    Assert-Crlf $b 'GENERAR-CODIGO.cmd'
    $t = [System.Text.Encoding]::ASCII.GetString($b)
    Assert-Cierto ($t -match '(?im)^\s*powershell(\.exe)?\s+-NoProfile\s+-ExecutionPolicy\s+Bypass\s+-File\s+"%~dp0aviso-madre\.ps1"\s+-Modo\s+Codigo') 'GENERAR-CODIGO.cmd no lanza powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0aviso-madre.ps1" -Modo Codigo'
    Assert-Cierto ($t -notmatch '(?im)^\s*:\w|\bgoto\b') 'GENERAR-CODIGO.cmd usa etiquetas o goto'
}

# ===========================================================================
# D1 . conectar-cliente.ps1 y CONECTAR-A-LA-MADRE.cmd
# ===========================================================================

Test-Contrato 'D1 el .exe se copia a %LOCALAPPDATA%\MemoryIndustry\bin' 'conectar-cliente.ps1' {
    Assert-Funcion @('Get-RutaBinCliente') 'conectar-cliente.ps1'
    Assert-Igual 'C:\Users\ana\AppData\Local\MemoryIndustry\bin\memory-industry.exe' (ConvertTo-RutaWin (Get-RutaBinCliente 'C:\Users\ana\AppData\Local')) 'destino del .exe'
}

Test-Contrato 'D1 el codigo se acepta como lo escriba la persona y se rechaza si no puede ser' 'conectar-cliente.ps1' {
    Assert-Funcion @('ConvertTo-CodigoEmparejamiento') 'conectar-cliente.ps1'
    foreach ($bien in @('K7QM-3WXP-9HNR', ' k7qm 3wxp-9hnr ', 'K7QM3WXP9HNR')) {
        Assert-Igual 'K7QM-3WXP-9HNR' (ConvertTo-CodigoEmparejamiento $bien) ('codigo "{0}"' -f $bien)
    }
    foreach ($mal in @('', '   ', 'K7QM-3WXP-9HN0', 'K7QM-3WXP-9HNI', 'K7QM-3WXP-9HN', 'K7QM-3WXP-9HNRR')) {
        Assert-Igual $null (ConvertTo-CodigoEmparejamiento $mal) ('codigo "{0}"' -f $mal)
    }
}

Test-Contrato 'D1 bridge pair primero busca solo y, si no la encuentra, va con --host' 'conectar-cliente.ps1' {
    Assert-Funcion @('Get-ArgumentosPair', 'Get-SiguientePasoPair') 'conectar-cliente.ps1'
    Assert-Igual @('bridge', 'pair', 'K7QM-3WXP-9HNR') @(Get-ArgumentosPair 'K7QM-3WXP-9HNR') 'sin madre indicada'
    Assert-Igual @('bridge', 'pair', 'K7QM-3WXP-9HNR', '--host', 'madre') @(Get-ArgumentosPair 'K7QM-3WXP-9HNR' 'madre') 'con nombre o IP'
    $o = $script:o
    $casos = @(
        @(0, 'madre: 192.168.1.30:8787 (huella 5F3A-91C2-77D0-AB14)', 'listo'),
        @(1, ('Error: no se encontr' + $o + ' la madre: barrido 254 direcciones'), 'pedir-madre'),
        @(1, ('Error: no se encontr' + $script:oRota + ' la madre: barrido'), 'pedir-madre'),
        @(1, ('Error: el equipo que contest' + $o + ' no conoce el c' + $o + 'digo: no es la madre. No se guard' + $o + ' nada'), 'pedir-madre'),
        @(1, ('Error: la madre no acept' + $o + ' el c' + $o + 'digo (puede estar mal escrito, caducado o ya usado)'), 'pedir-codigo'),
        @(1, ('Error: c' + $o + 'digo mal escrito: 0 no es un s' + [char]0xED + 'mbolo del c' + $o + 'digo'), 'pedir-codigo'),
        @(1, 'Error: Access is denied. (os error 5)', 'parar')
    )
    foreach ($c in $casos) {
        Assert-Igual $c[2] (Get-SiguientePasoPair -Codigo $c[0] -Salida $c[1]) ('salida: {0}' -f $c[1])
    }
}

Test-Contrato 'D1 setup client solo para Claude Code y Cursor si estan instalados' 'conectar-cliente.ps1' {
    Assert-Funcion @('Get-ClientesPresentes', 'Get-ArgumentosSetupCliente') 'conectar-cliente.ps1'
    $tmp = New-DirTemporal
    try {
        $perfil = Join-Path $tmp 'perfil'
        $appdata = Join-Path $tmp 'appdata'
        [void](New-Item -ItemType Directory -Path $perfil, $appdata -Force)
        Assert-Igual @() @(Get-ClientesPresentes -Perfil $perfil -AppData $appdata) 'ninguno instalado'
        [void](New-Item -ItemType Directory -Path (Join-Path $perfil '.cursor') -Force)
        Assert-Igual @('cursor') @(Get-ClientesPresentes -Perfil $perfil -AppData $appdata) 'solo Cursor'
        [System.IO.File]::WriteAllText((Join-Path $perfil '.claude.json'), '{}')
        Assert-Igual @('claude', 'cursor') @(Get-ClientesPresentes -Perfil $perfil -AppData $appdata) 'Claude Code y Cursor'
    }
    finally { Remove-Item -LiteralPath $tmp -Recurse -Force -ErrorAction SilentlyContinue }
    Assert-Igual @('setup', 'client', 'cursor', '--client-id', 'PC-02', '--apply') @(Get-ArgumentosSetupCliente 'cursor' 'PC-02') 'orden de setup client'
}

Test-Contrato 'D1 la espera de la conexion lee /bridge/health' 'conectar-cliente.ps1' {
    Assert-Funcion @('Get-EstadoPuente') 'conectar-cliente.ps1'
    $e = Get-EstadoPuente '{"paired":true,"state":"verified","host_fingerprint":"5F3A-91C2-77D0-AB14","last_good":"192.168.1.30:8787"}'
    Assert-Igual $true $e.Conectado 'verificada'
    Assert-Igual '192.168.1.30:8787' $e.Direccion 'direccion'
    Assert-Igual '5F3A-91C2-77D0-AB14' $e.Huella 'huella'
    foreach ($estado in @('unverified', 'unreachable', 'searching')) {
        $e = Get-EstadoPuente ('{"paired":true,"state":"' + $estado + '","host_fingerprint":null,"last_good":null}')
        Assert-Igual $false $e.Conectado ('estado {0}' -f $estado)
    }
    $e = Get-EstadoPuente '{"paired":true,"state":"revoked","host_fingerprint":"5F3A-91C2-77D0-AB14","last_good":null}'
    Assert-Igual $false $e.Conectado 'revocada'
    Assert-Igual $true $e.Revocado 'revocada se distingue de "aun buscando"'
    Assert-Igual $false (Get-EstadoPuente $null).Conectado 'el puente no responde'
    Assert-Igual $false (Get-EstadoPuente 'not found').Conectado 'respuesta que no es JSON'
}

Test-Contrato 'D1 el informe no lleva el codigo de emparejamiento' 'conectar-cliente.ps1' {
    Assert-Funcion @('Protect-Codigo') 'conectar-cliente.ps1'
    $texto = "bridge pair K7QM-3WXP-9HNR`ncodigo k7qm-3wxp-9hnr`nsin guiones K7QM3WXP9HNR`nmadre: 192.168.1.30:8787"
    $t = Protect-Codigo $texto 'K7QM-3WXP-9HNR'
    Assert-NoContiene $t 'K7QM-3WXP-9HNR' 'codigo con guiones'
    Assert-NoContiene $t 'K7QM3WXP9HNR' 'codigo sin guiones'
    Assert-Contiene $t '192.168.1.30:8787' 'lo demas del informe se conserva'
}

Test-Contrato 'D1 conectar-cliente.ps1 no pide administrador' 'conectar-cliente.ps1' {
    $ruta = Get-RutaGuion 'conectar-cliente.ps1'
    $tok = $null; $err = $null
    $ast = [System.Management.Automation.Language.Parser]::ParseFile($ruta, [ref]$tok, [ref]$err)
    $cadenas = @($ast.FindAll({ param($n) $n -is [System.Management.Automation.Language.StringConstantExpressionAst] }, $true) | ForEach-Object { $_.Value })
    foreach ($ancla in @('pair', 'install', 'client')) {
        Assert-Cierto ($cadenas -contains $ancla) ('conectar-cliente.ps1 no llama a bridge {0} / setup client: falta la cadena "{0}"' -f $ancla)
    }
    if ($ast.ScriptRequirements -and $ast.ScriptRequirements.IsElevationRequired) { throw 'conectar-cliente.ps1 lleva #Requires -RunAsAdministrator' }
    $prohibidos = @('New-NetFirewallRule', 'Set-NetFirewallRule', 'Register-ScheduledTask', 'New-NetIPAddress', 'Set-NetConnectionProfile', 'Set-ExecutionPolicy', 'icacls', 'icacls.exe', 'netsh', 'netsh.exe', 'fltmc')
    foreach ($cmd in @($ast.FindAll({ param($n) $n -is [System.Management.Automation.Language.CommandAst] }, $true))) {
        $nombre = $cmd.GetCommandName()
        if ($nombre -and ($prohibidos -contains $nombre)) { throw ('conectar-cliente.ps1 llama a {0}, que pide administrador' -f $nombre) }
        $texto = $cmd.Extent.Text
        if ($texto -match '(?i)-Verb\s+[''"]?RunAs') { throw ('conectar-cliente.ps1 se eleva: {0}' -f $texto) }
    }
    foreach ($c in $cadenas) {
        if ($c -match '(?i)ProgramData|HKLM:|RunAs') { throw ('conectar-cliente.ps1 usa "{0}", que es de administrador' -f $c) }
    }
}

Test-Contrato 'D1 CONECTAR-A-LA-MADRE.cmd lanza conectar-cliente.ps1 sin elevarse' $null {
    $ruta = Get-RutaGuion 'CONECTAR-A-LA-MADRE.cmd'
    $b = [System.IO.File]::ReadAllBytes($ruta)
    Assert-Ascii $b 'CONECTAR-A-LA-MADRE.cmd'
    Assert-Crlf $b 'CONECTAR-A-LA-MADRE.cmd'
    $t = [System.Text.Encoding]::ASCII.GetString($b)
    Assert-Cierto ($t -match '(?im)^\s*powershell(\.exe)?\s+-NoProfile\s+-ExecutionPolicy\s+Bypass\s+-File\s+"%~dp0conectar-cliente\.ps1"') 'CONECTAR-A-LA-MADRE.cmd no lanza powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0conectar-cliente.ps1"'
    foreach ($x in @('RunAs', 'fltmc', 'ProgramData', 'icacls', 'elevado')) { Assert-NoContiene $t $x 'CONECTAR-A-LA-MADRE.cmd no pide administrador' }
    Assert-Cierto ($t -notmatch '(?im)^\s*:\w|\bgoto\b') 'CONECTAR-A-LA-MADRE.cmd usa etiquetas o goto'
}

# ===========================================================================
# Guarda: lo que corre en Windows lo tiene que leer Windows PowerShell 5.1
# ===========================================================================

foreach ($g in @('ensayo-madre.ps1', 'aviso-madre.ps1', 'conectar-cliente.ps1')) {
    Test-Contrato ('D6 {0} lo lee Windows PowerShell 5.1 (ASCII, sin errores, nada de PS7)' -f $g) $null {
        $ruta = Get-RutaGuion $g
        Assert-Ascii ([System.IO.File]::ReadAllBytes($ruta)) $g
        $tok = $null; $err = $null
        $ast = [System.Management.Automation.Language.Parser]::ParseFile($ruta, [ref]$tok, [ref]$err)
        Assert-Igual 0 @($err).Count ('errores de analisis de {0}' -f $g)
        $ps7 = @($ast.FindAll({
                    param($n)
                    $tipo = $n.GetType().Name
                    if ($tipo -eq 'TernaryExpressionAst' -or $tipo -eq 'PipelineChainAst') { return $true }
                    if ($n -is [System.Management.Automation.Language.BinaryExpressionAst] -and [string]$n.Operator -eq 'QuestionQuestion') { return $true }
                    if ($n -is [System.Management.Automation.Language.AssignmentStatementAst] -and [string]$n.Operator -eq 'QuestionQuestionEquals') { return $true }
                    if (($n -is [System.Management.Automation.Language.MemberExpressionAst] -or $n -is [System.Management.Automation.Language.IndexExpressionAst]) -and $n.PSObject.Properties['NullConditional'] -and $n.NullConditional) { return $true }
                    if ($n -is [System.Management.Automation.Language.CommandParameterAst] -and @('AsByteStream', 'AsHashtable', 'SkipHttpErrorCheck', 'SkipCertificateCheck', 'Parallel') -contains $n.ParameterName) { return $true }
                    if ($n -is [System.Management.Automation.Language.CommandAst] -and $n.GetCommandName() -eq 'Join-Path' -and @($n.CommandElements | Where-Object { $_ -isnot [System.Management.Automation.Language.CommandParameterAst] }).Count -gt 3) { return $true }
                    return $false
                }, $true) | ForEach-Object { 'linea {0}: {1}' -f $_.Extent.StartLineNumber, $_.Extent.Text })
        Assert-Igual @() $ps7 ('sintaxis de PowerShell 7 en {0}' -f $g)
    }
}

# ===========================================================================
# D4 . empaquetar-visita.sh: tres paquetes, y CLIENTE sin nada de administrador
# ===========================================================================

function Invoke-EmpaquetadorDePrueba {
    # Copia el arbol que el empaquetador lee a un directorio temporal y lo corre alli con un
    # .exe falso (empieza por MZ y es mas nuevo que las fuentes falsas, de 2020). Sin git en
    # la copia, el empaquetador mira las fechas de los ficheros. `Sabotaje` cambia la copia.
    param([scriptblock]$Sabotaje)
    foreach ($h in @('bash', 'python3', 'sha256sum')) {
        if ($null -eq (Get-Command $h -CommandType Application -ErrorAction SilentlyContinue)) { throw ('falta {0}: el empaquetador lo necesita' -f $h) }
    }
    $tmp = New-DirTemporal
    $dScripts = Join-Path $tmp 'scripts'
    $dDocs = Join-Path $tmp 'docs'
    $dSrc = Join-Path (Join-Path $tmp 'rust') 'src'
    [void](New-Item -ItemType Directory -Path $dScripts, $dDocs, $dSrc -Force)
    Get-ChildItem -LiteralPath $script:DirScripts -File | Copy-Item -Destination $dScripts
    foreach ($d in @('visita-pc-madre.md', 'despliegue-lan.md')) { Copy-Item -LiteralPath (Join-Path (Join-Path $script:DirRaiz 'docs') $d) -Destination $dDocs }
    $viejo = Get-Date '2020-01-01'
    foreach ($f in @((Join-Path $dSrc 'lib.rs'), (Join-Path (Join-Path $tmp 'rust') 'Cargo.toml'), (Join-Path (Join-Path $tmp 'rust') 'Cargo.lock'))) {
        [System.IO.File]::WriteAllText($f, 'x')
        (Get-Item -LiteralPath $f).LastWriteTime = $viejo
    }
    $exe = Join-Path $tmp 'memory-industry.exe'
    $bytesExe = New-Object byte[] 64
    $bytesExe[0] = 0x4D; $bytesExe[1] = 0x5A
    [System.IO.File]::WriteAllBytes($exe, $bytesExe)
    if ($Sabotaje) { & $Sabotaje $dScripts }
    $previo = $env:MEMORY_INDUSTRY_WIN_EXE
    # Lo que el empaquetador escribe en stderr es parte de la salida que se mira, no un
    # error de este contrato.
    $ErrorActionPreference = 'Continue'
    try {
        $env:MEMORY_INDUSTRY_WIN_EXE = $exe
        $salida = (& bash (Join-Path $dScripts 'empaquetar-visita.sh') 2>&1 | Out-String)
        $codigo = $LASTEXITCODE
    }
    finally { $env:MEMORY_INDUSTRY_WIN_EXE = $previo }
    return [pscustomobject]@{ Raiz = $tmp; Codigo = $codigo; Salida = $salida; Dist = (Join-Path $tmp 'dist-visita'); Exe = $bytesExe }
}

function Read-Zip {
    # Nombre -> bytes de cada entrada.
    param([string]$Ruta)
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $res = @{}
    $z = [System.IO.Compression.ZipFile]::OpenRead($Ruta)
    try {
        foreach ($e in $z.Entries) {
            $s = $e.Open()
            $m = New-Object System.IO.MemoryStream
            try { $s.CopyTo($m) } finally { $s.Dispose() }
            $res[$e.FullName] = $m.ToArray()
        }
    }
    finally { $z.Dispose() }
    return $res
}

function Get-ClavesOrdenadas {
    # Orden ordinal: no depende de la cultura del equipo que corre el contrato.
    param([hashtable]$Tabla)
    [string[]]$k = @($Tabla.Keys)
    [System.Array]::Sort($k, [System.StringComparer]::Ordinal)
    return , $k
}

Test-Contrato 'D4 el paquete CLIENTE lleva solo CONECTAR, conectar-cliente.ps1, el .exe y LEEME' $null {
    $r = Invoke-EmpaquetadorDePrueba
    try {
        Assert-Igual 0 $r.Codigo ('el empaquetador termino con error: {0}' -f $r.Salida)
        $zip = Join-Path $r.Dist 'memory-industry-0.29-CLIENTE.zip'
        Assert-Cierto (Test-Path -LiteralPath $zip) ('no se genero memory-industry-0.29-CLIENTE.zip. Salida: {0}' -f $r.Salida)
        $c = Read-Zip $zip
        $p = 'memory-industry-0.29-CLIENTE/'
        Assert-Igual @(($p + 'CONECTAR-A-LA-MADRE.cmd'), ($p + 'LEEME.txt'), ($p + 'conectar-cliente.ps1'), ($p + 'memory-industry.exe')) (Get-ClavesOrdenadas $c) 'contenido del zip CLIENTE'
        Assert-Igual ([System.BitConverter]::ToString($r.Exe)) ([System.BitConverter]::ToString($c[$p + 'memory-industry.exe'])) 'el .exe es el indicado'
        Assert-Crlf $c[$p + 'CONECTAR-A-LA-MADRE.cmd'] 'CONECTAR-A-LA-MADRE.cmd dentro del zip'
        $leeme = $c[$p + 'LEEME.txt']
        Assert-Ascii $leeme 'LEEME.txt del CLIENTE'
        Assert-Crlf $leeme 'LEEME.txt del CLIENTE'
        $tl = [System.Text.Encoding]::ASCII.GetString($leeme)
        Assert-Cierto (@($tl -split "`r`n" | Where-Object { $_.Trim() }).Count -le 6) ('LEEME.txt del CLIENTE pasa de seis lineas: {0}' -f $tl)
        Assert-Contiene $tl 'CONECTAR-A-LA-MADRE.cmd' 'LEEME.txt del CLIENTE'
        Assert-NoContiene $tl 'administrador' 'LEEME.txt del CLIENTE no habla de administrador'
    }
    finally { Remove-Item -LiteralPath $r.Raiz -Recurse -Force -ErrorAction SilentlyContinue }
}

Test-Contrato 'D4 EQUIPO-REAL y ENSAYO llevan aviso-madre.ps1; EQUIPO-REAL tambien GENERAR-CODIGO y ya no pide la IP' $null {
    $r = Invoke-EmpaquetadorDePrueba
    try {
        Assert-Igual 0 $r.Codigo ('el empaquetador termino con error: {0}' -f $r.Salida)
        $real = Read-Zip (Join-Path $r.Dist 'memory-industry-0.29-EQUIPO-REAL.zip')
        $p = 'memory-industry-0.29-EQUIPO-REAL/'
        foreach ($n in @('aviso-madre.ps1', 'GENERAR-CODIGO.cmd')) { Assert-Cierto ($real.ContainsKey($p + $n)) ('EQUIPO-REAL no lleva {0}' -f $n) }
        Assert-Crlf $real[$p + 'GENERAR-CODIGO.cmd'] 'GENERAR-CODIGO.cmd dentro del zip'
        $sumas = [System.Text.Encoding]::ASCII.GetString($real[$p + 'SHA256SUMS.txt'])
        foreach ($n in @('aviso-madre.ps1', 'GENERAR-CODIGO.cmd')) { Assert-Contiene $sumas ('  ' + $n) 'SHA256SUMS.txt de EQUIPO-REAL (la prueba de humo compara con el)' }
        $leeme = [System.Text.Encoding]::ASCII.GetString($real[$p + 'LEEME.txt'])
        Assert-NoContiene $leeme 'direccion IP' 'LEEME.txt de EQUIPO-REAL: la IP ya no se elige'
        $ensayo = Read-Zip (Join-Path $r.Dist 'memory-industry-0.29-ENSAYO.zip')
        Assert-Cierto ($ensayo.ContainsKey('memory-industry-0.29-ENSAYO/aviso-madre.ps1')) 'ENSAYO no lleva aviso-madre.ps1, y su fase Antes registra la tarea del aviso'
    }
    finally { Remove-Item -LiteralPath $r.Raiz -Recurse -Force -ErrorAction SilentlyContinue }
}

foreach ($g in @('aviso-madre.ps1', 'conectar-cliente.ps1')) {
    Test-Contrato ('D4 el empaquetador se niega si {0} no es ASCII' -f $g) $null {
        $r = Invoke-EmpaquetadorDePrueba {
            param($d)
            $f = Join-Path $d $g
            [System.IO.File]::AppendAllText($f, ('# caf' + [char]0xE9 + "`n"), (New-Object System.Text.UTF8Encoding($false)))
        }
        try {
            Assert-Cierto ($r.Codigo -ne 0) ('empaqueto un {0} con un byte fuera de ASCII. Salida: {1}' -f $g, $r.Salida)
            Assert-Contiene $r.Salida 'NO SE EMPAQUETA' 'el motivo'
            Assert-Contiene $r.Salida $g 'el motivo nombra el fichero'
        }
        finally { Remove-Item -LiteralPath $r.Raiz -Recurse -Force -ErrorAction SilentlyContinue }
    }
}

Test-Contrato 'D4 el empaquetador se niega si conectar-cliente.ps1 no pasa el analizador' $null {
    $r = Invoke-EmpaquetadorDePrueba {
        param($d)
        [System.IO.File]::AppendAllText((Join-Path $d 'conectar-cliente.ps1'), "function Roto {`n")
    }
    try {
        Assert-Cierto ($r.Codigo -ne 0) ('empaqueto un conectar-cliente.ps1 que no parsea. Salida: {0}' -f $r.Salida)
        Assert-Contiene $r.Salida 'conectar-cliente.ps1' 'el motivo nombra el fichero'
    }
    finally { Remove-Item -LiteralPath $r.Raiz -Recurse -Force -ErrorAction SilentlyContinue }
}

# ===========================================================================
# D5 . documentacion
# ===========================================================================

Test-Contrato 'D5 la hoja de la visita no tiene el paso manual de IP y trae los pasos de cliente' $null {
    $t = [System.IO.File]::ReadAllText((Join-Path (Join-Path $script:DirRaiz 'docs') 'visita-pc-madre.md'))
    Assert-Cierto ($t -notmatch '(?m)^\d+\.\s+\*\*IP fija') 'docs/visita-pc-madre.md conserva el paso "IP fija" manual'
    foreach ($x in @('GENERAR-CODIGO.cmd', 'CONECTAR-A-LA-MADRE.cmd', 'memory-industry-0.29-CLIENTE.zip')) { Assert-Contiene $t $x 'docs/visita-pc-madre.md' }
}

Test-Contrato 'D5 la guia LAN explica el modo automatico y el descubrimiento' $null {
    $t = [System.IO.File]::ReadAllText((Join-Path (Join-Path $script:DirRaiz 'docs') 'despliegue-lan.md'))
    Assert-Cierto ($t -match '(?im)^#+ .*automatic') 'docs/despliegue-lan.md no tiene una seccion del modo automatico'
    foreach ($x in @('CUBA_HTTP_ADDR=lan:8787', 'UDP', 'CONECTAR-A-LA-MADRE.cmd')) { Assert-Contiene $t $x 'docs/despliegue-lan.md' }
}

# ---------------------------------------------------------------------------
$fallan = @($script:Resultados | Where-Object { $_.Resultado -ne 'PASA' })
Write-Host ''
Write-Host ('{0} contratos, {1} pasan, {2} fallan' -f $script:Resultados.Count, ($script:Resultados.Count - $fallan.Count), $fallan.Count)
if ($fallan.Count -gt 0) { exit 1 }
exit 0
