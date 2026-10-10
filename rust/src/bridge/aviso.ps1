# MemoryIndustry: el aviso del puente en pantalla (B3 de la autoconexion 0.29).
# Lo lanza el puente con -EncodedCommand, sin consola. Los datos llegan en $aviso, ya leidos del
# base64 de UTF-8 por la linea que el puente antepone. Windows PowerShell 5.1: sin operadores de 7.
# Todo el guion viaja en la linea de comandos (limite de 32 767): cuanto mas largo, menos informe cabe.
$ErrorActionPreference = 'Stop'

function Get-Resumen {
    $s = "Madre: $($aviso.host_name)"
    if ($aviso.fingerprint) { $s += " (huella $($aviso.fingerprint))" }
    if ($aviso.kind -eq 'needs_pair') {
        return "$s`r`n$($aviso.reason)`r`nPide un código nuevo al operador de la madre y ejecuta:`r`n`"$($aviso.exe)`" bridge pair <código>"
    }
    $ultima = 'ninguna'
    if ($aviso.last_good) { $ultima = $aviso.last_good }
    $desde = (Get-Date).AddSeconds(-[double]$aviso.down_for_secs).ToString('HH:mm')
    $min = [math]::Floor([double]$aviso.down_for_secs / 60)
    "$s`r`nÚltima dirección buena: $ultima`r`nSin conexión desde las $desde (hace $min min).`r`nEl puente sigue buscando solo y avisará al reconectar.`r`n`r`nLo probado: $($aviso.report)`r`n`r`nPrograma: $($aviso.exe)"
}

# Toast con el AUMID de PowerShell: funciona sin registrar nada y sale como «Windows PowerShell».
function Show-Toast([string]$titulo, [string]$texto) {
    [void][Windows.UI.Notifications.ToastNotificationManager, Windows.UI.Notifications, ContentType = WindowsRuntime]
    [void][Windows.Data.Xml.Dom.XmlDocument, Windows.Data.Xml.Dom.XmlDocument, ContentType = WindowsRuntime]
    $xml = New-Object Windows.Data.Xml.Dom.XmlDocument
    $xml.LoadXml('<toast><visual><binding template="ToastGeneric"><text/><text/></binding></visual></toast>')
    $nodos = $xml.GetElementsByTagName('text')
    [void]$nodos.Item(0).AppendChild($xml.CreateTextNode($titulo))
    [void]$nodos.Item(1).AppendChild($xml.CreateTextNode($texto))
    $aumid = '{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}\WindowsPowerShell\v1.0\powershell.exe'
    $toast = New-Object Windows.UI.Notifications.ToastNotification $xml
    [Windows.UI.Notifications.ToastNotificationManager]::CreateToastNotifier($aumid).Show($toast)
}

function Show-Globo([string]$titulo, [string]$texto) {
    Add-Type -AssemblyName System.Windows.Forms, System.Drawing
    $icono = New-Object System.Windows.Forms.NotifyIcon
    $icono.Icon = [System.Drawing.SystemIcons]::Information
    $icono.Visible = $true
    $icono.ShowBalloonTip(8000, $titulo, $texto, [System.Windows.Forms.ToolTipIcon]::Info)
    Start-Sleep -Seconds 9
    $icono.Dispose()
}

# El ultimo respaldo, cuando el modo de lenguaje restringido no deja cargar WinForms.
function Show-Msg([string]$texto) {
    try { & msg.exe $env:USERNAME /TIME:600 $texto } catch { }
}

# Corre el .exe sin consola; devuelve si salio 0 y lo que dijo.
function Invoke-Reconectar([string]$anfitrion) {
    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName = [string]$aviso.exe
    $psi.Arguments = 'bridge reconnect'
    if ($anfitrion) { $psi.Arguments += " --host $anfitrion" }
    $psi.UseShellExecute = $false
    $psi.CreateNoWindow = $true
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    try {
        $p = [System.Diagnostics.Process]::Start($psi)
        $salida = $p.StandardOutput.ReadToEnd() + $p.StandardError.ReadToEnd()
        $p.WaitForExit()
        return @{ ok = ($p.ExitCode -eq 0); texto = $salida.Trim() }
    } catch {
        return @{ ok = $false; texto = "No se pudo ejecutar $($aviso.exe): $($_.Exception.Message)" }
    }
}

function Show-Ventana {
    Add-Type -AssemblyName System.Windows.Forms, System.Drawing, Microsoft.VisualBasic
    [System.Windows.Forms.Application]::EnableVisualStyles()
    $f = New-Object System.Windows.Forms.Form
    $f.Text = 'MemoryIndustry'
    $f.Size = New-Object System.Drawing.Size(660, 460)
    $f.StartPosition = 'CenterScreen'
    $f.FormBorderStyle = 'FixedDialog'
    $f.MaximizeBox = $false
    $f.TopMost = $true

    $titulo = New-Object System.Windows.Forms.Label
    $titulo.Text = 'MemoryIndustry no encuentra la madre'
    if ($aviso.kind -eq 'needs_pair') { $titulo.Text = 'MemoryIndustry tiene que emparejarse de nuevo' }
    $titulo.Font = New-Object System.Drawing.Font('Segoe UI', 13, [System.Drawing.FontStyle]::Bold)
    $titulo.SetBounds(12, 10, 620, 30)

    $detalle = New-Object System.Windows.Forms.TextBox
    $detalle.Multiline = $true
    $detalle.ReadOnly = $true
    $detalle.ScrollBars = 'Vertical'
    $detalle.Text = Get-Resumen
    $detalle.SetBounds(12, 46, 620, 260)

    $estado = New-Object System.Windows.Forms.Label
    $estado.SetBounds(12, 312, 620, 44)

    $botones = New-Object System.Windows.Forms.FlowLayoutPanel
    $botones.SetBounds(12, 362, 620, 44)
    $f.Controls.AddRange(@($titulo, $detalle, $estado, $botones))

    $reconectar = {
        param([string]$anfitrion)
        $estado.ForeColor = [System.Drawing.Color]::Black
        $estado.Text = 'Buscando la madre...'
        $f.UseWaitCursor = $true
        $f.Refresh()
        $r = Invoke-Reconectar $anfitrion
        $f.UseWaitCursor = $false
        $estado.Text = $r.texto
        if ($r.ok) { $estado.ForeColor = [System.Drawing.Color]::DarkGreen } else { $estado.ForeColor = [System.Drawing.Color]::DarkRed }
    }
    $nuevo = {
        param([string]$texto, [scriptblock]$accion)
        $b = New-Object System.Windows.Forms.Button
        $b.Text = $texto
        $b.AutoSize = $true
        $b.Add_Click($accion)
        $botones.Controls.Add($b)
    }
    & $nuevo 'Reintentar ahora' { & $reconectar '' }
    & $nuevo 'Escribir nombre o IP de la madre...' {
        $x = [Microsoft.VisualBasic.Interaction]::InputBox('Nombre o IP de la madre (por ejemplo 192.168.1.20 o pc-madre):', 'MemoryIndustry', '').Trim()
        if (-not $x) { return }
        # Solo letras, cifras y . : [ ] -: nada que pueda colarse como otro argumento del .exe.
        if ($x -notmatch '^[A-Za-z0-9.:\[\]-]+$') {
            $estado.ForeColor = [System.Drawing.Color]::DarkRed
            $estado.Text = "'$x' no es un nombre ni una IP."
            return
        }
        & $reconectar $x
    }
    & $nuevo 'Copiar informe' {
        [System.Windows.Forms.Clipboard]::SetText($detalle.Text)
        $estado.ForeColor = [System.Drawing.Color]::Black
        $estado.Text = 'Informe copiado al portapapeles.'
    }
    & $nuevo 'Cerrar' { $f.Close() }
    [void]$f.ShowDialog()
}

$breve = @{ connected = 'MemoryIndustry conectado'; reconnected = 'MemoryIndustry reconectado' }
try {
    if ($breve.ContainsKey([string]$aviso.kind)) {
        $titulo = $breve[[string]$aviso.kind]
        $texto = "Madre: $($aviso.host_name) en $($aviso.address)"
        try { Show-Toast $titulo $texto } catch { Show-Globo $titulo $texto }
    } else {
        Show-Ventana
    }
} catch {
    Show-Msg "MemoryIndustry: $(Get-Resumen)"
}
