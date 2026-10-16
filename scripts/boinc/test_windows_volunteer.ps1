#Requires -Version 5.1
# Dependency-free contract tests. All system/network/installer operations are mocked.
[CmdletBinding()]
param()
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$script:Passed = 0
$script:Helper = Join-Path $PSScriptRoot 'setup_windows_volunteer.ps1'
. $script:Helper
$script:OriginalFunctions = @{}
$script:MockedNames = @{}
$ast = [Management.Automation.Language.Parser]::ParseFile($script:Helper, [ref]$null, [ref]$null)
foreach ($definition in $ast.EndBlock.Statements) {
    if ($definition -is [Management.Automation.Language.FunctionDefinitionAst]) {
        $script:OriginalFunctions[$definition.Name] = (Get-Item "Function:$($definition.Name)").ScriptBlock
    }
}
# Non-Windows machines lack these variables. They are used only with mocked host operations.
if (-not $env:ProgramData) { $env:ProgramData = $PSScriptRoot }
if (-not $env:LOCALAPPDATA) { $env:LOCALAPPDATA = $PSScriptRoot }
if (-not $env:SystemRoot) { $env:SystemRoot = $PSScriptRoot }

function Assert-True($Value, [string]$Message) {
    if (-not $Value) { throw "Assertion failed: $Message" }
}

function Assert-Throws([scriptblock]$Action, [string]$Pattern) {
    try { & $Action } catch {
        Assert-True ($_.Exception.Message -match $Pattern) "expected '$Pattern', got '$($_.Exception.Message)'"
        return
    }
    throw "Expected error matching '$Pattern'"
}

function Set-Mock([string]$Name, [scriptblock]$Body) {
    $script:MockedNames[$Name] = $true
    Set-Item -Path "Function:script:$Name" -Value $Body
}

function Set-FlowMocks {
    $script:Calls = New-Object 'Collections.Generic.List[string]'
    $script:Installation = [pscustomobject]@{
        Directory = $PSScriptRoot; DataDirectory = $PSScriptRoot; Version = [version]'8.2.11'
    }
    Set-Mock Assert-NativeWindows {}
    Set-Mock Get-BoincInstallation { param($Directory) return $script:Installation }
    Set-Mock Get-WindowsReadiness {
        param($DataDirectory, $CpuAllowed, $DeferDiskCheck)
        $script:Calls.Add('readiness')
        return [pscustomobject]@{
            RamGiB = 16; DiskGiB = 18
            Gpu = [pscustomobject]@{ Name = 'NVIDIA test'; Driver = '560.94'; FreeMiB = 8000 }
        }
    }
    Set-Mock Test-ProjectReachable { $script:Calls.Add('network') }
    Set-Mock Confirm-Consent { param($Action) $script:Calls.Add('consent') }
    Set-Mock Open-BoincManager { param($Installation) $script:Calls.Add('manager') }
    Set-Mock Test-ProjectAttached { param($Installation) $script:Calls.Add('status'); return $true }
    Set-Mock Install-OfficialBoinc { $script:Calls.Add('install') }
    Set-Mock Get-Process { param($Name, $ErrorAction) return $null }
    Set-Mock Read-Host { param($Prompt) $script:Calls.Add('prompt'); return '' }
}

function Test-Case([string]$Name, [scriptblock]$Body) {
    foreach ($mocked in @($script:MockedNames.Keys)) {
        Microsoft.PowerShell.Management\Remove-Item -Path "Function:$mocked"
    }
    $script:MockedNames.Clear()
    foreach ($functionName in $script:OriginalFunctions.Keys) {
        Set-Item -Path "Function:script:$functionName" -Value $script:OriginalFunctions[$functionName]
    }
    & $Body
    $script:Passed++
    Write-Host "PASS $Name"
}

Test-Case 'PowerShell parser accepts entrypoint' {
    $tokens = $null; $parseErrors = $null
    $null = [Management.Automation.Language.Parser]::ParseFile($script:Helper, [ref]$tokens, [ref]$parseErrors)
    Assert-True ($parseErrors.Count -eq 0) 'no parse errors'
}

