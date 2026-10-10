# La PC madre de MemoryIndustry 0.29, vista por quien se sienta delante.
#   -Modo Aviso   (por omision) lo corre la tarea "MemoryIndustry Madre aviso" al iniciar
#                 sesion: dice si la madre esta activa y en que IP, y avisa si la IP cambio.
#   -Modo Codigo  lo lanza GENERAR-CODIGO.cmd: crea el codigo de emparejamiento de una
#                 persona y lo muestra en pantalla, con el nombre y la IP de la madre.
#
# ESQUELETO de la pasada roja (tajada D de .cursor/handoffs/0.29.0-autoconexion.spec.md):
# lo que debe hacer esta escrito en scripts/visita-contratos.ps1, y el comportamiento
# llega en la pasada verde.
#
# Compatible con Windows PowerShell 5.1 y ASCII puro, como ensayo-madre.ps1.
[CmdletBinding()]
param(
    [ValidateSet('Aviso', 'Codigo')]
    [string]$Modo = 'Aviso'
)

$script:Codigo = 0

function Invoke-Principal {
    Write-Host 'aviso-madre.ps1 aun no esta implementado.' -ForegroundColor Red
    $script:Codigo = 1
}

# Cargado con ". .\aviso-madre.ps1" solo define las funciones (asi se prueban sin
# Windows); ejecutado, corre el modo pedido.
if ($MyInvocation.InvocationName -ne '.') {
    Invoke-Principal
    exit $script:Codigo
}
