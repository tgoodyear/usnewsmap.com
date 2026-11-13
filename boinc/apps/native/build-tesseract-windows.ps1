# Builds the Tesseract of the native Windows `usnm_ocr` and `usnm_gold` app versions (#1779) from
# the sources pinned in static-sources.lock: one x86-64 executable, statically linked against
# those libraries and the C runtime, that imports nothing but system DLLs.
#
#   boinc/apps/native/build-tesseract-windows.ps1 -Out build -Work $env:RUNNER_TEMP\tesseract
#
# Needs Visual Studio 2022 with the C++ tools, CMake and git, as GitHub's windows-2022 runner has
# them. Writes <Out>\usnm_tesseract.exe and <Out>\usnm_tesseract.sources, which records the
# sources, the compiler and the DLLs the executable imports. Differences from the Linux build
# (Containerfile) are in docs/boinc/apps/native-ocr.md: upstream sources without Debian's
# patches, no GIF, no OpenMP, libjpeg-turbo without SIMD, and libtiff with only zlib and JPEG.
#
# SIMD: as on macOS, nothing is compiled for a CPU newer than the x86-64 baseline (SSE2).
# Tesseract's CMake builds its AVX, AVX2, FMA and SSE4.1 files with their own flags (-DENABLE_NATIVE
# is off, so no global /arch) and chooses among them when it runs.
#
# Tesseract gets the upstream patches of tesseract-patches.lock (#1835), each checked against its
# sha256 there, as the Linux build does. -NoPatches builds the stock 5.5.0 instead, which CI does
# to compare the two on one runner.
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$Out,
    [Parameter(Mandatory = $true)][string]$Work,
    [switch]$NoPatches
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

if (-not $IsWindows) { throw 'this builds on Windows only' }
if ($env:PROCESSOR_ARCHITECTURE -ne 'AMD64') { throw "unsupported architecture $env:PROCESSOR_ARCHITECTURE" }