Test-Case 'non-Windows runtime fails before doing work' {
    if ([Environment]::OSVersion.Platform -ne [PlatformID]::Win32NT) {
        Assert-Throws { Invoke-WindowsOnboarding $false $false $false '' } 'native Windows only'
    }
}

Test-Case 'default preview and install flag alone cannot mutate or use network' {
    Set-FlowMocks
    Invoke-WindowsOnboarding $false $true $false ''
    Assert-True (($script:Calls -join ',') -eq 'readiness') 'only local readiness inspected'
}

Test-Case 'existing client uses same consent/Manager/status path without installation' {
    Set-FlowMocks
    Invoke-WindowsOnboarding $true $true $false ''
    Assert-True (($script:Calls -join ',') -eq 'readiness,network,consent,manager,prompt,status') 'existing installation reused, no reattach/install'
}

Test-Case 'declined Manager consent never launches client' {
    Set-FlowMocks
    Set-Mock Confirm-Consent { param($Action) throw 'Cancelled' }
    Assert-Throws { Invoke-WindowsOnboarding $true $false $false '' } 'Cancelled'
    Assert-True (-not $script:Calls.Contains('manager')) 'no Manager start'
}

Test-Case 'missing client requires separate installation permission' {
    Set-FlowMocks
    $script:Installation = $null
    Assert-Throws { Invoke-WindowsOnboarding $true $false $false '' } 'InstallBoinc'
    Assert-True (-not $script:Calls.Contains('install')) 'no automatic install'
}

Test-Case 'eligible installation verified after explicit install request' {
    Set-FlowMocks
    $script:Installation = $null
    Set-Mock Install-OfficialBoinc {
        $script:Calls.Add('install')
        $script:Installation = [pscustomobject]@{
            Directory = $PSScriptRoot; DataDirectory = $PSScriptRoot; Version = [version]'8.2.11'
        }
    }
    Invoke-WindowsOnboarding $true $true $false ''
    Assert-True (($script:Calls -join ',') -eq 'readiness,network,install,readiness,consent,manager,prompt,status') 'installation and its data volume rechecked'
}

Test-Case 'cancelled/incomplete installer cannot claim success' {
    Set-FlowMocks
    $script:Installation = $null
    Assert-Throws { Invoke-WindowsOnboarding $true $true $false '' } 'not detected after installer'
    Assert-True (-not $script:Calls.Contains('manager')) 'no proceeding with missing client'
}

Test-Case 'fresh install defers default-volume check then verifies chosen data location' {
    Set-FlowMocks
    $script:Installation = $null
    $script:CustomData = Join-Path $PSScriptRoot 'custom-mounted-data'
    Set-Mock Get-WindowsReadiness {
        param($DataDirectory, $CpuAllowed, $DeferDiskCheck)
        if (-not $script:Installation) {
            Assert-True $DeferDiskCheck 'unselected default location cannot block installer'
        } else {
            Assert-True (-not $DeferDiskCheck) 'post-install disk check mandatory'
            Assert-True ($DataDirectory -eq $script:CustomData) 'chosen data directory checked'
        }
        $script:Calls.Add('readiness')
        return [pscustomobject]@{ RamGiB = 16; DiskGiB = 18; Gpu = $null }
    }
    Set-Mock Install-OfficialBoinc {
        $script:Calls.Add('install')
        $script:Installation = [pscustomobject]@{
            Directory = $PSScriptRoot; DataDirectory = $script:CustomData; Version = [version]'8.2.11'
        }
    }
    Invoke-WindowsOnboarding $true $true $false ''
    Assert-True (($script:Calls -join ',') -eq 'readiness,network,install,readiness,consent,manager,prompt,status') 'custom volume verified before client launch'
}

Test-Case 'old running client is never interrupted for upgrade' {
    Set-FlowMocks
    $script:Installation.Version = [version]'8.0.2'
    Set-Mock Get-Process { param($Name, $ErrorAction) return [pscustomobject]@{ Id = 1 } }
    Assert-Throws { Invoke-WindowsOnboarding $true $true $false '' } 'will not interrupt work'
    Assert-True (-not $script:Calls.Contains('install')) 'no install over running client'
}

