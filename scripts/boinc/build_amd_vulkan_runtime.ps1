# Requires MSVC, CMake and an installed Vulkan SDK (as in upstream b8799).
param(
    [Parameter(Mandatory = $true)][string]$Source,
    [Parameter(Mandatory = $true)][string]$Build,
    [Parameter(Mandatory = $true)][string]$Output
)
$ErrorActionPreference = 'Stop'
$pin = 'e1a9a6dcbefccb4b864d9385ce8494f2a7f2ffcd'
$head = & git -C $Source rev-parse HEAD
if ($LASTEXITCODE -ne 0 -or $head -ne $pin) { throw 'Expected pinned llama.cpp b8799 source checkout' }
$dirty = & git -C $Source status --porcelain
if ($LASTEXITCODE -ne 0 -or $dirty) { throw 'Expected clean pinned llama.cpp source checkout' }
if (!$env:VULKAN_SDK) { throw 'Install the Vulkan SDK and set VULKAN_SDK first' }
if ((Test-Path $Build) -or (Test-Path $Output)) { throw 'Use new build and output directories' }
$patch = Join-Path $PSScriptRoot 'patch_amd_vulkan_identity.py'
& python $patch --source (Join-Path $Source 'ggml/src/ggml-vulkan/ggml-vulkan.cpp')
if ($LASTEXITCODE -ne 0) { throw 'Pinned identity patch failed' }
& cmake -S $Source -B $Build -A x64 -DGGML_VULKAN=ON -DBUILD_SHARED_LIBS=ON -DLLAMA_BUILD_TESTS=OFF -DLLAMA_BUILD_EXAMPLES=OFF -DLLAMA_BUILD_SERVER=OFF
if ($LASTEXITCODE -ne 0) { throw 'Vulkan runtime configuration failed' }
& cmake --build $Build --config Release --target ggml-vulkan --parallel 2
if ($LASTEXITCODE -ne 0) { throw 'Vulkan runtime build failed' }
New-Item -ItemType Directory -Path $Output -ErrorAction Stop | Out-Null
$dll = Join-Path $Output 'ggml-vulkan.dll'
Copy-Item (Join-Path $Build 'bin/Release/ggml-vulkan.dll') $dll
$receipt = @{
    schema_version = 1
    source_commit = $pin
    patch_sha256 = (Get-FileHash $patch -Algorithm SHA256).Hash.ToLowerInvariant()
    runtime_sha256 = (Get-FileHash $dll -Algorithm SHA256).Hash.ToLowerInvariant()
    vulkan_sdk = $env:VULKAN_SDK
}
$receipt | ConvertTo-Json | Set-Content (Join-Path $Output 'AMD_RUNTIME_BUILD.json') -Encoding ascii
