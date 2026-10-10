# La PC madre de MemoryIndustry 0.29, vista por quien se sienta delante.
#   -Modo Aviso   (por omision) lo corre la tarea "MemoryIndustry Madre aviso" al iniciar
#                 sesion: dice si la madre esta activa y en que IP, y si la IP cambio desde
#                 la ultima vez abre una ventana que ofrece fijarla.
#   -Modo Codigo  lo lanza GENERAR-CODIGO.cmd: crea el codigo de emparejamiento de una
#                 persona y lo muestra en pantalla, con el nombre y la IP de la madre.
#
# Nada de esto pide administrador salvo el boton "Fijar", que lo pide en ese momento.
# El codigo de emparejamiento solo sale en la pantalla de -Modo Codigo: no se escribe en
# ningun fichero ni en ninguna transcripcion.
#
# Usa las funciones de ensayo-madre.ps1, que viaja en el mismo paquete y en la misma copia
# protegida: cargado con "punto", ese fichero solo define funciones y constantes.
#
# Compatible con Windows PowerShell 5.1 y ASCII puro, como ensayo-madre.ps1.
[CmdletBinding()]
param(
    [ValidateSet('Aviso', 'Codigo')]
    [string]$Modo = 'Aviso'
)

$script:RutaEnsayo = Join-Path $PSScriptRoot 'ensayo-madre.ps1'
if (-not (Test-Path -LiteralPath $script:RutaEnsayo)) { throw ('falta ensayo-madre.ps1 junto a aviso-madre.ps1 ({0}): el paquete esta incompleto' -f $script:RutaEnsayo) }
. $script:RutaEnsayo

$script:Codigo = 0
$script:TituloAviso = 'MemoryIndustry'
# Al iniciar sesion el servicio puede estar arrancando todavia (arranca con el equipo y
# calienta los modelos) o esperando a que la red le de IP: se espera esto antes de avisar.
$script:EsperaAvisoSeg = 300
$script:PasoAvisoSeg = 10
$script:AlfabetoCuenta = '^[a-z0-9][a-z0-9._-]{0,31}$'

# ===========================================================================
# Funciones puras: se prueban sin Windows (scripts/visita-contratos.ps1).
# ===========================================================================

function Read-SaludMadre {
    # El texto de /health en lo que importa al aviso. $null si no hubo respuesta o no es
    # un objeto JSON; un objeto sin bloque lan (binario anterior) o sin IP da Ip $null.
    param([string]$Texto)
    if ([string]::IsNullOrWhiteSpace($Texto)) { return $null }
    try { $j = $Texto | ConvertFrom-Json } catch { return $null }
    if ($j -isnot [System.Management.Automation.PSCustomObject]) { return $null }
    $modo = $null
    if ($j.PSObject.Properties['lan'] -and $null -ne $j.lan) { $modo = [string]$j.lan.mode }
    $direccion = Get-LanDeSalud $j
    $partes = Split-Direccion $direccion
    $ip = $null; $puerto = $null
    if ($null -ne $partes) { $ip = $partes.Ip; $puerto = $partes.Puerto }
    return [pscustomobject]@{ Modo = $modo; Direccion = $direccion; Ip = $ip; Puerto = $puerto }
}

