# SPDX-License-Identifier: MIT OR Apache-2.0
<#
.SYNOPSIS
    Builds the Headphone for All Windows installer with Inno Setup.

.DESCRIPTION
    Stages the Flutter release output (app\build\windows\<arch>\runner\Release),
    adds the MSVC C++ runtime DLLs the app and hfa_ffi.dll need (app-local
    deployment, so users do not need the VC++ redistributable), and compiles
    packaging\windows\hfa.iss with ISCC. Inno Setup is installed with
    Chocolatey when ISCC.exe is not found.

    Used by CI on a windows runner:
        pwsh packaging/windows/build-installer.ps1 -Build

.PARAMETER Version
    Version shown by the installer and used in the file name: numeric (up to
    four parts) with an optional SemVer pre-release suffix, for example 1.2.3
    or 1.2.3-beta.1. The Windows version resource gets the numeric part only.
    Defaults to the "version:" of app\pubspec.yaml without its "+build" suffix.

.PARAMETER Arch
    x64 (default) or arm64: selects the Flutter build folder.

.PARAMETER SourceDir
    Flutter release folder. Defaults to app\build\windows\<Arch>\runner\Release.

.PARAMETER OutputDir
    Where the setup .exe is written. Defaults to packaging\dist.

.PARAMETER Build
    Run "flutter build windows --release" first.

.PARAMETER SkipVcRuntime
    Do not bundle the MSVC runtime DLLs.
#>
[CmdletBinding()]
param(
    [string] $Version,
    [ValidateSet('x64', 'arm64')]
    [string] $Arch = 'x64',
    [string] $SourceDir,
    [string] $OutputDir,
    [switch] $Build,
    [switch] $SkipVcRuntime
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$AppDir = Join-Path $RepoRoot 'app'
$IssFile = Join-Path $PSScriptRoot 'hfa.iss'

function Find-Iscc {
    $command = Get-Command 'iscc.exe' -ErrorAction SilentlyContinue
    if ($command) { return $command.Source }
    $candidates = @(
        (Join-Path ${env:ProgramFiles(x86)} 'Inno Setup 6\ISCC.exe'),
        (Join-Path $env:ProgramFiles 'Inno Setup 6\ISCC.exe'),
        (Join-Path $env:ProgramFiles 'Inno Setup 7\ISCC.exe'),
        (Join-Path $env:LOCALAPPDATA 'Programs\Inno Setup 6\ISCC.exe')
    )
    foreach ($candidate in $candidates) {
        if ($candidate -and (Test-Path $candidate)) { return $candidate }
    }
    return $null
}

function Get-VcRuntimeDll([string] $TargetArch) {
    $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
    if (-not (Test-Path $vswhere)) {
        throw "vswhere.exe not found; install Visual Studio (C++ workload) or pass -SkipVcRuntime"
    }
    $vs = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
    if (-not $vs) { throw 'No Visual Studio with the C++ tools found' }
    # VC\Redist\MSVC\<version>\<arch>\Microsoft.VC14x.CRT\*.dll, newest version first.
    $redist = Get-ChildItem (Join-Path $vs 'VC\Redist\MSVC') -Directory |
        Where-Object { $_.Name -match '^\d+(\.\d+)+$' } |
        Sort-Object { [version] $_.Name } -Descending |
        ForEach-Object { Get-ChildItem (Join-Path $_.FullName $TargetArch) -Directory -Filter 'Microsoft.VC*.CRT' -ErrorAction SilentlyContinue } |
        Select-Object -First 1
    if (-not $redist) { throw "No MSVC CRT redist folder for $TargetArch under $vs" }
    return Get-ChildItem $redist.FullName -Filter '*.dll'
}

if ($Build) {
    Push-Location $AppDir
    try {
        flutter build windows --release
        if ($LASTEXITCODE -ne 0) { throw "flutter build windows failed ($LASTEXITCODE)" }
    }
    finally { Pop-Location }
}

if (-not $Version) {
    $line = Select-String -Path (Join-Path $AppDir 'pubspec.yaml') -Pattern '^version:\s*([^+\s]+)' | Select-Object -First 1
    if (-not $line) { throw 'Cannot read the version from app\pubspec.yaml; pass -Version' }
    $Version = $line.Matches[0].Groups[1].Value
}
if ($Version -match '^(\d+(?:\.\d+){0,3})(-[0-9A-Za-z.-]+)?$') {
    # Windows version resources take up to four numbers only.
    $NumericVersion = $Matches[1]
}
else {
    throw "Version '$Version' must be numeric with an optional pre-release suffix (for example 1.2.3 or 1.2.3-beta.1)"
}

if (-not $SourceDir) { $SourceDir = Join-Path $AppDir "build\windows\$Arch\runner\Release" }
if (-not $OutputDir) { $OutputDir = Join-Path $RepoRoot 'packaging\dist' }
foreach ($required in @('headphone_for_all.exe', 'hfa_ffi.dll', 'flutter_windows.dll', 'data\app.so')) {
    if (-not (Test-Path (Join-Path $SourceDir $required))) {
        throw "$required is missing in $SourceDir; run 'flutter build windows --release' (or pass -Build)"
    }
}

$staging = Join-Path ([IO.Path]::GetTempPath()) ("hfa-installer-" + [guid]::NewGuid())
New-Item -ItemType Directory -Path $staging | Out-Null
try {
    Copy-Item -Path (Join-Path $SourceDir '*') -Destination $staging -Recurse
    if (-not $SkipVcRuntime) {
        foreach ($dll in Get-VcRuntimeDll $Arch) {
            Copy-Item $dll.FullName -Destination $staging
            Write-Information "bundled $($dll.Name)" -InformationAction Continue
        }
    }

    $iscc = Find-Iscc
    if (-not $iscc) {
        Write-Information 'Inno Setup not found; installing it with Chocolatey' -InformationAction Continue
        choco install innosetup --yes --no-progress
        if ($LASTEXITCODE -ne 0) { throw "choco install innosetup failed ($LASTEXITCODE)" }
        $iscc = Find-Iscc
        if (-not $iscc) { throw 'ISCC.exe still not found after installing Inno Setup' }
    }

    New-Item -ItemType Directory -Path $OutputDir -Force | Out-Null
    & $iscc "/DAppVersion=$Version" "/DAppVersionNumeric=$NumericVersion" "/DAppArch=$Arch" "/DSourceDir=$staging" "/DOutputDir=$OutputDir" $IssFile
    if ($LASTEXITCODE -ne 0) { throw "ISCC failed ($LASTEXITCODE)" }
    Write-Information "wrote $(Join-Path $OutputDir "Headphone_for_All-$Version-windows-$Arch-setup.exe")" -InformationAction Continue
}
finally {
    Remove-Item -Recurse -Force $staging -ErrorAction SilentlyContinue
}
