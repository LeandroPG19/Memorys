# Conecta este equipo con la PC madre de MemoryIndustry 0.29, una sola vez y sin
# administrador. Lo lanza CONECTAR-A-LA-MADRE.cmd (paquete CLIENTE).
#
# ESQUELETO de la pasada roja (tajada D de .cursor/handoffs/0.29.0-autoconexion.spec.md):
# lo que debe hacer esta escrito en scripts/visita-contratos.ps1, y el comportamiento
# llega en la pasada verde.
#
# Compatible con Windows PowerShell 5.1 y ASCII puro, como ensayo-madre.ps1.
[CmdletBinding()]
param()

$script:Codigo = 0

function Invoke-Principal {
    Write-Host 'conectar-cliente.ps1 aun no esta implementado.' -ForegroundColor Red
    $script:Codigo = 1
}

# Cargado con ". .\conectar-cliente.ps1" solo define las funciones (asi se prueban sin
# Windows); ejecutado, conecta.
if ($MyInvocation.InvocationName -ne '.') {
    Invoke-Principal
    exit $script:Codigo
}