Test-Case 'network failure stops before installation or Manager launch' {
    Set-FlowMocks
    Set-Mock Test-ProjectReachable { throw 'Project HTTPS endpoint is unavailable' }
    Assert-Throws { Invoke-WindowsOnboarding $true $true $false '' } 'unavailable'
    Assert-True (($script:Calls -join ',') -eq 'readiness') 'no mutations after network failure'
}

Test-Case 'missing attachment is actionable rather than successful' {
    Set-FlowMocks
    Set-Mock Test-ProjectAttached { param($Installation) return $false }
    Assert-Throws { Invoke-WindowsOnboarding $true $false $false '' } 'not attached'
}

Test-Case 'only exact affirmative response grants consent' {
    Set-Mock Read-Host { param($Prompt) return 'yes' }
    Assert-Throws { Confirm-Consent 'Test' } 'Cancelled'
    Set-Mock Read-Host { param($Prompt) return 'YES' }
    Confirm-Consent 'Test'
}

Test-Case 'GPU selection ignores old drivers/low VRAM and selects strongest eligible GPU' {
    $gpu = Select-NvidiaGpu @'
NVIDIA old, 527.99, 24000, 24000
NVIDIA busy, 560.94, 8000, 2999
NVIDIA small, 551.61, 4000, 3000
NVIDIA large, 560.94, 12000, 11000
'@
    Assert-True ($gpu.Name -eq 'NVIDIA large') 'largest usable GPU selected'
    Assert-True ($null -eq (Select-NvidiaGpu 'NVIDIA broken, [N/A], [N/A], [N/A]')) 'driver failures do not become readiness'
    Assert-True ($null -eq (Select-NvidiaGpu 'NVIDIA old, 527.99, 24000, 24000')) 'CUDA 12.4 capable driver required'
}

Test-Case 'malformed GPU output fails closed' {
    Assert-True ($null -eq (Select-NvidiaGpu 'NVIDIA unknown, new, lots')) 'malformed CSV rejected'
}

Test-Case 'bounded subprocess captures both streams and actual exit code' {
    $shell = (Get-Process -Id $PID).Path
    $result = Invoke-BoundedProcess $shell `
        '-NoProfile -NonInteractive -Command "[Console]::Out.Write(''probe''); [Console]::Error.Write(''diagnostic''); exit 7"' `
        $PSScriptRoot
    Assert-True ($result.ExitCode -eq 7) 'nonzero exit preserved'
    Assert-True ($result.Output -eq 'probe' -and $result.ErrorOutput -eq 'diagnostic') 'streams captured without leaking them'
}

function Set-ReadinessMocks {
    Set-Mock Assert-NativeWindows {}
    Set-Mock Get-CimInstance {
        param($ClassName)
        return [pscustomobject]@{ BuildNumber = 22631; ProductType = 1; TotalVisibleMemorySize = 16GB / 1KB }
    }
    Set-Mock Get-Command { param($Name, $CommandType, $ErrorAction) return $null }
    Set-Mock Get-Service { param($Name, $ErrorAction) return $null }
    Set-Mock Test-Path { param($LiteralPath) return $false }
    Set-Mock Get-DataFreeBytes { param($Directory) return 50GB }
}

Test-Case 'disk headroom accounts for every supported large GPU even while busy' {
    Set-ReadinessMocks
    Set-Mock Get-Command {
        param($Name, $CommandType, $ErrorAction)
        return [pscustomobject]@{ Source = 'mock-nvidia-smi.exe' }
    }
    Set-Mock Invoke-BoundedProcess {
        param($File, $Arguments, $Directory)
        return [pscustomobject]@{
            ExitCode = 0
            Output = "NVIDIA big busy, 560.94, 12000, 2000`nNVIDIA small free, 560.94, 6000, 3500"
            ErrorOutput = ''
        }
    }
    $ready = Get-WindowsReadiness $PSScriptRoot $false
    Assert-True ($ready.Gpu.Name -eq 'NVIDIA small free') 'currently eligible adapter reported'
    Assert-True ($ready.DiskGiB -eq 18) 'busy/secondary 8 GiB adapter can later qualify for the larger model'
}

