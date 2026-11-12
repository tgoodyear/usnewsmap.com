# Checks that a Windows executable of the native OCR build is an x86-64 image that imports only
# DLLs of Windows itself, and writes the names of those DLLs, one per line.
#
#   boinc/apps/native/check-windows-imports.ps1 -Exe build\usnm_tesseract.exe
#
# A DLL counts as the system's when it sits in System32 or is an API set (api-ms-win-core-*, which
# Windows resolves itself), and isn't a redistributable of the C runtime or the compiler: a
# volunteer's host may not have those installed. Needs dumpbin, from the Visual Studio that is
# installed on GitHub's windows runners.
[CmdletBinding()]
param([Parameter(Mandatory = $true)][string]$Exe)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$Exe = (Resolve-Path $Exe).Path
$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
$vs = (& $vswhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath | Select-Object -First 1).Trim()
$dumpbin = Get-ChildItem (Join-Path $vs 'VC\Tools\MSVC') -Recurse -Filter dumpbin.exe |
    Where-Object { $_.FullName -match 'Hostx64\\x64' } | Select-Object -First 1
if (-not $dumpbin) { throw 'no dumpbin in the Visual Studio install' }

$headers = & $dumpbin.FullName /headers $Exe
if (-not ($headers -match '8664 machine \(x64\)')) { throw "$Exe is not an x64 executable" }
$loads = @(& $dumpbin.FullName /dependents $Exe | ForEach-Object { $_.Trim() } | Where-Object { $_ -match '\.dll$' })
if (-not $loads) { throw "dumpbin lists no imports for $Exe" }
$redistributed = '^(vcruntime|msvcp|msvcr|vcomp|concrt|vccorlib|ucrtbase|api-ms-win-crt-)'
foreach ($dll in $loads) {
    $system = (Test-Path (Join-Path $env:SystemRoot "System32\$dll")) -or ($dll -match '^api-ms-win-core-')
    if ($dll -match $redistributed -or -not $system) {
        throw "$Exe imports $dll, which isn't one of the system's own DLLs"
    }
}
$loads
