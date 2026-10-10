# Conecta este equipo con la PC madre de MemoryIndustry 0.29, una sola vez y sin
# administrador. Lo lanza CONECTAR-A-LA-MADRE.cmd (paquete CLIENTE). En orden:
#   1. copia memory-industry.exe a %LOCALAPPDATA%\MemoryIndustry\bin;
#   2. pide el codigo que se genero en la madre (GENERAR-CODIGO.cmd);
#   3. `bridge pair <codigo>`, que busca la madre solo; si no la encuentra, pide su
#      nombre o su IP y repite con --host;
#   4. `bridge install` (el puente arranca en cada inicio de sesion) y `setup client`
#      para Claude Code y Cursor, si estan;
#   5. espera la conexion hasta 60 s y dice el resultado;
#   6. guarda un informe sin el codigo en %LOCALAPPDATA%\MemoryIndustry\conectar-informe.txt.
#
# El puente avisa por si mismo cuando conecta o pierde la madre: esto no repite esos avisos.
# Va solo en el paquete CLIENTE, sin ensayo-madre.ps1: por eso lleva su propio
# Invoke-Nativo.
#
# Compatible con Windows PowerShell 5.1 y ASCII puro, como ensayo-madre.ps1.
[CmdletBinding()]
param()

$script:Codigo = 0
$script:Informe = New-Object System.Collections.ArrayList
$script:AlfabetoCodigo = 'ABCDEFGHJKLMNPQRSTUVWXYZ23456789'
$script:TareaPuente = 'MemoryIndustry Bridge'
$script:PuertoPuente = 8797
$script:EsperaConexionSeg = 60
$script:Intentos = 3

# ===========================================================================
# Funciones puras: se prueban sin Windows (scripts/visita-contratos.ps1).
# ===========================================================================