Test-Case 'unsupported GPU requires explicit optional CPU participation' {
    Set-ReadinessMocks
    Assert-Throws { Get-WindowsReadiness $PSScriptRoot $false } 'AMD/Intel GPUs are not supported'
    $ready = Get-WindowsReadiness $PSScriptRoot $true
    Assert-True ($null -eq $ready.Gpu -and $ready.DiskGiB -eq 12) 'CPU exception only when explicitly permitted'
}

Test-Case 'unsupported Windows has actionable error' {
    Set-ReadinessMocks
    Set-Mock Get-CimInstance {
        param($ClassName)
        return [pscustomobject]@{ BuildNumber = 17763; ProductType = 1; TotalVisibleMemorySize = 16GB / 1KB }
    }
    Assert-Throws { Get-WindowsReadiness $PSScriptRoot $true } 'Older Windows'
}

Test-Case 'existing service-mode client cannot claim GPU readiness' {
    Set-ReadinessMocks
    Set-Mock Get-Command {
        param($Name, $CommandType, $ErrorAction)
        return [pscustomobject]@{ Source = 'mock-nvidia-smi.exe' }
    }
    Set-Mock Invoke-BoundedProcess {
        param($File, $Arguments, $Directory)
        return [pscustomobject]@{ ExitCode = 0; Output = 'NVIDIA test, 560.94, 12000, 11000'; ErrorOutput = '' }
    }
    Set-Mock Get-Service { param($Name, $ErrorAction) return [pscustomobject]@{ Name = 'BOINC' } }
    Assert-Throws { Get-WindowsReadiness $PSScriptRoot $false } 'Windows service'
}

Test-Case 'insufficient RAM and cold disk cannot count as ready' {
    Set-ReadinessMocks
    Set-Mock Get-DataFreeBytes { param($Directory) return 5GB }
    Assert-Throws { Get-WindowsReadiness $PSScriptRoot $true } '12 GiB free'
    Set-Mock Get-CimInstance {
        param($ClassName)
        return [pscustomobject]@{ BuildNumber = 22631; ProductType = 1; TotalVisibleMemorySize = 4GB / 1KB }
    }
    Assert-Throws { Get-WindowsReadiness $PSScriptRoot $true } '6 GiB host RAM'
}

Test-Case 'mounted data-directory query does not substitute drive-letter free space' {
    $script:MountedData = Join-Path $PSScriptRoot 'mounted-data'
    Set-Mock Test-Path { param($LiteralPath, $PathType) return $true }
    Set-Mock Get-DirectoryFreeBytes {
        param($Directory)
        Assert-True ($Directory -eq $script:MountedData) 'pass full mounted directory, not drive root'
        return 5GB
    }
    Assert-True ((Get-DataFreeBytes $script:MountedData) -eq 5GB) 'backing-volume space returned unchanged'
}

Test-Case 'missing data directory queries nearest existing parent' {
    $script:DataParent = $PSScriptRoot
    Set-Mock Test-Path {
        param($LiteralPath, $PathType)
        return $LiteralPath -eq $script:DataParent
    }
    Set-Mock Get-DirectoryFreeBytes {
        param($Directory)
        Assert-True ($Directory -eq $script:DataParent) 'nearest existing parent retained'
        return 20GB
    }
    Assert-True ((Get-DataFreeBytes (Join-Path $PSScriptRoot 'not-created-yet')) -eq 20GB) 'parent volume inspected'
}

Test-Case 'deferred preinstall check still validates hardware but never queries model disk' {
    Set-ReadinessMocks
    Set-Mock Get-DataFreeBytes { param($Directory) throw 'must defer model disk check' }
    $ready = Get-WindowsReadiness $PSScriptRoot $true $true
    Assert-True ($ready.DiskGiB -eq 12) 'required allowance still reported'
}

Test-Case 'native directory-aware disk API works on Windows without changing host' {
    if ([Environment]::OSVersion.Platform -eq [PlatformID]::Win32NT) {
        Assert-True ((Get-DirectoryFreeBytes $PSScriptRoot) -gt 0) 'Windows native directory space query succeeds'
    }
}

