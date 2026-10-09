# Ensayo de la instalacion de Windows de MemoryIndustry 0.29 (tarea programada +
# regla de firewall + arranque sin sesion). Sirve para ensayarlo en una PC propia
# y para repetirlo en el equipo real con el menor tiempo posible.
#
#   ENSAYO-se-desinstala-al-final.cmd   (doble clic, PC de ensayo)  o  INSTALAR-EQUIPO-REAL.cmd
#                                    (hacen todo lo de abajo con UN reinicio; ver LEEME.txt)
#   .\ensayo-madre.ps1              (= -Fase Todo; sin -Conservar retira todo al final)
#   .\ensayo-madre.ps1 -Fase Humo   (prueba de 2 minutos, solo lectura, no instala nada)
#
# Cada fase tambien se puede usar por separado:
#   .\ensayo-madre.ps1 -Fase Antes   [-Ip 192.168.0.10] [-Puerto 8797]   (consola de administrador)
#   (reiniciar desde el menu de Windows: Reiniciar, no Apagar)
#   .\ensayo-madre.ps1 -Fase Despues                                      (consola de administrador)
#   .\ensayo-madre.ps1 -Fase Limpiar [-BorrarEnv]                         (solo en el ensayo)
#
# Compatible con Windows PowerShell 5.1 (el equipo real puede no tener PowerShell 7).
# El fichero es ASCII puro, sin BOM y sin acentos: 5.1 lee un .ps1 sin BOM con la
# pagina de codigos ANSI, y cualquier byte fuera de ASCII se leeria distinto en cada
# equipo. Los textos que escribe (informe.txt) salen en UTF-8; los JSON, en UTF-8
# sin BOM.
#
# Que NO hace: no toca la tarea ni el firewall con comandos propios; los crea el
# propio memory-industry.exe (`setup service --apply --register`) y se retiran con
# lo que imprime `setup service --uninstall`. El ensayo solo mide y comprueba.
#
# Nunca escribe ni imprime el token: de el solo salen su LARGO y una huella SHA-256
# para comparar antes y despues del reinicio.
[CmdletBinding()]
param(
    # 'Humo' es una prueba de solo lectura (y el primer paso de Todo). 'Todo' es la fase
    # por omision: humo, Antes, un reinicio y, al volver a iniciar
    # sesion, Despues y Limpiar solas. 'Final' es lo que ejecuta la tarea de inicio de
    # sesion que Todo arma. 'Arranque' no es una fase para el operador: es lo que ejecuta la tarea
    # memory-industry-ensayo-reinicio al arrancar el equipo, antes de que nadie
    # inicie sesion. Vive en este mismo fichero para que el paquete lleve uno solo.
    [ValidateSet('Todo', 'Humo', 'Antes', 'Despues', 'Limpiar', 'Arranque', 'Final')]
    [string]$Fase = 'Todo',
    [string]$Binario,
    [string]$Ip,
    # 8797: lejos de 8787/8788, que la guia ya avisa que pueden estar ocupados en
    # una estacion con otros daemons o con el editor.
    [ValidateRange(1024, 65535)]
    [int]$Puerto = 8797,
    [string]$Salida,
    # Limpiar conserva el env file (lleva el token que los clientes ya usan) salvo
    # que se pida lo contrario: en el equipo real un Limpiar por error no debe
    # dejar a todos los clientes sin token.
    [switch]$BorrarEnv,
    # Pasar por encima de la guarda de "esta instalacion ya existia antes del ensayo".
    [switch]$Forzar,
    # En Despues: matar el daemon otra vez, ahora que la repeticion de 5 minutos del
    # BootTrigger ya corre (antes del primer reinicio no puede estar activa).
    [switch]$MatarTrasReinicio,
    # En Todo: al terminar NO retirar lo instalado (el equipo real se queda con el
    # servicio; el ensayo en una PC propia lo retira).
    [switch]$Conservar
)

# ---------------------------------------------------------------------------
# Constantes. Los nombres y ficheros son los del producto (rust/src/service.rs:
# TASK_NAME, FIREWALL_RULE_NAME, ENV_BASENAME, LAUNCHER_BASENAME, TASK_XML_BASENAME,
# LOG_BASENAME). Si cambian alla, `Antes` lo ve rojo en la primera comprobacion de
# ficheros; no se leen del binario porque un ensayo que confia en lo que mide no
# ensaya nada.
# ---------------------------------------------------------------------------
$script:NombreTarea = 'memory-industry'
$script:NombreRegla = 'memory-industry'
$script:TareaReinicio = 'memory-industry-ensayo-reinicio'
$script:TareaDespues = 'memory-industry-ensayo-despues'
$script:ArchivoSumas = 'SHA256SUMS.txt'
$script:ArchivoEnv = 'memory-industry.env'
$script:ArchivoLauncher = 'memory-industry.cmd'
$script:ArchivoXml = 'memory-industry-task.xml'
$script:ArchivoLog = 'memory-industry.log'
$script:TokenMinimo = 32
$script:ClaveDireccion = 'CUBA_HTTP_ADDR'
$script:ClaveToken = 'CUBA_HTTP_TOKEN'

# `serve` abre el puerto y solo despues calienta los modelos (hasta 180 s por
# defecto) antes de aceptar: el puerto aparece antes que el primer /health. 360 s =
# 180 s de calentamiento + conexion a la base + margen.
$script:LimiteArranqueSeg = 360
# 5 min de la repeticion del BootTrigger + margen.
$script:LimiteReaparicionSeg = 330
# RestartOnFailure reintenta a los PT1M: hasta ~100 s es coherente con el; mas
# tarde apunta a la repeticion de 5 min.
$script:UmbralReinicioRapidoSeg = 100
# M1 paso 5: tras el reinicio, el /health responde a <= 5 min.
$script:UmbralArranqueSistemaSeg = 300
$script:EsperaArranqueMin = 10
# Cuanto espera la fase Final a que la comprobacion del arranque termine su medicion:
# su vigilancia (EsperaArranqueMin) mas un minuto de margen.
$script:EsperaMedicionSeg = ($script:EsperaArranqueMin + 1) * 60
$script:TimeoutSaludMs = 4000

$script:Checks = New-Object System.Collections.ArrayList
$script:Lineas = New-Object System.Collections.ArrayList
$script:Codigo = 0
$script:Ctx = $null
$script:ModoTodo = $false

# ===========================================================================
# Funciones puras o casi puras: se prueban sin Windows (ver el final del fichero).
# ===========================================================================

function ConvertTo-ArgString {
    # ProcessStartInfo.Arguments es un solo texto (5.1 no tiene ArgumentList). Las
    # reglas son las de CommandLineToArgvW: las barras invertidas solo cuentan
    # delante de una comilla, o al final de un argumento que se entrecomilla.
    param([string[]]$Argumentos)
    $partes = New-Object System.Collections.ArrayList
    foreach ($a in $Argumentos) {
        if ($null -eq $a -or $a.Length -eq 0) { [void]$partes.Add('""'); continue }
        if ($a -notmatch '[\s"]') { [void]$partes.Add($a); continue }
        $s = $a -replace '(\\*)"', '$1$1\"'
        $s = $s -replace '(\\+)$', '$1$1'
        [void]$partes.Add('"' + $s + '"')
    }
    return ($partes -join ' ')
}

function Get-ValorEnv {
    # Misma regla que service_apply::env_value: gana la ultima linea efectiva, una
    # linea con # no cuenta, y la clave tiene que ser el nombre entero.
    param([string]$Texto, [string]$Clave)
    $valor = $null
    foreach ($linea in ($Texto -split "`n")) {
        $l = $linea.TrimStart()
        if ($l.StartsWith('#')) { continue }
        $i = $l.IndexOf('=')
        if ($i -lt 0) { continue }
        if ($l.Substring(0, $i).Trim() -eq $Clave) { $valor = $l.Substring($i + 1).Trim() }
    }
    return $valor
}

function Get-HashTexto {
    param([string]$Texto)
    $sha = [System.Security.Cryptography.SHA256]::Create()
    try { $bytes = $sha.ComputeHash([System.Text.Encoding]::UTF8.GetBytes($Texto)) }
    finally { $sha.Dispose() }
    return (($bytes | ForEach-Object { $_.ToString('x2') }) -join '')
}

function Get-HashToken {
    # Huella de la linea del token ya normalizada (CLAVE=valor sin espacios ni CR):
    # asi un cambio de saltos de linea no cuenta como "token distinto".
    param([string]$TextoEnv)
    $valor = Get-ValorEnv $TextoEnv $script:ClaveToken
    if ([string]::IsNullOrEmpty($valor)) { return $null }
    return (Get-HashTexto ($script:ClaveToken + '=' + $valor))
}

function Test-IpPrivada {
    param([string]$Direccion)
    $ip = $null
    if (-not [System.Net.IPAddress]::TryParse($Direccion, [ref]$ip)) { return $false }
    if ($ip.AddressFamily -ne [System.Net.Sockets.AddressFamily]::InterNetwork) { return $false }
    $b = $ip.GetAddressBytes()
    if ($b[0] -eq 10) { return $true }
    if ($b[0] -eq 172 -and $b[1] -ge 16 -and $b[1] -le 31) { return $true }
    if ($b[0] -eq 192 -and $b[1] -eq 168) { return $true }
    return $false
}

function Select-IpPrivada {
    # Candidatos: {Ip, Alias, TieneGateway, Virtual}. Una pasarela predeterminada
    # distingue la tarjeta real de las que crean WSL, Hyper-V o Docker, que tambien
    # son privadas pero a las que nadie de la LAN llega.
    param($Candidatos)
    $privados = @($Candidatos | Where-Object { Test-IpPrivada $_.Ip })
    foreach ($filtro in @(
            { $_.TieneGateway -and -not $_.Virtual },
            { -not $_.Virtual },
            { $_.TieneGateway },
            { $true })) {
        $hallado = @($privados | Where-Object $filtro)
        if ($hallado.Count -gt 0) { return $hallado[0] }
    }
    return $null
}

function Select-Escucha {
    # `Direcciones` son las LocalAddress de los sockets en LISTEN en el puerto.
    param([string[]]$Direcciones, [string]$Ip)
    $enIp = @($Direcciones | Where-Object { $_ -eq $Ip }).Count -gt 0
    $loop = @($Direcciones | Where-Object { $_ -eq '127.0.0.1' -or $_ -eq '::1' }).Count -gt 0
    $todas = @($Direcciones | Where-Object { $_ -eq '0.0.0.0' -or $_ -eq '::' }).Count -gt 0
    return [pscustomobject]@{
        EnIp         = $enIp
        Loopback     = $loop
        Todas        = $todas
        SoloLoopback = ($loop -and -not $enIp -and -not $todas)
    }
}

function Protect-Secreto {
    # Todo texto que viene de un comando o de un log pasa por aqui antes de tocar
    # un fichero: el log del daemon o la salida de un comando podrian llevar el token.
    param([string]$Texto, [string[]]$Secretos)
    if ([string]::IsNullOrEmpty($Texto)) { return $Texto }
    foreach ($s in $Secretos) {
        if ($s -and $s.Length -ge 8) { $Texto = $Texto.Replace($s, '<redactado>') }
    }
    $Texto = [regex]::Replace($Texto, '(?i)(CUBA_HTTP_TOKEN\s*=\s*)\S+', '$1<redactado>')
    $Texto = [regex]::Replace($Texto, '(?i)(Bearer\s+)\S+', '$1<redactado>')
    # La clave de una URL de base de datos: postgresql://usuario:CLAVE@servidor/...
    $Texto = [regex]::Replace($Texto, '(?i)(://[^:/\s]+:)[^@\s]+@', '$1<redactado>@')
    return [regex]::Replace($Texto, '\b[0-9A-Fa-f]{32,}\b', '<redactado-hex>')
}

function Test-LineaError {
    # Lo mismo que doctor::is_error_line: una linea ERROR de tracing (el log es JSON),
    # o lo que imprimen el launcher ("memory-industry: ...") y main ("serve: ...")
    # antes de que exista tracing. Bajo SYSTEM no hay consola: esas lineas son la
    # unica razon visible de un arranque fallido.
    param([string]$Linea)
    $l = $Linea.Trim()
    if ($l.Length -eq 0) { return $false }
    if ($l.StartsWith('serve: ') -or $l.StartsWith('memory-industry:') -or $l.StartsWith('[MemoryIndustry]')) { return $true }
    if ($l.StartsWith('{')) {
        try { $o = $l | ConvertFrom-Json; return ($o.level -eq 'ERROR') } catch { return $false }
    }
    return ($l -cmatch '\bERROR\b')
}

function Get-LineasError {
    param([string]$Texto)
    if ([string]::IsNullOrEmpty($Texto)) { return @() }
    return @(($Texto -split "`r?`n") | Where-Object { Test-LineaError $_ })
}

function Get-ColaLog {
    # Los bytes del log desde `Desde` (o los ultimos `Maximo` si hay mas), leidos
    # con FileShare.ReadWrite: el daemon lo tiene abierto para escribir. Si el
    # fichero es mas corto que `Desde`, roto (5 MiB) y se lee desde el principio.
    param([string]$Ruta, [long]$Desde = 0, [long]$Maximo = 65536)
    if (-not (Test-Path -LiteralPath $Ruta)) { return [pscustomobject]@{ Existe = $false; Texto = ''; Longitud = 0 } }
    $fs = New-Object System.IO.FileStream($Ruta, [System.IO.FileMode]::Open, [System.IO.FileAccess]::Read, [System.IO.FileShare]::ReadWrite)
    try {
        $largo = $fs.Length
        $inicio = $Desde
        if ($inicio -gt $largo) { $inicio = 0 }
        if (($largo - $inicio) -gt $Maximo) { $inicio = $largo - $Maximo }
        [void]$fs.Seek($inicio, [System.IO.SeekOrigin]::Begin)
        $buf = New-Object byte[] ([int]($largo - $inicio))
        $leido = 0
        while ($leido -lt $buf.Length) {
            $n = $fs.Read($buf, $leido, $buf.Length - $leido)
            if ($n -le 0) { break }
            $leido += $n
        }
        $texto = [System.Text.Encoding]::UTF8.GetString($buf, 0, $leido)
        return [pscustomobject]@{ Existe = $true; Texto = $texto; Longitud = $largo }
    }
    finally { $fs.Dispose() }
}

function ConvertTo-Offset {
    # ConvertFrom-Json devuelve un DateTime en PowerShell 7 y un texto en 5.1: se
    # acepta cualquiera de los dos.
    param($Valor)
    if ($null -eq $Valor -or "$Valor" -eq '') { return $null }
    if ($Valor -is [DateTimeOffset]) { return $Valor }
    if ($Valor -is [DateTime]) { return [DateTimeOffset]$Valor }
    return [DateTimeOffset]::Parse([string]$Valor, [System.Globalization.CultureInfo]::InvariantCulture)
}

function Get-AhoraTexto { return [DateTimeOffset]::Now.ToString('o') }

function Get-SegundosEntre {
    param($Desde, $Hasta)
    $a = ConvertTo-Offset $Desde
    $b = ConvertTo-Offset $Hasta
    if ($null -eq $a -or $null -eq $b) { return $null }
    return [math]::Round(($b - $a).TotalSeconds, 1)
}

function Format-Seg {
    param($Valor)
    if ($null -eq $Valor) { return 'n/d' }
    return ([double]$Valor).ToString('0.0', [System.Globalization.CultureInfo]::InvariantCulture) + ' s'
}

function Format-Check {
    param($Check)
    return ('[{0,-5}] {1,-28} {2}' -f $Check.resultado, $Check.id, $Check.detalle)
}

function Get-Veredicto {
    # Un AVISO no tumba el ensayo; un FALLA si.
    param($Checks)
    if (@($Checks | Where-Object { $_.resultado -eq 'FALLA' }).Count -gt 0) { return 'FALLA' }
    return 'PASA'
}

function Get-ClasificacionReaparicion {
    # `Segundos` desde que se mato el proceso hasta que el /health volvio a
    # responder. Devuelve el resultado del check y la lectura honesta.
    param($Segundos, $RepeticionActiva)
    if ($null -eq $Segundos) {
        return [pscustomobject]@{ Resultado = 'FALLA'; Lectura = ('no reaparecio en {0} s: ni RestartOnFailure ni la repeticion levantaron el daemon' -f $script:LimiteReaparicionSeg) }
    }
    if ($Segundos -le $script:UmbralReinicioRapidoSeg) {
        $q = if ($RepeticionActiva) { 'no se distingue entre RestartOnFailure (PT1M) y la repeticion de 5 min, que ya corre' } else { 'la repeticion del BootTrigger aun no corre (no hubo reinicio desde que se registro la tarea): lo levanto RestartOnFailure' }
        return [pscustomobject]@{ Resultado = 'PASA'; Lectura = $q }
    }
    return [pscustomobject]@{ Resultado = 'AVISO'; Lectura = 'mas lento que el minuto de RestartOnFailure: o RestartOnFailure no reacciono al .cmd que sale con codigo != 0 y lo levanto la repeticion de 5 min, o el daemon tardo en abrirse. Anotar como hallazgo' }
}

function Get-HaySesion {
    # explorer.exe en una sesion > 0 es la huella de un escritorio interactivo;
    # quser lista las sesiones (no existe en Windows Home: $null); y el usuario de la
    # consola viene de Win32_ComputerSystem. Win32_LogonSession NO sirve: UMFD y DWM
    # figuran como inicios de sesion interactivos hasta en la pantalla de bloqueo.
    param($Explorer, $Quser, [string]$UsuarioConsola)
    if (($null -ne $Explorer) -and ([int]$Explorer -gt 0)) { return $true }
    if (($null -ne $Quser) -and ([int]$Quser -gt 0)) { return $true }
    if (-not [string]::IsNullOrWhiteSpace($UsuarioConsola)) { return $true }
    if (($null -eq $Explorer) -and ($null -eq $Quser) -and ([string]::IsNullOrWhiteSpace($UsuarioConsola))) { return $null }
    return $false
}

function Get-ChecksDespues {
    # Todo el veredicto de la fase Despues sale de aqui, a partir de datos ya
    # leidos: se prueba sin reiniciar nada.
    param($Antes, $Arranque, $Actual, [double]$UmbralSeg)
    $c = New-Object System.Collections.ArrayList
    $add = { param($id, $res, $det) [void]$c.Add([pscustomobject]@{ id = $id; resultado = $res; detalle = $det }) }

    if ($null -eq $Arranque) {
        & $add 'reinicio_registrado' 'FALLA' 'no hay despues-arranque.json: la tarea memory-industry-ensayo-reinicio no corrio (no se reinicio el equipo, o se reinicio antes de armar la comprobacion)'
    }
    elseif ($Arranque.estado -eq 'iniciado') {
        & $add 'reinicio_registrado' 'FALLA' 'la tarea de comprobacion arranco pero no termino: el equipo se reinicio o se apago antes de que el daemon respondiera o de que se agotara su espera'
    }
    else {
        & $add 'reinicio_registrado' 'PASA' ('la comprobacion corrio al arrancar el equipo ({0})' -f $Arranque.arranque_sistema)
    }

    if ($null -ne $Arranque) {
        $arme = ConvertTo-Offset $Antes.armado_en
        $boot = ConvertTo-Offset $Arranque.arranque_sistema
        if ($null -eq $boot) {
            & $add 'hubo_reinicio' 'FALLA' 'no se pudo leer la hora de arranque del sistema'
        }
        elseif ($null -ne $arme -and $boot -gt $arme) {
            & $add 'hubo_reinicio' 'PASA' 'el sistema arranco despues de armar la comprobacion'
        }
        else {
            & $add 'hubo_reinicio' 'FALLA' 'el arranque del sistema es anterior a la comprobacion armada: el resultado es de otro arranque. Con Inicio rapido, "Apagar" no reinicia el kernel: usar Reiniciar'
        }

        if ($null -ne $Arranque.segundos_arranque_hasta_health) {
            $s = [double]$Arranque.segundos_arranque_hasta_health
            if ($s -le $UmbralSeg) {
                & $add 'health_tras_reinicio' 'PASA' ('/health respondio {0} despues del arranque (limite {1} s)' -f (Format-Seg $s), $UmbralSeg)
            }
            else {
                & $add 'health_tras_reinicio' 'FALLA' ('/health respondio {0} despues del arranque, por encima del limite de {1} s' -f (Format-Seg $s), $UmbralSeg)
            }
        }
        else {
            & $add 'health_tras_reinicio' 'FALLA' 'el daemon no respondio /health durante la espera de la comprobacion'
        }

        $ses = $Arranque.sesion_al_primer_health
        $inicioProc = $null
        if ($null -ne $Arranque.proceso) { $inicioProc = ConvertTo-Offset $Arranque.proceso.inicio }
        $primera = ConvertTo-Offset $Actual.primera_sesion
        if ($Antes.autologon -eq $true) {
            & $add 'arranque_sin_sesion' 'AVISO' 'este equipo inicia sesion solo (AutoAdminLogon): no se puede demostrar el arranque sin sesion. El resto del ensayo sigue valiendo'
        }
        elseif ($null -ne $inicioProc -and $null -ne $primera) {
            # Lo que importa es que el proceso ya existia antes de la primera sesion,
            # no cuando entro el operador: asi puede iniciar sesion en cuanto quiera.
            if ($inicioProc -lt $primera) {
                & $add 'arranque_sin_sesion' 'PASA' ('el daemon arranco {0} y la primera sesion de usuario {1}' -f $inicioProc.ToString('HH:mm:ss'), $primera.ToString('HH:mm:ss'))
            }
            else {
                & $add 'arranque_sin_sesion' 'FALLA' ('el daemon arranco {0}, despues de la primera sesion de usuario ({1}): no se demuestra que arranque con nadie conectado' -f $inicioProc.ToString('HH:mm:ss'), $primera.ToString('HH:mm:ss'))
            }
        }
        elseif ($null -eq $ses) {
            & $add 'arranque_sin_sesion' 'AVISO' 'no se pudo determinar si habia una sesion de usuario cuando respondio el daemon'
        }
        elseif ($ses) {
            & $add 'arranque_sin_sesion' 'FALLA' 'habia una sesion de usuario cuando respondio el daemon: no se demuestra que arranca con nadie conectado. Repetir el reinicio sin iniciar sesion'
        }
        else {
            & $add 'arranque_sin_sesion' 'PASA' 'el daemon respondio sin ninguna sesion de usuario abierta'
        }

        $proc = $Arranque.proceso
        if ($null -eq $proc) {
            & $add 'proceso_servicio' 'AVISO' 'no se pudo identificar el proceso que escucha'
        }
        elseif ([int]$proc.sesion_id -eq 0) {
            & $add 'proceso_servicio' 'PASA' ('el daemon corre en la sesion 0 de servicios (usuario {0})' -f $proc.usuario)
        }
        else {
            & $add 'proceso_servicio' 'AVISO' ('el daemon corre en la sesion {0}, no en la 0 de servicios' -f $proc.sesion_id)
        }
    }

    if ($Actual.direccion -eq $Antes.direccion) {
        & $add 'misma_direccion' 'PASA' ('el env file sigue con {0}' -f $Antes.direccion)
    }
    else {
        & $add 'misma_direccion' 'FALLA' ('antes {0}, ahora {1}' -f $Antes.direccion, $Actual.direccion)
    }

    if ([string]::IsNullOrEmpty($Actual.token_sha256)) {
        & $add 'mismo_token' 'FALLA' 'el env file ya no tiene token'
    }
    elseif ($Actual.token_sha256 -eq $Antes.token_sha256) {
        & $add 'mismo_token' 'PASA' ('la huella del token no cambio ({0}...)' -f $Antes.token_sha256.Substring(0, 12))
    }
    else {
        & $add 'mismo_token' 'FALLA' 'la huella del token cambio: algo reescribio el token y los clientes configurados dejarian de entrar'
    }

    $errores = @($Actual.errores_log)
    if ($errores.Count -eq 0) {
        & $add 'log_sin_errores' 'PASA' 'el log no tiene lineas de error desde el final del ensayo previo'
    }
    else {
        & $add 'log_sin_errores' 'FALLA' ('{0} linea(s) de error en el log; ultima: {1}' -f $errores.Count, $errores[$errores.Count - 1])
    }

    if ($Actual.health_ahora) {
        & $add 'health_ahora' 'PASA' 'el daemon responde ahora'
    }
    else {
        & $add 'health_ahora' 'FALLA' 'el daemon no responde ahora'
    }
    return @($c)
}

