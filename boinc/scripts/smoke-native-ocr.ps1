# The Windows form of smoke-native-ocr.sh: runs the native OCR worker on one small page with no
# BOINC client, as a slot would have it, and checks that it refuses a job for another build.
#
#   boinc/scripts/smoke-native-ocr.ps1 -Build build -Fixture target\release\examples\native_fixture.exe
#
# The build directory holds the files by their open names, none with an extension, so the worker
# is copied to a name Windows will run.
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$Build,
    [Parameter(Mandatory = $true)][string]$Fixture
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

# The worker's errors then name their causes and where they came from.
$env:RUST_BACKTRACE = '1'
$Build = (Resolve-Path $Build).Path
$Fixture = (Resolve-Path $Fixture).Path
$repo = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$work = Join-Path ([System.IO.Path]::GetTempPath()) ("native-ocr-smoke-" + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $work | Out-Null
try {
    $manifest = Join-Path $Build 'usnm_native_build.json'
    $image = 'sha256:' + (Get-FileHash -Algorithm SHA256 $manifest).Hash.ToLower()
    $worker = Join-Path $work 'usnm_ocr.exe'
    Copy-Item (Join-Path $Build 'usnm_ocr') $worker

    # The client links a slot's files with pointer files, as it does for every file of a build.
    function New-Slot([string]$name) {
        $slot = Join-Path $work $name
        New-Item -ItemType Directory -Path $slot | Out-Null
        Get-ChildItem $Build -File | Where-Object { $_.Name -ne 'usnm_ocr' } | ForEach-Object {
            Set-Content -Encoding ascii -Path (Join-Path $slot $_.Name) -Value "<soft_link>$($_.FullName)</soft_link>"
        }
        $slot
    }

    # Runs the worker in a slot, and returns its exit code. Its standard error goes to a log.
    function Invoke-Worker([string]$slot, [string]$log) {
        Push-Location $slot
        try {
            & $worker 2> $log
            return $LASTEXITCODE
        } finally {
            Pop-Location
        }
    }

    function Invoke-Fixture([string]$image, [string]$slot) {
        & $Fixture --scan $scan --language eng --image $image --dir $slot
        if ($LASTEXITCODE -ne 0) { throw "the fixture failed with $LASTEXITCODE" }
    }

    $scan = Join-Path $repo 'web\public\og-image.png'

    $good = New-Slot 'good'
    Invoke-Fixture $image $good
    $code = Invoke-Worker $good (Join-Path $work 'good.log')
    if ($code -ne 0) {
        Get-Content (Join-Path $work 'good.log') | Write-Host
        throw "the worker failed with $code"
    }
    foreach ($file in 'result.json', 'text.txt', 'layout.xml', 'boinc_finish_called') {
        $path = Join-Path $good $file
        if (-not (Test-Path $path) -or (Get-Item $path).Length -eq 0) { throw "no $file" }
    }
    if (-not (Select-String -Path (Join-Path $good 'result.json') -Pattern '"text_sha256"' -Quiet)) {
        throw 'result.json has no text_sha256'
    }

    $wrong = New-Slot 'wrong'
    Invoke-Fixture ('sha256:' + ('2' * 64)) $wrong
    $code = Invoke-Worker $wrong (Join-Path $work 'wrong.log')
    if ($code -eq 0) { throw 'a job for another build ran' }
    if (-not (Select-String -Path (Join-Path $work 'wrong.log') -Pattern 'made for build' -Quiet)) {
        Get-Content (Join-Path $work 'wrong.log') | Write-Host
        throw 'the worker refused the job for another reason'
    }
    if (Test-Path (Join-Path $wrong 'boinc_finish_called')) { throw 'the refused job finished' }
    Write-Host "native worker read a page and refused another build's job"
} finally {
    Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
}