Test-Case 'attachment query is local read-only, captures output and never passes a password' {
    $script:Installation = [pscustomobject]@{ Directory = $PSScriptRoot; DataDirectory = $PSScriptRoot }
    Set-Mock Invoke-BoundedProcess {
        param($File, $Arguments, $Directory)
        Assert-True ($Arguments -eq '--get_project_status') 'read-only command only'
        Assert-True ($Directory -eq $PSScriptRoot) 'boinccmd can read normal local RPC auth without argument leakage'
        return [pscustomobject]@{ ExitCode = 0; Output = "1) ----------`n   master URL: https://boinc.usnewsmap.com/`n"; ErrorOutput = '' }
    }
    Assert-True (Test-ProjectAttached $script:Installation) 'exact project detected'
}

Test-Case 'other project/similar hostname cannot count as attached' {
    Set-Mock Invoke-BoundedProcess {
        param($File, $Arguments, $Directory)
        return [pscustomobject]@{ ExitCode = 0; Output = 'master URL: https://boinc.usnewsmap.com.evil.invalid/'; ErrorOutput = '' }
    }
    Assert-True (-not (Test-ProjectAttached ([pscustomobject]@{ Directory = $PSScriptRoot; DataDirectory = $PSScriptRoot }))) 'URL must match exactly'
}

Test-Case 'RPC authorization failure is redacted and actionable' {
    Set-Mock Invoke-BoundedProcess {
        param($File, $Arguments, $Directory)
        return [pscustomobject]@{ ExitCode = 1; Output = 'private output must not be printed'; ErrorOutput = 'Authorization failure' }
    }
    Assert-Throws {
        Test-ProjectAttached ([pscustomobject]@{ Directory = $PSScriptRoot; DataDirectory = $PSScriptRoot })
    } 'Cannot query the local BOINC client'
}

Test-Case 'HTTP errors do not count as scheduler acceptance' {
    Set-Mock Invoke-WebRequest { throw 'network details' }
    Assert-Throws { Test-ProjectReachable } 'not scheduler acceptance'
}

Test-Case 'hash mismatch rejects installer before signature validation' {
    Set-Mock Get-FileHash { param($LiteralPath, $Algorithm) return [pscustomobject]@{ Hash = 'BAD' } }
    Set-Mock Get-AuthenticodeSignature { throw 'must not get this far' }
    Assert-Throws { Assert-VerifiedInstaller 'not-executed.exe' } 'SHA256 mismatch'
}

Test-Case 'invalid signature rejects even hash-pinned installer' {
    Set-Mock Get-FileHash { param($LiteralPath, $Algorithm) return [pscustomobject]@{ Hash = $script:InstallerHash } }
    Set-Mock Get-AuthenticodeSignature {
        param($LiteralPath)
        return [pscustomobject]@{ Status = 'NotTrusted'; SignerCertificate = $null }
    }
    Assert-Throws { Assert-VerifiedInstaller 'not-executed.exe' } 'signature is not valid'
}

Test-Case 'hash plus valid BOINC publisher required' {
    Set-Mock Get-FileHash { param($LiteralPath, $Algorithm) return [pscustomobject]@{ Hash = $script:InstallerHash } }
    Set-Mock Get-AuthenticodeSignature {
        param($LiteralPath)
        $certificate = [pscustomobject]@{ Subject = 'CN=BOINC, O=BOINC, C=US' }
        $certificate | Add-Member ScriptMethod GetNameInfo { param($Type, $Issuer) return 'BOINC' }
        return [pscustomobject]@{ Status = 'Valid'; SignerCertificate = $certificate }
    }
    Assert-VerifiedInstaller 'not-executed.exe'
    Set-Mock Get-AuthenticodeSignature {
        param($LiteralPath)
        $certificate = [pscustomobject]@{ Subject = 'CN=Other, O=Other, C=US' }
        $certificate | Add-Member ScriptMethod GetNameInfo { param($Type, $Issuer) return 'Other' }
        return [pscustomobject]@{ Status = 'Valid'; SignerCertificate = $certificate }
    }
    Assert-Throws { Assert-VerifiedInstaller 'not-executed.exe' } 'signature is not valid'
}