function Get-LineasServicioDoctor {
    # Las lineas `service_*` de `doctor` en texto: `[  ok  ] nombre   detalle`, y
    # debajo la pista con una flecha. Se busca la pista por la sangria (>= 20
    # espacios) y no por la flecha: si la consola la decodifica mal, no se pierde.
    param([string]$Texto)
    $res = New-Object System.Collections.ArrayList
    $lineas = @($Texto -split "`r?`n")
    for ($i = 0; $i -lt $lineas.Count; $i++) {
        if ($lineas[$i] -match '^\[\s*(ok|warn|FAIL)\s*\]\s+(service_\w+)\s*(.*)$') {
            # Los grupos se copian antes de la segunda comparacion, que pisa $Matches.
            $estado = $Matches[1].ToLower()
            $nombre = $Matches[2]
            $detalle = $Matches[3].Trim()
            $pista = ''
            if (($i + 1) -lt $lineas.Count -and $lineas[$i + 1] -match '^\s{20,}\S+\s+(.*)$') { $pista = $Matches[1].Trim() }
            [void]$res.Add([pscustomobject]@{ nombre = $nombre; estado = $estado; detalle = $detalle; pista = $pista })
        }
    }
    return @($res)
}

function Get-OrdenesDesinstalar {
    # De la salida de `setup service --uninstall` toma SOLO las dos ordenes que ese
    # comando imprime para retirar la tarea y la regla. Una lista blanca exacta: lo
    # que no sea eso no se ejecuta, venga de donde venga.
    param([string]$Texto)
    $ordenes = New-Object System.Collections.ArrayList
    foreach ($linea in ($Texto -split "`r?`n")) {
        if ($linea -notmatch '^\s{2}(schtasks|netsh)\s+(.+?)\s*$') { continue }
        $programa = $Matches[1]
        $partes = @($Matches[2] -split '\s+')
        $permitida = $false
        if ($programa -eq 'schtasks' -and ($partes -join ' ') -eq ('/Delete /TN {0} /F' -f $script:NombreTarea)) { $permitida = $true }
        if ($programa -eq 'netsh' -and ($partes -join ' ') -eq ('advfirewall firewall delete rule name={0}' -f $script:NombreRegla)) { $permitida = $true }
        if ($permitida) { [void]$ordenes.Add([pscustomobject]@{ Programa = $programa; Argumentos = $partes; Linea = $linea.Trim() }) }
    }
    return @($ordenes)
}

function Read-Sumas {
    # `hash  nombre` o `hash *nombre` (sha256sum, modo texto o binario). Devuelve
    # nombre -> hash en minusculas. Un @{} de PowerShell compara las claves sin
    # distinguir mayusculas (como el sistema de ficheros de Windows), y el nombre se
    # guarda tal cual para poder abrir el fichero tambien donde si las distinguen.
    param([string]$Texto)
    $mapa = @{}
    foreach ($l in ($Texto -split "`r?`n")) {
        if ($l -match '^\s*([0-9A-Fa-f]{64})\s+\*?(.+?)\s*$') { $mapa[$Matches[2]] = $Matches[1].ToLower() }
    }
    return $mapa
}

function Compare-Sumas {
    # `Sumas` sale de Read-Sumas; `Reales` es nombre -> hash de lo que hay en disco
    # (o $null si el fichero no esta). El binario es obligatorio: un paquete cuyo
    # SHA256SUMS.txt no lo lista no demuestra nada sobre el .exe.
    param($Sumas, $Reales)
    $problemas = New-Object System.Collections.ArrayList
    if (-not $Sumas.ContainsKey('memory-industry.exe')) {
        [void]$problemas.Add('SHA256SUMS.txt no lista memory-industry.exe')
    }
    foreach ($nombre in @($Sumas.Keys | Sort-Object)) {
        $real = $null
        if ($Reales.ContainsKey($nombre)) { $real = $Reales[$nombre] }
        if ([string]::IsNullOrEmpty($real)) { [void]$problemas.Add(('falta {0}' -f $nombre)) }
        elseif ($real.ToLower() -ne $Sumas[$nombre]) { [void]$problemas.Add(('{0} no coincide con SHA256SUMS.txt (esperado {1}..., hay {2}...)' -f $nombre, $Sumas[$nombre].Substring(0, 12), $real.Substring(0, 12))) }
    }
    return @($problemas)
}

function Test-AutoLogonValor {
    # HKLM\...\Winlogon\AutoAdminLogon vale "1" cuando el equipo inicia sesion solo.
    param($Valor)
    if ($null -eq $Valor) { return $false }
    return ("$Valor".Trim() -eq '1')
}

function Test-RespuestaSi {
    param([string]$Respuesta)
    # S, s, SI, si, "si" con tilde. Vacio es NO. El acento se construye con [char] para
    # que el fichero siga siendo ASCII.
    if ($null -eq $Respuesta) { return $false }
    return ($Respuesta.Trim().ToLower() -in @('s', 'si', ('s' + [string][char]0xED)))
}

function Get-ResumenTodo {
    # Lo que se le dice a quien hace doble clic antes de preguntar por el reinicio.
    param([string]$Direccion, [bool]$AutoLogon, [bool]$Conservar, [string]$Salida, $Avisos = @(), $SesionesAjenas = @(), $Pendientes = @())
    $l = New-Object System.Collections.ArrayList
    [void]$l.Add('Primera parte lista: todo salio bien.')
    [void]$l.Add(('  - Se instalo MemoryIndustry como servicio de Windows en {0} (tarea programada y regla de firewall).' -f $Direccion))
    [void]$l.Add('  - Se comprobo que responde, que vuelve solo si se detiene y que repetir la instalacion no la rompe.')
    [void]$l.Add('')
    [void]$l.Add('Ahora hay que REINICIAR el equipo, para comprobar que el servicio arranca solo.')
    [void]$l.Add('  - Guarda lo que tengas abierto: el reinicio cierra los programas.')
    [void]$l.Add('  - Al volver, inicia sesion cuando quieras: el resultado se abre solo.')
    $aj = @($SesionesAjenas)
    if ($aj.Count -gt 0) {
        [void]$l.Add(('  - ATENCION: hay {0} sesion(es) de otras personas abiertas; el reinicio les cierra su trabajo:' -f $aj.Count))
        foreach ($x in $aj) { [void]$l.Add(('      ' + $x)) }
    }
    if (@($Pendientes).Count -gt 0) { [void]$l.Add(('  - Windows ya tenia un reinicio pendiente por: {0}.' -f (@($Pendientes) -join ', '))) }
    if ($AutoLogon) {
        [void]$l.Add('')
        [void]$l.Add('AVISO: este equipo inicia sesion solo, sin contrasena. La prueba "arranca sin que nadie inicie sesion"')
        [void]$l.Add('no podra darse por valida. Todo lo demas (arranque, respuesta, firewall, token) se comprueba igual.')
    }
    $av = @($Avisos)
    if ($av.Count -gt 0) {
        [void]$l.Add('')
        [void]$l.Add('Avisos de la primera parte (no impiden seguir):')
        foreach ($x in $av) { [void]$l.Add(('  - {0}: {1}' -f $x.id, $x.detalle)) }
    }
    [void]$l.Add('')
    if ($Conservar) { [void]$l.Add('Al terminar el servicio se queda instalado.') }
    else { [void]$l.Add(('Al terminar se retira todo lo instalado (era un ensayo). El informe se conserva en {0}' -f $Salida)) }
    return @($l)
}