$here = $PSScriptRoot
New-Item -ItemType Directory -Force -Path $Out, $Work | Out-Null
$Out = (Resolve-Path $Out).Path
$Work = (Resolve-Path $Work).Path
$src = Join-Path $Work 'src'
$prefix = Join-Path $Work 'prefix'
$builds = Join-Path $Work 'build'
foreach ($dir in $src, $prefix, $builds) {
    if (Test-Path $dir) { Remove-Item -Recurse -Force $dir }
    New-Item -ItemType Directory -Force -Path $dir | Out-Null
}
$jobs = $env:NUMBER_OF_PROCESSORS
# CMake reads forward slashes everywhere.
function Slash([string]$path) { $path.Replace('\', '/') }
$prefixSlash = Slash $prefix

# Runs a program, and stops if it fails. Its output goes to the log, or to the console without one.
function Invoke-Native([string]$log, [string]$program, [string[]]$arguments) {
    if ($log) {
        & $program @arguments *>> $log
    } else {
        & $program @arguments
    }
    if ($LASTEXITCODE -ne 0) { throw "$program $($arguments -join ' ') failed with $LASTEXITCODE" }
}

# The same generator and runtime for every library: Visual Studio's x64 compiler, and the static
# C runtime (/MT), which CMP0091 makes CMake honour in projects whose minimum version is older.
$common = @(
    '-G', 'Visual Studio 17 2022', '-A', 'x64',
    "-DCMAKE_INSTALL_PREFIX=$prefixSlash",
    '-DCMAKE_INSTALL_LIBDIR=lib',
    "-DCMAKE_PREFIX_PATH=$prefixSlash",
    '-DCMAKE_MSVC_RUNTIME_LIBRARY=MultiThreaded',
    '-DCMAKE_POLICY_DEFAULT_CMP0091=NEW',
    '-DCMAKE_FIND_USE_PACKAGE_REGISTRY=OFF',
    '-DCMAKE_FIND_USE_SYSTEM_PACKAGE_REGISTRY=OFF',
    '-DBUILD_SHARED_LIBS=OFF',
    # zlib and libjpeg-turbo declare minimums CMake 4 no longer accepts.
    '-DCMAKE_POLICY_VERSION_MINIMUM=3.5'
)

$current = ''
function Fetch([string]$name, [string]$url, [string]$tag, [string]$commit) {
    $dir = Join-Path $src $name
    Invoke-Native $null git @('-c', 'init.defaultBranch=pinned', 'init', '-q', $dir)
    Invoke-Native $null git @('-C', $dir, 'fetch', '-q', '--depth', '1', $url, "refs/tags/$tag")
    $got = (& git -C $dir rev-parse 'FETCH_HEAD^{commit}').Trim()
    if ($got -ne $commit) { throw "$name $tag is $got, the lock says $commit" }
    Invoke-Native $null git @('-C', $dir, '-c', 'advice.detachedHead=false', 'checkout', '-q', $commit)
}

# Configures, builds and installs one library, with its own CMake options. A failed step shows
# the end of its log; the logs are quiet otherwise.
function Build([string]$name, [string[]]$options) {
    $log = Join-Path $builds "$name.log"
    $script:current = $log
    $dir = Join-Path $builds $name
    Invoke-Native $log cmake (@('-S', (Join-Path $src $name), '-B', $dir) + $common + $options)
    Invoke-Native $log cmake @('--build', $dir, '--config', 'Release', '--parallel', $jobs)
    Invoke-Native $log cmake @('--install', $dir, '--config', 'Release')
}

function Show-Log {
    if ($script:current -and (Test-Path $script:current)) {
        Write-Host "---- end of $script:current"
        Get-Content $script:current -Tail 60 | Write-Host
    }
}

try {
    Get-Content (Join-Path $here 'static-sources.lock') | Where-Object { $_ -notmatch '^\s*(#|$)' } | ForEach-Object {
        $name, $url, $tag, $commit = $_ -split '\s+'
        Fetch $name $url $tag $commit
    }

    # A static build: nothing may link a library's DLL, and the names CMake's Find modules look
    # for are the static libraries'. Run after each library that installs a shared copy.
    function Use-Static([string]$from, [string]$to) {
        $lib = Join-Path $prefix 'lib'
        Copy-Item (Join-Path $lib $from) (Join-Path $lib $to) -Force
    }

    # The upstream patches, in the lock's order: every file in patches/ must be in the lock with
    # its sha256, and every line of the lock must have its file. --ignore-whitespace, since a
    # Windows git may have checked the source out with CRLF line ends.
    $patchLines = @(Get-Content (Join-Path $here 'tesseract-patches.lock') | Where-Object { $_ -notmatch '^\s*(#|$)' })
    if (-not $NoPatches) {
        $expected = ($patchLines | ForEach-Object { ($_ -split '\s+')[0] }) -join ','
        $found = (Get-ChildItem (Join-Path $here 'patches') -File | Where-Object { $_.Name -ne '.gitattributes' } |
            ForEach-Object { $_.Name } | Sort-Object { $_ } ) -join ','
        if ($expected -ne $found) { throw 'patches/ and tesseract-patches.lock list different files' }
        foreach ($line in $patchLines) {
            $file, $commit, $sum = $line -split '\s+'
            $path = Join-Path $here "patches\$file"
            if ((Get-FileHash -Algorithm SHA256 $path).Hash.ToLower() -ne $sum) {
                throw "$file does not have the sha256 in tesseract-patches.lock"
            }
            Invoke-Native $null git @('-C', (Join-Path $src 'tesseract'), 'apply', '--ignore-whitespace', $path)
        }
    }

    Build 'zlib' @('-DZLIB_BUILD_EXAMPLES=OFF')
    # zlib installs a shared copy and its import library under the plain names.
    Remove-Item -Force -ErrorAction SilentlyContinue (Join-Path $prefix 'bin\*.dll'), (Join-Path $prefix 'lib\zlib.lib')
    Use-Static 'zlibstatic.lib' 'zlib.lib'

    # libjpeg-turbo resolves a relative CMAKE_INSTALL_LIBDIR against the working directory, not the prefix.
    Build 'libjpeg-turbo' @("-DCMAKE_INSTALL_LIBDIR=$prefixSlash/lib", '-DENABLE_SHARED=OFF', '-DENABLE_STATIC=ON',
        '-DWITH_SIMD=OFF', '-DWITH_TURBOJPEG=OFF', '-DWITH_CRT_DLL=OFF')
    Use-Static 'jpeg-static.lib' 'jpeg.lib'

    Build 'libpng' @('-DPNG_SHARED=OFF', '-DPNG_STATIC=ON', '-DPNG_TESTS=OFF', '-DPNG_TOOLS=OFF', '-DPNG_FRAMEWORK=OFF')
    # With MSVC the static library is libpng16_static.lib, which FindPNG doesn't look for.
    if (Test-Path (Join-Path $prefix 'lib\libpng16_static.lib')) { Use-Static 'libpng16_static.lib' 'libpng16.lib' }

    Build 'libtiff' @('-Dtiff-tools=OFF', '-Dtiff-tests=OFF', '-Dtiff-contrib=OFF', '-Dtiff-docs=OFF', '-Dcxx=OFF',
        '-Dzlib=ON', '-Djpeg=ON', '-Djpeg12=OFF', '-Djbig=OFF', '-Dlerc=OFF', '-Dlibdeflate=OFF', '-Dlzma=OFF',
        '-Dwebp=OFF', '-Dzstd=OFF')

    Build 'openjpeg' @('-DBUILD_STATIC_LIBS=ON', '-DBUILD_CODEC=OFF', '-DBUILD_TESTING=OFF', '-DBUILD_DOC=OFF')

    Build 'libwebp' @('-DWEBP_BUILD_ANIM_UTILS=OFF', '-DWEBP_BUILD_CWEBP=OFF', '-DWEBP_BUILD_DWEBP=OFF',
        '-DWEBP_BUILD_GIF2WEBP=OFF', '-DWEBP_BUILD_IMG2WEBP=OFF', '-DWEBP_BUILD_VWEBP=OFF', '-DWEBP_BUILD_WEBPINFO=OFF',
        '-DWEBP_BUILD_WEBPMUX=OFF', '-DWEBP_BUILD_EXTRAS=OFF', '-DWEBP_BUILD_LIBWEBPMUX=ON', '-DWEBP_LINK_STATIC=ON')

    # The static libraries' installed configs don't import their own dependencies. libtiff's says
    # "TODO: import dependencies", so TIFF::tiff links JPEG::JPEG and a CMath::CMath no package
    # defines, and Leptonica's links WebP::webp, WebP::libwebpmux and ZLIB::ZLIB without loading
    # them. Leptonica and Tesseract both load this first (Tesseract's TIFF check recreates the
    # targets it finds). The maths library is part of the C runtime, so an empty imported target
    # is the right CMath.
    $staticDeps = Join-Path $Work 'static-deps.cmake'
    @'
if(NOT TARGET CMath::CMath)
  add_library(CMath::CMath INTERFACE IMPORTED)
endif()
find_package(ZLIB REQUIRED)
find_package(JPEG REQUIRED)
find_package(WebP CONFIG REQUIRED)
'@ | Set-Content -Encoding ascii $staticDeps
    $staticDepsSlash = Slash $staticDeps

    # OPJ_STATIC: without it OpenJPEG's header declares its functions as imported from a DLL.
    Build 'leptonica' @('-DSW_BUILD=OFF', '-DBUILD_PROG=OFF', '-DENABLE_GIF=OFF', '-DCMAKE_DISABLE_FIND_PACKAGE_GIF=ON',
        '-DCMAKE_C_FLAGS=/DOPJ_STATIC', '-DCMAKE_CXX_FLAGS=/DOPJ_STATIC', "-DCMAKE_PROJECT_INCLUDE=$staticDepsSlash")

    # The linker takes archives in any order, so naming every library here covers whatever
    # Leptonica's exported target leaves out. A library nothing needs adds nothing.
    $libs = (Get-ChildItem (Join-Path $prefix 'lib') -Filter '*.lib' | ForEach-Object { $_.Name }) -join ' '
    $log = Join-Path $builds 'tesseract.log'
    $script:current = $log
    $dir = Join-Path $builds 'tesseract'
    Invoke-Native $log cmake (@('-S', (Join-Path $src 'tesseract'), '-B', $dir) + $common + @(
        '-DSW_BUILD=OFF', '-DOPENMP_BUILD=OFF', '-DBUILD_TRAINING_TOOLS=OFF', '-DBUILD_TESTS=OFF',
        '-DDISABLE_ARCHIVE=ON', '-DDISABLE_CURL=ON', '-DENABLE_LTO=OFF', '-DENABLE_NATIVE=OFF', '-DINSTALL_CONFIGS=OFF',
        '-DCMAKE_C_FLAGS=/DOPJ_STATIC', '-DCMAKE_CXX_FLAGS=/DOPJ_STATIC',
        "-DCMAKE_PROJECT_INCLUDE=$staticDepsSlash",
        "-DCMAKE_EXE_LINKER_FLAGS=/LIBPATH:$prefixSlash/lib $libs"))
    Invoke-Native $log cmake @('--build', $dir, '--config', 'Release', '--parallel', $jobs, '--target', 'tesseract')
    $engine = Get-ChildItem $dir -Recurse -Filter 'tesseract.exe' | Select-Object -First 1
    if (-not $engine) { throw 'the Tesseract build made no executable' }
    Copy-Item $engine.FullName (Join-Path $Out 'usnm_tesseract.exe') -Force
} catch {
    Show-Log
    throw
}

$exe = Join-Path $Out 'usnm_tesseract.exe'

# x86-64, and nothing outside the system: every DLL the engine imports is one of Windows's own.
$loads = @(& (Join-Path $here 'check-windows-imports.ps1') -Exe $exe)
$version = (& $exe --version 2>&1 | Select-Object -First 1).ToString().Trim()
if ($version -ne 'tesseract 5.5.0') { throw "usnm_tesseract.exe reports '$version', not Tesseract 5.5.0" }

$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
$vs = (& $vswhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath | Select-Object -First 1).Trim()
$msvc = (Get-ChildItem (Join-Path $vs 'VC\Tools\MSVC') -Directory | Sort-Object Name | Select-Object -Last 1).Name
$sdk = (Get-ChildItem "${env:ProgramFiles(x86)}\Windows Kits\10\Include" -Directory | Sort-Object Name | Select-Object -Last 1).Name
$record = @()
Get-Content (Join-Path $here 'static-sources.lock') | Where-Object { $_ -notmatch '^\s*(#|$)' } | ForEach-Object {
    $name, $url, $tag, $commit = $_ -split '\s+'
    $record += "source $name $tag $commit"
}
if ($NoPatches) { $record += 'tesseract-patches none' }
else { $record += $patchLines | ForEach-Object { 'tesseract-patch ' + ($_ -replace '\s+', ' ') } }
$record += "compiler msvc $msvc"
$record += "sdk $sdk"
$record += "runtime static (/MT)"
$record += $loads | ForEach-Object { "loads $_" }
$record += (& $exe --version 2>&1 | ForEach-Object { "version $($_.ToString().Trim())" })
Set-Content -Encoding ascii (Join-Path $Out 'usnm_tesseract.sources') $record
Get-FileHash -Algorithm SHA256 $exe | ForEach-Object { "$($_.Hash.ToLower())  $($_.Path)" }