function Set-InstallerMocks {
    $script:Calls = New-Object 'Collections.Generic.List[string]'
    Set-Mock Confirm-Consent { param($Action) $script:Calls.Add('consent') }
    Set-Mock New-Item { param($ItemType, $Path) $script:Calls.Add('mkdir') }
    Set-Mock Invoke-WebRequest {
        param($Uri, $OutFile, [switch]$UseBasicParsing, $MaximumRedirection, $TimeoutSec)
        Assert-True ($Uri -eq 'https://boinc.berkeley.edu/dl/boinc_8.2.11_windows_x86_64.exe') 'fixed official stable HTTPS download'
        Assert-True ($MaximumRedirection -eq 0) 'no unreviewed redirect'
        $script:Calls.Add('download')
    }
    Set-Mock Assert-VerifiedInstaller { param($Path) $script:Calls.Add('verify') }
    Set-Mock Start-Process {
        param($FilePath, $ArgumentList, $Verb, [switch]$Wait, [switch]$PassThru)
        Assert-True ($Verb -eq 'RunAs') 'only installer requests elevation'
        Assert-True ($ArgumentList -match 'LAUNCHPROGRAM=0') 'MSI default must not launch Manager before separate consent'
        Assert-True ($ArgumentList -notmatch 'ENABLEPROTECTEDAPPLICATIONEXECUTION3=0') 'preserve BOINC protected application execution'
        Assert-True ($ArgumentList -notmatch 'LAUNCHWSLIMAGEINSTALLER=1') 'no WSL auto-launch'
        Assert-True ($ArgumentList -notmatch 'quiet|silent|password|account') 'interactive, no credentials'
        $script:Calls.Add('execute')
        return [pscustomobject]@{ ExitCode = 0 }
    }
    Set-Mock Read-Host { param($Prompt) $script:Calls.Add('prompt'); return '' }
    Set-Mock Test-Path { param($LiteralPath) return $true }
    Set-Mock Remove-Item { param($LiteralPath, [switch]$Force) $script:Calls.Add('cleanup') }
}

Test-Case 'install requires consent, verified bytes, second consent, then UAC and cleanup' {
    Set-InstallerMocks
    Install-OfficialBoinc
    Assert-True (($script:Calls -join ',') -eq 'consent,mkdir,download,verify,consent,execute,prompt,cleanup,cleanup') 'secure install ordering'
}

Test-Case 'declining installation performs no download or filesystem operation' {
    Set-InstallerMocks
    Set-Mock Confirm-Consent { param($Action) throw 'Cancelled' }
    Assert-Throws { Install-OfficialBoinc } 'Cancelled'
    Assert-True ($script:Calls.Count -eq 0) 'no mutation before first consent'
}

Test-Case 'unverified download never executes and is cleaned up' {
    Set-InstallerMocks
    Set-Mock Assert-VerifiedInstaller { param($Path) throw 'SHA256 mismatch' }
    Assert-Throws { Install-OfficialBoinc } 'SHA256 mismatch'
    Assert-True (-not $script:Calls.Contains('execute')) 'no unverified execution'
    Assert-True (($script:Calls -join ',') -match 'cleanup,cleanup$') 'download removed'
}

Test-Case 'download network failure never executes and is cleaned up' {
    Set-InstallerMocks
    Set-Mock Invoke-WebRequest { throw 'TLS failure' }
    Assert-Throws { Install-OfficialBoinc } 'download failed'
    Assert-True (-not $script:Calls.Contains('execute')) 'no execution after network failure'
    Assert-True (($script:Calls -join ',') -match 'cleanup,cleanup$') 'failed download removed'
}

Test-Case 'declining execution after verification still cleans download' {
    Set-InstallerMocks
    Set-Mock Confirm-Consent {
        param($Action)
        $script:Calls.Add('consent')
        if ($script:Calls.Contains('verify')) { throw 'Cancelled' }
    }
    Assert-Throws { Install-OfficialBoinc } 'Cancelled'
    Assert-True (-not $script:Calls.Contains('execute')) 'second consent enforced'
    Assert-True (($script:Calls -join ',') -match 'cleanup,cleanup$') 'cancelled download removed'
}

Write-Host "$script:Passed Windows onboarding contract tests passed. Mocks are NOT live Windows/GPU/installer acceptance."