function Get-DecisionAviso {
    # Que se le dice a quien inicia sesion. `Ultima` es la IP que vio el aviso anterior.
    param($Salud, [string]$Ultima, [string]$Nombre, [string]$Log, [string]$Exe = 'memory-industry.exe')
    if ($null -eq $Salud) {
        return [pscustomobject]@{
            Tipo = 'sin-respuesta'; Titulo = 'MemoryIndustry no responde'; Botones = @('Cerrar'); Ip = $null
            Mensaje = ("La madre (MemoryIndustry) no responde en este equipo.`r`n`r`nRegistro: {0}`r`nPara ver la causa, en PowerShell:`r`n  & `"{1}`" doctor" -f $Log, $Exe)
        }
    }
    if ([string]::IsNullOrEmpty($Salud.Ip)) {
        return [pscustomobject]@{
            Tipo = 'sin-red'; Titulo = 'MemoryIndustry sin red'; Botones = @('Cerrar'); Ip = $null
            Mensaje = 'La madre responde en este equipo, pero no tiene una IP de red privada: los otros equipos no la ven. Revisa el cable o el wifi; en cuanto haya red, la madre la toma sola.'
        }
    }
    if (-not [string]::IsNullOrEmpty($Ultima) -and $Ultima -ne $Salud.Ip) {
        return [pscustomobject]@{
            Tipo = 'cambio'; Titulo = 'MemoryIndustry: la IP cambio'; Ip = $Salud.Ip
            Botones = @(('Fijar {0}' -f $Salud.Ip), 'Dejar automatica')
            Mensaje = ("La IP cambio de {0} a {1}. Los equipos la encuentran solos.`r`n`r`nFijar {1}: queda fija y no vuelve a cambiar (pide permiso de administrador).`r`nDejar automatica: si vuelve a cambiar, los equipos la vuelven a buscar." -f $Ultima, $Salud.Ip)
        }
    }
    return [pscustomobject]@{ Tipo = 'activa'; Titulo = $script:TituloAviso; Botones = @(); Ip = $Salud.Ip; Mensaje = ('Madre activa: {0} en {1}' -f $Nombre, $Salud.Ip) }
}

function Get-RutaEstadoAviso {
    param([string]$LocalAppData)
    return ($LocalAppData.TrimEnd('\', '/') + '\MemoryIndustry\aviso-madre.json')
}

function Save-UltimaVista {
    param([string]$Ruta, [string]$IpVista)
    Write-TextoArchivo $Ruta (ConvertTo-Json -InputObject ([ordered]@{ ip = $IpVista; visto = (Get-AhoraTexto) }))
}

function Read-UltimaVista {
    # Un fichero que falta o esta roto es "nunca vista": el aviso no se cae por eso.
    param([string]$Ruta)
    $j = Read-Json $Ruta
    if ($j -isnot [System.Management.Automation.PSCustomObject] -or [string]::IsNullOrEmpty([string]$j.ip)) { return $null }
    return [string]$j.ip
}

function Get-PlanFijarIp {
    # `Actual`: {InterfaceIndex, Ip, PrefixLength, Puerta, Dns} de la tarjeta que tiene la IP.
    # Se fija la MISMA IP con la mascara, la puerta y los DNS que ya tiene.
    param($Actual)
    $motivo = $null
    if ($null -eq $Actual -or [string]::IsNullOrEmpty([string]$Actual.Ip)) { $motivo = 'no se sabe que IP fijar' }
    elseif ([int]$Actual.InterfaceIndex -le 0) { $motivo = 'no se encontro la tarjeta de red de esa IP' }
    elseif ([int]$Actual.PrefixLength -le 0 -or [int]$Actual.PrefixLength -gt 32) { $motivo = 'no se pudo leer la mascara de red de esa tarjeta' }
    elseif ([string]::IsNullOrEmpty([string]$Actual.Puerta)) { $motivo = 'esa tarjeta no tiene puerta de enlace: fijar la IP sin ella dejaria al equipo sin salida. Dejala automatica' }
    if ($motivo) { return [pscustomobject]@{ Error = $motivo } }
    return [pscustomobject]@{
        Error          = $null
        InterfaceIndex = [int]$Actual.InterfaceIndex
        IPAddress      = [string]$Actual.Ip
        PrefixLength   = [int]$Actual.PrefixLength
        DefaultGateway = [string]$Actual.Puerta
        Dns            = [string[]]@($Actual.Dns | Where-Object { -not [string]::IsNullOrEmpty([string]$_) })
        Riesgo         = 'Fijar la IP la saca del DHCP. Si el router reparte esa misma direccion a otro equipo, los dos chocaran y la madre se quedara sin red. Pide a quien lleva la red que la reserve para este equipo o que la deje fuera del rango del DHCP.'
    }
}

function Get-OrdenFijarIp {
    # El guion que corre elevado. Los valores salen de la configuracion de Windows, no de
    # lo que alguien teclea, y van entre comillas simples.
    param($Plan)
    $dns = ''
    if (@($Plan.Dns).Count -gt 0) { $dns = ("Set-DnsClientServerAddress -InterfaceIndex `$ix -ServerAddresses @({0})" -f ((@($Plan.Dns) | ForEach-Object { "'" + $_ + "'" }) -join ',')) }
    $l = @(
        '$ErrorActionPreference = ''Stop''',
        'try {',
        ('    $ix = {0}' -f $Plan.InterfaceIndex),
        '    Set-NetIPInterface -InterfaceIndex $ix -Dhcp Disabled',
        '    Get-NetIPAddress -InterfaceIndex $ix -AddressFamily IPv4 -ErrorAction SilentlyContinue | Remove-NetIPAddress -Confirm:$false',
        '    Get-NetRoute -InterfaceIndex $ix -DestinationPrefix ''0.0.0.0/0'' -ErrorAction SilentlyContinue | Remove-NetRoute -Confirm:$false',
        ("    [void](New-NetIPAddress -InterfaceIndex `$ix -IPAddress '{0}' -PrefixLength {1} -DefaultGateway '{2}')" -f $Plan.IPAddress, $Plan.PrefixLength, $Plan.DefaultGateway),
        ('    ' + $dns),
        '    exit 0',
        '}',
        'catch { Write-Host $_.Exception.Message; Start-Sleep -Seconds 15; exit 1 }'
    )
    return ($l -join "`r`n")
}

function Get-CodigoDeSalida {
    # El codigo y la huella de lo que imprime `account pair`.
    param([string]$Texto)
    if ([string]::IsNullOrEmpty($Texto) -or $Texto -notmatch 'bridge pair ([A-Z0-9]{4}-[A-Z0-9]{4}-[A-Z0-9]{4})') { return $null }
    $codigo = $Matches[1]
    $huella = $null
    if ($Texto -match 'fingerprint:\s*([0-9A-F]{4}(?:-[0-9A-F]{4}){3})') { $huella = $Matches[1] }
    return [pscustomobject]@{ Codigo = $codigo; Huella = $huella }
}

function Test-NombreCuenta {
    # La misma forma que accounts::validate_account_name, despues de pasar a minusculas.
    param([string]$Nombre)
    if ([string]::IsNullOrEmpty($Nombre)) { return $false }
    return ($Nombre.ToLowerInvariant() -cmatch $script:AlfabetoCuenta)
}

function Get-OrdenesCodigo {
    # `account pair` exige que la cuenta exista: primero se crea (si ya existia, se sigue).
    param([string]$Persona)
    $n = $Persona.Trim().ToLowerInvariant()
    $alta = @('account', 'add', $n)
    $par = @('account', 'pair', $n)
    return @($alta, $par)
}

function Test-CuentaYaExiste {
    param([string]$Texto)
    return ($Texto -match 'already exists')
}

function Format-PantallaCodigo {
    param([string]$Codigo, [string]$Persona, [string]$Madre, [string]$Ip, [string]$Huella)
    $grande = (($Codigo.ToCharArray() | ForEach-Object { [string]$_ }) -join ' ')
    $donde = $Ip
    if ([string]::IsNullOrEmpty($donde)) { $donde = 'sin IP de red ahora (revisa el cable o el wifi)' }
    $l = @(
        '==============================================================',
        '',
        ('   CODIGO PARA {0}:' -f $Persona),
        '',
        ('        {0}' -f $grande),
        '',
        '==============================================================',
        ('Codigo:         {0}' -f $Codigo),
        ('Madre:          {0}' -f $Madre),
        ('IP de la madre: {0}' -f $donde),
        ('Huella:         {0} (el equipo nuevo muestra la misma)' -f $Huella),
        'Vale 24 horas y un solo uso.',
        '',
        'En el equipo nuevo:',
        '  1. Descomprime memory-industry-0.29-CLIENTE.zip.',
        '  2. Doble clic en CONECTAR-A-LA-MADRE.cmd.',
        '  3. Escribe este codigo cuando lo pida.',
        ('  Si no encuentra la madre sola, escribe su nombre ({0}) o su IP ({1}).' -f $Madre, $donde)
    )
    return ($l -join "`r`n")
}

function Get-PuertoMadre {
    # El puerto de CUBA_HTTP_ADDR (lan:8787 o ip:puerto); 8787 si no se puede leer.
    param([string]$TextoEnv)
    $v = $null
    if ($TextoEnv) { $v = Get-ValorEnv $TextoEnv $script:ClaveDireccion }
    if ($v -and $v -match ':(\d{1,5})$') { return [int]$Matches[1] }
    return 8787
}

# ===========================================================================
# Ventanas y avisos (solo Windows)
# ===========================================================================

function Show-Toast {
    # Toast con el AUMID de Windows PowerShell (no hay que registrar nada); si no se puede,
    # el globo de la bandeja; y si tampoco, msg.exe.
    param([string]$Titulo, [string]$Texto)
    try {
        [Windows.UI.Notifications.ToastNotificationManager, Windows.UI.Notifications, ContentType = WindowsRuntime] > $null
        [Windows.Data.Xml.Dom.XmlDocument, Windows.Data.Xml.Dom.XmlDocument, ContentType = WindowsRuntime] > $null
        $xml = New-Object Windows.Data.Xml.Dom.XmlDocument
        $xml.LoadXml(('<toast><visual><binding template="ToastGeneric"><text>{0}</text><text>{1}</text></binding></visual></toast>' -f [System.Security.SecurityElement]::Escape($Titulo), [System.Security.SecurityElement]::Escape($Texto)))
        $aumid = '{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}\WindowsPowerShell\v1.0\powershell.exe'
        [Windows.UI.Notifications.ToastNotificationManager]::CreateToastNotifier($aumid).Show((New-Object Windows.UI.Notifications.ToastNotification $xml))
        return
    }
    catch { }
    try {
        Add-Type -AssemblyName System.Windows.Forms, System.Drawing -ErrorAction Stop
        $n = New-Object System.Windows.Forms.NotifyIcon
        $n.Icon = [System.Drawing.SystemIcons]::Information
        $n.BalloonTipTitle = $Titulo
        $n.BalloonTipText = $Texto
        $n.Visible = $true
        $n.ShowBalloonTip(10000)
        Start-Sleep -Seconds 11
        $n.Dispose()
        return
    }
    catch { }
    [void](Invoke-Nativo 'msg.exe' @($env:USERNAME, '/TIME:60', ('{0}: {1}' -f $Titulo, $Texto)) 15)
}

function Show-Ventana {
    # Una ventana con el texto y un boton por opcion. Devuelve el texto del boton pulsado,
    # o $null si se cerro sin elegir o si solo se pudo avisar con msg.exe (WinForms
    # bloqueado por el modo de lenguaje restringido).
    param([string]$Titulo, [string]$Texto, [string[]]$Botones)
    $script:Pulsado = $null
    try {
        Add-Type -AssemblyName System.Windows.Forms, System.Drawing -ErrorAction Stop
        [System.Windows.Forms.Application]::EnableVisualStyles()
        $f = New-Object System.Windows.Forms.Form
        $f.Text = $Titulo
        $f.TopMost = $true
        $f.StartPosition = 'CenterScreen'
        $f.FormBorderStyle = 'FixedDialog'
        $f.MaximizeBox = $false
        $f.MinimizeBox = $false
        $f.AutoSize = $true
        $f.AutoSizeMode = 'GrowAndShrink'
        $f.Padding = New-Object System.Windows.Forms.Padding(14)
        $todo = New-Object System.Windows.Forms.FlowLayoutPanel
        $todo.FlowDirection = 'TopDown'
        $todo.AutoSize = $true
        $etiqueta = New-Object System.Windows.Forms.Label
        $etiqueta.Text = $Texto
        $etiqueta.AutoSize = $true
        $etiqueta.MaximumSize = New-Object System.Drawing.Size(560, 0)
        $etiqueta.Font = New-Object System.Drawing.Font('Segoe UI', 11)
        [void]$todo.Controls.Add($etiqueta)
        $fila = New-Object System.Windows.Forms.FlowLayoutPanel
        $fila.FlowDirection = 'LeftToRight'
        $fila.AutoSize = $true
        $fila.Margin = New-Object System.Windows.Forms.Padding(0, 14, 0, 0)
        foreach ($b in $Botones) {
            $boton = New-Object System.Windows.Forms.Button
            $boton.Text = $b
            $boton.AutoSize = $true
            $boton.Font = New-Object System.Drawing.Font('Segoe UI', 10)
            $boton.Add_Click({ $script:Pulsado = $this.Text; $this.FindForm().Close() })
            [void]$fila.Controls.Add($boton)
        }
        [void]$todo.Controls.Add($fila)
        [void]$f.Controls.Add($todo)
        [void]$f.ShowDialog()
        $f.Dispose()
        return $script:Pulsado
    }
    catch {
        [void](Invoke-Nativo 'msg.exe' @($env:USERNAME, '/TIME:300', ('{0}: {1}' -f $Titulo, $Texto)) 15)
        return $null
    }
}

function Get-ConfiguracionIpActual {
    param([string]$IpActual)
    $a = @(Get-NetIPAddress -IPAddress $IpActual -AddressFamily IPv4 -ErrorAction Stop)[0]
    $cfg = Get-NetIPConfiguration -InterfaceIndex $a.InterfaceIndex -ErrorAction Stop
    $puerta = @($cfg.IPv4DefaultGateway | ForEach-Object { [string]$_.NextHop } | Where-Object { $_ })
    $dns = @((Get-DnsClientServerAddress -InterfaceIndex $a.InterfaceIndex -AddressFamily IPv4 -ErrorAction Stop).ServerAddresses)
    return [pscustomobject]@{
        InterfaceIndex = [int]$a.InterfaceIndex
        Ip             = $IpActual
        PrefixLength   = [int]$a.PrefixLength
        Puerta         = $(if ($puerta.Count -gt 0) { $puerta[0] } else { $null })
        Dns            = $dns
    }
}

function Invoke-FijarIp {
    param([string]$IpActual)
    try { $actual = Get-ConfiguracionIpActual $IpActual }
    catch {
        [void](Show-Ventana 'MemoryIndustry' ('No pude leer la configuracion de red de {0}: {1}' -f $IpActual, $_.Exception.Message) @('Cerrar'))
        return
    }
    $plan = Get-PlanFijarIp -Actual $actual
    if ($plan.Error) {
        [void](Show-Ventana 'MemoryIndustry' ('No se fija la IP: {0}.' -f $plan.Error) @('Cerrar'))
        return
    }
    $resumen = ("{0}`r`n`r`nSe fijara {1}/{2}, puerta de enlace {3}, DNS {4}. Windows pedira permiso de administrador." -f $plan.Riesgo, $plan.IPAddress, $plan.PrefixLength, $plan.DefaultGateway, ((@($plan.Dns)) -join ', '))
    if ((Show-Ventana 'Fijar la IP de la madre' $resumen @('Fijar', 'Cancelar')) -ne 'Fijar') { return }
    $b64 = [System.Convert]::ToBase64String([System.Text.Encoding]::Unicode.GetBytes((Get-OrdenFijarIp $plan)))
    try {
        $p = Start-Process -FilePath 'powershell.exe' -ArgumentList @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-EncodedCommand', $b64) -Verb RunAs -Wait -PassThru -ErrorAction Stop
        $salida = $p.ExitCode
    }
    catch {
        [void](Show-Ventana 'MemoryIndustry' 'No se concedio el permiso de administrador: la IP sigue automatica.' @('Cerrar'))
        return
    }
    if ($salida -eq 0) { [void](Show-Ventana 'MemoryIndustry' ('IP fijada: {0}. Comprueba con ipconfig /all que dice DHCP deshabilitado.' -f $plan.IPAddress) @('Cerrar')) }
    else { [void](Show-Ventana 'MemoryIndustry' ('No se pudo fijar la IP (codigo {0}). La red puede tardar unos segundos en volver; si no vuelve, en despliegue-lan.md, "Fixed address on the host", esta como volver al DHCP.' -f $salida) @('Cerrar')) }
}

# ===========================================================================
# Modos
# ===========================================================================

function Get-SaludMadreLocal {
    param([int]$PuertoMadre)
    $h = Get-Salud (Get-UrlSaludLocal $PuertoMadre) $script:TimeoutSaludMs
    if (-not $h.Ok) { return $null }
    return (Read-SaludMadre (ConvertTo-Json -InputObject $h.Json -Depth 6 -Compress))
}

function Invoke-ModoAviso {
    $raiz = Join-Path $env:LOCALAPPDATA 'MemoryIndustry'
    $puertoMadre = Get-PuertoMadre (Read-Texto (Join-Path $raiz $script:ArchivoEnv))
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    while ($true) {
        $salud = Get-SaludMadreLocal $puertoMadre
        if ($null -ne $salud -and -not [string]::IsNullOrEmpty($salud.Ip)) { break }
        if ($sw.Elapsed.TotalSeconds -ge $script:EsperaAvisoSeg) { break }
        Start-Sleep -Seconds $script:PasoAvisoSeg
    }
    $ruta = Get-RutaEstadoAviso $env:LOCALAPPDATA
    $d = Get-DecisionAviso -Salud $salud -Ultima (Read-UltimaVista $ruta) -Nombre $env:COMPUTERNAME -Log (Join-Path $raiz $script:ArchivoLog) -Exe (Join-Path $PSScriptRoot 'memory-industry.exe')
    switch ($d.Tipo) {
        'activa' {
            Save-UltimaVista $ruta $d.Ip
            Show-Toast $d.Titulo $d.Mensaje
        }
        'cambio' {
            # Se guarda antes de preguntar: el aviso del cambio sale una vez, no en cada sesion.
            Save-UltimaVista $ruta $d.Ip
            if ((Show-Ventana $d.Titulo $d.Mensaje $d.Botones) -eq $d.Botones[0]) { Invoke-FijarIp $d.Ip }
        }
        default { [void](Show-Ventana $d.Titulo $d.Mensaje $d.Botones) }
    }
}

function Invoke-ModoCodigo {
    Write-Host 'MemoryIndustry: codigo para conectar un equipo nuevo' -ForegroundColor Cyan
    $exe = Join-Path $PSScriptRoot 'memory-industry.exe'
    if (-not (Test-Path -LiteralPath $exe)) {
        Write-Host ('No encuentro memory-industry.exe junto a este archivo ({0}). Descomprime el zip completo.' -f $exe) -ForegroundColor Red
        $script:Codigo = 1
        return
    }
    $raiz = Join-Path $env:LOCALAPPDATA 'MemoryIndustry'
    $rutaEnv = Join-Path $raiz $script:ArchivoEnv
    $textoEnv = Read-Texto $rutaEnv
    $url = $null
    if ($textoEnv) { $url = Get-ValorEnv $textoEnv 'DATABASE_URL' }
    if ([string]::IsNullOrEmpty($url)) {
        # Sin la base de la madre, `account` buscaria una por su cuenta (y hasta intentaria
        # crearla en Docker): mejor parar y decir quien puede hacerlo.
        Write-Host ('No encuentro la configuracion de la madre en {0}. Genera el codigo con el usuario de Windows que instalo MemoryIndustry en este equipo.' -f $rutaEnv) -ForegroundColor Red
        $script:Codigo = 1
        return
    }
    # Solo para este proceso y lo que lanza; nunca se imprime.
    $env:DATABASE_URL = $url

    $persona = $null
    for ($i = 0; $i -lt 3 -and $null -eq $persona; $i++) {
        $t = Read-Host 'Nombre de la persona (por ejemplo ana o ana.perez)'
        if (Test-NombreCuenta $t) { $persona = $t.Trim().ToLowerInvariant() }
        else { Write-Host 'Solo minusculas, numeros, punto, guion y guion bajo; hasta 32; empieza por letra o numero.' -ForegroundColor Yellow }
    }
    if ($null -eq $persona) { $script:Codigo = 1; return }

    $ordenes = @(Get-OrdenesCodigo $persona)
    $alta = Invoke-Nativo $exe $ordenes[0] 120 -Utf8
    if ($alta.Codigo -ne 0 -and -not (Test-CuentaYaExiste ($alta.Salida + $alta.Error))) {
        Write-Host ('No se pudo crear la cuenta {0}: {1}' -f $persona, (Protect-Secreto ($alta.Error + $alta.Salida).Trim() @($url))) -ForegroundColor Red
        $script:Codigo = 1
        return
    }
    $par = Invoke-Nativo $exe $ordenes[1] 120 -Utf8
    $c = $null
    if ($par.Codigo -eq 0) { $c = Get-CodigoDeSalida $par.Salida }
    if ($null -eq $c) {
        Write-Host ('No se pudo generar el codigo (codigo de salida {0}): {1}' -f $par.Codigo, (Protect-Secreto $par.Error.Trim() @($url))) -ForegroundColor Red
        $script:Codigo = 1
        return
    }
    $salud = Get-SaludMadreLocal (Get-PuertoMadre $textoEnv)
    $ip = $null
    if ($null -ne $salud) { $ip = $salud.Ip }
    Write-Host ''
    Write-Host (Format-PantallaCodigo -Codigo $c.Codigo -Persona $persona -Madre $env:COMPUTERNAME -Ip $ip -Huella $c.Huella) -ForegroundColor Yellow
    Write-Host ''
}

function Invoke-Principal {
    Set-StrictMode -Version 1
    $ErrorActionPreference = 'Stop'
    try {
        if ($Modo -eq 'Codigo') { Invoke-ModoCodigo } else { Invoke-ModoAviso }
    }
    catch {
        $script:Codigo = 1
        $msg = ('{0} aviso-madre -Modo {1}: {2}' -f (Get-AhoraTexto), $Modo, $_.Exception.Message)
        Write-Host $msg -ForegroundColor Red
        # El aviso corre sin consola: el error queda junto al estado del aviso.
        try { [System.IO.File]::AppendAllText((Join-Path (Join-Path $env:LOCALAPPDATA 'MemoryIndustry') 'aviso-madre.log'), ($msg + "`r`n")) } catch { }
    }
}

# Cargado con ". .\aviso-madre.ps1" solo define las funciones (asi se prueban sin
# Windows); ejecutado, corre el modo pedido.
if ($MyInvocation.InvocationName -ne '.') {
    Invoke-Principal
    exit $script:Codigo
}
