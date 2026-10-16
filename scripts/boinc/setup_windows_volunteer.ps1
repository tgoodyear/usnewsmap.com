#Requires -Version 5.1
<#
.SYNOPSIS
Preview native Windows BOINC onboarding; -Apply enables an interactive setup.
.DESCRIPTION
Run from a reviewed checkout in a normal (not administrator) PowerShell window.
Credentials are entered only in BOINC Manager, never in this script or arguments.
Project participation includes production and experimental tasks. Operators phase
campaigns server-side; attachment does not guarantee work. No WSL, Podman or CUDA toolkit needed.
.EXAMPLE
.\scripts\boinc\setup_windows_volunteer.ps1
.EXAMPLE
.\scripts\boinc\setup_windows_volunteer.ps1 -Apply -InstallBoinc
#>
[CmdletBinding()]
param(
    [switch]$Apply,
    [switch]$InstallBoinc,
    [switch]$AllowCpuOnly,
    [string]$BoincDirectory
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
$script:ProjectUrl = 'https://boinc.usnewsmap.com/'
$script:MinimumVersion = [version]'8.2.11'
$script:InstallerUrl = 'https://boinc.berkeley.edu/dl/boinc_8.2.11_windows_x86_64.exe'
# Matches the official download and Microsoft winget UCBerkeley.BOINC 8.2.11
# manifest (checked 2026-10-15); also require valid BOINC Authenticode.
$script:InstallerHash = '3C9992CD3EAFD90A65D45FDC8BD411ABD574E657710384966672AC0E264D92CB'

function Confirm-Consent([string]$Action) {
    if ((Read-Host "$Action Type YES to consent (anything else cancels)") -cne 'YES') {
        throw 'Cancelled. No further changes made.'
    }
}

function Get-BoincInstallation([string]$Directory) {
    $setup = Get-ItemProperty 'HKLM:\SOFTWARE\Space Sciences Laboratory, U.C. Berkeley\BOINC Setup' `
        -ErrorAction SilentlyContinue
    $data = Join-Path $env:ProgramData 'BOINC'
    if ($setup -and $setup.PSObject.Properties['DATADIR'] -and $setup.DATADIR) {
        $data = $setup.DATADIR
    }
    $candidates = @()
    if ($Directory) {
        $candidates = @($Directory)
    } else {
        if ($setup -and $setup.PSObject.Properties['INSTALLDIR'] -and $setup.INSTALLDIR) {
            $candidates += $setup.INSTALLDIR
        }
        $candidates += (Join-Path $env:ProgramFiles 'BOINC')
    }
    foreach ($candidate in $candidates) {
        $client = Join-Path $candidate 'boinc.exe'
        if (Test-Path -LiteralPath $client -PathType Leaf) {
            $versionText = [Diagnostics.FileVersionInfo]::GetVersionInfo($client).ProductVersion
            if ($versionText -notmatch '^\d+\.\d+\.\d+') {
                throw 'Cannot read installed BOINC version. Repair using an official BOINC installer.'
            }
            $version = [version]$Matches[0]
            foreach ($name in @('boincmgr.exe', 'boinccmd.exe')) {
                if (-not (Test-Path -LiteralPath (Join-Path $candidate $name) -PathType Leaf)) {
                    throw "Incomplete BOINC installation: missing $name. Repair the official installation."
                }
            }
            return [pscustomobject]@{ Directory = $candidate; DataDirectory = $data; Version = $version }
        }
    }
    if ($Directory) { throw 'No BOINC installation at -BoincDirectory. Supply its existing program directory.' }
    return $null
}

function Invoke-BoundedProcess([string]$File, [string]$Arguments, [string]$Directory) {
    $info = New-Object Diagnostics.ProcessStartInfo
    $info.FileName = $File
    $info.Arguments = $Arguments
    $info.WorkingDirectory = $Directory
    $info.UseShellExecute = $false
    $info.CreateNoWindow = $true
    $info.RedirectStandardOutput = $true
    $info.RedirectStandardError = $true
    $process = New-Object Diagnostics.Process
    $process.StartInfo = $info
    try {
        [void]$process.Start()
        $output = $process.StandardOutput.ReadToEndAsync()
        $errors = $process.StandardError.ReadToEndAsync()
        if (-not $process.WaitForExit(30000)) {
            $process.Kill()
            throw 'Local BOINC/driver check timed out. Check BOINC Manager and the NVIDIA driver, then retry.'
        }
        return [pscustomobject]@{
            ExitCode = $process.ExitCode
            Output = $output.GetAwaiter().GetResult()
            ErrorOutput = $errors.GetAwaiter().GetResult()
        }
    } finally {
        $process.Dispose()
    }
}

function Assert-NativeWindows {
    if ([Environment]::OSVersion.Platform -ne [PlatformID]::Win32NT) {
        throw 'This helper is for native Windows only. See docs/boinc-volunteer.md for Linux/macOS.'
    }
    if (-not [Environment]::Is64BitProcess -or $env:PROCESSOR_ARCHITECTURE -ne 'AMD64') {
        throw 'Use 64-bit PowerShell on Intel/AMD x64 Windows. Windows ARM and 32-bit hosts are not supported by this helper.'
    }
}

function Get-DirectoryFreeBytes([string]$Directory) {
    if (-not ('UsnmOnboarding.DiskSpace' -as [type])) {
        Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
namespace UsnmOnboarding {
    public static class DiskSpace {
        [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        [return: MarshalAs(UnmanagedType.Bool)]
        public static extern bool GetDiskFreeSpaceEx(
            string path, out ulong available, out ulong total, out ulong free);
    }
}
'@
    }
    [uint64]$available = 0; [uint64]$total = 0; [uint64]$free = 0
    if (-not [UsnmOnboarding.DiskSpace]::GetDiskFreeSpaceEx($Directory, [ref]$available, [ref]$total, [ref]$free)) {
        throw 'Cannot inspect the BOINC data directory volume. Check that its mount/drive is connected and accessible to your normal user.'
    }
    return $available
}

function Get-DataFreeBytes([string]$Directory) {
    $existing = [IO.Path]::GetFullPath($Directory)
    while (-not (Test-Path -LiteralPath $existing -PathType Container)) {
        $parent = [IO.Directory]::GetParent($existing)
        if (-not $parent) { throw 'No accessible parent directory for BOINC data. Check its data-directory setting and mounted volume.' }
        $existing = $parent.FullName
    }
    # Query the directory itself, not its drive-letter root: Windows follows
    # mount points/junctions and reports the actual backing volume and user quota.
    return (Get-DirectoryFreeBytes $existing)
}

function Get-WindowsReadiness([string]$DataDirectory, [bool]$CpuAllowed, [bool]$DeferDiskCheck = $false) {
    Assert-NativeWindows
    $os = Get-CimInstance Win32_OperatingSystem
    if ([int]$os.BuildNumber -lt 19045 -or [int]$os.ProductType -ne 1) {
        throw 'Use a supported Windows 11 desktop, or Windows 10 22H2 with current ESU updates. Older Windows and Windows Server are outside this pilot.'
    }
    $ramGiB = [math]::Round(([double]$os.TotalVisibleMemorySize * 1KB / 1GB), 1)
    if ($ramGiB -lt 6) { throw 'Native Qwen onboarding requires at least 6 GiB host RAM. Use a larger host or ask the operator about CPU-quality-only participation.' }

    $gpu = $null
    $gpus = @()
    $smi = Get-Command nvidia-smi.exe -CommandType Application -ErrorAction SilentlyContinue
    if (-not $smi) {
        $smiPath = Join-Path $env:SystemRoot 'System32\nvidia-smi.exe'
        if (Test-Path -LiteralPath $smiPath) { $smi = [pscustomobject]@{ Source = $smiPath } }
    }
    if ($smi) {
        $result = Invoke-BoundedProcess $smi.Source `
            '--query-gpu=name,driver_version,memory.total,memory.free --format=csv,noheader,nounits' $env:SystemRoot
        if ($result.ExitCode -eq 0) {
            $gpus = @(Get-NvidiaGpus $result.Output)
            $gpu = $gpus | Where-Object { $_.FreeMiB -ge 3000 } | Sort-Object FreeMiB -Descending | Select-Object -First 1
        }
    }
    if (-not $gpu -and -not $CpuAllowed) {
        throw 'No eligible NVIDIA CUDA GPU found (driver >= 551.61 and >= 3000 MiB free VRAM). Update the official NVIDIA Windows driver, close GPU-heavy apps and retry. AMD/Intel GPUs are not supported for this native CUDA pilot. For optional CPU participation, rerun with -AllowCpuOnly; no CPU preferences are changed automatically.'
    }
    $service = Get-Service -Name BOINC -ErrorAction SilentlyContinue
    if ($service) {
        if (-not $CpuAllowed) {
            throw 'BOINC is installed as a Windows service, which cannot access GPUs. Finish/report existing work, then use the official installer Advanced options to remove Service install before GPU volunteering. This helper will not stop/reconfigure the service. -AllowCpuOnly permits optional CPU participation without enabling GPU access.'
        }
        Write-Warning 'Existing BOINC service detected: GPU access is unavailable in service mode. Only optional CPU participation is possible until you review the official installation.'
        $gpu = $null
    }
    $diskGiB = 12
    if ($gpus | Where-Object { $_.TotalMiB -ge 7000 }) { $diskGiB = 18 }
    if (-not $DeferDiskCheck -and (Get-DataFreeBytes $DataDirectory) -lt ($diskGiB * 1GB)) {
        throw "Need at least $diskGiB GiB free on the BOINC data volume (cold model download/unpacking). Free space without deleting shared sticky model files, then retry."
    }
    return [pscustomobject]@{ RamGiB = $ramGiB; Gpu = $gpu; DiskGiB = $diskGiB }
}