function Get-RutaBinCliente {
    param([string]$LocalAppData)
    return ($LocalAppData.TrimEnd('\', '/') + '\MemoryIndustry\bin\memory-industry.exe')
}

function ConvertTo-CodigoEmparejamiento {
    # Lo mismo que accounts::normalize_code: mayusculas o minusculas, espacios y guiones
    # donde sea; 12 simbolos sin 0, O, 1 ni I. Devuelve XXXX-XXXX-XXXX o $null.
    param([string]$Texto)
    if ($null -eq $Texto) { return $null }
    $s = ($Texto -replace '[\s-]', '').ToUpperInvariant()
    if ($s -cnotmatch ('^[' + $script:AlfabetoCodigo + ']{12}$')) { return $null }
    return ('{0}-{1}-{2}' -f $s.Substring(0, 4), $s.Substring(4, 4), $s.Substring(8, 4))
}

function Get-ArgumentosPair {
    param([string]$CodigoPar, [string]$Madre)
    $a = @('bridge', 'pair', $CodigoPar)
    if (-not [string]::IsNullOrWhiteSpace($Madre)) { $a += @('--host', $Madre.Trim()) }
    return $a
}

function Get-SiguientePasoPair {
    # Que hacer tras `bridge pair`, segun sus mensajes (rust/src/bridge/pair.rs). Se busca
    # solo la parte ASCII: si la consola decodifica mal la tilde, sigue valiendo.
    param([int]$Codigo, [string]$Salida)
    if ($Codigo -eq 0) { return 'listo' }
    $t = [string]$Salida
    if ($t -match 'no se encontr' -or $t -match 'no conoce el c') { return 'pedir-madre' }
    if ($t -match 'no acept' -or $t -match 'mal escrito') { return 'pedir-codigo' }
    return 'parar'
}

function Get-ClientesPresentes {
    # Los clientes MCP de este usuario a los que `setup client` sabe escribir.
    param([string]$Perfil, [string]$AppData)
    $res = New-Object System.Collections.ArrayList
    if ((Test-Path -LiteralPath (Join-Path $Perfil '.claude.json')) -or (Test-Path -LiteralPath (Join-Path $Perfil '.claude'))) { [void]$res.Add('claude') }
    if ((Test-Path -LiteralPath (Join-Path $Perfil '.cursor')) -or (Test-Path -LiteralPath (Join-Path $AppData 'Cursor'))) { [void]$res.Add('cursor') }
    return @($res)
}

function Get-ArgumentosSetupCliente {
    param([string]$Cliente, [string]$IdCliente)
    return @('setup', 'client', $Cliente, '--client-id', $IdCliente, '--apply')
}

function Get-EstadoPuente {
    # El JSON de /bridge/health: `verified` es conectado; `revoked`, la madre lo dio de baja.
    param([string]$Texto)
    $j = $null
    if (-not [string]::IsNullOrWhiteSpace($Texto)) { try { $j = $Texto | ConvertFrom-Json } catch { $j = $null } }
    if ($j -isnot [System.Management.Automation.PSCustomObject]) {
        return [pscustomobject]@{ Conectado = $false; Revocado = $false; Estado = $null; Direccion = $null; Huella = $null }
    }
    return [pscustomobject]@{
        Conectado = ([string]$j.state -eq 'verified')
        Revocado  = ([string]$j.state -eq 'revoked')
        Estado    = [string]$j.state
        Direccion = $j.last_good
        Huella    = $j.host_fingerprint
    }
}

function Protect-Codigo {
    # El codigo, escrito como sea (con o sin guiones, en minusculas), sale del informe.
    param([string]$Texto, [string]$CodigoPar)
    if ([string]::IsNullOrEmpty($Texto) -or [string]::IsNullOrEmpty($CodigoPar)) { return $Texto }
    $s = ($CodigoPar -replace '[\s-]', '')
    if ($s.Length -eq 0) { return $Texto }
    $patron = '(?i)' + (($s.ToCharArray() | ForEach-Object { [regex]::Escape([string]$_) }) -join '[\s-]*')
    return [regex]::Replace($Texto, $patron, '<codigo>')
}

# ===========================================================================
# Lo que toca Windows
# ===========================================================================

function ConvertTo-ArgString {
    # ProcessStartInfo.Arguments es un solo texto (5.1 no tiene ArgumentList): reglas de
    # CommandLineToArgvW, como en ensayo-madre.ps1.
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

function Invoke-Nativo {
    # Codigo de salida explicito, tope de tiempo y la salida en UTF-8 (la del .exe).
    param([string]$Exe, [string[]]$Argumentos = @(), [int]$TimeoutSeg = 60)
    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName = $Exe
    $psi.Arguments = ConvertTo-ArgString $Argumentos
    $psi.UseShellExecute = $false
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    $psi.CreateNoWindow = $true
    $psi.StandardOutputEncoding = [System.Text.Encoding]::UTF8
    $psi.StandardErrorEncoding = [System.Text.Encoding]::UTF8
    try { $p = [System.Diagnostics.Process]::Start($psi) }
    catch { return [pscustomobject]@{ Codigo = -1; Salida = ''; Error = $_.Exception.Message } }
    $o = $p.StandardOutput.ReadToEndAsync()
    $e = $p.StandardError.ReadToEndAsync()
    if (-not $p.WaitForExit($TimeoutSeg * 1000)) {
        try { $p.Kill() } catch { }
        return [pscustomobject]@{ Codigo = -1; Salida = ''; Error = ('agotado el tiempo ({0} s)' -f $TimeoutSeg) }
    }
    $p.WaitForExit()
    $r = [pscustomobject]@{ Codigo = $p.ExitCode; Salida = $o.Result; Error = $e.Result }
    $p.Dispose()
    return $r
}

function Get-TextoHttp {
    # Sin proxy (el de la empresa no conoce 127.0.0.1) y con tiempo maximo. $null si falla.
    param([string]$Url, [int]$TimeoutMs = 3000)
    try {
        $req = [System.Net.HttpWebRequest]::Create($Url)
        $req.Timeout = $TimeoutMs
        $req.ReadWriteTimeout = $TimeoutMs
        $req.Proxy = $null
        $req.KeepAlive = $false
        $resp = $req.GetResponse()
        try { return (New-Object System.IO.StreamReader($resp.GetResponseStream())).ReadToEnd() }
        finally { $resp.Close() }
    }
    catch { return $null }
}

function Write-Paso {
    param([string]$Texto, [string]$Color = '')
    [void]$script:Informe.Add($Texto)
    if ($Color) { Write-Host $Texto -ForegroundColor $Color } else { Write-Host $Texto }
}

function Add-Salida {
    # Lo que dijo el .exe va al informe; en pantalla, solo si se pide.
    param($Resultado, [switch]$Mostrar)
    foreach ($l in ((([string]$Resultado.Salida) + "`n" + ([string]$Resultado.Error)) -split "`r?`n")) {
        if (-not $l.Trim()) { continue }
        [void]$script:Informe.Add(('  | ' + $l.TrimEnd()))
        if ($Mostrar) { Write-Host ('  ' + $l.TrimEnd()) }
    }
}

function Copy-Binario {
    # Si el puente ya corre (una segunda vez), tiene el .exe abierto: se para su tarea y
    # sus procesos, que son de este mismo usuario, y se copia.
    param([string]$Origen, [string]$Destino)
    [void](New-Item -ItemType Directory -Path (Split-Path -Parent $Destino) -Force)
    try { Copy-Item -LiteralPath $Origen -Destination $Destino -Force -ErrorAction Stop }
    catch {
        [void](Invoke-Nativo 'schtasks.exe' @('/End', '/TN', $script:TareaPuente) 30)
        Get-Process -Name 'memory-industry' -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq $Destino } | Stop-Process -Force -ErrorAction SilentlyContinue
        Start-Sleep -Seconds 2
        Copy-Item -LiteralPath $Origen -Destination $Destino -Force -ErrorAction Stop
    }
    try { Unblock-File -LiteralPath $Destino -ErrorAction Stop } catch { }
}

function Read-CodigoPersona {
    for ($i = 0; $i -lt $script:Intentos; $i++) {
        $c = ConvertTo-CodigoEmparejamiento (Read-Host 'Escribe el codigo que te dieron en la madre (por ejemplo K7QM-3WXP-9HNR)')
        if ($c) { return $c }
        Write-Paso 'Ese codigo no es valido: son 12 letras y numeros, sin 0, O, 1 ni I.' 'Yellow'
    }
    return $null
}

function Invoke-Emparejar {
    # Devuelve $true si quedo emparejado. Un codigo vale una sola vez: tras un exito no se
    # vuelve a usar.
    param([string]$Exe)
    $codigoPar = Read-CodigoPersona
    if ($null -eq $codigoPar) { return $false }
    $madre = $null
    for ($i = 0; $i -lt ($script:Intentos * 2); $i++) {
        if ($madre) { Write-Paso ('Buscando la madre en {0}...' -f $madre) } else { Write-Paso 'Buscando la madre en la red (hasta un minuto)...' }
        $r = Invoke-Nativo $Exe (Get-ArgumentosPair $codigoPar $madre) 180
        $r.Salida = Protect-Codigo $r.Salida $codigoPar
        $r.Error = Protect-Codigo $r.Error $codigoPar
        Add-Salida $r -Mostrar
        switch (Get-SiguientePasoPair $r.Codigo ($r.Salida + "`n" + $r.Error)) {
            'listo' { Write-Paso 'Emparejado con la madre.' 'Green'; return $true }
            'pedir-madre' {
                $madre = Read-Host 'No la encontre sola. Escribe el nombre o la IP de la madre (salen en la pantalla del codigo)'
                if ([string]::IsNullOrWhiteSpace($madre)) { return $false }
                [void]$script:Informe.Add(('Madre indicada a mano: {0}' -f $madre.Trim()))
            }
            'pedir-codigo' {
                Write-Paso 'La madre no acepto el codigo: puede estar mal escrito, caducado (24 horas) o ya usado.' 'Yellow'
                $codigoPar = Read-CodigoPersona
                if ($null -eq $codigoPar) { return $false }
            }
            default { Write-Paso 'No se pudo emparejar (ver arriba).' 'Red'; return $false }
        }
    }
    return $false
}

function Get-PuertoPuente {
    # El puerto del puente, de su estado (que tambien guarda la credencial: solo se lee
    # el numero, no se copia ni se muestra nada mas).
    param([string]$LocalAppData)
    try {
        $j = Get-Content -LiteralPath (Join-Path (Join-Path $LocalAppData 'MemoryIndustry') 'bridge.json') -Raw -Encoding UTF8 -ErrorAction Stop | ConvertFrom-Json
        if ([int]$j.port -gt 0) { return [int]$j.port }
    }
    catch { }
    return $script:PuertoPuente
}

function Wait-Conexion {
    param([int]$PuertoLocal)
    $url = 'http://127.0.0.1:{0}/bridge/health' -f $PuertoLocal
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    $e = Get-EstadoPuente $null
    while ($sw.Elapsed.TotalSeconds -lt $script:EsperaConexionSeg) {
        $e = Get-EstadoPuente (Get-TextoHttp $url)
        if ($e.Conectado -or $e.Revocado) { return $e }
        Start-Sleep -Seconds 2
    }
    return $e
}

function Save-InformeCliente {
    param([string]$Ruta)
    $texto = ($script:Informe -join "`r`n") + "`r`n"
    [System.IO.File]::WriteAllText($Ruta, $texto, (New-Object System.Text.UTF8Encoding($true)))
}

function Invoke-Principal {
    Set-StrictMode -Version 1
    $ErrorActionPreference = 'Stop'
    $raiz = Join-Path $env:LOCALAPPDATA 'MemoryIndustry'
    $rutaInforme = Join-Path $raiz 'conectar-informe.txt'
    try {
        Write-Paso ('MemoryIndustry: conectar {0} con la madre - {1}' -f $env:COMPUTERNAME, (Get-Date -Format 'yyyy-MM-dd HH:mm')) 'Cyan'
        $origen = Join-Path $PSScriptRoot 'memory-industry.exe'
        if (-not (Test-Path -LiteralPath $origen)) {
            Write-Paso 'No encuentro memory-industry.exe junto a este archivo. Descomprime el zip completo y vuelve a hacer doble clic.' 'Red'
            $script:Codigo = 1
            return
        }

        $exe = Get-RutaBinCliente $env:LOCALAPPDATA
        Write-Paso ('1. Copiando el programa a {0}' -f $exe)
        Copy-Binario $origen $exe

        Write-Paso '2. Emparejar con la madre'
        if (-not (Invoke-Emparejar $exe)) {
            Write-Paso 'No quedo conectado. Pide un codigo nuevo en la madre (GENERAR-CODIGO.cmd) y repite.' 'Red'
            $script:Codigo = 1
            return
        }

        Write-Paso '3. El puente arranca en cada inicio de sesion'
        $inst = Invoke-Nativo $exe @('bridge', 'install') 120
        Add-Salida $inst
        if ($inst.Codigo -ne 0) {
            Write-Paso ('bridge install termino con codigo {0}: {1}' -f $inst.Codigo, $inst.Error.Trim()) 'Red'
            $script:Codigo = 1
            return
        }
        $clientes = @(Get-ClientesPresentes -Perfil $env:USERPROFILE -AppData $env:APPDATA)
        if ($clientes.Count -eq 0) { Write-Paso '   No hay Claude Code ni Cursor en este usuario: cuando los instales, repite este doble clic.' 'Yellow' }
        foreach ($c in $clientes) {
            $sc = Invoke-Nativo $exe (Get-ArgumentosSetupCliente $c $env:COMPUTERNAME) 120
            Add-Salida $sc
            if ($sc.Codigo -eq 0) { Write-Paso ('   {0}: configurado' -f $c) 'Green' }
            else { Write-Paso ('   {0}: no se pudo configurar ({1})' -f $c, $sc.Error.Trim()) 'Yellow' }
        }

        Write-Paso ('4. Esperando la conexion (hasta {0} s)...' -f $script:EsperaConexionSeg)
        $e = Wait-Conexion (Get-PuertoPuente $env:LOCALAPPDATA)
        if ($e.Conectado) {
            Write-Paso ('CONECTADO a la madre en {0} (huella {1}). Reinicia Claude Code o Cursor.' -f $e.Direccion, $e.Huella) 'Green'
        }
        elseif ($e.Revocado) {
            Write-Paso 'La madre dio de baja este equipo: pide un codigo nuevo.' 'Red'
            $script:Codigo = 1
        }
        else {
            Write-Paso ('Aun no conecta (estado: {0}). El puente sigue buscando y avisara cuando lo haga. Para mirarlo: "{1}" bridge status' -f $(if ($e.Estado) { $e.Estado } else { 'el puente no responde' }), $exe) 'Yellow'
            $script:Codigo = 1
        }
    }
    catch {
        Write-Paso ('ERROR inesperado: {0}' -f $_.Exception.Message) 'Red'
        $script:Codigo = 1
    }
    finally {
        try {
            [void](New-Item -ItemType Directory -Path $raiz -Force)
            Save-InformeCliente $rutaInforme
            Write-Host ('Informe (sin el codigo): {0}' -f $rutaInforme)
        }
        catch { }
    }
}

# Cargado con ". .\conectar-cliente.ps1" solo define las funciones (asi se prueban sin
# Windows); ejecutado, conecta.
if ($MyInvocation.InvocationName -ne '.') {
    Invoke-Principal
    exit $script:Codigo
}