function Format-Resultado {
    # RESULTADO.txt: lo unico que lee quien hizo doble clic. Sin jerga: que paso, y si
    # fallo, que fue.
    param([string]$Veredicto, $Checks, [string]$Limpieza, $Arranque, [string]$Salida, [string]$Equipo, [string]$Fecha, $AvisosAntes = @())
    $l = New-Object System.Collections.ArrayList
    [void]$l.Add(('RESULTADO DEL ENSAYO DE MEMORYINDUSTRY: {0}' -f $Veredicto))
    [void]$l.Add(('Equipo: {0}    Fecha: {1}' -f $Equipo, $Fecha))
    [void]$l.Add('')
    if ($Veredicto -eq 'PASA') {
        [void]$l.Add('En palabras simples:')
        [void]$l.Add('  El servicio se instalo, el equipo se reinicio y el servicio volvio a funcionar solo.')
        $seg = $null
        if ($null -ne $Arranque) { $seg = $Arranque.segundos_arranque_hasta_health }
        if ($null -ne $seg) { [void]$l.Add(('  Respondio {0} despues de arrancar el equipo.' -f (Format-Seg $seg))) }
    }
    else {
        [void]$l.Add('Algo no salio como se esperaba. Lo que fallo:')
        foreach ($c in @($Checks | Where-Object { $_.resultado -eq 'FALLA' })) { [void]$l.Add(('  - {0}: {1}' -f $c.id, $c.detalle)) }
    }
    $avisos = @($Checks | Where-Object { $_.resultado -eq 'AVISO' })
    if ($avisos.Count -gt 0) {
        [void]$l.Add('')
        [void]$l.Add('Avisos (no cambian el resultado):')
        foreach ($c in $avisos) { [void]$l.Add(('  - {0}: {1}' -f $c.id, $c.detalle)) }
    }
    $aa = @($AvisosAntes)
    if ($aa.Count -gt 0) {
        [void]$l.Add('')
        [void]$l.Add('Avisos de la primera parte (la instalacion):')
        foreach ($c in $aa) { [void]$l.Add(('  - {0}: {1}' -f $c.id, $c.detalle)) }
    }
    [void]$l.Add('')
    [void]$l.Add(('Limpieza: {0}' -f $Limpieza))
    [void]$l.Add('')
    [void]$l.Add('Pendiente a mano: desde OTRA maquina de la red, abrir http://<ip>:<puerto>/health. Es la unica prueba real del firewall.')
    [void]$l.Add(('Detalle completo: {0}\informe.txt' -f $Salida.TrimEnd('\', '/')))
    return (($l -join "`r`n") + "`r`n")
}

function Write-TextoArchivo {
    param([string]$Ruta, [string]$Texto, [switch]$ConBom)
    $dir = Split-Path -Parent $Ruta
    if ($dir -and -not (Test-Path -LiteralPath $dir)) { [void](New-Item -ItemType Directory -Path $dir -Force) }
    $enc = New-Object System.Text.UTF8Encoding([bool]$ConBom)
    [System.IO.File]::WriteAllText($Ruta, $Texto, $enc)
}

function Write-Json {
    # JSON sin BOM: un BOM rompe a los lectores estrictos (json.load de Python).
    param([string]$Ruta, $Objeto)
    Write-TextoArchivo $Ruta (ConvertTo-Json -InputObject $Objeto -Depth 8)
}

function Read-Json {
    param([string]$Ruta)
    if (-not (Test-Path -LiteralPath $Ruta)) { return $null }
    try { return (Get-Content -LiteralPath $Ruta -Raw -Encoding UTF8 | ConvertFrom-Json) } catch { return $null }
}

function Read-Texto {
    param([string]$Ruta)
    if (-not (Test-Path -LiteralPath $Ruta)) { return $null }
    return [System.IO.File]::ReadAllText($Ruta)
}

# ===========================================================================
# Informe en pantalla y en memoria
# ===========================================================================

function Write-Informe {
    param([string]$Texto = '', [string]$Color = '')
    [void]$script:Lineas.Add($Texto)
    if ($Color) { Write-Host $Texto -ForegroundColor $Color } else { Write-Host $Texto }
}

function Add-Check {
    param([string]$Id, [string]$Resultado, [string]$Detalle)
    $c = [pscustomobject]@{ id = $Id; resultado = $Resultado; detalle = $Detalle }
    [void]$script:Checks.Add($c)
    $color = 'Yellow'
    if ($Resultado -eq 'PASA') { $color = 'Green' }
    if ($Resultado -eq 'FALLA') { $color = 'Red' }
    Write-Informe (Format-Check $c) $color
}

function Write-Paso {
    param([string]$Titulo)
    Write-Informe ''
    Write-Informe ('== {0} ==' -f $Titulo) 'Cyan'
}

function Stop-Rechazo {
    # Salida 2: el ensayo ni empezo. No se toco nada.
    param([string]$Mensaje)
    Write-Informe ('RECHAZADO: {0}' -f $Mensaje) 'Red'
    $script:Codigo = 2
    throw 'RECHAZO'
}

# ===========================================================================
# Comandos externos y sondeos (solo Windows)
# ===========================================================================

function Invoke-Nativo {
    # Toda orden externa pasa por aqui: codigo de salida explicito, tope de tiempo y
    # nunca una espera sin limite. Un `schtasks` colgado no cuelga el ensayo.
    param(
        [string]$Exe,
        [string[]]$Argumentos = @(),
        [int]$TimeoutSeg = 60,
        [switch]$Utf8
    )
    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName = $Exe
    $psi.Arguments = ConvertTo-ArgString $Argumentos
    $psi.UseShellExecute = $false
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    $psi.CreateNoWindow = $true
    # memory-industry.exe escribe UTF-8; las herramientas de Windows, en la pagina
    # OEM de la consola (que es la que usa .NET si no se dice otra cosa).
    if ($Utf8) {
        $psi.StandardOutputEncoding = [System.Text.Encoding]::UTF8
        $psi.StandardErrorEncoding = [System.Text.Encoding]::UTF8
    }
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    try { $p = [System.Diagnostics.Process]::Start($psi) }
    catch {
        return [pscustomobject]@{ Codigo = -1; Salida = ''; Error = $_.Exception.Message; Agotado = $false; NoArranco = $true; Segundos = 0 }
    }
    $o = $p.StandardOutput.ReadToEndAsync()
    $e = $p.StandardError.ReadToEndAsync()
    if (-not $p.WaitForExit($TimeoutSeg * 1000)) {
        try { $p.Kill() } catch { }
        return [pscustomobject]@{ Codigo = -1; Salida = ''; Error = ('agotado el tiempo ({0} s)' -f $TimeoutSeg); Agotado = $true; NoArranco = $false; Segundos = [math]::Round($sw.Elapsed.TotalSeconds, 1) }
    }
    $p.WaitForExit()
    $res = [pscustomobject]@{ Codigo = $p.ExitCode; Salida = $o.Result; Error = $e.Result; Agotado = $false; NoArranco = $false; Segundos = [math]::Round($sw.Elapsed.TotalSeconds, 1) }
    $p.Dispose()
    return $res
}

function Get-Salud {
    # HttpWebRequest y no Invoke-WebRequest: con Proxy = $null no sale por el proxy
    # de la empresa (que no conoce la IP de esta LAN), y el tiempo maximo es explicito
    # tambien en 5.1, donde Invoke-WebRequest ademas necesita -UseBasicParsing.
    param([string]$Url, [int]$TimeoutMs = 4000)
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    try {
        $req = [System.Net.HttpWebRequest]::Create($Url)
        $req.Method = 'GET'
        $req.Timeout = $TimeoutMs
        $req.ReadWriteTimeout = $TimeoutMs
        $req.Proxy = $null
        $req.KeepAlive = $false
        $resp = $req.GetResponse()
        try {
            $reader = New-Object System.IO.StreamReader($resp.GetResponseStream())
            $cuerpo = $reader.ReadToEnd()
            $codigo = [int]$resp.StatusCode
        }
        finally { $resp.Close() }
        $json = $null
        try { $json = $cuerpo | ConvertFrom-Json } catch { }
        return [pscustomobject]@{ Ok = ($codigo -eq 200); Codigo = $codigo; Json = $json; Ms = $sw.ElapsedMilliseconds; Error = $null }
    }
    catch {
        return [pscustomobject]@{ Ok = $false; Codigo = 0; Json = $null; Ms = $sw.ElapsedMilliseconds; Error = $_.Exception.Message }
    }
}

function Get-CandidatosIp {
    $lista = New-Object System.Collections.ArrayList
    foreach ($ni in [System.Net.NetworkInformation.NetworkInterface]::GetAllNetworkInterfaces()) {
        if ($ni.OperationalStatus -ne [System.Net.NetworkInformation.OperationalStatus]::Up) { continue }
        if ($ni.NetworkInterfaceType -eq [System.Net.NetworkInformation.NetworkInterfaceType]::Loopback) { continue }
        if ($ni.NetworkInterfaceType -eq [System.Net.NetworkInformation.NetworkInterfaceType]::Tunnel) { continue }
        $props = $ni.GetIPProperties()
        $gateway = @($props.GatewayAddresses | Where-Object {
                $_.Address.AddressFamily -eq [System.Net.Sockets.AddressFamily]::InterNetwork -and $_.Address.ToString() -ne '0.0.0.0'
            }).Count -gt 0
        $virtual = (($ni.Name + ' ' + $ni.Description) -match 'Virtual|vEthernet|VMware|VirtualBox|Hyper-V|TAP|WSL|Docker|Loopback')
        foreach ($u in $props.UnicastAddresses) {
            if ($u.Address.AddressFamily -ne [System.Net.Sockets.AddressFamily]::InterNetwork) { continue }
            [void]$lista.Add([pscustomobject]@{ Ip = $u.Address.ToString(); Alias = $ni.Name; TieneGateway = $gateway; Virtual = $virtual })
        }
    }
    return @($lista)
}

function Get-Escuchas {
    param([int]$PuertoLocal)
    $res = New-Object System.Collections.ArrayList
    try {
        foreach ($x in @(Get-NetTCPConnection -State Listen -LocalPort $PuertoLocal -ErrorAction SilentlyContinue)) {
            [void]$res.Add([pscustomobject]@{ Direccion = [string]$x.LocalAddress; IdProceso = [int]$x.OwningProcess })
        }
    }
    catch { }
    return @($res)
}

function Get-InfoProceso {
    param([int]$IdProceso)
    try { $p = Get-CimInstance -ClassName Win32_Process -Filter ('ProcessId = {0}' -f $IdProceso) -ErrorAction Stop }
    catch { return $null }
    if (-not $p) { return $null }
    $usuario = $null
    try {
        $o = Invoke-CimMethod -InputObject $p -MethodName GetOwner -ErrorAction Stop
        if ($o.ReturnValue -eq 0) { $usuario = ('{0}\{1}' -f $o.Domain, $o.User) }
    }
    catch { }
    return [pscustomobject]@{
        IdProceso = $IdProceso
        Nombre    = $p.Name
        Ruta      = $p.ExecutablePath
        SesionId  = [int]$p.SessionId
        Inicio    = $p.CreationDate
        Usuario   = $usuario
    }
}

function Get-ArranqueSistema {
    # LastBootUpTime tarda en estar disponible justo al arrancar (WMI aun levanta):
    # se reintenta unos segundos antes de recurrir al contador de ticks.
    for ($i = 0; $i -lt 20; $i++) {
        try {
            $os = Get-CimInstance -ClassName Win32_OperatingSystem -ErrorAction Stop
            if ($os.LastBootUpTime) { return [DateTimeOffset]$os.LastBootUpTime }
        }
        catch { }
        Start-Sleep -Seconds 3
    }
    # TickCount es un Int32 que da la vuelta (negativo) a los 24,9 dias de encendido:
    # se reinterpreta sin signo en vez de convertirlo, que lanzaria un desbordamiento.
    $ms = [double][BitConverter]::ToUInt32([BitConverter]::GetBytes([int][Environment]::TickCount), 0)
    return [DateTimeOffset]::Now.AddMilliseconds(-$ms)
}

function Get-SesionesUsuario {
    $explorer = $null
    try { $explorer = @(Get-Process -Name explorer -ErrorAction SilentlyContinue | Where-Object { $_.SessionId -gt 0 }).Count } catch { }
    $quser = $null
    $q = Invoke-Nativo 'query.exe' @('user') 15
    if (-not $q.NoArranco -and -not $q.Agotado) {
        $filas = @(($q.Salida -split "`r?`n") | Where-Object { $_.Trim().Length -gt 0 })
        if ($q.Codigo -eq 0 -and $filas.Count -gt 0) { $quser = [math]::Max(0, $filas.Count - 1) } else { $quser = 0 }
    }
    $consola = ''
    try { $consola = [string](Get-CimInstance -ClassName Win32_ComputerSystem -ErrorAction Stop).UserName } catch { }
    $hay = Get-HaySesion $explorer $quser $consola
    return [pscustomobject]@{ Hay = $hay; Explorer = $explorer; Quser = $quser; ConsolaTieneUsuario = (-not [string]::IsNullOrWhiteSpace($consola)) }
}

function Get-InfoTarea {
    param([string]$Nombre)
    try {
        $t = Get-ScheduledTask -TaskName $Nombre -ErrorAction Stop
        $i = Get-ScheduledTaskInfo -TaskName $Nombre -ErrorAction Stop
        return [pscustomobject]@{ Estado = [string]$t.State; UltimaEjecucion = $i.LastRunTime; UltimoResultado = [int]$i.LastTaskResult; Cantidad = @($t).Count }
    }
    catch { return $null }
}

function Get-UsuarioActual {
    # DOMINIO\usuario de quien ejecuta esto (ya elevado): la tarea de resultado tiene que
    # ser de ESE usuario para abrir la ventana en su escritorio.
    try { return [System.Security.Principal.WindowsIdentity]::GetCurrent().Name }
    catch { return ('{0}\{1}' -f $env:USERDOMAIN, $env:USERNAME) }
}

function Get-AutoLogon {
    try {
        return (Test-AutoLogonValor (Get-ItemProperty -LiteralPath 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Winlogon' -Name AutoAdminLogon -ErrorAction Stop).AutoAdminLogon)
    }
    catch { return $false }
}

function Get-PrimeraSesion {
    # Cuando empezo el primer escritorio interactivo de este arranque: el explorer.exe
    # mas antiguo de una sesion > 0. $null si no se puede leer.
    try {
        $horas = @(Get-Process -Name explorer -ErrorAction Stop | Where-Object { $_.SessionId -gt 0 } | ForEach-Object { $_.StartTime } | Sort-Object)
        if ($horas.Count -eq 0) { return $null }
        return ([DateTimeOffset]$horas[0]).ToString('o')
    }
    catch { return $null }
}

function Get-DirTrabajo { return (Join-Path $env:ProgramData 'MemoryIndustryEnsayo') }

function Get-DiagnosticoArranque {
    # Cuando el daemon no responde, lo que el operador necesita en la misma linea:
    # que dice la tarea y cual fue la ultima linea de error del log. Bajo SYSTEM no
    # hay consola; el log es la unica explicacion.
    param($Ctx, [long]$DesdeByte, [string[]]$Secretos)
    $t = Get-InfoTarea $script:NombreTarea
    $cola = Get-ColaLog $Ctx.Log $DesdeByte 65536
    $errs = @(Get-LineasError $cola.Texto)
    $tarea = if ($t) { 'tarea {0}, ultimo resultado {1}' -f $t.Estado, $t.UltimoResultado } else { 'no se pudo leer la tarea' }
    $log = if (-not $cola.Existe) { 'no existe memory-industry.log (el launcher no llego a escribir)' } elseif ($errs.Count -gt 0) { 'ultimo error del log: ' + (Protect-Secreto $errs[$errs.Count - 1] $Secretos) } else { 'el log no tiene lineas de error' }
    $dirs = @(Get-Escuchas $Ctx.Puerto | ForEach-Object { $_.Direccion })
    $puerto = if ($dirs.Count -gt 0) { 'en el puerto {0} escucha: {1}' -f $Ctx.Puerto, ($dirs -join ', ') } else { 'nadie escucha en el puerto {0}' -f $Ctx.Puerto }
    return ('{0}; {1}; {2}' -f $tarea, $puerto, $log)
}

function Wait-Daemon {
    # Espera a que el daemon abra su puerto y responda /health. Mide los dos
    # tiempos por separado: `serve` abre el puerto y solo despues calienta, asi que
    # la distancia entre ambos es el calentamiento. `IdAnterior` excluye al proceso
    # que se mato, para no dar por vivo al que ya no esta.
    param($Ctx, [int]$LimiteSeg, [int]$IdAnterior = 0, [int]$PasoMs = 1000)
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    $tPuerto = $null
    $idNuevo = 0
    $url = 'http://{0}:{1}/health' -f $Ctx.Ip, $Ctx.Puerto
    while ($true) {
        $enIp = @(Get-Escuchas $Ctx.Puerto | Where-Object { $_.Direccion -eq $Ctx.Ip -and $_.IdProceso -ne $IdAnterior })
        if ($enIp.Count -gt 0) {
            $idNuevo = $enIp[0].IdProceso
            if ($null -eq $tPuerto) { $tPuerto = [math]::Round($sw.Elapsed.TotalSeconds, 1) }
            $h = Get-Salud $url $script:TimeoutSaludMs
            if ($h.Ok) {
                return [pscustomobject]@{ Ok = $true; SegPuerto = $tPuerto; SegHealth = [math]::Round($sw.Elapsed.TotalSeconds, 1); Salud = $h; IdProceso = $idNuevo }
            }
        }
        if ($sw.Elapsed.TotalSeconds -ge $LimiteSeg) { break }
        Start-Sleep -Milliseconds $PasoMs
    }
    return [pscustomobject]@{ Ok = $false; SegPuerto = $tPuerto; SegHealth = $null; Salud = $null; IdProceso = $idNuevo }
}

function Stop-Daemon {
    # Stop-Process primero; si el sistema lo niega (el proceso es de SYSTEM),
    # taskkill con su codigo de salida.
    param([int]$IdProceso)
    try { Stop-Process -Id $IdProceso -Force -ErrorAction Stop; return $true } catch { }
    $r = Invoke-Nativo 'taskkill.exe' @('/F', '/PID', [string]$IdProceso) 30
    return ((-not $r.NoArranco) -and (-not $r.Agotado) -and ($r.Codigo -eq 0))
}

function Test-ProcesoEsNuestro {
    param($Info, [string]$RutaBinario)
    if ($null -eq $Info -or [string]::IsNullOrEmpty($Info.Ruta)) { return $false }
    return ($Info.Ruta -ieq $RutaBinario)
}

function Get-ConteoTarea {
    param([string]$Nombre)
    try { return @(Get-ScheduledTask -TaskName $Nombre -ErrorAction Stop).Count } catch { return 0 }
}

function Get-DetalleRegla {
    # Cmdlets y no el texto de netsh: el texto de netsh sale en el idioma del
    # Windows ("Nombre de la regla"), y esto tiene que valer en cualquiera.
    param([string]$Nombre)
    $reglas = @()
    try { $reglas = @(Get-NetFirewallRule -DisplayName $Nombre -ErrorAction Stop) } catch { return @() }
    $res = New-Object System.Collections.ArrayList
    foreach ($r in $reglas) {
        $pf = $null
        try { $pf = Get-NetFirewallPortFilter -AssociatedNetFirewallRule $r -ErrorAction Stop } catch { }
        [void]$res.Add([pscustomobject]@{
                Habilitada = [string]$r.Enabled
                Direccion  = [string]$r.Direction
                Accion     = [string]$r.Action
                Perfil     = [string]$r.Profile
                Protocolo  = $(if ($pf) { [string]$pf.Protocol } else { '' })
                Puerto     = $(if ($pf) { [string]$pf.LocalPort } else { '' })
            })
    }
    return @($res)
}

function Test-Elevado {
    try {
        $id = [System.Security.Principal.WindowsIdentity]::GetCurrent()
        return (New-Object System.Security.Principal.WindowsPrincipal($id)).IsInRole([System.Security.Principal.WindowsBuiltInRole]::Administrator)
    }
    catch { return $false }
}

function Get-RutaAbsoluta {
    param([string]$Ruta)
    if ([System.IO.Path]::IsPathRooted($Ruta)) { return [System.IO.Path]::GetFullPath($Ruta) }
    return [System.IO.Path]::GetFullPath((Join-Path (Get-Location).Path $Ruta))
}

function Save-Informe {
    param($Ctx, [switch]$Anexar)
    $ruta = Join-Path $Ctx.Salida 'informe.txt'
    $texto = ($script:Lineas -join "`r`n") + "`r`n"
    if ($Anexar -and (Test-Path -LiteralPath $ruta)) {
        $previo = [System.IO.File]::ReadAllText($ruta)
        $texto = $previo.TrimEnd() + "`r`n`r`n" + $texto
    }
    Write-TextoArchivo $ruta $texto -ConBom
}

function New-Contexto {
    # Todo lo que las fases comparten, resuelto una sola vez.
    $bin = $Binario
    if ([string]::IsNullOrWhiteSpace($bin)) { $bin = Join-Path $PSScriptRoot 'memory-industry.exe' }
    $sal = $Salida
    if ([string]::IsNullOrWhiteSpace($sal)) { $sal = 'ensayo-informe' }
    # Sin LOCALAPPDATA el binario tampoco sabe donde instalar (rust/src/service.rs,
    # install_root), asi que aqui es una negativa y no un valor inventado.
    if ([string]::IsNullOrEmpty($env:LOCALAPPDATA) -or [string]::IsNullOrEmpty($env:ProgramData)) {
        Stop-Rechazo 'LOCALAPPDATA o ProgramData no estan definidas: este ensayo es para una sesion de Windows.'
    }
    $raiz = Join-Path $env:LOCALAPPDATA 'MemoryIndustry'
    return [pscustomobject]@{
        Binario  = (Get-RutaAbsoluta $bin)
        Salida   = (Get-RutaAbsoluta $sal)
        Ip       = $Ip
        Puerto   = $Puerto
        Raiz     = $raiz
        Env      = (Join-Path $raiz $script:ArchivoEnv)
        Launcher = (Join-Path $raiz $script:ArchivoLauncher)
        Xml      = (Join-Path $raiz $script:ArchivoXml)
        Log      = (Join-Path $raiz $script:ArchivoLog)
        Trabajo  = (Get-DirTrabajo)
    }
}

function Test-PrerequisitosComunes {
    # Elevacion y plataforma: lo unico que Antes, Despues y Limpiar exigen igual.
    if (-not (Test-EsWindows)) {
        Stop-Rechazo 'este ensayo es para Windows (usa tareas programadas, netsh y el firewall de Windows)'
    }
    if (-not (Test-Elevado)) {
        Stop-Rechazo 'hace falta una consola de PowerShell ELEVADA (Ejecutar como administrador): crear una tarea de SYSTEM y abrir un puerto en el firewall son derechos de administrador. No se hizo nada.'
    }
}

function Resolve-Ip {
    param($Ctx)
    $candidatos = Get-CandidatosIp
    if ([string]::IsNullOrWhiteSpace($Ctx.Ip)) {
        $elegida = Select-IpPrivada $candidatos
        if ($null -eq $elegida) {
            Stop-Rechazo 'no encuentro una IPv4 privada en un adaptador activo. Pasala con -Ip <ipv4 de este equipo>.'
        }
        $Ctx.Ip = $elegida.Ip
        Write-Informe ('IP elegida: {0} (adaptador "{1}")' -f $elegida.Ip, $elegida.Alias)
        return
    }
    $parsed = $null
    if (-not [System.Net.IPAddress]::TryParse($Ctx.Ip, [ref]$parsed) -or $parsed.AddressFamily -ne [System.Net.Sockets.AddressFamily]::InterNetwork) {
        Stop-Rechazo ('-Ip "{0}" no es una IPv4' -f $Ctx.Ip)
    }
    if ([System.Net.IPAddress]::IsLoopback($parsed)) {
        Stop-Rechazo 'el perfil lan necesita la direccion de una interfaz que otras maquinas alcancen, no 127.0.0.1'
    }
    if (@($candidatos | Where-Object { $_.Ip -eq $Ctx.Ip }).Count -eq 0) {
        Stop-Rechazo ('la IP {0} no esta asignada a ningun adaptador activo de este equipo. `serve` esperaria hasta 5 min y saldria con "is not assigned to this machine". Direcciones que si tiene: {1}' -f $Ctx.Ip, ((@($candidatos | ForEach-Object { $_.Ip }) -join ', ')))
    }
}

# ===========================================================================
# Humo: prueba de 2 minutos, de SOLO LECTURA, antes de instalar nada
# ===========================================================================

$script:VersionEsperada = 'memory-industry 0.29.0'
# Ensayo: una base que no existe, a proposito. `serve` arranca degradado y no toca
# Docker ni ninguna base real del equipo.
$script:UrlEnsayo = 'postgresql://ensayo:ensayo@127.0.0.1:1/brain'
# Valor por omision del repositorio (docker-compose.yml / .env.example).
$script:UrlPorOmision = 'postgresql://cuba:memorys2026@127.0.0.1:5488/brain'
$script:ContenedorBase = 'memory-industry-db'
$script:TextoDocker = 'Esto es ESPERABLE en un equipo con Docker Desktop justo despues de reiniciar: Docker arranca al INICIAR SESION, no al encender. El servicio arranca degradado (sin base) y recupera sus herramientas solo cuando Docker este arriba. Los bucles de fondo (rem y sync) solo arrancan en el siguiente reinicio del servicio.'
$script:FilasHumo = New-Object System.Collections.ArrayList
$script:UsuarioInteractivo = $null

function New-Fila {
    param([string]$Id, [string]$Estado, [string]$Detalle, [string]$Accion = '')
    return [pscustomobject]@{ id = $Id; estado = $Estado; detalle = $Detalle; accion = $Accion }
}

function Get-VeredictoHumo {
    # Un solo PARA basta: nada se instala con una fila en PARA.
    param($Filas)
    if (@($Filas | Where-Object { $_.estado -eq 'PARA' }).Count -gt 0) { return 'PARA' }
    return 'OK'
}

function Format-FilaHumo {
    param($Fila)
    $l = '[{0,-5}] {1,-22} {2}' -f $Fila.estado, $Fila.id, $Fila.detalle
    if ($Fila.accion) { $l += "`r`n" + (' ' * 9) + '-> ' + $Fila.accion }
    return $l
}

function Test-MismoUsuario {
    # DOMINIO\usuario: si los dos traen dominio se comparan enteros; si no, el nombre.
    param([string]$A, [string]$B)
    if ([string]::IsNullOrWhiteSpace($A) -or [string]::IsNullOrWhiteSpace($B)) { return $false }
    $pa = @($A -split '\\')
    $pb = @($B -split '\\')
    if ($pa.Count -ge 2 -and $pb.Count -ge 2) { return ($A -ieq $B) }
    return ($pa[$pa.Count - 1] -ieq $pb[$pb.Count - 1])
}

function Get-FilasCuentas {
    param([bool]$Elevado, [string]$Instala, [string]$Interactivo)
    $f = New-Object System.Collections.ArrayList
    if ($Elevado) { [void]$f.Add((New-Fila 'elevacion' 'OK' 'esta consola es de administrador (fltmc responde)')) }
    else { [void]$f.Add((New-Fila 'elevacion' 'PARA' 'esta consola NO es de administrador' 'Cierra esta ventana y usa el lanzador .cmd de la carpeta: pide el aviso de administrador. No se instalo nada.')) }
    if ([string]::IsNullOrWhiteSpace($Interactivo)) {
        [void]$f.Add((New-Fila 'cuenta' 'AVISO' ('se instalara con la cuenta {0}; no pude saber quien esta sentado delante (no hay explorer.exe en esta sesion)' -f $Instala) 'Comprueba que esa es tu cuenta.'))
    }
    elseif (Test-MismoUsuario $Instala $Interactivo) {
        [void]$f.Add((New-Fila 'cuenta' 'OK' ('se instalara con la cuenta {0}, que es la de la persona sentada' -f $Instala)))
    }
    else {
        [void]$f.Add((New-Fila 'cuenta' 'PARA' ('se instalaria con la cuenta {0}, pero la persona sentada es {1}' -f $Instala, $Interactivo) ('Cierra sesion, entra con {0} y repite el lanzador. Cuando salga el aviso de administrador acepta con "Si" SIN escribir otra contrasena. Asi el resultado se abre en tu escritorio.' -f $Interactivo)))
    }
    return @($f)
}

function Get-ProblemasRuta {
    # Una ruta que entra en la tarea, el launcher o el env file tiene que ser ASCII y
    # sin %: cmd.exe, schtasks y el XML la leen cada uno con su propia pagina de codigos.
    param([string]$Ruta, [string]$TipoUnidad = '')
    $p = New-Object System.Collections.ArrayList
    if ([string]::IsNullOrEmpty($Ruta)) { [void]$p.Add('no hay ruta'); return @($p) }
    if ($Ruta -match '[^\x20-\x7E]') { [void]$p.Add('tiene caracteres que no son ASCII (acentos, enies, simbolos)') }
    if ($Ruta.Contains('%')) { [void]$p.Add('tiene un signo de porcentaje') }
    if ($Ruta -match '^\\\\') { [void]$p.Add('esta en la red') }
    elseif ($TipoUnidad -and $TipoUnidad -ne 'Fixed') { [void]$p.Add(('no esta en un disco fijo del equipo (es {0})' -f $TipoUnidad)) }
    if ($Ruta -match '(?i)\\(OneDrive[^\\]*|Downloads|Descargas|Desktop|Escritorio)(\\|$)') { [void]$p.Add('esta en una carpeta sincronizada, de descargas o del escritorio') }
    return @($p)
}

function Get-DirPaquete {
    # A mano y no con Join-Path: asi se puede calcular (y probar) sin que la unidad exista.
    $d = [string][System.IO.Path]::DirectorySeparatorChar
    return ($env:ProgramData.TrimEnd('\', '/') + $d + 'MemoryIndustry' + $d + 'paquete')
}

function Test-EnCopiaProtegida {
    param([string]$Carpeta)
    if ([string]::IsNullOrEmpty($Carpeta) -or [string]::IsNullOrEmpty($env:ProgramData)) { return $false }
    return ($Carpeta.TrimEnd('\', '/') -ieq (Get-DirPaquete).TrimEnd('\', '/'))
}

function Get-FilasPaquete {
    param([string]$Carpeta, [string]$TipoUnidad, $ProblemasSumas, [bool]$EnCopiaProtegida, [bool]$ExigirCopiaProtegida)
    $f = New-Object System.Collections.ArrayList
    $pr = @(Get-ProblemasRuta $Carpeta $TipoUnidad)
    if ($pr.Count -gt 0) { [void]$f.Add((New-Fila 'paquete_ruta' 'PARA' ('la carpeta del paquete {0}' -f ($pr -join '; ')) 'Descomprime el zip en C:\memory-industry-visita y repite.')) }
    else { [void]$f.Add((New-Fila 'paquete_ruta' 'OK' ('carpeta del paquete: {0}' -f $Carpeta))) }
    $ps = @($ProblemasSumas)
    if ($ps.Count -gt 0) { [void]$f.Add((New-Fila 'paquete_sumas' 'PARA' ('SHA256SUMS.txt: {0}' -f ($ps -join '; ')) 'Vuelve a copiar el paquete original: el archivo se corrompio o no es el que se ensayo. No se instalo nada.')) }
    else { [void]$f.Add((New-Fila 'paquete_sumas' 'OK' 'todos los ficheros coinciden con SHA256SUMS.txt')) }
    if ($EnCopiaProtegida) { [void]$f.Add((New-Fila 'copia_protegida' 'OK' 'se ejecuta desde la copia protegida (solo SYSTEM y Administradores pueden modificarla)')) }
    elseif ($ExigirCopiaProtegida) { [void]$f.Add((New-Fila 'copia_protegida' 'PARA' 'no se ejecuta desde la copia protegida de C:\ProgramData\MemoryIndustry\paquete' 'Usa el lanzador .cmd: copia el paquete a una carpeta que solo administradores pueden tocar y lo ejecuta desde alli.')) }
    else { [void]$f.Add((New-Fila 'copia_protegida' 'AVISO' 'no se ejecuta desde la copia protegida (esta fase suelta no la exige)' 'El lanzador .cmd si la usa.')) }
    return @($f)
}

function Get-DiagnosticoExe {
    # Del resultado de `--version` (hasta dos intentos) y de lo que dejo Windows en sus
    # registros, que le paso al .exe y que hacer. Una DLL que falta NO es el antivirus,
    # aunque haya eventos de Defender en el registro: se miran despues.
    param($Intentos, [bool]$DllPresente, $EventosDefender, $EventosPolitica, $EventosApp, [string]$Esperada)
    $i = @($Intentos)
    $ult = $i[$i.Count - 1]
    $ev = @($EventosApp | Select-Object -First 2) -join ' | '
    if (-not $ult.NoArranco -and -not $ult.Agotado -and $ult.Codigo -eq 0) {
        $salida = $ult.Salida.Trim()
        if ($salida -eq $Esperada) {
            return [pscustomobject]@{ Estado = 'OK'; Detalle = ('--version imprime "{0}" y sale con 0 ({1} intento(s))' -f $salida, $i.Count); Accion = '' }
        }
        return [pscustomobject]@{ Estado = 'PARA'; Detalle = ('--version imprime "{0}" y se esperaba "{1}"' -f $salida, $Esperada); Accion = 'Este no es el .exe del paquete: vuelve a copiar el paquete original.' }
    }
    $dll = @(-1073741515, -1073741701, 3221225781, 3221225595) -contains [int64]$ult.Codigo
    if ($dll) {
        $dd = 'vcruntime140.dll '
        if ($DllPresente) { $dd += 'esta en System32, pero otra libreria falta o no es compatible' } else { $dd += 'NO esta en System32' }
        return [pscustomobject]@{ Estado = 'PARA'; Detalle = ('el .exe no arranca porque le falta una libreria de Windows ({0}). No lo bloquea el antivirus.' -f $dd); Accion = 'Instala "Microsoft Visual C++ Redistributable x64" (vc_redist.x64.exe, de Microsoft) y repite.' }
    }
    if ($ult.Agotado) {
        $quien = ''
        if (@($EventosDefender).Count -gt 0) { $quien = ' Defender dejo avisos: ' + (@($EventosDefender | Select-Object -First 2) -join ' | ') }
        return [pscustomobject]@{ Estado = 'PARA'; Detalle = ('--version no respondio en el tiempo dado, dos veces.{0}' -f $quien); Accion = 'Probablemente el antivirus esta analizando el .exe por primera vez: espera 5 minutos y repite. Si vuelve a pasar, manda la carpeta ensayo-informe.' }
    }
    if (@($EventosPolitica).Count -gt 0) {
        return [pscustomobject]@{ Estado = 'PARA'; Detalle = ('Windows no dejo ejecutar el .exe por una politica de control de aplicaciones: {0}' -f (@($EventosPolitica | Select-Object -First 2) -join ' | ')); Accion = 'Pide a quien administra el equipo que permita este .exe (AppLocker o Control de aplicaciones) y repite.' }
    }
    if (@($EventosDefender).Count -gt 0) {
        return [pscustomobject]@{ Estado = 'PARA'; Detalle = ('el antivirus bloqueo o aviso del .exe: {0}' -f (@($EventosDefender | Select-Object -First 2) -join ' | ')); Accion = 'Pide a quien administra el equipo que lo permita (exclusion del .exe) y repite.' }
    }
    $razon = ''
    if ($ult.Error) { $razon = ' ' + $ult.Error.Trim() }
    if ($ult.NoArranco -or $ult.Codigo -eq -1) {
        return [pscustomobject]@{ Estado = 'PARA'; Detalle = ('Windows no dejo ejecutar el .exe.{0}{1}' -f $razon, $(if ($ev) { ' Errores recientes: ' + $ev } else { '' })); Accion = 'Manda la carpeta ensayo-informe: no hay una causa clara en los registros.' }
    }
    return [pscustomobject]@{ Estado = 'PARA'; Detalle = ('--version termino con codigo {0}.{1}{2}' -f $ult.Codigo, $razon, $(if ($ev) { ' Errores recientes: ' + $ev } else { '' })); Accion = 'Manda la carpeta ensayo-informe.' }
}

function Get-FilasPolitica {
    param($Politicas, [string]$Modo, $Antivirus, $Sac, [string]$AppLocker)
    $f = New-Object System.Collections.ArrayList
    $def = @($Politicas | Where-Object { ($_.Scope -eq 'MachinePolicy' -or $_.Scope -eq 'UserPolicy') -and $_.Policy -ne 'Undefined' })
    if ($def.Count -gt 0) {
        [void]$f.Add((New-Fila 'politica_scripts' 'AVISO' ('una directiva de grupo define la politica de scripts: {0}' -f (($def | ForEach-Object { '{0}={1}' -f $_.Scope, $_.Policy }) -join ', ')) 'Los lanzadores usan -ExecutionPolicy Bypass, que una directiva de grupo NO anula. Si el script no arranca, es esto.'))
    }
    else { [void]$f.Add((New-Fila 'politica_scripts' 'OK' 'ninguna directiva de grupo fija la politica de scripts')) }
    if ($Modo -eq 'FullLanguage') { [void]$f.Add((New-Fila 'modo_powershell' 'OK' 'PowerShell en FullLanguage')) }
    else { [void]$f.Add((New-Fila 'modo_powershell' 'PARA' ('PowerShell esta restringido (modo {0}): hay un control de aplicaciones activo' -f $Modo) 'Este ensayo no puede correr asi. Pide a quien administra el equipo que permita estos scripts, o manda la carpeta ensayo-informe.')) }
    $av = @($Antivirus)
    if ($av.Count -gt 0) { [void]$f.Add((New-Fila 'antivirus' 'INFO' ('antivirus registrado: {0}' -f ($av -join ', ')))) }
    else { [void]$f.Add((New-Fila 'antivirus' 'INFO' 'no pude leer el antivirus registrado (Centro de seguridad)')) }
    if ($null -eq $Sac) { [void]$f.Add((New-Fila 'smart_app_control' 'INFO' 'Smart App Control: no aplica en este Windows')) }
    elseif ([int]$Sac -eq 1) { [void]$f.Add((New-Fila 'smart_app_control' 'AVISO' 'Smart App Control esta ACTIVADO: puede bloquear un .exe sin firma' 'Si el .exe no arranca, es esto: hay que desactivarlo (no se puede volver a activar sin reinstalar Windows) o firmar el .exe.')) }
    elseif ([int]$Sac -eq 2) { [void]$f.Add((New-Fila 'smart_app_control' 'AVISO' 'Smart App Control esta en evaluacion: puede bloquear un .exe sin firma' 'Si el .exe no arranca, es esto.')) }
    else { [void]$f.Add((New-Fila 'smart_app_control' 'OK' 'Smart App Control desactivado')) }
    if ([string]::IsNullOrEmpty($AppLocker)) { [void]$f.Add((New-Fila 'applocker' 'INFO' 'AppLocker: sin politica efectiva')) }
    else { [void]$f.Add((New-Fila 'applocker' 'AVISO' ('AppLocker tiene politica efectiva: {0}' -f $AppLocker) 'Si el .exe o los scripts no arrancan, es esto.')) }
    return @($f)
}

function Test-AdaptadorExcluido {
    # VPN, tuneles y adaptadores virtuales: tienen IP privada, pero ningun cliente de la
    # LAN llega por ahi. TAP va aparte y sensible a mayusculas para no cazar "adapter".
    param([string]$Descripcion)
    if ([string]::IsNullOrEmpty($Descripcion)) { return $false }
    if ($Descripcion -match 'VPN|AnyConnect|Fortinet|PANGP|WireGuard|Wintun|Tailscale|ZeroTier|Hamachi|Bluetooth|Virtual|vEthernet|WSL|Hyper-V|VirtualBox|VMware') { return $true }
    return ($Descripcion -cmatch '\bTAP\b')
}

function Select-IpPorMetrica {
    # La que el equipo usaria para salir (ruta por omision de menor metrica) entre las
    # privadas de adaptadores reales; sin ruta por omision, la primera privada real.
    param($Candidatos)
    $reales = @($Candidatos | Where-Object { (Test-IpPrivada $_.Ip) -and -not $_.Excluida })
    if ($reales.Count -eq 0) { return $null }
    $con = @($reales | Where-Object { $_.Gateway })
    if ($con.Count -eq 0) { return $reales[0] }
    $mejor = $con[0]
    foreach ($c in $con) {
        $m = 9999; if ($null -ne $c.Metrica) { $m = [int]$c.Metrica }
        $mm = 9999; if ($null -ne $mejor.Metrica) { $mm = [int]$mejor.Metrica }
        if ($m -lt $mm) { $mejor = $c }
    }
    return $mejor
}

function Format-Candidato {
    param($C, [int]$Numero)
    $gw = 'sin puerta de enlace'; if ($C.Gateway) { $gw = 'con puerta de enlace' }
    $ip = 'IP desconocida'; if ($C.Dhcp -eq $true) { $ip = 'DHCP (puede cambiar)' } elseif ($C.Dhcp -eq $false) { $ip = 'fija' }
    $x = ''; if ($C.Excluida) { $x = '  [descartada: VPN o adaptador virtual]' }
    return ('{0}) {1}  {2}  {3}, {4}, red {5}, metrica {6}{7}' -f $Numero, $C.Ip, $C.Alias, $gw, $ip, $C.Perfil, $(if ($null -ne $C.Metrica) { $C.Metrica } else { 'n/d' }), $x)
}

function Get-HostPuertoUrl {
    # Solo el host y el puerto: lo demas de la URL lleva la clave.
    param([string]$Url)
    if ($Url -notmatch '^(?i)postgres(ql)?://(?:[^@/]*@)?(\[[^\]]+\]|[^:/\s?]+)(?::(\d+))?(?:[/?]|$)') { return $null }
    $puerto = 5432
    if ($Matches[3]) { $puerto = [int]$Matches[3] }
    return [pscustomobject]@{ Host = $Matches[2].Trim('[', ']'); Puerto = $puerto }
}

function Get-FilaBaseDatos {
    param([string]$Modo, $Alcanzable, $HostPuerto, $Docker)
    if ($Modo -ne 'real') {
        return (New-Fila 'base_datos' 'INFO' 'ensayo: la base se fija a una URL inalcanzable a proposito (127.0.0.1:1); el servicio arrancara degradado y no toca Docker ni ninguna base del equipo')
    }
    $hp = 'la base'
    if ($null -ne $HostPuerto) { $hp = '{0}:{1}' -f $HostPuerto.Host, $HostPuerto.Puerto }
    $dock = ''
    if ($null -ne $Docker -and $Docker.Existe) {
        if (-not $Docker.Motor) { $dock = ' Docker esta instalado pero su motor NO esta en marcha.' }
        elseif ($Docker.Contenedor) { $dock = (' Docker en marcha; contenedor {0}: {1}.' -f $script:ContenedorBase, $Docker.Contenedor) }
        else { $dock = (' Docker en marcha, pero no existe el contenedor {0}.' -f $script:ContenedorBase) }
    }
    elseif ($null -ne $Docker) { $dock = ' No encuentro docker.exe.' }
    if ($Alcanzable) { return (New-Fila 'base_datos' 'OK' ('{0} responde.{1}' -f $hp, $dock)) }
    return (New-Fila 'base_datos' 'AVISO' ('{0} NO responde ahora.{1} {2}' -f $hp, $dock, $script:TextoDocker) 'No hace falta que la base este arriba para instalar. Cuando alguien inicie sesion y Docker Desktop arranque, la base vuelve.')
}

function Get-SesionesAjenas {
    # Salida de `query user`: la primera linea es la cabecera y la sesion propia lleva '>'.
    param($Texto)
    if ($null -eq $Texto) { return $null }
    $l = @(($Texto -split "`r?`n") | Where-Object { $_.Trim().Length -gt 0 })
    if ($l.Count -le 1) { return @() }
    return @($l | Select-Object -Skip 1 | Where-Object { -not $_.TrimStart().StartsWith('>') -and -not $_.StartsWith('>') } | ForEach-Object { $_.Trim() })
}

function Test-ConfirmaReinicio {
    # Con otras personas con sesion abierta, un reinicio les cierra su trabajo: hace
    # falta escribir SI entero, no basta una S.
    param([string]$Respuesta, [bool]$HayOtrasSesiones)
    if (-not (Test-RespuestaSi $Respuesta)) { return $false }
    if (-not $HayOtrasSesiones) { return $true }
    $r = $Respuesta.Trim().ToLower()
    return ($r -eq 'si' -or $r -eq ('s' + [string][char]0xED))
}

function Get-FilaSesiones {
    param($Ajenas, $Pendientes)
    $pend = @($Pendientes)
    $pt = ''
    if ($pend.Count -gt 0) { $pt = ' Pendiente de reinicio por: ' + ($pend -join ', ') + '.' }
    if ($null -eq $Ajenas) { return (New-Fila 'sesiones' 'INFO' ('no pude listar las sesiones (query user).{0}' -f $pt)) }
    $aj = @($Ajenas)
    if ($aj.Count -gt 0) { return (New-Fila 'sesiones' 'AVISO' ('hay {0} sesion(es) de otras personas abiertas.{1}' -f $aj.Count, $pt) 'El reinicio les cerrara el trabajo: avisales. Se pedira escribir SI.') }
    if ($pend.Count -gt 0) { return (New-Fila 'sesiones' 'AVISO' ('no hay otras sesiones.{0}' -f $pt) 'Windows ya tiene un reinicio pendiente; el del ensayo lo aprovecha.') }
    return (New-Fila 'sesiones' 'OK' 'no hay otras sesiones abiertas ni reinicios pendientes')
}

function Test-InstalacionAjena {
    # Una instalacion que NO es de este ensayo: otra direccion, una tarea que ejecuta
    # otra cosa o una regla de firewall con otro puerto. Devuelve el motivo, o $null.
    param($DirPrevia, [string]$Direccion, [bool]$HayEnv, [bool]$HayTarea, $ComandoTarea, [string]$Launcher, [bool]$HayRegla, $PuertoRegla, [int]$Puerto)
    if (-not [string]::IsNullOrEmpty($DirPrevia) -and $DirPrevia -ne $Direccion) { return ('el env file ya tiene otra direccion ({0})' -f $DirPrevia) }
    if (($HayTarea -or $HayRegla) -and -not $HayEnv) { return 'hay tarea o regla de firewall pero no env file' }
    if ($HayTarea) {
        if ([string]::IsNullOrEmpty($ComandoTarea)) { return 'no pude leer lo que ejecuta la tarea memory-industry' }
        if ($ComandoTarea.Trim() -ine $Launcher) { return ('la tarea memory-industry ejecuta otra cosa ({0})' -f $ComandoTarea.Trim()) }
    }
    if ($HayRegla -and -not [string]::IsNullOrEmpty([string]$PuertoRegla) -and [string]$PuertoRegla -ne [string]$Puerto) { return ('la regla de firewall memory-industry abre el puerto {0}' -f $PuertoRegla) }
    return $null
}

function Get-AvisosAntes {
    param($Antes)
    if ($null -eq $Antes -or $null -eq $Antes.checks) { return @() }
    return @($Antes.checks | Where-Object { $_.resultado -eq 'AVISO' })
}

function Get-ProblemasRutasInstalacion {
    # El .exe, la carpeta de instalacion y el perfil del usuario: lo que acaba escrito en
    # el launcher, la tarea y el env file.
    param([string]$Binario, [string]$Raiz, [string]$Perfil)
    $r = New-Object System.Collections.ArrayList
    foreach ($par in @(@('el .exe', $Binario), @('la carpeta de instalacion', $Raiz), @('el perfil del usuario (USERPROFILE)', $Perfil))) {
        foreach ($m in @(Get-ProblemasRuta $par[1] '' | Where-Object { $_ -notmatch 'sincronizada' })) { [void]$r.Add(('{0} {1}' -f $par[0], $m)) }
    }
    return @($r)
}

function Get-TextoBaseNoUp {
    param([bool]$Real)
    if ($Real) { return ('el servicio responde pero la base de datos no esta arriba. ' + $script:TextoDocker) }
    return 'el servicio responde sin base de datos: es lo previsto en el ensayo (se uso una URL inalcanzable a proposito). Las herramientas que la usan no se comprueban.'
}

function Format-UrlSinClave {
    param([string]$Url)
    return [regex]::Replace($Url, '(://[^:/\s]+:)[^@\s]+@', '$1***@')
}

function Resolve-EleccionIp {
    # `Texto` es lo que escribio el operador: vacio confirma la propuesta; un numero elige
    # de la lista que se le mostro. Una VPN o un adaptador virtual no se acepta.
    param([string]$Texto, $Candidatas, $Propuesta)
    $t = ''
    if ($null -ne $Texto) { $t = $Texto.Trim() }
    if ($t -eq '') {
        if ($null -eq $Propuesta) { return [pscustomobject]@{ Candidata = $null; Error = 'no hay ninguna direccion propuesta; escribe el numero de una' } }
        return [pscustomobject]@{ Candidata = $Propuesta; Error = $null }
    }
    $n = 0
    if (-not [int]::TryParse($t, [ref]$n) -or $n -lt 1 -or $n -gt @($Candidatas).Count) { return [pscustomobject]@{ Candidata = $null; Error = ('"{0}" no es un numero de la lista' -f $t) } }
    $c = @($Candidatas)[$n - 1]
    if ($c.Excluida) { return [pscustomobject]@{ Candidata = $null; Error = 'esa es una VPN o un adaptador virtual: los clientes no llegan por ahi' } }
    if (-not (Test-IpPrivada $c.Ip)) { return [pscustomobject]@{ Candidata = $null; Error = 'esa direccion no es privada (10.x, 172.16-31.x, 192.168.x)' } }
    return [pscustomobject]@{ Candidata = $c; Error = $null }
}

# --- Lo que lee de Windows (cada sondeo es una funcion para poder simularlo) ---------

function Test-EsWindows { return ([System.Environment]::OSVersion.Platform -eq [System.PlatformID]::Win32NT) }

function Get-UsuarioInteractivo {
    # Quien esta sentado: el propietario del explorer.exe de ESTA sesion. Quien elevo puede
    # ser otra cuenta (UAC pidiendo credenciales de un administrador distinto).
    try {
        $mi = (Get-Process -Id $PID -ErrorAction Stop).SessionId
        $p = @(Get-CimInstance -ClassName Win32_Process -Filter "Name = 'explorer.exe'" -ErrorAction Stop | Where-Object { [int]$_.SessionId -eq $mi }) | Select-Object -First 1
        if (-not $p) { return $null }
        $o = Invoke-CimMethod -InputObject $p -MethodName GetOwner -ErrorAction Stop
        if ($o.ReturnValue -ne 0) { return $null }
        return ('{0}\{1}' -f $o.Domain, $o.User)
    }
    catch { return $null }
}

function Get-TipoUnidad {
    param([string]$Ruta)
    try { return [string](New-Object System.IO.DriveInfo($Ruta.Substring(0, 1))).DriveType } catch { return '' }
}

function Test-DllSistema {
    param([string]$Nombre)
    $dir = Join-Path $env:SystemRoot 'System32'
    return (Test-Path -LiteralPath (Join-Path $dir $Nombre))
}

function Get-EventosFalloExe {
    # Solo cuando el .exe fallo: los ultimos errores de Application, los avisos de
    # Defender (1116, 1117, 5007) y los bloqueos de CodeIntegrity y AppLocker.
    $res = [pscustomobject]@{ Defender = @(); Politica = @(); App = @() }
    $desde = (Get-Date).AddMinutes(-15)
    $resumen = { param($e) ('{0} {1}: {2}' -f $e.ProviderName, $e.Id, (([string]$e.Message) -replace '\s+', ' ').Substring(0, [math]::Min(110, ([string]$e.Message).Length))) }
    try { $res.Defender = @(Get-WinEvent -FilterHashtable @{ LogName = 'Microsoft-Windows-Windows Defender/Operational'; Id = 1116, 1117, 5007; StartTime = $desde } -MaxEvents 4 -ErrorAction Stop | ForEach-Object { & $resumen $_ }) } catch { }
    try { $res.Politica += @(Get-WinEvent -FilterHashtable @{ LogName = 'Microsoft-Windows-CodeIntegrity/Operational'; Level = 2, 3; StartTime = $desde } -MaxEvents 3 -ErrorAction Stop | ForEach-Object { & $resumen $_ }) } catch { }
    try { $res.Politica += @(Get-WinEvent -FilterHashtable @{ LogName = 'Microsoft-Windows-AppLocker/EXE and DLL'; Id = 8002, 8004; StartTime = $desde } -MaxEvents 3 -ErrorAction Stop | ForEach-Object { & $resumen $_ }) } catch { }
    try { $res.App = @(Get-WinEvent -FilterHashtable @{ LogName = 'Application'; Level = 2; StartTime = $desde } -MaxEvents 3 -ErrorAction Stop | ForEach-Object { & $resumen $_ }) } catch { }
    return $res
}

function Get-DatosPolitica {
    $r = [pscustomobject]@{ Lista = @(); Modo = ''; Av = @(); Sac = $null; AppLocker = '' }
    try { $r.Lista = @(Get-ExecutionPolicy -List | ForEach-Object { [pscustomobject]@{ Scope = [string]$_.Scope; Policy = [string]$_.ExecutionPolicy } }) } catch { }
    try { $r.Modo = [string]$ExecutionContext.SessionState.LanguageMode } catch { }
    try { $r.Av = @(Get-CimInstance -Namespace 'root/SecurityCenter2' -ClassName AntiVirusProduct -ErrorAction Stop | ForEach-Object { [string]$_.displayName }) } catch { }
    try { $r.Sac = (Get-ItemProperty -LiteralPath 'HKLM:\SYSTEM\CurrentControlSet\Control\CI\Policy' -Name VerifiedAndReputablePolicyState -ErrorAction Stop).VerifiedAndReputablePolicyState } catch { }
    try {
        $x = [xml](Get-AppLockerPolicy -Effective -Xml -ErrorAction Stop)
        $cols = @($x.AppLockerPolicy.RuleCollection | Where-Object { $_.EnforcementMode -and $_.EnforcementMode -ne 'NotConfigured' })
        if ($cols.Count -gt 0) { $r.AppLocker = (($cols | ForEach-Object { '{0}={1}' -f $_.Type, $_.EnforcementMode }) -join ', ') }
    }
    catch { }
    return $r
}

function Test-TareasYFirewall {
    # Lo unico que "toca" Humo: una tarea de prueba como SYSTEM, creada y borrada en el
    # acto. Sin esto no se sabe si Task Scheduler deja registrar tareas de SYSTEM hasta
    # que el instalador ya ha escrito sus ficheros.
    $r = [pscustomobject]@{ Tarea = $false; ErrorTarea = ''; Firewall = $false; ErrorFirewall = '' }
    $nombre = 'memory-industry-humo-prueba'
    try {
        $accion = New-ScheduledTaskAction -Execute 'cmd.exe' -Argument '/c exit 0'
        $principal = New-ScheduledTaskPrincipal -UserId 'SYSTEM' -LogonType ServiceAccount -RunLevel Highest
        $disp = New-ScheduledTaskTrigger -AtStartup
        [void](Register-ScheduledTask -TaskName $nombre -Action $accion -Trigger $disp -Principal $principal -Force -ErrorAction Stop)
        $r.Tarea = ((Get-ConteoTarea $nombre) -eq 1)
        if (-not $r.Tarea) { $r.ErrorTarea = 'la tarea no figura tras registrarla' }
    }
    catch { $r.ErrorTarea = $_.Exception.Message }
    finally { try { Unregister-ScheduledTask -TaskName $nombre -Confirm:$false -ErrorAction Stop } catch { } }
    try { [void](Get-NetFirewallProfile -ErrorAction Stop); $r.Firewall = $true } catch { $r.ErrorFirewall = $_.Exception.Message }
    return $r
}

function Get-CandidatosHumo {
    $metricas = @{}
    try {
        foreach ($rt in @(Get-NetRoute -DestinationPrefix '0.0.0.0/0' -ErrorAction Stop)) {
            $im = 0
            try { $im = [int](Get-NetIPInterface -InterfaceIndex $rt.InterfaceIndex -AddressFamily IPv4 -ErrorAction Stop).InterfaceMetric } catch { }
            $total = [int]$rt.RouteMetric + $im
            $k = [int]$rt.InterfaceIndex
            if (-not $metricas.ContainsKey($k) -or $total -lt $metricas[$k]) { $metricas[$k] = $total }
        }
    }
    catch { }
    $lista = New-Object System.Collections.ArrayList
    foreach ($ni in [System.Net.NetworkInformation.NetworkInterface]::GetAllNetworkInterfaces()) {
        if ($ni.OperationalStatus -ne [System.Net.NetworkInformation.OperationalStatus]::Up) { continue }
        if ($ni.NetworkInterfaceType -eq [System.Net.NetworkInformation.NetworkInterfaceType]::Loopback) { continue }
        if ($ni.NetworkInterfaceType -eq [System.Net.NetworkInformation.NetworkInterfaceType]::Tunnel) { continue }
        $props = $ni.GetIPProperties()
        $p4 = $null
        try { $p4 = $props.GetIPv4Properties() } catch { }
        $idx = 0; $dhcp = $null
        if ($p4) { $idx = [int]$p4.Index; $dhcp = [bool]$p4.IsDhcpEnabled }
        $gw = @($props.GatewayAddresses | Where-Object { $_.Address.AddressFamily -eq [System.Net.Sockets.AddressFamily]::InterNetwork -and $_.Address.ToString() -ne '0.0.0.0' }).Count -gt 0
        $perfil = 'desconocida'
        try { $perfil = [string](Get-NetConnectionProfile -InterfaceIndex $idx -ErrorAction Stop).NetworkCategory } catch { }
        $metrica = $null
        if ($metricas.ContainsKey($idx)) { $metrica = $metricas[$idx] }
        foreach ($u in $props.UnicastAddresses) {
            if ($u.Address.AddressFamily -ne [System.Net.Sockets.AddressFamily]::InterNetwork) { continue }
            [void]$lista.Add([pscustomobject]@{
                    Ip = $u.Address.ToString(); Alias = $ni.Name; Descripcion = $ni.Description
                    Gateway = $gw; Metrica = $metrica; Dhcp = $dhcp; Perfil = $perfil; Indice = $idx
                    Excluida = (Test-AdaptadorExcluido ($ni.Name + ' ' + $ni.Description))
                })
        }
    }
    return @($lista)
}

function Get-OtrosMemory {
    # Otras cosas que se llaman memory o cuba (los daemons de otros usos de esta PC) y
    # quien escucha en 8787 y 8788. Solo informacion.
    $r = [pscustomobject]@{ Tareas = @(); Reglas = @(); Procesos = @(); Escuchas = @() }
    try { $r.Tareas = @(Get-ScheduledTask -ErrorAction Stop | Where-Object { $_.TaskName -match 'memory|cuba' -and $_.TaskName -ne $script:NombreTarea -and $_.TaskName -notlike 'memory-industry-ensayo*' -and $_.TaskName -ne 'memory-industry-humo-prueba' } | ForEach-Object { [string]$_.TaskName }) } catch { }
    try { $r.Reglas = @(Get-NetFirewallRule -ErrorAction Stop | Where-Object { $_.DisplayName -match 'memory|cuba' -and $_.DisplayName -ne $script:NombreRegla } | ForEach-Object { [string]$_.DisplayName }) } catch { }
    try { $r.Procesos = @(Get-Process -ErrorAction Stop | Where-Object { $_.ProcessName -match 'memory|cuba' } | ForEach-Object { '{0} (PID {1})' -f $_.ProcessName, $_.Id }) } catch { }
    foreach ($pt in 8787, 8788) { foreach ($e in @(Get-Escuchas $pt)) { $r.Escuchas += ('{0}:{1} (PID {2})' -f $e.Direccion, $pt, $e.IdProceso) } }
    return $r
}

function Test-PuertoTcp {
    param([string]$Servidor, [int]$Puerto, [int]$TimeoutMs = 3000)
    $c = New-Object System.Net.Sockets.TcpClient
    try {
        $ar = $c.BeginConnect($Servidor, $Puerto, $null, $null)
        if (-not $ar.AsyncWaitHandle.WaitOne($TimeoutMs, $false)) { return $false }
        $c.EndConnect($ar)
        return $c.Connected
    }
    catch { return $false }
    finally { $c.Close() }
}

function Get-EstadoDocker {
    $r = [pscustomobject]@{ Existe = $false; Motor = $false; Contenedor = '' }
    $exe = $null
    try { $exe = (Get-Command docker.exe -ErrorAction Stop).Source } catch { }
    if (-not $exe -and (Test-Path -LiteralPath 'C:\Program Files\Docker\Docker\resources\bin\docker.exe')) { $exe = 'C:\Program Files\Docker\Docker\resources\bin\docker.exe' }
    if (-not $exe) { return $r }
    $r.Existe = $true
    $i = Invoke-Nativo $exe @('info', '--format', '{{.ServerVersion}}') 20 -Utf8
    $r.Motor = ((-not $i.NoArranco) -and (-not $i.Agotado) -and $i.Codigo -eq 0 -and $i.Salida.Trim().Length -gt 0)
    if ($r.Motor) {
        $c = Invoke-Nativo $exe @('ps', '-a', '--filter', ('name=^{0}$' -f $script:ContenedorBase), '--format', '{{.Status}}') 20 -Utf8
        if ($c.Codigo -eq 0) { $r.Contenedor = $c.Salida.Trim() }
    }
    return $r
}

function Get-TextoQuser {
    $q = Invoke-Nativo 'query.exe' @('user') 15
    if ($q.NoArranco -or $q.Agotado) { return $null }
    return $q.Salida
}

function Get-RebootPendiente {
    $m = New-Object System.Collections.ArrayList
    if (Test-Path -LiteralPath 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Component Based Servicing\RebootPending') { [void]$m.Add('mantenimiento de Windows (CBS)') }
    if (Test-Path -LiteralPath 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\WindowsUpdate\Auto Update\RebootRequired') { [void]$m.Add('Windows Update') }
    return @($m)
}

function Invoke-VersionExe {
    # 90 s y UN reintento: la primera vez que se ejecuta un .exe nuevo, el antivirus lo
    # analiza antes de dejarlo correr y puede tardar mas de lo normal.
    param([string]$Exe)
    $intentos = New-Object System.Collections.ArrayList
    for ($n = 1; $n -le 2; $n++) {
        $r = Invoke-Nativo $Exe @('--version') 90 -Utf8
        [void]$intentos.Add($r)
        if (-not $r.NoArranco -and -not $r.Agotado -and $r.Codigo -eq 0) { break }
        if ($r.NoArranco) { break }
    }
    return @($intentos)
}

# --- El humo --------------------------------------------------------------------------

function Get-ResultadoHumo {
    # Devuelve las filas y lo que Todo necesita para seguir (IP elegida, candidatas,
    # usuario interactivo). No instala nada y no pregunta nada.
    param($Ctx, [bool]$ExigirCopiaProtegida)
    $filas = New-Object System.Collections.ArrayList
    $modo = 'ensayo'
    if ($Conservar) { $modo = 'real' }
    $elevado = (Test-Elevado)
    if ($elevado) {
        $fl = Invoke-Nativo 'fltmc.exe' @() 15
        $elevado = ((-not $fl.NoArranco) -and (-not $fl.Agotado) -and $fl.Codigo -eq 0)
    }
    $instala = Get-UsuarioActual
    $interactivo = Get-UsuarioInteractivo
    $script:UsuarioInteractivo = $interactivo
    foreach ($f in @(Get-FilasCuentas $elevado $instala $interactivo)) { [void]$filas.Add($f) }

    # (2) El paquete
    $carpeta = $PSScriptRoot
    $sumas = @()
    $sumas = @(Get-ProblemasSumas $carpeta $Ctx.Binario)
    foreach ($f in @(Get-FilasPaquete $carpeta (Get-TipoUnidad $carpeta) $sumas (Test-EnCopiaProtegida $carpeta) $ExigirCopiaProtegida)) { [void]$filas.Add($f) }
    $rutas = @(Get-ProblemasRutasInstalacion $Ctx.Binario $Ctx.Raiz $env:USERPROFILE)
    if ($rutas.Count -gt 0) { [void]$filas.Add((New-Fila 'rutas_ascii' 'PARA' ($rutas -join '; ') 'Mueve el paquete a C:\memory-industry-visita y usa un usuario cuyo nombre sea solo letras y numeros, o manda la carpeta ensayo-informe.')) }
    else { [void]$filas.Add((New-Fila 'rutas_ascii' 'OK' 'las rutas del .exe, de la instalacion y del perfil son ASCII y sin %')) }

    # (3) El .exe
    if (Test-Path -LiteralPath $Ctx.Binario -PathType Leaf) {
        $intentos = @(Invoke-VersionExe $Ctx.Binario)
        $ult = $intentos[$intentos.Count - 1]
        $ok = (-not $ult.NoArranco -and -not $ult.Agotado -and $ult.Codigo -eq 0)
        $dll = Test-DllSistema 'vcruntime140.dll'
        $ev = [pscustomobject]@{ Defender = @(); Politica = @(); App = @() }
        if (-not $ok) { $ev = Get-EventosFalloExe }
        $dx = Get-DiagnosticoExe $intentos $dll $ev.Defender $ev.Politica $ev.App $script:VersionEsperada
        [void]$filas.Add((New-Fila 'exe' $dx.Estado $dx.Detalle $dx.Accion))
        if ($dll) { [void]$filas.Add((New-Fila 'vcruntime140' 'OK' 'vcruntime140.dll esta en System32')) }
        elseif ($ok) { [void]$filas.Add((New-Fila 'vcruntime140' 'INFO' 'vcruntime140.dll no esta en System32, pero el .exe arranca: no la necesita (CRT estatico)')) }
        else { [void]$filas.Add((New-Fila 'vcruntime140' 'INFO' 'vcruntime140.dll NO esta en System32')) }
    }
    else { [void]$filas.Add((New-Fila 'exe' 'PARA' ('no encuentro el .exe en {0}' -f $Ctx.Binario) 'Descomprime el zip entero y usa el lanzador de la misma carpeta.')) }

    # (4) Politicas
    $pol = Get-DatosPolitica
    foreach ($f in @(Get-FilasPolitica $pol.Lista $pol.Modo $pol.Av $pol.Sac $pol.AppLocker)) { [void]$filas.Add($f) }

    # (5) Task Scheduler y firewall
    if ($elevado) {
        $tf = Test-TareasYFirewall
        if ($tf.Tarea) { [void]$filas.Add((New-Fila 'task_scheduler' 'OK' 'se pudo crear y borrar una tarea de prueba como SYSTEM')) }
        else { [void]$filas.Add((New-Fila 'task_scheduler' 'PARA' ('no se pudo crear una tarea como SYSTEM: {0}' -f $tf.ErrorTarea) 'Task Scheduler o una directiva lo impiden: manda la carpeta ensayo-informe.')) }
        if ($tf.Firewall) { [void]$filas.Add((New-Fila 'firewall' 'OK' 'el firewall de Windows responde (Get-NetFirewallProfile)')) }
        else { [void]$filas.Add((New-Fila 'firewall' 'PARA' ('no se puede consultar el firewall de Windows: {0}' -f $tf.ErrorFirewall) 'Si lo gestiona otro producto o una directiva, avisa: el instalador no podra abrir el puerto.')) }
    }
    else { [void]$filas.Add((New-Fila 'task_scheduler' 'INFO' 'sin administrador no se probo Task Scheduler ni el firewall')) }

    # (6) Red
    $cands = @(Get-CandidatosHumo)
    $elegida = Select-IpPorMetrica $cands
    $n = 0
    $texto = New-Object System.Collections.ArrayList
    foreach ($c in $cands) { $n++; [void]$texto.Add((Format-Candidato $c $n)) }
    if ($cands.Count -eq 0) { [void]$filas.Add((New-Fila 'red_candidatas' 'PARA' 'no hay ninguna direccion IPv4 en un adaptador activo' 'Conecta el cable o el wifi y repite.')) }
    else { [void]$filas.Add((New-Fila 'red_candidatas' 'INFO' ("direcciones IPv4 de este equipo:`r`n         " + ($texto -join "`r`n         ")))) }
    if ($null -eq $elegida) { [void]$filas.Add((New-Fila 'red_elegida' 'PARA' 'ninguna direccion es privada y de un adaptador real (no VPN ni virtual)' 'Conecta el equipo a la red de la planta y repite.')) }
    else {
        $esDhcp = ($elegida.Dhcp -eq $true)
        $det = 'se usaria {0} ({1}), la de menor metrica de la ruta por omision' -f $elegida.Ip, $elegida.Alias
        if ($esDhcp) { [void]$filas.Add((New-Fila 'red_elegida' 'AVISO' ($det + '. ATENCION: esta direccion la reparte el DHCP y PUEDE CAMBIAR; los clientes dejarian de encontrar el servicio') 'Fija la IP del equipo (despliegue-lan.md, seccion "Fixed address on the host") antes de instalar.')) }
        else { [void]$filas.Add((New-Fila 'red_elegida' 'OK' $det)) }
        if ($elegida.Perfil -eq 'Public') { [void]$filas.Add((New-Fila 'red_perfil' 'AVISO' 'la red de esa direccion es Publica: la regla de firewall solo abre Privada y Dominio' 'En el equipo real se te ofrecera cambiarla a Privada.')) }
    }
    $puertoLibre = $true
    foreach ($e in @(Get-Escuchas $Ctx.Puerto)) {
        $info = Get-InfoProceso $e.IdProceso
        if (-not (Test-ProcesoEsNuestro $info $Ctx.Binario)) { $puertoLibre = $false; [void]$filas.Add((New-Fila 'puerto' 'PARA' ('el puerto {0} ya lo usa otro programa (PID {1}, {2})' -f $Ctx.Puerto, $e.IdProceso, $(if ($info) { $info.Nombre } else { 'desconocido' })) 'Manda la carpeta ensayo-informe: hay que elegir otro puerto.')) }
    }
    if ($puertoLibre) { [void]$filas.Add((New-Fila 'puerto' 'OK' ('el puerto {0} esta libre (o es de un ensayo anterior)' -f $Ctx.Puerto))) }
    $otros = Get-OtrosMemory
    $partes = New-Object System.Collections.ArrayList
    if (@($otros.Tareas).Count) { [void]$partes.Add('tareas: ' + (@($otros.Tareas) -join ', ')) }
    if (@($otros.Reglas).Count) { [void]$partes.Add('reglas: ' + (@($otros.Reglas) -join ', ')) }
    if (@($otros.Procesos).Count) { [void]$partes.Add('procesos: ' + (@($otros.Procesos) -join ', ')) }
    if (@($otros.Escuchas).Count) { [void]$partes.Add('escuchan en 8787/8788: ' + (@($otros.Escuchas) -join ', ')) }
    if ($partes.Count -gt 0) { [void]$filas.Add((New-Fila 'otros_memory' 'INFO' ('hay otras cosas llamadas memory o cuba en este equipo (no se tocan): ' + ($partes -join '; ')))) }
    else { [void]$filas.Add((New-Fila 'otros_memory' 'OK' 'no hay otras tareas, reglas ni procesos memory o cuba, ni nadie en 8787 u 8788')) }

    # (7) Base de datos
    $url = $env:DATABASE_URL
    if ($modo -eq 'real' -and [string]::IsNullOrEmpty($url)) { $url = $script:UrlPorOmision }
    $hp = $null
    if ($url) { $hp = Get-HostPuertoUrl $url }
    $alc = $null
    if ($modo -eq 'real' -and $hp) { $alc = Test-PuertoTcp $hp.Host $hp.Puerto 3000 }
    $dk = $null
    if ($modo -eq 'real') { $dk = Get-EstadoDocker }
    [void]$filas.Add((Get-FilaBaseDatos $modo $alc $hp $dk))

    # (8) Sesiones
    [void]$filas.Add((Get-FilaSesiones (Get-SesionesAjenas (Get-TextoQuser)) (Get-RebootPendiente)))

    return [pscustomobject]@{ Filas = @($filas); Veredicto = (Get-VeredictoHumo $filas); Candidatas = $cands; Elegida = $elegida; Interactivo = $interactivo }
}

function Write-FilasHumo {
    param($Filas)
    foreach ($f in @($Filas)) {
        $color = 'Green'
        if ($f.estado -eq 'PARA') { $color = 'Red' } elseif ($f.estado -eq 'AVISO') { $color = 'Yellow' } elseif ($f.estado -eq 'INFO') { $color = '' }
        Write-Informe (Format-FilaHumo $f) $color
    }
}

function Invoke-FaseHumo {
    $ctx = $script:Ctx
    if (-not (Test-EsWindows)) {
        Stop-Rechazo 'este ensayo es para Windows'
    }
    [void](New-Item -ItemType Directory -Path $ctx.Salida -Force)
    Write-Informe ('MemoryIndustry - prueba de humo (solo lectura, no instala nada) - {0}' -f (Get-Date -Format 'yyyy-MM-dd HH:mm:ss')) 'Cyan'
    $h = Get-ResultadoHumo $ctx $false
    Write-FilasHumo $h.Filas
    Write-Json (Join-Path $ctx.Salida 'humo.json') ([ordered]@{ fase = 'Humo'; fecha = (Get-AhoraTexto); veredicto = $h.Veredicto; filas = @($h.Filas) })
    Write-Informe ''
    Write-Informe ('HUMO: {0}' -f $h.Veredicto) $(if ($h.Veredicto -eq 'OK') { 'Green' } else { 'Red' })
    Save-Informe $ctx -Anexar
    if ($h.Veredicto -ne 'OK') { $script:Codigo = 1 }
}


function Read-TextoSecreto {
    # Lo que se teclea aqui (una URL con clave) no se muestra ni queda en la transcripcion.
    param([string]$Pregunta)
    $r = Read-Host $Pregunta -AsSecureString
    if ($r -is [string]) { return $r }
    $bstr = [System.Runtime.InteropServices.Marshal]::SecureStringToBSTR($r)
    try { return [System.Runtime.InteropServices.Marshal]::PtrToStringAuto($bstr) }
    finally { [System.Runtime.InteropServices.Marshal]::ZeroFreeBSTR($bstr) }
}

function Resolve-BaseDatos {
    # Ensayo: URL inalcanzable a proposito. Equipo real: una URL explicita; Enter ofrece la
    # del repositorio. Se deja en DATABASE_URL del proceso, que es de donde `setup service`
    # la copia al env file. Nunca se imprime entera.
    param([bool]$Real)
    if (-not $Real) {
        $env:DATABASE_URL = $script:UrlEnsayo
        Write-Informe 'Base de datos: ensayo, URL inalcanzable a proposito (el servicio arrancara degradado).'
        return
    }
    Write-Informe ('Base de datos: pulsa Enter para usar la del repositorio ({0}) o escribe otra URL postgresql:// (no se muestra al escribir).' -f (Format-UrlSinClave $script:UrlPorOmision))
    $url = $null
    for ($i = 0; $i -lt 3 -and -not $url; $i++) {
        $t = Read-TextoSecreto 'URL de la base'
        if ([string]::IsNullOrWhiteSpace($t)) { $t = $script:UrlPorOmision }
        $t = $t.Trim()
        if ($null -ne (Get-HostPuertoUrl $t)) { $url = $t } else { Write-Informe 'Eso no parece una URL postgresql://usuario:clave@servidor:puerto/base. Otra vez.' 'Yellow' }
    }
    if (-not $url) { Stop-Rechazo 'no se dio una URL de base de datos valida. No se instalo nada.' }
    $env:DATABASE_URL = $url
    $hp = Get-HostPuertoUrl $url
    $alc = Test-PuertoTcp $hp.Host $hp.Puerto 3000
    $fila = Get-FilaBaseDatos 'real' $alc $hp (Get-EstadoDocker)
    Write-Informe (Format-FilaHumo $fila) $(if ($fila.estado -eq 'OK') { 'Green' } else { 'Yellow' })
}

function Resolve-IpReal {
    # En el equipo real la IP no se elige sola: se muestra la lista y se pide confirmar o
    # elegir un numero ANTES de instalar. Una IP de DHCP se avisa en grande.
    param($Ctx, $Humo)
    $cands = @($Humo.Candidatas)
    $n = 0
    foreach ($c in $cands) { $n++; Write-Informe ('  ' + (Format-Candidato $c $n)) }
    $elegida = $Humo.Elegida
    for ($i = 0; $i -lt 3; $i++) {
        $ayuda = 'Escribe el numero de la direccion que usaran los clientes'
        if ($elegida) { $ayuda += (' o pulsa Enter para confirmar {0}' -f $elegida.Ip) }
        $t = Read-Host $ayuda
        $r = Resolve-EleccionIp $t $cands $elegida
        if ($r.Error) { Write-Informe $r.Error 'Yellow'; continue }
        $c = $r.Candidata
        if ($c.Dhcp -eq $true) {
            Write-Informe ('ATENCION: {0} la reparte el DHCP y PUEDE CAMBIAR en cualquier momento. Si cambia, ningun cliente encontrara el servicio. Lo correcto es fijar la IP antes (despliegue-lan.md, "Fixed address on the host").' -f $c.Ip) 'Red'
            $sigue = Read-Host 'Continuar igualmente con esta direccion? (S/N)'
            if (-not (Test-RespuestaSi $sigue)) { continue }
        }
        $Ctx.Ip = $c.Ip
        Write-Informe ('Direccion elegida: {0} ({1})' -f $c.Ip, $c.Alias) 'Green'
        return
    }
    Stop-Rechazo 'no se confirmo ninguna direccion. No se instalo nada.'
}

function Get-ComandoTarea {
    # Lo que ejecuta la tarea registrada, leido de su definicion (no del nombre).
    $q = Invoke-Nativo 'schtasks.exe' @('/Query', '/TN', $script:NombreTarea, '/XML') 30 -Utf8
    if ($q.NoArranco -or $q.Agotado -or $q.Codigo -ne 0 -or $q.Salida.IndexOf('<') -lt 0) { return $null }
    try {
        $doc = New-Object System.Xml.XmlDocument
        $doc.LoadXml($q.Salida.Substring($q.Salida.IndexOf('<')))
        $ns = New-Object System.Xml.XmlNamespaceManager($doc.NameTable)
        $ns.AddNamespace('t', 'http://schemas.microsoft.com/windows/2004/02/mit/task')
        $c = $doc.SelectSingleNode('//t:Exec/t:Command', $ns)
        if ($c) { return $c.InnerText.Trim() }
    }
    catch { }
    return $null
}

# ===========================================================================
# Fase Antes
# ===========================================================================

function Invoke-FaseAntes {
    $ctx = $script:Ctx
    Test-PrerequisitosComunes
    if (-not (Test-Path -LiteralPath $ctx.Binario)) {
        Stop-Rechazo ('no encuentro el binario en {0}. Pasa -Binario <ruta a memory-industry.exe>.' -f $ctx.Binario)
    }
    $rutasMalas = @(Get-ProblemasRutasInstalacion $ctx.Binario $ctx.Raiz $env:USERPROFILE)
    if ($rutasMalas.Count -gt 0) {
        Stop-Rechazo ('rutas que el instalador no puede usar: {0}. Mueve el paquete a C:\memory-industry-visita o usa otro usuario, o manda la carpeta ensayo-informe.' -f ($rutasMalas -join '; '))
    }
    Resolve-Ip $ctx
    $direccion = '{0}:{1}' -f $ctx.Ip, $ctx.Puerto
    $url = 'http://{0}/health' -f $direccion
    [void](New-Item -ItemType Directory -Path $ctx.Salida -Force)
    try { Unblock-File -LiteralPath $ctx.Binario -ErrorAction Stop } catch { }

    $a = [ordered]@{
        fase = 'Antes'; version_script = 1
        inicio = (Get-AhoraTexto); fin = $null
        equipo = $env:COMPUTERNAME; usuario = $env:USERNAME
        so = [System.Environment]::OSVersion.VersionString
        ip = $ctx.Ip; puerto = $ctx.Puerto; direccion = $direccion
        binario = [ordered]@{ ruta = $ctx.Binario; sha256 = $null; version = $null }
        raiz = $ctx.Raiz
        token = [ordered]@{ longitud = $null; sha256 = $null; sha256_tras_reaplicar = $null }
        mediciones = [ordered]@{}
        armado_en = $null; log_bytes = 0; token_sha256 = $null
        checks = @(); veredicto = $null
    }
    $secretos = @()

    try {
        Write-Informe ('MemoryIndustry - ensayo de instalacion (Antes) - {0}' -f (Get-Date -Format 'yyyy-MM-dd HH:mm:ss'))
        Write-Informe ('Equipo {0}, usuario {1}, {2}' -f $env:COMPUTERNAME, $env:USERNAME, $a.so)
        Write-Informe ('Direccion del ensayo: {0}' -f $direccion)

        # --- 0. El binario -------------------------------------------------
        Write-Paso '0. Binario'
        $a.binario.sha256 = (Get-FileHash -LiteralPath $ctx.Binario -Algorithm SHA256).Hash.ToLower()
        $intentos = @(Invoke-VersionExe $ctx.Binario)
        $v = $intentos[$intentos.Count - 1]
        if ($v.NoArranco -or $v.Agotado -or $v.Codigo -ne 0) {
            $ev = Get-EventosFalloExe
            $dx = Get-DiagnosticoExe $intentos (Test-DllSistema 'vcruntime140.dll') $ev.Defender $ev.Politica $ev.App $script:VersionEsperada
            Stop-Rechazo ('el binario no se ejecuta ({0}): {1} {2}' -f $ctx.Binario, $dx.Detalle, $dx.Accion)
        }
        $a.binario.version = $v.Salida.Trim()
        Write-Informe ('Binario {0}' -f $ctx.Binario)
        Write-Informe ('  {0}, sha256 {1}' -f $a.binario.version, $a.binario.sha256)
        if ($ctx.Binario -match '\\(Downloads|Desktop|Temp|OneDrive)\\') {
            Add-Check 'ruta_binario' 'AVISO' 'la ruta del .exe queda escrita en la tarea y en el launcher; si esta carpeta se mueve o se sincroniza, el servicio no arranca. Mejor una carpeta fija (C:\memory-industry-visita)'
        }

        # --- 1. Guardas: no pisar una instalacion ajena ---------------------
        Write-Paso '1. Guardas'
        $envPrevio = Read-Texto $ctx.Env
        $hayTarea = (Get-ConteoTarea $script:NombreTarea) -gt 0
        $hayRegla = @(Get-DetalleRegla $script:NombreRegla).Count -gt 0
        $estadoRuta = Join-Path $ctx.Salida 'estado-ensayo.json'
        $estado = Read-Json $estadoRuta
        $preexistia = $false
        if ($null -ne $estado) {
            $preexistia = [bool]$estado.preexistia
        }
        else {
            $preexistia = ($null -ne $envPrevio) -or $hayTarea -or $hayRegla
            $dirPrevia = $null
            if ($null -ne $envPrevio) { $dirPrevia = Get-ValorEnv $envPrevio $script:ClaveDireccion }
            $comando = $null
            if ($hayTarea) { $comando = Get-ComandoTarea }
            $puertoRegla = $null
            if ($hayRegla) { $puertoRegla = @(Get-DetalleRegla $script:NombreRegla)[0].Puerto }
            $motivo = Test-InstalacionAjena $dirPrevia $direccion ($null -ne $envPrevio) $hayTarea $comando $ctx.Launcher $hayRegla $puertoRegla $ctx.Puerto
            if ($motivo -and -not $Forzar) {
                # Quien hace doble clic no puede pasar -Forzar: lo que se le dice es a quien avisar.
                Stop-Rechazo ('ya hay una instalacion de memory-industry en este equipo que no es de este ensayo ({0}). Instalar encima la pisaria: la tarea y la regla de firewall son unicas por nombre. No se toco nada. Manda la carpeta ensayo-informe.' -f $motivo)
            }
            Write-TextoArchivo $estadoRuta (ConvertTo-Json -InputObject ([ordered]@{ preexistia = $preexistia; creado = (Get-AhoraTexto); ip = $ctx.Ip; puerto = $ctx.Puerto; binario = $ctx.Binario }) -Depth 4)
        }
        $a.preexistia = $preexistia
        $autoLogon = Get-AutoLogon
        $a.autologon = $autoLogon
        if ($autoLogon) { Add-Check 'autologon' 'AVISO' 'el equipo inicia sesion solo (AutoAdminLogon=1): la prueba de arranque sin sesion no podra darse por valida; lo demas si' }
        Add-Check 'guarda_instalacion' 'PASA' ('instalacion previa: {0}' -f $(if ($preexistia) { 'si (se conserva en Limpiar salvo -Forzar)' } else { 'no' }))

        $propias = @()
        foreach ($e in @(Get-Escuchas $ctx.Puerto)) {
            $info = Get-InfoProceso $e.IdProceso
            if (Test-ProcesoEsNuestro $info $ctx.Binario) { $propias += $e.IdProceso }
            else {
                Stop-Rechazo ('el puerto {0} ya lo usa otro proceso (PID {1}, {2}). Usa -Puerto con uno libre.' -f $ctx.Puerto, $e.IdProceso, $(if ($info) { $info.Nombre } else { 'desconocido' }))
            }
        }
        if ($propias.Count -gt 0) {
            # Un Antes repetido: se para el daemon del ensayo anterior para volver a
            # medir el arranque desde cero.
            [void](Invoke-Nativo 'schtasks.exe' @('/End', '/TN', $script:NombreTarea) 30)
            foreach ($idp in ($propias | Select-Object -Unique)) { [void](Stop-Daemon $idp) }
            $libre = Wait-Hasta { @(Get-Escuchas $ctx.Puerto).Count -eq 0 } 20 500
            if (-not $libre.Cumplido) { Stop-Rechazo 'el daemon del ensayo anterior no se detuvo en 20 s' }
            Write-Informe 'Se detuvo el daemon de un ensayo anterior para medir desde cero.'
        }

        $nombresEntorno = @($(if ($script:ModoTodo) { @() } else { @('DATABASE_URL') }) + @('CUBA_HTTP_ADDR', 'CUBA_HTTP_TOKEN', 'CUBA_MODE', 'ONNX_MODEL_PATH', 'ORT_DYLIB_PATH')) | Where-Object { [Environment]::GetEnvironmentVariable($_) }
        if (@($nombresEntorno).Count -gt 0) {
            Add-Check 'entorno_variables' 'AVISO' ('definidas en esta consola: {0}. DATABASE_URL, CUBA_MODE, ONNX_MODEL_PATH y ORT_DYLIB_PATH se copian al env file; CUBA_HTTP_ADDR en el entorno gana sobre el fichero. El daemon usara esa base.' -f (@($nombresEntorno) -join ', '))
        }

        # --- 2. El plan ----------------------------------------------------
        Write-Paso '2. Plan (no escribe nada)'
        $plan = Invoke-Nativo $ctx.Binario @('setup', 'service', '--profile', 'lan', '--addr', $direccion) 60 -Utf8
        Write-TextoArchivo (Join-Path $ctx.Salida 'plan.txt') (Protect-Secreto ($plan.Salida + $plan.Error) $secretos)
        if ($plan.NoArranco -or $plan.Agotado -or $plan.Codigo -ne 0) {
            Add-Check 'plan' 'FALLA' ('setup service (sin --apply) termino con codigo {0}: {1}' -f $plan.Codigo, ($plan.Error.Trim()))
            return
        }
        Add-Check 'plan' 'PASA' ('plan impreso ({0} lineas); guardado en plan.txt' -f @($plan.Salida -split "`n").Count)

        # --- 3. Instalar y registrar ---------------------------------------
        Write-Paso '3. setup service --apply --register'
        $ap = Invoke-Nativo $ctx.Binario @('setup', 'service', '--profile', 'lan', '--addr', $direccion, '--apply', '--register') 120 -Utf8
        $textoAp = Protect-Secreto ($ap.Salida + $ap.Error) $secretos
        Write-TextoArchivo (Join-Path $ctx.Salida 'apply.txt') $textoAp
        foreach ($l in ((Protect-Secreto $ap.Salida $secretos) -split "`r?`n")) { if ($l.Trim()) { Write-Informe ('  | {0}' -f $l.TrimEnd()) } }
        if ($ap.NoArranco -or $ap.Agotado -or $ap.Codigo -ne 0) {
            $hecho = @(($textoAp -split "`r?`n") | Where-Object { $_ -match '^(escrito|conservado)\s' })
            Add-Check 'apply_register' 'FALLA' ('termino con codigo {0}. Quedo hecho: {1}. Repetir es seguro; ver apply.txt' -f $ap.Codigo, $(if ($hecho.Count) { ($hecho -join ' / ') } else { 'nada que conste' }))
            return
        }
        Add-Check 'apply_register' 'PASA' 'codigo de salida 0'

        # --- 4. Ficheros y env file ----------------------------------------
        Write-Paso '4. Ficheros y env file'
        $faltan = @($ctx.Launcher, $ctx.Xml, $ctx.Env | Where-Object { -not (Test-Path -LiteralPath $_) })
        if ($faltan.Count -eq 0) { Add-Check 'ficheros' 'PASA' ('los tres existen en {0}' -f $ctx.Raiz) }
        else { Add-Check 'ficheros' 'FALLA' ('faltan: {0}' -f ($faltan -join ', ')) }

        $envTexto = Read-Texto $ctx.Env
        $dirEnv = Get-ValorEnv $envTexto $script:ClaveDireccion
        if ($dirEnv -eq $direccion) { Add-Check 'env_direccion' 'PASA' ('CUBA_HTTP_ADDR={0}' -f $dirEnv) }
        else { Add-Check 'env_direccion' 'FALLA' ('CUBA_HTTP_ADDR es "{0}", se esperaba "{1}"' -f $dirEnv, $direccion) }

        $token = Get-ValorEnv $envTexto $script:ClaveToken
        if ($token) { $secretos = @($token) }
        $largo = if ($token) { $token.Length } else { 0 }
        $a.token.longitud = $largo
        $a.token.sha256 = Get-HashToken $envTexto
        $a.token_sha256 = $a.token.sha256
        if ($largo -ge $script:TokenMinimo) { Add-Check 'env_token' 'PASA' ('token de {0} caracteres (minimo {1}); huella {2}...' -f $largo, $script:TokenMinimo, $a.token.sha256.Substring(0, 12)) }
        else { Add-Check 'env_token' 'FALLA' ('token de {0} caracteres; `serve` exige al menos {1} en una direccion que otras maquinas alcanzan' -f $largo, $script:TokenMinimo) }

        $lanzador = Read-Texto $ctx.Launcher
        $xmlTexto = Read-Texto $ctx.Xml
        if ($lanzador -and $lanzador.IndexOf($ctx.Binario, [System.StringComparison]::OrdinalIgnoreCase) -ge 0 -and $xmlTexto -and $xmlTexto.IndexOf($ctx.Launcher, [System.StringComparison]::OrdinalIgnoreCase) -ge 0) {
            Add-Check 'rutas_horneadas' 'PASA' 'el launcher apunta a este .exe y la tarea apunta al launcher'
        }
        else { Add-Check 'rutas_horneadas' 'FALLA' 'el launcher no apunta a este .exe o la tarea no apunta al launcher' }

        # --- 5. Tarea programada -------------------------------------------
        Write-Paso '5. Tarea programada'
        $q = Invoke-Nativo 'schtasks.exe' @('/Query', '/TN', $script:NombreTarea) 30
        if ($q.Codigo -eq 0) { Add-Check 'tarea_existe' 'PASA' ('schtasks /Query /TN {0} devuelve 0' -f $script:NombreTarea) }
        else { Add-Check 'tarea_existe' 'FALLA' ('schtasks /Query /TN {0} devolvio {1}: {2}' -f $script:NombreTarea, $q.Codigo, $q.Error.Trim()) }
        $n = Get-ConteoTarea $script:NombreTarea
        if ($n -eq 1) { Add-Check 'tarea_unica' 'PASA' 'exactamente una tarea con ese nombre' } else { Add-Check 'tarea_unica' 'FALLA' ('{0} tareas con ese nombre' -f $n) }

        $qx = Invoke-Nativo 'schtasks.exe' @('/Query', '/TN', $script:NombreTarea, '/XML') 30 -Utf8
        $dif = New-Object System.Collections.ArrayList
        try {
            $txt = $qx.Salida
            $txt = $txt.Substring($txt.IndexOf('<'))
            $doc = New-Object System.Xml.XmlDocument
            $doc.LoadXml($txt)
            $ns = New-Object System.Xml.XmlNamespaceManager($doc.NameTable)
            $ns.AddNamespace('t', 'http://schemas.microsoft.com/windows/2004/02/mit/task')
            $uid = $doc.SelectSingleNode('//t:Principal/t:UserId', $ns)
            if (-not $uid -or ($uid.InnerText -ne 'S-1-5-18' -and $uid.InnerText -notmatch 'SYSTEM')) { [void]$dif.Add('no corre como SYSTEM') }
            if (-not $doc.SelectSingleNode('//t:BootTrigger', $ns)) { [void]$dif.Add('sin BootTrigger') }
            if (-not $doc.SelectSingleNode('//t:LogonTrigger', $ns)) { [void]$dif.Add('sin LogonTrigger') }
            $rep = $doc.SelectSingleNode('//t:BootTrigger/t:Repetition/t:Interval', $ns)
            if (-not $rep -or $rep.InnerText -ne 'PT5M') { [void]$dif.Add('sin repeticion PT5M') }
            $cnt = $doc.SelectSingleNode('//t:RestartOnFailure/t:Count', $ns)
            if (-not $cnt -or $cnt.InnerText -ne '999') { [void]$dif.Add('RestartOnFailure sin Count 999') }
            $mip = $doc.SelectSingleNode('//t:MultipleInstancesPolicy', $ns)
            if (-not $mip -or $mip.InnerText -ne 'IgnoreNew') { [void]$dif.Add('MultipleInstancesPolicy distinto de IgnoreNew') }
        }
        catch { [void]$dif.Add(('no se pudo leer la definicion: {0}' -f $_.Exception.Message)) }
        if ($dif.Count -eq 0) { Add-Check 'tarea_definicion' 'PASA' 'SYSTEM, arranque + inicio de sesion, repeticion PT5M, reinicio x999, IgnoreNew' }
        else { Add-Check 'tarea_definicion' 'FALLA' ($dif -join '; ') }

        # --- 6. Firewall ----------------------------------------------------
        Write-Paso '6. Regla de firewall'
        $ns1 = Invoke-Nativo 'netsh.exe' @('advfirewall', 'firewall', 'show', 'rule', ('name=' + $script:NombreRegla)) 30
        if ($ns1.Codigo -eq 0) { Add-Check 'regla_existe' 'PASA' 'netsh show rule devuelve 0' } else { Add-Check 'regla_existe' 'FALLA' ('netsh show rule devolvio {0}' -f $ns1.Codigo) }
        $reglas = @(Get-DetalleRegla $script:NombreRegla)
        if ($reglas.Count -eq 1) { Add-Check 'regla_unica' 'PASA' 'exactamente una regla con ese nombre' } else { Add-Check 'regla_unica' 'FALLA' ('{0} reglas con ese nombre' -f $reglas.Count) }
        if ($reglas.Count -ge 1) {
            $r = $reglas[0]
            $bien = ($r.Habilitada -eq 'True') -and ($r.Direccion -eq 'Inbound') -and ($r.Accion -eq 'Allow') -and ($r.Protocolo -eq 'TCP') -and ($r.Puerto -eq [string]$ctx.Puerto) -and ($r.Perfil -match 'Private') -and ($r.Perfil -match 'Domain') -and ($r.Perfil -notmatch 'Public')
            if ($bien) { Add-Check 'regla_definicion' 'PASA' ('entrada, permitir, TCP {0}, perfiles {1}' -f $r.Puerto, $r.Perfil) }
            else { Add-Check 'regla_definicion' 'FALLA' ('habilitada={0} direccion={1} accion={2} protocolo={3} puerto={4} perfil={5}' -f $r.Habilitada, $r.Direccion, $r.Accion, $r.Protocolo, $r.Puerto, $r.Perfil) }
        }
        try {
            $idx = (Get-NetIPAddress -IPAddress $ctx.Ip -ErrorAction Stop).InterfaceIndex
            $cat = [string](Get-NetConnectionProfile -InterfaceIndex $idx -ErrorAction Stop).NetworkCategory
            $a.red_categoria = $cat
            if ($cat -eq 'Public' -and $Conservar) {
                # Equipo real: con la red Publica ningun cliente entra, y eso no puede quedar
                # callado. El arreglo se ofrece, no se aplica solo.
                Write-Informe 'La red de esta direccion es PUBLICA: la regla de firewall solo abre Privada y Dominio, asi que ningun otro equipo podria entrar.' 'Red'
                Write-Informe ('  Arreglo:  Set-NetConnectionProfile -InterfaceIndex {0} -NetworkCategory Private' -f $idx)
                $quiere = Read-Host 'Ejecutar ese cambio ahora? (S/N)'
                if (Test-RespuestaSi $quiere) {
                    try {
                        Set-NetConnectionProfile -InterfaceIndex $idx -NetworkCategory Private -ErrorAction Stop
                        $cat = [string](Get-NetConnectionProfile -InterfaceIndex $idx -ErrorAction Stop).NetworkCategory
                        $a.red_categoria = $cat
                    }
                    catch { Write-Informe ('  no se pudo cambiar: {0}' -f $_.Exception.Message) 'Red' }
                }
            }
            if ($cat -eq 'Public' -and $Conservar) { Add-Check 'red_perfil' 'FALLA' 'la red de esta IP es Public: la regla solo abre Private y Domain, asi que NINGUN otro equipo entraria. Cambiala a Privada (Set-NetConnectionProfile -NetworkCategory Private) o abre el puerto a mano.' }
            elseif ($cat -eq 'Public') { Add-Check 'red_perfil' 'AVISO' 'la red de esta IP es Public: la regla solo abre Private y Domain, asi que otras maquinas NO entrarian (este ensayo, desde el mismo equipo, no lo detecta)' }
            else { Add-Check 'red_perfil' 'PASA' ('la red de esta IP es {0}' -f $cat) }
        }
        catch { Add-Check 'red_perfil' 'AVISO' 'no se pudo leer el perfil de red de esta IP' }

        # --- 7. Arranque manual y medicion ----------------------------------
        Write-Paso '7. Arranque (schtasks /Run) y escucha'
        $logAntes = (Get-ColaLog $ctx.Log 0 1).Longitud
        $run = Invoke-Nativo 'schtasks.exe' @('/Run', '/TN', $script:NombreTarea) 30
        if ($run.Codigo -ne 0) {
            Add-Check 'tarea_run' 'FALLA' ('schtasks /Run devolvio {0}: {1}' -f $run.Codigo, $run.Error.Trim())
            return
        }
        Write-Informe ('Esperando al daemon (hasta {0} s)...' -f $script:LimiteArranqueSeg)
        $w = Wait-Daemon $ctx $script:LimiteArranqueSeg
        $a.mediciones.arranque_segundos_hasta_puerto = $w.SegPuerto
        $a.mediciones.arranque_segundos_hasta_health = $w.SegHealth
        if (-not $w.Ok) {
            Add-Check 'arranque_health' 'FALLA' ('sin respuesta de {0} en {1} s (puerto abierto: {2}); {3}' -f $url, $script:LimiteArranqueSeg, $(if ($null -ne $w.SegPuerto) { Format-Seg $w.SegPuerto } else { 'nunca' }), (Get-DiagnosticoArranque $ctx $logAntes $secretos))
            return
        }
        $j = $w.Salud.Json
        $a.mediciones.health = [ordered]@{ status = $j.status; database = $j.database; ready = $j.ready; version = $j.version; ms = $w.Salud.Ms }
        Add-Check 'arranque_health' 'PASA' ('puerto a los {0}, /health a los {1}: status={2} database={3} ready={4} version={5}' -f (Format-Seg $w.SegPuerto), (Format-Seg $w.SegHealth), $j.status, $j.database, $j.ready, $j.version)
        if ($j.database -ne 'up') { Add-Check 'base_de_datos' 'AVISO' (Get-TextoBaseNoUp ([bool]$Conservar)) }

        $dirs = @(Get-Escuchas $ctx.Puerto | ForEach-Object { $_.Direccion })
        $esc = Select-Escucha $dirs $ctx.Ip
        $a.mediciones.escucha = ($dirs -join ',')
        if (-not $esc.EnIp) { Add-Check 'escucha' 'FALLA' ('no escucha en {0}; direcciones: {1}' -f $direccion, ($dirs -join ', ')) }
        elseif ($esc.Todas) { Add-Check 'escucha' 'AVISO' ('escucha en {0} pero tambien en todas las interfaces ({1})' -f $direccion, ($dirs -join ', ')) }
        else { Add-Check 'escucha' 'PASA' ('escucha en {0}; direcciones del puerto: {1}' -f $direccion, ($dirs -join ', ')) }
        if ($esc.SoloLoopback) { Add-Check 'escucha_no_solo_loopback' 'FALLA' 'solo escucha en loopback: otras maquinas no entrarian' }
        else { Add-Check 'escucha_no_solo_loopback' 'PASA' 'no escucha solo en 127.0.0.1' }

        $idDaemon = $w.IdProceso
        $info = Get-InfoProceso $idDaemon
        if ($info) {
            $a.mediciones.proceso = [ordered]@{ pid = $idDaemon; nombre = $info.Nombre; sesion_id = $info.SesionId; usuario = $info.Usuario; inicio = $(if ($info.Inicio) { ([DateTimeOffset]$info.Inicio).ToString('o') } else { $null }) }
            Write-Informe ('  proceso {0} PID {1}, sesion {2}, usuario {3}' -f $info.Nombre, $idDaemon, $info.SesionId, $info.Usuario)
        }

        # --- 8. Matar el daemon y medir cuanto tarda en volver ---------------
        Write-Paso '8. Matar el daemon (supervision)'
        $arranqueSistema = Get-ArranqueSistema
        $xmlHora = (Get-Item -LiteralPath $ctx.Xml).LastWriteTime
        # La repeticion de 5 min cuelga del BootTrigger: no corre hasta que el equipo
        # arranca de nuevo despues de registrar la tarea. Antes de ese primer reinicio,
        # lo que levanta al daemon solo puede ser RestartOnFailure.
        $repActiva = ([DateTimeOffset]$arranqueSistema) -gt ([DateTimeOffset]$xmlHora)
        $a.mediciones.repeticion_activa_probable = $repActiva
        $tMuerte = Get-Date
        $swMuerte = [System.Diagnostics.Stopwatch]::StartNew()
        # Un fallo aqui se anota y el ensayo SIGUE: cortar a medias deja sin medir la
        # idempotencia, doctor y el log, que es justo lo que hace falta para entender el fallo.
        $mato = Stop-Daemon $idDaemon
        $desaparece = [pscustomobject]@{ Cumplido = $false; Segundos = 0 }
        if (-not $mato) { Add-Check 'matar_daemon' 'FALLA' ('no se pudo terminar el proceso {0}' -f $idDaemon) }
        else {
            $desaparece = Wait-Hasta { @(Get-Escuchas $ctx.Puerto | Where-Object { $_.IdProceso -eq $idDaemon }).Count -eq 0 } 20 250
            if (-not $desaparece.Cumplido) { Add-Check 'matar_daemon' 'FALLA' 'el puerto siguio abierto 20 s despues de matar el proceso' }
        }
        if ($mato -and $desaparece.Cumplido) {
        Write-Informe ('Proceso {0} terminado. Vigilando hasta {1} s...' -f $idDaemon, $script:LimiteReaparicionSeg)
        $resto = [int][math]::Max(5, $script:LimiteReaparicionSeg - $swMuerte.Elapsed.TotalSeconds)
        $w2 = Wait-Daemon $ctx $resto $idDaemon 2000
        $segDesdeMuerte = [math]::Round($swMuerte.Elapsed.TotalSeconds, 1)
        $segReal = if ($w2.Ok) { $segDesdeMuerte } else { $null }
        $a.mediciones.reaparicion_segundos_hasta_health = $segReal
        $a.mediciones.reaparicion_segundos_hasta_puerto = $(if ($null -ne $w2.SegPuerto) { [math]::Round($segDesdeMuerte - $w2.SegHealth + $w2.SegPuerto, 1) } else { $null })
        $cl = Get-ClasificacionReaparicion $segReal $repActiva
        $a.mediciones.reaparicion_lectura = $cl.Lectura
        $infoTarea = Get-InfoTarea $script:NombreTarea
        $deltaTarea = $null
        if ($infoTarea -and $infoTarea.UltimaEjecucion) {
            $deltaTarea = [math]::Round(($infoTarea.UltimaEjecucion - $tMuerte).TotalSeconds, 1)
            $a.mediciones.reaparicion_tarea_arranco_a_los_segundos = $deltaTarea
            $a.mediciones.tarea_ultimo_resultado = $infoTarea.UltimoResultado
        }
        if ($w2.Ok) {
            Add-Check 'reaparicion' $cl.Resultado ('/health volvio a los {0} de matar el proceso; {1}' -f (Format-Seg $segReal), $cl.Lectura)
            if ($null -ne $deltaTarea) { Write-Informe ('  la tarea registra su ultimo arranque {0} despues de la muerte; ultimo resultado de la tarea: {1}' -f (Format-Seg $deltaTarea), $infoTarea.UltimoResultado) }
        }
        else {
            Add-Check 'reaparicion' 'FALLA' ('{0}; {1}' -f $cl.Lectura, (Get-DiagnosticoArranque $ctx $logAntes $secretos))
            # Se levanta a mano para poder seguir con lo demas.
            $run2 = Invoke-Nativo 'schtasks.exe' @('/Run', '/TN', $script:NombreTarea) 30
            $w4 = Wait-Daemon $ctx $script:LimiteArranqueSeg
            if ($w4.Ok) { Write-Informe ('  se arranco a mano con schtasks /Run (codigo {0}) y /health respondio a los {1}: el ensayo sigue' -f $run2.Codigo, (Format-Seg $w4.SegHealth)) 'Yellow' }
            else { Add-Check 'revivir_a_mano' 'FALLA' ('tampoco arranco con schtasks /Run (codigo {0}); {1}' -f $run2.Codigo, (Get-DiagnosticoArranque $ctx $logAntes $secretos)) }
        }
        }

        # --- 9. Idempotencia -----------------------------------------------
        # Dos repeticiones, como el paso 3 de M1: con las mismas banderas y sin
        # ninguna (esta ultima es la que NO debe devolver una instalacion LAN a
        # 127.0.0.1).
        Write-Paso '9. Idempotencia: --apply --register otra vez'
        $variantes = @(
            [pscustomobject]@{ N = '2'; Que = 'con las mismas banderas'; Argumentos = @('setup', 'service', '--profile', 'lan', '--addr', $direccion, '--apply', '--register') },
            [pscustomobject]@{ N = '3'; Que = 'sin banderas'; Argumentos = @('setup', 'service', '--apply', '--register') }
        )
        foreach ($vr in $variantes) {
            $ap2 = Invoke-Nativo $ctx.Binario $vr.Argumentos 120 -Utf8
            $texto2 = Protect-Secreto ($ap2.Salida + $ap2.Error) $secretos
            Write-TextoArchivo (Join-Path $ctx.Salida ('apply-{0}.txt' -f $vr.N)) $texto2
            Write-Informe ('  repeticion {0} ({1}):' -f $vr.N, $vr.Que)
            foreach ($l in ((Protect-Secreto $ap2.Salida $secretos) -split "`r?`n")) { if ($l.Trim()) { Write-Informe ('  | {0}' -f $l.TrimEnd()) } }
            $id = 'reapply_' + $vr.N
            if ($ap2.Codigo -eq 0) { Add-Check ($id + '_codigo') 'PASA' ('--apply --register {0} sale con 0' -f $vr.Que) }
            else { Add-Check ($id + '_codigo') 'FALLA' ('--apply --register {0} devolvio {1}' -f $vr.Que, $ap2.Codigo) }
            if ($texto2 -match '(?m)^conservado\s') { Add-Check ($id + '_conserva_env') 'PASA' 'el env file figura como "conservado"' }
            else { Add-Check ($id + '_conserva_env') 'FALLA' 'la salida no dice que el env file se conservo' }
            $n2 = Get-ConteoTarea $script:NombreTarea
            $r2 = @(Get-DetalleRegla $script:NombreRegla).Count
            if ($n2 -eq 1 -and $r2 -eq 1) { Add-Check ($id + '_unicos') 'PASA' 'una tarea y una regla tras repetir' } else { Add-Check ($id + '_unicos') 'FALLA' ('tareas={0} reglas={1}' -f $n2, $r2) }
            $envDespues = Read-Texto $ctx.Env
            $hashDespues = Get-HashToken $envDespues
            $a.token.sha256_tras_reaplicar = $hashDespues
            if ($hashDespues -and $hashDespues -eq $a.token.sha256) { Add-Check ($id + '_token') 'PASA' 'la huella del token no cambio' } else { Add-Check ($id + '_token') 'FALLA' 'la huella del token cambio o no se pudo leer' }
            if ((Get-ValorEnv $envDespues $script:ClaveDireccion) -eq $direccion) { Add-Check ($id + '_direccion') 'PASA' ('la direccion sigue siendo {0}' -f $direccion) } else { Add-Check ($id + '_direccion') 'FALLA' 'la direccion cambio' }
            $viva = Get-Salud $url $script:TimeoutSaludMs
            if ($viva.Ok) { Add-Check ($id + '_daemon_vivo') 'PASA' 'el daemon siguio respondiendo al reaplicar' }
            else {
                # Re-registrar una tarea con una instancia viva no esta verificado en
                # Windows: si la tira, el ensayo lo apunta y mide cuanto tarda en volver.
                $w3 = Wait-Daemon $ctx $script:LimiteReaparicionSeg 0 2000
                if ($w3.Ok) { Add-Check ($id + '_daemon_vivo') 'AVISO' ('reaplicar tiro el daemon; /health volvio a los {0}' -f (Format-Seg $w3.SegHealth)) }
                else { Add-Check ($id + '_daemon_vivo') 'FALLA' ('reaplicar tiro el daemon y no volvio en {0} s' -f $script:LimiteReaparicionSeg) }
            }
        }

        # --- 10. doctor -----------------------------------------------------
        Write-Paso '10. doctor'
        $d = Invoke-Nativo $ctx.Binario @('doctor') 120 -Utf8
        $textoDoc = Protect-Secreto ($d.Salida + "`r`n" + $d.Error) $secretos
        Write-TextoArchivo (Join-Path $ctx.Salida 'doctor.txt') $textoDoc
        $svc = @(Get-LineasServicioDoctor $d.Salida)
        $a.mediciones.doctor = @($svc)
        if ($svc.Count -eq 0) {
            Add-Check 'doctor' 'AVISO' ('doctor (codigo {0}) no llego a imprimir los chequeos de servicio; sin base de datos alcanzable aborta antes. Ver doctor.txt' -f $d.Codigo)
        }
        else {
            foreach ($s in $svc) { Write-Informe ('  {0,-26} {1,-5} {2}' -f $s.nombre, $s.estado, $s.detalle) }
            $malos = @($svc | Where-Object { $_.estado -eq 'fail' })
            $avisos = @($svc | Where-Object { $_.estado -eq 'warn' })
            if ($malos.Count -gt 0) { Add-Check 'doctor' 'FALLA' ('{0} chequeo(s) de servicio en FAIL: {1}' -f $malos.Count, (($malos | ForEach-Object { $_.nombre }) -join ', ')) }
            elseif ($avisos.Count -gt 0) { Add-Check 'doctor' 'AVISO' ('avisos de servicio: {0}' -f (($avisos | ForEach-Object { '{0}: {1}' -f $_.nombre, $_.detalle }) -join ' | ')) }
            else { Add-Check 'doctor' 'PASA' ('{0} chequeos de servicio en ok' -f $svc.Count) }
        }

        # --- 11. Log --------------------------------------------------------
        Write-Paso '11. Cola del log'
        $cola = Get-ColaLog $ctx.Log $logAntes 65536
        if (-not $cola.Existe) { Add-Check 'log' 'FALLA' ('no existe {0}: el launcher no llego a escribir' -f $ctx.Log) }
        else {
            $ult = @(($cola.Texto -split "`r?`n") | Where-Object { $_.Trim() } | Select-Object -Last 40)
            Write-TextoArchivo (Join-Path $ctx.Salida 'log-cola.txt') ((Protect-Secreto ($ult -join "`r`n") $secretos) + "`r`n")
            $errs = @(Get-LineasError $cola.Texto)
            if ($errs.Count -eq 0) { Add-Check 'log' 'PASA' ('{0} bytes de log desde el primer arranque, sin lineas de error' -f ($cola.Longitud - $logAntes)) }
            else { Add-Check 'log' 'FALLA' ('{0} linea(s) de error; ultima: {1}' -f $errs.Count, (Protect-Secreto $errs[$errs.Count - 1] $secretos)) }
            $a.log_bytes = $cola.Longitud
        }

        # --- 12. Armar la comprobacion del reinicio -------------------------
        Write-Paso '12. Armar la comprobacion del reinicio'
        Install-ComprobacionReinicio $ctx $a
    }
    catch {
        if ($_.Exception.Message -ne 'RECHAZO') {
            Add-Check 'excepcion' 'FALLA' ('error inesperado del script: {0} (linea {1})' -f $_.Exception.Message, $_.InvocationInfo.ScriptLineNumber)
        }
        else { throw }
    }
    finally {
        if ($script:Codigo -ne 2) {
            $a.fin = Get-AhoraTexto
            $a.veredicto = Get-Veredicto $script:Checks
            $a.checks = @($script:Checks)
            Write-Json (Join-Path $ctx.Salida 'antes.json') $a
            Write-Informe ''
            Write-Informe ('VEREDICTO: {0}' -f $a.veredicto) $(if ($a.veredicto -eq 'PASA') { 'Green' } else { 'Red' })
            if ($script:ModoTodo) {
                # Todo ya cuenta lo que sigue con sus propias palabras.
            }
            elseif ($a.veredicto -eq 'PASA' -and $a.armado_en) {
                Write-Informe 'Siguiente paso (la comprobacion del reinicio ya esta armada):'
                Write-Informe '  1. Reiniciar desde el menu de Windows (REINICIAR; "Apagar" con Inicio rapido no reinicia el kernel).'
                Write-Informe '  2. En la pantalla de inicio de sesion, NO entrar. Esperar 6 minutos (la comprobacion termina antes si el daemon responde antes; vigila hasta 10).'
                Write-Informe ('  3. Desde OTRA maquina de la LAN: curl http://{0}/health  (prueba el firewall de verdad; desde este equipo no se puede)' -f $direccion)
                Write-Informe '  4. Entrar y ejecutar:  .\ensayo-madre.ps1 -Fase Despues'
            }
            elseif ($a.veredicto -eq 'PASA') {
                Write-Informe 'La comprobacion del reinicio NO quedo armada: no hay nada que medir en el reinicio.'
            }
            else {
                Write-Informe 'No sigas con el reinicio. Manda la carpeta del informe tal cual.'
            }
            Save-Informe $ctx
            if ($a.veredicto -eq 'FALLA') { $script:Codigo = 1 }
        }
    }
}

function Wait-Hasta {
    # Sondeo acotado: nunca una espera sin tope. La condicion puede tardar lo que
    # tarde en una pasada; el tope se comprueba entre pasadas.
    param([scriptblock]$Condicion, [int]$TimeoutSeg, [int]$IntervaloMs = 1000)
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    while ($true) {
        $r = & $Condicion
        if ($r) { return [pscustomobject]@{ Cumplido = $true; Segundos = [math]::Round($sw.Elapsed.TotalSeconds, 1); Valor = $r } }
        if ($sw.Elapsed.TotalSeconds -ge $TimeoutSeg) { return [pscustomobject]@{ Cumplido = $false; Segundos = [math]::Round($sw.Elapsed.TotalSeconds, 1); Valor = $null } }
        Start-Sleep -Milliseconds $IntervaloMs
    }
}

# ---------------------------------------------------------------------------
# La comprobacion del reinicio: una tarea de SYSTEM que corre al arrancar el
# equipo, ANTES de que nadie inicie sesion, y apunta cuando respondio el daemon.
# ---------------------------------------------------------------------------

function Install-ComprobacionReinicio {
    param($Ctx, $A)
    if ([string]::IsNullOrEmpty($PSCommandPath)) {
        Add-Check 'reinicio_armado' 'FALLA' 'no se conoce la ruta de este script ($PSCommandPath vacio): ejecutarlo con -File o desde un .ps1, no pegado en la consola'
        return
    }
    $wd = $Ctx.Trabajo
    [void](New-Item -ItemType Directory -Path $wd -Force)
    # Este directorio contiene un script que corre como SYSTEM: si un usuario
    # normal pudiera modificarlo, seria una escalada de privilegios. Se cortan las
    # herencias y solo quedan SYSTEM (S-1-5-18) y Administradores (S-1-5-32-544);
    # por SID porque los nombres cambian con el idioma de Windows.
    $ic = Invoke-Nativo 'icacls.exe' @($wd, '/inheritance:r', '/grant:r', '*S-1-5-18:(OI)(CI)F', '*S-1-5-32-544:(OI)(CI)F') 30
    if ($ic.Codigo -ne 0) {
        Add-Check 'reinicio_armado' 'FALLA' ('icacls devolvio {0}: {1}. No se arma una tarea de SYSTEM sobre una carpeta sin proteger' -f $ic.Codigo, $ic.Error.Trim())
        return
    }
    $copia = Join-Path $wd 'ensayo-madre.ps1'
    Copy-Item -LiteralPath $PSCommandPath -Destination $copia -Force
    $resultadoPrevio = Join-Path $wd 'despues-arranque.json'
    if (Test-Path -LiteralPath $resultadoPrevio) { Remove-Item -LiteralPath $resultadoPrevio -Force }
    $armado = Get-AhoraTexto
    Write-Json (Join-Path $wd 'arranque-config.json') ([ordered]@{ ip = $Ctx.Ip; puerto = $Ctx.Puerto; limite_min = $script:EsperaArranqueMin; armado_en = $armado })
    try {
        $accion = New-ScheduledTaskAction -Execute 'powershell.exe' -Argument ('-NoProfile -NonInteractive -ExecutionPolicy Bypass -File "{0}" -Fase Arranque' -f $copia)
        $disparador = New-ScheduledTaskTrigger -AtStartup
        $principal = New-ScheduledTaskPrincipal -UserId 'SYSTEM' -LogonType ServiceAccount -RunLevel Highest
        $ajustes = New-ScheduledTaskSettingsSet -ExecutionTimeLimit (New-TimeSpan -Minutes ($script:EsperaArranqueMin + 10)) -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -StartWhenAvailable
        [void](Register-ScheduledTask -TaskName $script:TareaReinicio -Action $accion -Trigger $disparador -Principal $principal -Settings $ajustes -Description 'Ensayo MemoryIndustry: mide el arranque sin sesion. Se borra sola al terminar.' -Force)
    }
    catch {
        Add-Check 'reinicio_armado' 'FALLA' ('no se pudo registrar {0}: {1}' -f $script:TareaReinicio, $_.Exception.Message)
        return
    }
    if ((Get-ConteoTarea $script:TareaReinicio) -eq 1) {
        $A.armado_en = $armado
        Add-Check 'reinicio_armado' 'PASA' ('tarea {0} (SYSTEM, al arrancar, retardo 0) armada; vigilara {1} min tras el arranque' -f $script:TareaReinicio, $script:EsperaArranqueMin)
    }
    else { Add-Check 'reinicio_armado' 'FALLA' 'la tarea de comprobacion no figura tras registrarla' }
}

function Invoke-FaseArranque {
    # Corre como SYSTEM al arrancar el equipo. Sin elevacion que comprobar y sin
    # consola: todo lo que dice va a un fichero de traza.
    $wd = Get-DirTrabajo
    $traza = Join-Path $wd 'arranque.log'
    $cfg = Read-Json (Join-Path $wd 'arranque-config.json')
    $res = [ordered]@{
        estado = 'iniciado'; version_script = 1
        tarea_inicio = (Get-AhoraTexto); arranque_sistema = $null
        primer_puerto = $null; primer_health = $null
        segundos_arranque_hasta_puerto = $null; segundos_arranque_hasta_health = $null
        segundos_tarea_hasta_health = $null
        sesion_al_iniciar = $null; sesion_al_primer_health = $null; detalle_sesion = $null
        proceso = $null; salud = $null; error = $null
    }
    $guardar = { Write-Json (Join-Path $wd 'despues-arranque.json') $res }
    try {
        if ($null -eq $cfg) { throw 'falta arranque-config.json' }
        $ctx = [pscustomobject]@{ Ip = [string]$cfg.ip; Puerto = [int]$cfg.puerto }
        $limite = [int]$cfg.limite_min * 60
        $boot = Get-ArranqueSistema
        $res.arranque_sistema = $boot.ToString('o')
        $s0 = Get-SesionesUsuario
        $res.sesion_al_iniciar = $s0.Hay
        & $guardar
        $sw = [System.Diagnostics.Stopwatch]::StartNew()
        $url = 'http://{0}:{1}/health' -f $ctx.Ip, $ctx.Puerto
        while ($sw.Elapsed.TotalSeconds -lt $limite) {
            if ($null -eq $res.primer_puerto) {
                $e = @(Get-Escuchas $ctx.Puerto | Where-Object { $_.Direccion -eq $ctx.Ip })
                if ($e.Count -gt 0) {
                    $ahora = [DateTimeOffset]::Now
                    $res.primer_puerto = $ahora.ToString('o')
                    $res.segundos_arranque_hasta_puerto = [math]::Round(($ahora - $boot).TotalSeconds, 1)
                }
            }
            $h = Get-Salud $url $script:TimeoutSaludMs
            if ($h.Ok) {
                $ahora = [DateTimeOffset]::Now
                $res.primer_health = $ahora.ToString('o')
                $res.segundos_arranque_hasta_health = [math]::Round(($ahora - $boot).TotalSeconds, 1)
                $res.segundos_tarea_hasta_health = [math]::Round($sw.Elapsed.TotalSeconds, 1)
                $s1 = Get-SesionesUsuario
                $res.sesion_al_primer_health = $s1.Hay
                $res.detalle_sesion = [ordered]@{ explorer = $s1.Explorer; quser = $s1.Quser; consola_con_usuario = $s1.ConsolaTieneUsuario }
                $res.salud = [ordered]@{ status = $h.Json.status; database = $h.Json.database; ready = $h.Json.ready; version = $h.Json.version }
                $esc = @(Get-Escuchas $ctx.Puerto | Where-Object { $_.Direccion -eq $ctx.Ip })
                if ($esc.Count -gt 0) {
                    $info = Get-InfoProceso $esc[0].IdProceso
                    if ($info) {
                        $res.proceso = [ordered]@{
                            pid = $info.IdProceso; nombre = $info.Nombre; sesion_id = $info.SesionId; usuario = $info.Usuario
                            inicio = $(if ($info.Inicio) { ([DateTimeOffset]$info.Inicio).ToString('o') } else { $null })
                        }
                    }
                }
                break
            }
            Start-Sleep -Seconds 2
        }
        $res.estado = 'completo'
    }
    catch { $res.error = $_.Exception.Message; $res.estado = 'completo' }
    finally {
        & $guardar
        # Una sola vez: si no se borrara, repetiria la medicion en cada arranque y
        # pisaria el resultado que Despues aun no ha leido.
        try { Unregister-ScheduledTask -TaskName $script:TareaReinicio -Confirm:$false -ErrorAction Stop; $nota = 'tarea retirada' } catch { $nota = ('no se pudo retirar la tarea: ' + $_.Exception.Message) }
        try { [System.IO.File]::AppendAllText($traza, ((Get-AhoraTexto) + ' ' + $res.estado + ' ' + $nota + "`r`n")) } catch { }
    }
}

# ===========================================================================
# Fase Despues
# ===========================================================================

function Invoke-FaseDespues {
    $ctx = $script:Ctx
    Test-PrerequisitosComunes
    $antes = Read-Json (Join-Path $ctx.Salida 'antes.json')
    if ($null -eq $antes) {
        Stop-Rechazo ('no encuentro antes.json en {0}. Ejecuta primero -Fase Antes, o pasa -Salida con la carpeta donde quedo.' -f $ctx.Salida)
    }
    $ctx.Ip = [string]$antes.ip
    $ctx.Puerto = [int]$antes.puerto
    if ($antes.raiz) {
        # La raiz sale de LOCALAPPDATA de quien instalo: si Despues lo ejecuta otro
        # administrador, mirar la suya daria un "no existe" falso.
        $ctx.Raiz = [string]$antes.raiz
        $ctx.Env = Join-Path $ctx.Raiz $script:ArchivoEnv
        $ctx.Log = Join-Path $ctx.Raiz $script:ArchivoLog
        $ctx.Launcher = Join-Path $ctx.Raiz $script:ArchivoLauncher
        $ctx.Xml = Join-Path $ctx.Raiz $script:ArchivoXml
    }
    $direccion = '{0}:{1}' -f $ctx.Ip, $ctx.Puerto
    $url = 'http://{0}/health' -f $direccion
    $secretos = @()
    try {
        Write-Informe ('MemoryIndustry - ensayo de instalacion (Despues) - {0}' -f (Get-Date -Format 'yyyy-MM-dd HH:mm:ss'))
        Write-Informe ('Direccion del ensayo: {0}' -f $direccion)

        Write-Paso 'Lo que midio la comprobacion al arrancar'
        $origen = Join-Path $ctx.Trabajo 'despues-arranque.json'
        if (Test-Path -LiteralPath $origen) { Copy-Item -LiteralPath $origen -Destination (Join-Path $ctx.Salida 'despues-arranque.json') -Force }
        $arranque = Read-Json (Join-Path $ctx.Salida 'despues-arranque.json')
        if ($null -ne $arranque) {
            Write-Informe ('  arranque del sistema:     {0}' -f $arranque.arranque_sistema)
            Write-Informe ('  puerto abierto a los:     {0} del arranque' -f (Format-Seg $arranque.segundos_arranque_hasta_puerto))
            Write-Informe ('  /health respondio a los:  {0} del arranque ({1} desde que arranco la tarea)' -f (Format-Seg $arranque.segundos_arranque_hasta_health), (Format-Seg $arranque.segundos_tarea_hasta_health))
            Write-Informe ('  sesion de usuario: al iniciar la tarea {0}, al primer /health {1}' -f $arranque.sesion_al_iniciar, $arranque.sesion_al_primer_health)
            if ($arranque.proceso) { Write-Informe ('  proceso: {0} PID {1}, sesion {2}, usuario {3}, inicio {4}' -f $arranque.proceso.nombre, $arranque.proceso.pid, $arranque.proceso.sesion_id, $arranque.proceso.usuario, $arranque.proceso.inicio) }
            if ($arranque.error) { Write-Informe ('  error de la comprobacion: {0}' -f $arranque.error) 'Yellow' }
        }

        Write-Paso 'Estado actual'
        $envTexto = Read-Texto $ctx.Env
        $cola = Get-ColaLog $ctx.Log ([long]$antes.log_bytes) 1048576
        $errores = @(Get-LineasError $cola.Texto)
        $token = Get-ValorEnv $envTexto $script:ClaveToken
        if ($token) { $secretos = @($token) }
        $errores = @($errores | ForEach-Object { Protect-Secreto $_ $secretos })
        $salud = Get-Salud $url $script:TimeoutSaludMs
        $actual = [pscustomobject]@{
            direccion    = (Get-ValorEnv $envTexto $script:ClaveDireccion)
            token_sha256 = (Get-HashToken $envTexto)
            errores_log  = $errores
            health_ahora = $salud.Ok
            primera_sesion = (Get-PrimeraSesion)
        }
        if ($cola.Existe) {
            $ult = @(($cola.Texto -split "`r?`n") | Where-Object { $_.Trim() } | Select-Object -Last 60)
            Write-TextoArchivo (Join-Path $ctx.Salida 'log-cola-despues.txt') ((Protect-Secreto ($ult -join "`r`n") $secretos) + "`r`n")
        }

        Write-Paso 'Veredicto por comprobacion'
        $checks = @(Get-ChecksDespues $antes $arranque $actual $script:UmbralArranqueSistemaSeg)
        foreach ($c in $checks) { [void]$script:Checks.Add($c); Write-Informe (Format-Check $c) $(if ($c.resultado -eq 'PASA') { 'Green' } elseif ($c.resultado -eq 'FALLA') { 'Red' } else { 'Yellow' }) }

        if ($MatarTrasReinicio) {
            Write-Paso 'Matar el daemon con la repeticion de 5 min ya activa'
            $escuchas = @(Get-Escuchas $ctx.Puerto | Where-Object { $_.Direccion -eq $ctx.Ip })
            if ($escuchas.Count -eq 0) { Add-Check 'reaparicion_tras_reinicio' 'FALLA' 'no hay daemon que matar' }
            else {
                $idm = $escuchas[0].IdProceso
                $tM = Get-Date
                if (-not (Stop-Daemon $idm)) { Add-Check 'reaparicion_tras_reinicio' 'FALLA' ('no se pudo terminar el proceso {0}' -f $idm) }
                else {
                    [void](Wait-Hasta { @(Get-Escuchas $ctx.Puerto | Where-Object { $_.IdProceso -eq $idm }).Count -eq 0 } 20 250)
                    $w = Wait-Daemon $ctx $script:LimiteReaparicionSeg $idm 2000
                    $cl = Get-ClasificacionReaparicion $(if ($w.Ok) { $w.SegHealth } else { $null }) $true
                    if ($w.Ok) { Add-Check 'reaparicion_tras_reinicio' $cl.Resultado ('volvio /health a los {0}; {1}' -f (Format-Seg $w.SegHealth), $cl.Lectura) }
                    else { Add-Check 'reaparicion_tras_reinicio' 'FALLA' $cl.Lectura }
                }
            }
        }
    }
    catch {
        if ($_.Exception.Message -ne 'RECHAZO') { Add-Check 'excepcion' 'FALLA' ('error inesperado del script: {0} (linea {1})' -f $_.Exception.Message, $_.InvocationInfo.ScriptLineNumber) }
        else { throw }
    }
    finally {
        if ($script:Codigo -ne 2) {
            $veredicto = Get-Veredicto $script:Checks
            $despues = [ordered]@{ fase = 'Despues'; fin = (Get-AhoraTexto); veredicto = $veredicto; checks = @($script:Checks); arranque = $arranque }
            Write-Json (Join-Path $ctx.Salida 'despues.json') $despues
            Write-Informe ''
            Write-Informe ('VEREDICTO: {0}' -f $veredicto) $(if ($veredicto -eq 'PASA') { 'Green' } else { 'Red' })
            Write-Informe ('Pendiente a mano, desde otra maquina de la LAN: curl http://{0}/health y una herramienta MCP con el token (el firewall no se puede probar desde el propio equipo).' -f $direccion)
            Save-Informe $ctx -Anexar
            if ($veredicto -eq 'FALLA') { $script:Codigo = 1 }
        }
    }
}

# ===========================================================================
# Fase Limpiar
# ===========================================================================

function Invoke-FaseLimpiar {
    $ctx = $script:Ctx
    Test-PrerequisitosComunes
    if (-not (Test-Path -LiteralPath $ctx.Binario)) {
        Stop-Rechazo ('no encuentro el binario en {0}: `setup service --uninstall` es lo que dice que retirar, y sin el no invento las ordenes. Pasa -Binario.' -f $ctx.Binario)
    }
    $estado = Read-Json (Join-Path $ctx.Salida 'estado-ensayo.json')
    if ($null -ne $estado) {
        $ctx.Puerto = [int]$estado.puerto
        $ctx.Ip = [string]$estado.ip
    }
    if (($null -eq $estado -or [bool]$estado.preexistia) -and -not $Forzar) {
        Stop-Rechazo ('no consta que esta instalacion sea solo del ensayo (estado-ensayo.json {0}). Limpiar quitaria la tarea y la regla de una instalacion que ya existia, y no se hace sin que alguien lo confirme. No se toco nada. Manda la carpeta ensayo-informe.' -f $(if ($null -eq $estado) { 'no existe en ' + $ctx.Salida } else { 'dice que ya existia' }))
    }
    $quitado = New-Object System.Collections.ArrayList
    try {
        Write-Informe ('MemoryIndustry - limpieza del ensayo - {0}' -f (Get-Date -Format 'yyyy-MM-dd HH:mm:ss'))

        Write-Paso '1. Comprobacion del reinicio'
        $origen = Join-Path $ctx.Trabajo 'despues-arranque.json'
        if ((Test-Path -LiteralPath $origen) -and -not (Test-Path -LiteralPath (Join-Path $ctx.Salida 'despues-arranque.json'))) {
            Copy-Item -LiteralPath $origen -Destination (Join-Path $ctx.Salida 'despues-arranque.json') -Force
            Write-Informe '  se copio despues-arranque.json a la carpeta del informe antes de borrar nada'
        }
        foreach ($nombreTarea in @($script:TareaReinicio, $script:TareaDespues)) {
            if ((Get-ConteoTarea $nombreTarea) -gt 0) {
                try { Unregister-ScheduledTask -TaskName $nombreTarea -Confirm:$false -ErrorAction Stop; [void]$quitado.Add('tarea ' + $nombreTarea) } catch { Add-Check 'limpiar_reinicio' 'FALLA' $_.Exception.Message }
            }
        }
        if (Test-Path -LiteralPath $ctx.Trabajo) {
            try { Remove-Item -LiteralPath $ctx.Trabajo -Recurse -Force -ErrorAction Stop; [void]$quitado.Add('carpeta ' + $ctx.Trabajo) } catch { Add-Check 'limpiar_trabajo' 'FALLA' $_.Exception.Message }
        }
        if ((Get-ConteoTarea $script:TareaReinicio) -eq 0) { Add-Check 'limpiar_reinicio' 'PASA' ('la tarea {0} ya no esta' -f $script:TareaReinicio) }

        Write-Paso '2. Detener el daemon del ensayo'
        # `--uninstall` retira la tarea, no el proceso: sin esto el daemon seguiria
        # sirviendo en el puerto del ensayo sin nadie que lo supervise.
        [void](Invoke-Nativo 'schtasks.exe' @('/End', '/TN', $script:NombreTarea) 30)
        if ($ctx.Puerto) {
            foreach ($e in @(Get-Escuchas $ctx.Puerto)) {
                $info = Get-InfoProceso $e.IdProceso
                if (Test-ProcesoEsNuestro $info $ctx.Binario) {
                    if (Stop-Daemon $e.IdProceso) { [void]$quitado.Add(('proceso memory-industry PID {0}' -f $e.IdProceso)) } else { Add-Check 'limpiar_proceso' 'FALLA' ('no se pudo terminar el PID {0}' -f $e.IdProceso) }
                }
                else { Write-Informe ('  el puerto {0} lo usa otro programa (PID {1}); no se toca' -f $ctx.Puerto, $e.IdProceso) }
            }
        }

        Write-Paso '3. setup service --uninstall'
        $un = Invoke-Nativo $ctx.Binario @('setup', 'service', '--uninstall') 60 -Utf8
        $textoUn = $un.Salida + $un.Error
        foreach ($l in ($un.Salida -split "`r?`n")) { if ($l.Trim()) { Write-Informe ('  | {0}' -f $l.TrimEnd()) } }
        if ($un.NoArranco -or $un.Agotado -or $un.Codigo -ne 0) {
            Add-Check 'uninstall' 'FALLA' ('setup service --uninstall devolvio {0}; no se ejecuto ninguna orden de retirada' -f $un.Codigo)
            return
        }
        Add-Check 'uninstall' 'PASA' 'imprimio las ordenes de retirada'
        $ordenes = @(Get-OrdenesDesinstalar $textoUn)
        if ($ordenes.Count -eq 0) { Add-Check 'ordenes' 'FALLA' 'no encontre las ordenes schtasks/netsh en lo que imprimio --uninstall' }
        foreach ($o in $ordenes) {
            $exe = if ($o.Programa -eq 'schtasks') { 'schtasks.exe' } else { 'netsh.exe' }
            $r = Invoke-Nativo $exe $o.Argumentos 60
            if ($r.Codigo -eq 0) { Add-Check ('retirar_' + $o.Programa) 'PASA' ('{0} -> 0' -f $o.Linea); [void]$quitado.Add($o.Linea) }
            else {
                # Un codigo != 0 es normal si ya no estaba: se comprueba que NO esta.
                $sigue = if ($o.Programa -eq 'schtasks') { (Get-ConteoTarea $script:NombreTarea) -gt 0 } else { @(Get-DetalleRegla $script:NombreRegla).Count -gt 0 }
                if ($sigue) { Add-Check ('retirar_' + $o.Programa) 'FALLA' ('{0} -> {1}: {2}' -f $o.Linea, $r.Codigo, $r.Error.Trim()) }
                else { Add-Check ('retirar_' + $o.Programa) 'PASA' ('{0} -> {1}, pero ya no estaba' -f $o.Linea, $r.Codigo) }
            }
        }

        Write-Paso '4. Ficheros de datos'
        if ($BorrarEnv) {
            foreach ($f in @($ctx.Env, $ctx.Log, ($ctx.Log + '.1'))) {
                if (Test-Path -LiteralPath $f) {
                    try { Remove-Item -LiteralPath $f -Force -ErrorAction Stop; [void]$quitado.Add($f) } catch { Add-Check 'limpiar_datos' 'FALLA' $_.Exception.Message }
                }
            }
            if ((Test-Path -LiteralPath $ctx.Raiz) -and @(Get-ChildItem -LiteralPath $ctx.Raiz -Force).Count -eq 0) { Remove-Item -LiteralPath $ctx.Raiz -Force; [void]$quitado.Add($ctx.Raiz) }
            Write-Informe '  -BorrarEnv: env file (con el token) y logs borrados'
        }
        else { Write-Informe ('  se conservan {0} (token) y {1} (evidencia); -BorrarEnv los quita' -f $ctx.Env, $ctx.Log) }

        Write-Paso '5. Comprobacion final'
        $queda = New-Object System.Collections.ArrayList
        if ((Get-ConteoTarea $script:NombreTarea) -gt 0) { [void]$queda.Add('tarea ' + $script:NombreTarea) }
        if ((Get-ConteoTarea $script:TareaReinicio) -gt 0) { [void]$queda.Add('tarea ' + $script:TareaReinicio) }
        if ((Get-ConteoTarea $script:TareaDespues) -gt 0) { [void]$queda.Add('tarea ' + $script:TareaDespues) }
        if (@(Get-DetalleRegla $script:NombreRegla).Count -gt 0) { [void]$queda.Add('regla ' + $script:NombreRegla) }
        foreach ($f in @($ctx.Launcher, $ctx.Xml)) { if (Test-Path -LiteralPath $f) { [void]$queda.Add($f) } }
        if ($ctx.Puerto -and @(Get-Escuchas $ctx.Puerto).Count -gt 0) { [void]$queda.Add(('algo escucha aun en el puerto {0}' -f $ctx.Puerto)) }
        if ($queda.Count -eq 0) { Add-Check 'limpieza' 'PASA' 'no quedan tarea, regla, launcher, XML ni daemon del ensayo' }
        else { Add-Check 'limpieza' 'FALLA' ('queda: {0}' -f ($queda -join '; ')) }
        Write-Informe ('Quitado: {0}' -f $(if ($quitado.Count) { $quitado -join ' | ' } else { 'nada' }))
    }
    catch {
        if ($_.Exception.Message -ne 'RECHAZO') { Add-Check 'excepcion' 'FALLA' ('error inesperado del script: {0} (linea {1})' -f $_.Exception.Message, $_.InvocationInfo.ScriptLineNumber) }
        else { throw }
    }
    finally {
        if ($script:Codigo -ne 2) {
            Write-Informe ''
            Write-Informe ('VEREDICTO: {0}' -f (Get-Veredicto $script:Checks)) $(if ((Get-Veredicto $script:Checks) -eq 'PASA') { 'Green' } else { 'Red' })
            if (Test-Path -LiteralPath $ctx.Salida) { Save-Informe $ctx -Anexar }
            if ((Get-Veredicto $script:Checks) -eq 'FALLA') { $script:Codigo = 1 }
        }
    }
}

# ===========================================================================
# Fase Todo (doble clic) y Final (al volver a iniciar sesion)
# ===========================================================================

function Get-ProblemasSumas {
    # Compara lo que hay en disco con SHA256SUMS.txt, que viaja junto al script. Lee
    # cada fichero que la lista nombra; el binario se busca donde diga -Binario.
    param([string]$Carpeta, [string]$RutaBinario)
    $texto = Read-Texto (Join-Path $Carpeta $script:ArchivoSumas)
    if ($null -eq $texto) {
        return @('no encuentro SHA256SUMS.txt junto al script: sin el no se puede comprobar que el .exe es el que se ensayo')
    }
    $sumas = Read-Sumas $texto
    $reales = @{}
    foreach ($nombre in @($sumas.Keys)) {
        if ($nombre -eq 'memory-industry.exe') { $ruta = $RutaBinario }
        else { $ruta = Join-Path $Carpeta ($nombre -replace '/', [string][System.IO.Path]::DirectorySeparatorChar) }
        if (Test-Path -LiteralPath $ruta -PathType Leaf) {
            $reales[$nombre] = (Get-FileHash -LiteralPath $ruta -Algorithm SHA256).Hash.ToLower()
        }
    }
    return @(Compare-Sumas $sumas $reales)
}

function Install-TareaDespues {
    # Una tarea de UN solo uso: al iniciar sesion ESTE usuario, con privilegios
    # elevados y la consola a la vista, ejecuta la fase Final (Despues + Limpiar) y se
    # borra a si misma. Corre como el usuario y no como SYSTEM: tiene que abrir una
    # ventana en su escritorio, y no guarda ninguna clave.
    param($Ctx)
    if ([string]::IsNullOrEmpty($PSCommandPath)) {
        Add-Check 'tarea_despues' 'FALLA' 'no se conoce la ruta de este script ($PSCommandPath vacio)'
        return $false
    }
    # La persona sentada (resuelta en Humo), no quien haya elevado: la ventana del
    # resultado tiene que abrirse en SU escritorio.
    $usuario = $script:UsuarioInteractivo
    if ([string]::IsNullOrEmpty($usuario)) { $usuario = Get-UsuarioActual }
    $extra = ''
    if ($Conservar) { $extra = ' -Conservar' }
    $argumento = '-NoProfile -ExecutionPolicy Bypass -File "{0}" -Fase Final -Binario "{1}" -Salida "{2}"{3}' -f $PSCommandPath, $Ctx.Binario, $Ctx.Salida, $extra
    try {
        $accion = New-ScheduledTaskAction -Execute 'powershell.exe' -Argument $argumento
        $disparador = New-ScheduledTaskTrigger -AtLogOn -User $usuario
        $principal = New-ScheduledTaskPrincipal -UserId $usuario -LogonType Interactive -RunLevel Highest
        $ajustes = New-ScheduledTaskSettingsSet -ExecutionTimeLimit (New-TimeSpan -Hours 2) -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -StartWhenAvailable
        [void](Register-ScheduledTask -TaskName $script:TareaDespues -Action $accion -Trigger $disparador -Principal $principal -Settings $ajustes -Description 'Ensayo MemoryIndustry: comprueba el resultado al volver a iniciar sesion. Se borra sola.' -Force)
    }
    catch {
        Add-Check 'tarea_despues' 'FALLA' ('no se pudo registrar {0}: {1}' -f $script:TareaDespues, $_.Exception.Message)
        return $false
    }
    if ((Get-ConteoTarea $script:TareaDespues) -ne 1) {
        Add-Check 'tarea_despues' 'FALLA' 'la tarea de resultado no figura tras registrarla'
        return $false
    }
    Add-Check 'tarea_despues' 'PASA' ('tarea {0} armada para {1}: al iniciar sesion comprueba y muestra el resultado' -f $script:TareaDespues, $usuario)
    return $true
}

function Reset-EstadoFase {
    $script:Checks.Clear()
    $script:Lineas.Clear()
    $script:Codigo = 0
}

function Invoke-FaseTodo {
    $ctx = $script:Ctx
    Test-PrerequisitosComunes
    $script:ModoTodo = $true
    $real = [bool]$Conservar
    Write-Informe 'MemoryIndustry - ensayo completo (un solo reinicio)' 'Cyan'
    [void](New-Item -ItemType Directory -Path $ctx.Salida -Force)

    Write-Paso 'Prueba de humo (no instala nada)'
    # Un zip bajado de internet marca cada fichero como "de internet" y Windows pregunta
    # o bloquea; esto lo quita en toda la carpeta del paquete.
    try { Get-ChildItem -LiteralPath $PSScriptRoot -Recurse -File -Force -ErrorAction Stop | Unblock-File -ErrorAction SilentlyContinue } catch { }
    $humo = Get-ResultadoHumo $ctx $true
    Write-FilasHumo $humo.Filas
    Write-Json (Join-Path $ctx.Salida 'humo.json') ([ordered]@{ fase = 'Humo'; fecha = (Get-AhoraTexto); veredicto = $humo.Veredicto; filas = @($humo.Filas) })
    if ($humo.Veredicto -ne 'OK') {
        Write-Informe ''
        Write-Informe 'HUMO: PARA. No se instalo nada.' 'Red'
        foreach ($f in @($humo.Filas | Where-Object { $_.estado -eq 'PARA' })) { Write-Informe ('  - {0}: {1}' -f $f.id, $f.accion) 'Red' }
        Write-Informe ('Cuando lo hayas corregido, vuelve a hacer doble clic. Si no puedes, manda la carpeta {0}.' -f $ctx.Salida) 'Red'
        Save-Informe $ctx -Anexar
        $script:Codigo = 2
        return
    }
    Write-Informe 'HUMO: OK. Se puede instalar.' 'Green'

    if ([string]::IsNullOrWhiteSpace($ctx.Ip)) {
        if ($real) { Resolve-IpReal $ctx $humo }
        else { $ctx.Ip = $humo.Elegida.Ip; Write-Informe ('Direccion del ensayo: {0} ({1})' -f $humo.Elegida.Ip, $humo.Elegida.Alias) }
    }
    Resolve-BaseDatos $real

    Invoke-FaseAntes
    if ($script:Codigo -ne 0) {
        Write-Informe ''
        Write-Informe 'La primera parte NO salio bien, asi que NO se reinicia el equipo.' 'Red'
        Write-Informe ('Manda la carpeta {0} (informe.txt dice que fallo).' -f $ctx.Salida) 'Red'
        return
    }

    $antes = Read-Json (Join-Path $ctx.Salida 'antes.json')
    $ajenas = @()
    $listaAjenas = Get-SesionesAjenas (Get-TextoQuser)
    if ($null -ne $listaAjenas) { $ajenas = @($listaAjenas) }
    $pendientes = @(Get-RebootPendiente)
    Write-Paso 'Falta el reinicio'
    foreach ($linea in @(Get-ResumenTodo ([string]$antes.direccion) ([bool]$antes.autologon) $real $ctx.Salida (Get-AvisosAntes $antes) $ajenas $pendientes)) {
        $color = ''
        if ($linea -like 'AVISO*' -or $linea -like 'no podra*' -or $linea -like '*ATENCION*') { $color = 'Yellow' }
        Write-Informe $linea $color
    }
    Write-Informe ''
    if ($ajenas.Count -gt 0) { $respuesta = Read-Host 'Hay otras personas con sesion abierta. Escribe SI (entero) para reiniciar de todos modos' }
    else { $respuesta = Read-Host 'Reiniciar ahora? (S/N)' }
    if (-not (Test-ConfirmaReinicio $respuesta ($ajenas.Count -gt 0))) {
        Write-Informe 'No se reinicio. Cuando quieras, haz doble clic otra vez en el lanzador: repite la primera parte (es segura) y vuelve a preguntar.' 'Yellow'
        return
    }
    if (-not (Install-TareaDespues $ctx)) {
        Write-Informe 'No se pudo preparar el resultado para despues del reinicio, asi que NO se reinicia.' 'Red'
        $script:Codigo = 1
        return
    }
    # shutdown.exe y no Restart-Computer -Force: da 60 segundos para cancelar y no cierra a
    # la fuerza el trabajo de nadie sin aviso.
    $sd = Invoke-Nativo 'shutdown.exe' @('/r', '/t', '60', '/c', 'MemoryIndustry: reinicio para comprobar que el servicio arranca solo. Cancela con: shutdown /a') 30
    if ($sd.NoArranco -or $sd.Agotado -or $sd.Codigo -ne 0) {
        Write-Informe ('No se pudo programar el reinicio (shutdown codigo {0}: {1}). Reinicia a mano desde el menu de Windows (Reiniciar, no Apagar).' -f $sd.Codigo, $sd.Error.Trim()) 'Red'
        $script:Codigo = 1
        return
    }
    Write-Informe 'El equipo se reiniciara en 60 segundos. Para CANCELARLO escribe:  shutdown /a' 'Yellow'
}

function Write-VeredictoGrande {
    param([string]$Veredicto)
    $fondo = 'DarkRed'
    if ($Veredicto -eq 'PASA') { $fondo = 'DarkGreen' }
    Write-Host ''
    Write-Host '                                                        ' -BackgroundColor $fondo
    Write-Host ('      RESULTADO DEL ENSAYO:   {0}                        ' -f $Veredicto) -ForegroundColor White -BackgroundColor $fondo
    Write-Host '                                                        ' -BackgroundColor $fondo
    Write-Host ''
}

function Invoke-FaseFinal {
    # Lo ejecuta la tarea de inicio de sesion que armo Todo. Despues, luego Limpiar
    # (salvo -Conservar) pase lo que pase: no se deja nada instalado por un ensayo
    # fallido, y el informe se conserva.
    $ctx = $script:Ctx
    Test-PrerequisitosComunes
    # Primero se borra la tarea: si algo de lo que sigue se cuelga o falla, no se repite
    # en cada inicio de sesion.
    try { Unregister-ScheduledTask -TaskName $script:TareaDespues -Confirm:$false -ErrorAction Stop } catch { }
    Write-Host 'MemoryIndustry: comprobando el servicio despues del reinicio...' -ForegroundColor Cyan

    # La medicion del arranque la hace otra tarea, independiente de quien inicie
    # sesion y cuando: aqui se espera a que termine para no leerla a medias.
    $archivo = Join-Path $ctx.Trabajo 'despues-arranque.json'
    $espera = Wait-Hasta { $j = Read-Json $archivo; ($null -ne $j) -and ($j.estado -eq 'completo') } $script:EsperaMedicionSeg 3000
    if (-not $espera.Cumplido) { Write-Host 'La medicion del arranque no termino a tiempo; se sigue con lo que haya.' -ForegroundColor Yellow }

    $motivo = $null
    try { Invoke-FaseDespues }
    catch {
        if ($_.Exception.Message -eq 'RECHAZO') { $motivo = [string]$script:Lineas[$script:Lineas.Count - 1] }
        else { Add-Check 'excepcion' 'FALLA' ('error inesperado del script: {0}' -f $_.Exception.Message) }
    }
    $checks = @($script:Checks)
    if ($script:Codigo -eq 2 -or $checks.Count -eq 0) {
        $checks += [pscustomobject]@{ id = 'despues'; resultado = 'FALLA'; detalle = $(if ($motivo) { $motivo } else { 'la comprobacion no pudo correr' }) }
    }
    $veredicto = Get-Veredicto $checks
    Reset-EstadoFase

    if ($Conservar) {
        $limpieza = 'el servicio se dejo instalado a proposito.'
    }
    else {
        try { Invoke-FaseLimpiar }
        catch { if ($_.Exception.Message -ne 'RECHAZO') { Add-Check 'excepcion' 'FALLA' ('error inesperado del script: {0}' -f $_.Exception.Message) } }
        $malos = @($script:Checks | Where-Object { $_.resultado -eq 'FALLA' })
        if ($script:Codigo -eq 0 -and $malos.Count -eq 0) {
            $limpieza = 'se retiro la tarea, la regla de firewall y el aviso de reinicio. Quedan el archivo de configuracion (con la clave) y el registro, por si hay que mirar algo.'
        }
        else {
            $limpieza = 'NO se pudo retirar todo: ' + $(if ($malos.Count) { (($malos | ForEach-Object { '{0}: {1}' -f $_.id, $_.detalle }) -join ' | ') } else { [string]$script:Lineas[$script:Lineas.Count - 1] })
        }
        Reset-EstadoFase
    }

    $arranque = Read-Json (Join-Path $ctx.Salida 'despues-arranque.json')
    $antesJson = Read-Json (Join-Path $ctx.Salida 'antes.json')
    $texto = Format-Resultado $veredicto $checks $limpieza $arranque $ctx.Salida $env:COMPUTERNAME (Get-Date -Format 'yyyy-MM-dd HH:mm') (Get-AvisosAntes $antesJson)
    $ruta = Join-Path $ctx.Salida 'RESULTADO.txt'
    Write-TextoArchivo $ruta $texto -ConBom
    Write-VeredictoGrande $veredicto
    foreach ($c in @($checks | Where-Object { $_.resultado -eq 'FALLA' })) { Write-Host ('  - {0}: {1}' -f $c.id, $c.detalle) -ForegroundColor Red }
    Write-Host ('Limpieza: {0}' -f $limpieza)
    Write-Host ('Detalle en {0}' -f $ruta)
    try { Start-Process -FilePath 'notepad.exe' -ArgumentList ('"{0}"' -f $ruta) } catch { }
    [void](Read-Host 'Pulsa Enter para cerrar esta ventana')
    if ($veredicto -eq 'PASA') { $script:Codigo = 0 } else { $script:Codigo = 1 }
}

# ===========================================================================
# Principal
# ===========================================================================

function Invoke-Principal {
    # Estricto y con parada ante errores, pero solo aqui dentro: al cargar el
    # fichero con "punto" para probar sus funciones no se toca la sesion de quien lo carga.
    Set-StrictMode -Version 1
    $ErrorActionPreference = 'Stop'
    if ($Fase -eq 'Arranque') {
        # Corre como SYSTEM al arrancar: no necesita el contexto del operador, y
        # LOCALAPPDATA puede no estar definida para esa cuenta.
        Invoke-FaseArranque
        return
    }
    # La transcripcion arranca antes que nada, en la carpeta del informe: todo rechazo,
    # por temprano que sea, deja un fichero que se puede mandar. Solo recoge lo que se
    # imprime, y nada de lo que se imprime lleva el token ni la URL de la base.
    $transcrito = $false
    try {
        $sal = $Salida
        if ([string]::IsNullOrWhiteSpace($sal)) { $sal = 'ensayo-informe' }
        $sal = Get-RutaAbsoluta $sal
        [void](New-Item -ItemType Directory -Path $sal -Force)
        [void](Start-Transcript -Path (Join-Path $sal ('transcripcion-{0}.txt' -f $Fase.ToLower())) -Append -ErrorAction Stop)
        $transcrito = $true
    }
    catch { }
    try {
        $script:Ctx = New-Contexto
        switch ($Fase) {
            'Todo' { Invoke-FaseTodo }
            'Humo' { Invoke-FaseHumo }
            'Final' { Invoke-FaseFinal }
            'Antes' { Invoke-FaseAntes }
            'Despues' { Invoke-FaseDespues }
            'Limpiar' { Invoke-FaseLimpiar }
        }
    }
    catch {
        if ($_.Exception.Message -ne 'RECHAZO') {
            Write-Host ('ERROR inesperado: {0}' -f $_.Exception.Message) -ForegroundColor Red
            $script:Codigo = 1
            if ($null -ne $script:Ctx) { try { Write-Informe ('ERROR inesperado: {0}' -f $_.Exception.Message); Save-Informe $script:Ctx -Anexar } catch { } }
        }
        else {
            # Un rechazo tambien deja informe: sin el, la persona solo tiene una ventana
            # que se cierra.
            if ($null -ne $script:Ctx) { try { Save-Informe $script:Ctx -Anexar } catch { } }
            if ($Fase -eq 'Todo') { Write-Host 'El ensayo se detuvo y NO se reinicia el equipo.' -ForegroundColor Red }
        }
    }
    finally {
        if ($transcrito) { try { [void](Stop-Transcript) } catch { } }
    }
}

# Cargado con ". .\ensayo-madre.ps1" solo define las funciones de arriba (asi se
# prueban sin Windows); ejecutado, corre la fase pedida.
if ($MyInvocation.InvocationName -ne '.') {
    Invoke-Principal
    exit $script:Codigo
}