function Get-NvidiaGpus([string]$Csv) {
    foreach ($line in ($Csv -split '\r?\n')) {
        $fields = $line -split ','
        if ($fields.Count -ne 4) { continue }
        $driver = $fields[1].Trim()
        $total = $fields[2].Trim()
        $free = $fields[3].Trim()
        if ($driver -notmatch '^\d+\.\d+$' -or $total -notmatch '^\d+$' -or $free -notmatch '^\d+$') {
            continue
        }
        if ([version]$driver -ge [version]'551.61') {
            [pscustomobject]@{
                Name = $fields[0].Trim(); Driver = $driver
                TotalMiB = [int]$total; FreeMiB = [int]$free
            }
        }
    }
}

function Select-NvidiaGpu([string]$Csv) {
    return (Get-NvidiaGpus $Csv | Where-Object { $_.FreeMiB -ge 3000 } |
        Sort-Object FreeMiB -Descending | Select-Object -First 1)
}

function Assert-VerifiedInstaller([string]$Path) {
    if ((Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash -ne $script:InstallerHash) {
        throw 'BOINC installer SHA256 mismatch. Nothing executed. Do not bypass this check; ask the operator to review an updated official installer pin.'
    }
    $signature = Get-AuthenticodeSignature -LiteralPath $Path
    if ($signature.Status -ne 'Valid' -or -not $signature.SignerCertificate -or
        $signature.SignerCertificate.GetNameInfo([Security.Cryptography.X509Certificates.X509NameType]::SimpleName, $false) -ne 'BOINC' -or
        $signature.SignerCertificate.Subject -notmatch '(^|,\s*)O=BOINC(,|$)') {
        throw 'BOINC installer signature is not valid for publisher BOINC. Nothing executed. Check Windows updates, clock, certificate/revocation connectivity; do not disable signature checks.'
    }
}

function Install-OfficialBoinc {
    Confirm-Consent 'Download and install/upgrade official recommended BOINC 8.2.11 (about 17 MB)? Existing project data is retained. Only the installer will request administrator permission.'
    $downloadDirectory = Join-Path $env:LOCALAPPDATA ('usnewsmap-onboarding-' + [guid]::NewGuid().ToString('N'))
    [void](New-Item -ItemType Directory -Path $downloadDirectory)
    $installer = Join-Path $downloadDirectory 'boinc_8.2.11_windows_x86_64.exe'
    try {
        try {
            Invoke-WebRequest -Uri $script:InstallerUrl -OutFile $installer -UseBasicParsing `
                -MaximumRedirection 0 -TimeoutSec 120
        } catch {
            throw 'Official BOINC download failed. Check internet/TLS/proxy access to boinc.berkeley.edu and retry. Nothing executed.'
        }
        Assert-VerifiedInstaller $installer
        Write-Host 'In Advanced options leave Service install OFF (Windows services cannot use the GPU) and leave BOINC protected application execution at its secure default. At Finish, verify Launch BOINC Manager is unchecked; its launch is disabled on the installer command line as a safeguard. No WSL/Podman/toolkit is needed.'
        Confirm-Consent 'Signature and SHA256 verified. Open the interactive BOINC installer now? Review its options before installing.'
        # LAUNCHPROGRAM is an upstream SecureCustomProperties MSI property.
        # Pass zero explicitly because the stock MSI defaults it to one; never
        # let Finish start the client before the later Manager-launch consent.
        $arguments = 'LAUNCHPROGRAM=0 /norestart'
        $process = Start-Process -FilePath $installer -ArgumentList $arguments -Verb RunAs -Wait -PassThru
        if ($process.ExitCode -notin @(0, 3010)) {
            throw 'BOINC installer did not complete. Finish/cancel any open installer, review its error, and rerun this helper.'
        }
        [void](Read-Host 'Finish the installer completely; reboot first if requested. Press Enter when finished (Ctrl+C to stop)')
    } finally {
        if (Test-Path -LiteralPath $installer) { Remove-Item -LiteralPath $installer -Force }
        Remove-Item -LiteralPath $downloadDirectory
    }
}

function Test-ProjectReachable {
    try {
        $response = Invoke-WebRequest -Uri $script:ProjectUrl -UseBasicParsing -TimeoutSec 30 -MaximumRedirection 0
        if ($response.StatusCode -ne 200) { throw 'Unavailable' }
    } catch {
        throw 'Project HTTPS endpoint is unavailable. Check internet/DNS, system clock, firewall/proxy and https://boinc.usnewsmap.com/ in your browser, then retry. Do not disable TLS validation. HTTP reachability is not scheduler acceptance.'
    }
}

function Test-ProjectAttached($Installation) {
    if (-not (Test-Path -LiteralPath $Installation.DataDirectory -PathType Container)) {
        throw 'BOINC data directory is not accessible. Open BOINC Manager once as your regular user; for a custom install check its DATADIR registry setting. Do not change data permissions or expose RPC remotely.'
    }
    $result = Invoke-BoundedProcess (Join-Path $Installation.Directory 'boinccmd.exe') `
        '--get_project_status' $Installation.DataDirectory
    if ($result.ExitCode -ne 0 -or ($result.Output + $result.ErrorOutput) -match '(?im)authorization failure|can.t connect|RPC:.*error|operation failed') {
        throw 'Cannot query the local BOINC client. In BOINC Manager select the local computer and start/connect the client, then retry. For authorization errors repair the official installation/user access; do not put gui_rpc_auth.cfg or its password on a command line. No client was started or killed by this check.'
    }
    return ($result.Output -match '(?im)^\s*master URL:\s*https://boinc\.usnewsmap\.com/\s*$')
}

function Open-BoincManager($Installation) {
    [void](Start-Process -FilePath (Join-Path $Installation.Directory 'boincmgr.exe'))
}

function Invoke-WindowsOnboarding {
    param([bool]$ApplyChanges, [bool]$InstallClient, [bool]$CpuAllowed, [string]$Directory)
    Assert-NativeWindows
    $installation = Get-BoincInstallation $Directory
    $dataDirectory = Join-Path $env:ProgramData 'BOINC'
    if ($installation) { $dataDirectory = $installation.DataDirectory }
    $ready = Get-WindowsReadiness $dataDirectory $CpuAllowed (-not $installation)
    if (-not $installation) {
        Write-Host 'BOINC data location is not yet selected. Model-space verification is deferred until the installer finishes; choose a volume with sufficient space in Advanced options.'
    }
    Write-Host "Native Windows preview: $($ready.RamGiB) GiB RAM; reserve $($ready.DiskGiB) GiB disk in BOINC preferences."
    if ($ready.Gpu) {
        Write-Host "NVIDIA: $($ready.Gpu.Name); driver $($ready.Gpu.Driver); $($ready.Gpu.FreeMiB) MiB currently free."
    } else {
        Write-Warning 'CPU-only participation explicitly allowed. CPU app publication is not guaranteed; set CPU/GPU choices yourself in project preferences.'
    }
    if ($installation) {
        Write-Host "Existing BOINC $($installation.Version); project data will not be moved or reset."
        if ($installation.Version -lt $script:MinimumVersion) {
            Write-Warning 'This client is below the recommended native pilot baseline (8.2.11).'
        }
    }
    else { Write-Host 'BOINC is not installed. Use -Apply -InstallBoinc for the verified interactive installer.' }
    Write-Warning 'Project participation includes production and experimental tasks, including Qwen. Operators bound pilot campaigns; work is not guaranteed. No separate experiment flag or profile update is required.'
    Write-Host 'No WSL, Podman, Docker or CUDA toolkit is installed. The helper does not edit BOINC preferences/account files, firewall or execution policy; the optional BOINC installer applies the installation settings you review.'
    if (-not $ApplyChanges) {
        Write-Host 'CHECK ONLY: no network, downloads, client starts or attachment. Rerun with -Apply to continue interactively; -InstallBoinc additionally permits an installation prompt.'
        return
    }
    Test-ProjectReachable
    if (-not $installation -or $installation.Version -lt $script:MinimumVersion) {
        if (-not $InstallClient) { throw 'Use BOINC >= 8.2.11 for this native pilot. Rerun with -Apply -InstallBoinc to review the official recommended installer, or install an eligible official client yourself.' }
        if (Get-Process -Name boinc, boincmgr -ErrorAction SilentlyContinue) {
            throw 'An older BOINC client/Manager is running. Finish/report existing tasks, exit BOINC Manager and its client, then rerun before upgrading. This helper will not interrupt work.'
        }
        Install-OfficialBoinc
        $installation = Get-BoincInstallation $Directory
        if (-not $installation -or $installation.Version -lt $script:MinimumVersion) {
            throw 'Eligible BOINC installation not detected after installer. Finish installation/reboot if requested, then rerun; use -BoincDirectory for a custom program location.'
        }
        # A custom data directory selected in the installer may use a different volume.
        $null = Get-WindowsReadiness $installation.DataDirectory $CpuAllowed
    }
    Confirm-Consent 'On a computer you own or have permission to use, open local BOINC Manager? Manager may start the client and resume its existing projects. Adding usnewsmap is a separate, deliberate step inside Manager.'
    Open-BoincManager $installation
    Write-Host @"
Before adding a project:
  1. In Manager's computing preferences choose idle-only CPU/GPU, no battery use,
     conservative CPU/memory limits and enough BOINC disk allowance. These local
     preferences affect ALL projects; review rather than overwrite existing limits.
  2. At $script:ProjectUrl create/sign in to YOUR account.
     Review project preferences: GPU-only volunteers disable Use CPU; CPU is optional.
     Participation includes production and experimental work. Your normal CPU/GPU
     choices and resource limits apply to both; there is no experiment checkbox.
  3. In BOINC Manager: Tools > Add project (or Add project/account manager),
     enter $script:ProjectUrl and sign in to the SAME account.
     Enter passwords ONLY in Manager/the HTTPS project site. Never paste account
     keys or gui_rpc_auth.cfg here. Already attached? Do NOT remove/re-add it.
  4. In Manager's Event Log verify the NVIDIA GPU is detected and look for
     Scheduler request completed. Service installs cannot access Windows GPUs.
"@
    [void](Read-Host 'Complete/review these steps in Manager, then press Enter to verify local attachment (Ctrl+C cancels)')
    if (-not (Test-ProjectAttached $installation)) {
        throw 'usnewsmap is not attached to the local client. In Manager add https://boinc.usnewsmap.com/ using your account, check account credentials and Event Log for network/scheduler errors, then rerun. No account or project was changed by this check.'
    }
    Write-Host 'Local usnewsmap attachment verified. This is NOT evidence of scheduler acceptance, app availability or a completed task.'
    Write-Host 'Experimental and production work use this SAME account, attachment, client, CPU/GPU choices, pause/resource preferences, project Update and automatic result/diagnostic return. No separate experiment enrollment; never reattach or sideload apps/models to switch cohorts.'
    Write-Host 'In Manager check Event Log and Tasks: no work may mean a server-side cohort is closed, no published Windows app, or unmet preferences/resources. Experimental failures appear as normal Computation error and diagnostics return automatically; no manual log upload. Do not enable WSL or delete model caches to fix this.'
    Write-Host 'Live Windows acceptance still requires task download/run, suspend/resume, upload, server validation and collection. Use No new tasks to stop new work; let existing tasks finish/report.'
}

if ($MyInvocation.InvocationName -ne '.') {
    try {
        Invoke-WindowsOnboarding -ApplyChanges $Apply.IsPresent -InstallClient $InstallBoinc.IsPresent `
            -CpuAllowed $AllowCpuOnly.IsPresent -Directory $BoincDirectory
    } catch {
        Write-Error $_.Exception.Message -ErrorAction Continue
        exit 1
    }
}
