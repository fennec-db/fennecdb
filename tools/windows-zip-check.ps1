# The Windows release's zip, installed and run before it is kept, as
# tools/linux-packages-check.sh holds a .deb and an .rpm: release.yml runs it
# on the zip it is about to upload, and CI's `windows (release zip)` job on
# the zip tools/windows-zip.sh made there.
#
#   pwsh tools/windows-zip-check.ps1 -Version 0.1.13 -Zip dist\fennec-v0.1.13-x86_64-pc-windows-msvc.zip
#
# What it holds: the zip is the folder of its name and the five files the
# tarballs hold; neither program imports the Visual C++ runtime
# (vcruntime140.dll, msvcp140.dll, the UCRT's api-ms-win-crt-*.dll), which a
# fresh Windows may lack; site/install.ps1, run by Windows PowerShell 5.1 and
# by PowerShell 7, installs it from the folder it is in, checked against a
# SHA256SUMS naming it as the release's will; and each installed program
# says its version, and the shell writes a row into a file and reads it back
# in another process. Every assertion prints what it found and what it
# wanted. Nothing is downloaded.

param(
    [Parameter(Mandatory = $true)] [string] $Version,
    [Parameter(Mandatory = $true)] [string] $Zip
)

Set-StrictMode -Version 2.0
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$script:failed = 0
function Pass([string] $what) { Write-Host "ok    $what" }
function Fail([string] $what) {
    Write-Host "FAIL  $what"
    $script:failed++
}
function Expect([string] $what, [string] $found, [string] $wanted) {
    if ($found -ceq $wanted) { Pass "$what`: $found" } else { Fail "$what`: found [$found], wanted [$wanted]" }
}

# The DLLs a program imports, from its import directory: the names a fresh
# Windows must hold for it to start. A PE32+ file's optional header has its
# data directories 112 bytes in, the imports the second.
function Get-Imports([string] $path) {
    $b = [IO.File]::ReadAllBytes($path)
    $pe = [BitConverter]::ToInt32($b, 0x3C)
    if ([BitConverter]::ToUInt32($b, $pe) -ne 0x00004550) { throw "$path is not a PE file" }
    $coff = $pe + 4
    $sections = [BitConverter]::ToUInt16($b, $coff + 2)
    $optional = $coff + 20
    $table = $optional + [BitConverter]::ToUInt16($b, $coff + 16)
    if ([BitConverter]::ToUInt16($b, $optional) -ne 0x20B) { throw "$path is not a 64-bit program" }
    $rva = [BitConverter]::ToUInt32($b, $optional + 112 + 8)
    $offset = {
        param([uint32] $at)
        for ($i = 0; $i -lt $sections; $i++) {
            $s = $table + 40 * $i
            $va = [BitConverter]::ToUInt32($b, $s + 12)
            $size = [Math]::Max([BitConverter]::ToUInt32($b, $s + 8), [BitConverter]::ToUInt32($b, $s + 16))
            if ($at -ge $va -and $at -lt $va + $size) { return [int]($at - $va + [BitConverter]::ToUInt32($b, $s + 20)) }
        }
        throw "$path`: the address $at is in no section"
    }
    $names = @()
    if ($rva -eq 0) { return , $names }
    $d = & $offset $rva
    while ($true) {
        $nameRva = [BitConverter]::ToUInt32($b, $d + 12)
        if ($nameRva -eq 0) { break }
        $n = & $offset $nameRva
        $end = [Array]::IndexOf($b, [byte]0, $n)
        $names += [Text.Encoding]::ASCII.GetString($b, $n, $end - $n)
        $d += 20
    }
    return , $names
}

$Zip = (Resolve-Path -LiteralPath $Zip).Path
$name = Split-Path -Leaf $Zip
if (-not ($name -match "^fennec-v$([regex]::Escape($Version))-(.+-pc-windows-msvc)\.zip$")) {
    throw "$name is not a Windows zip of release $Version"
}
$target = $Matches[1]
$folder = "fennec-v$Version-$target"

$work = Join-Path ([IO.Path]::GetTempPath()) ('fennec-zip-check-' + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $work | Out-Null
try {
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $archive = [IO.Compression.ZipFile]::OpenRead($Zip)
    try {
        $files = @($archive.Entries | Where-Object { -not $_.FullName.EndsWith('/') } | ForEach-Object { $_.FullName } | Sort-Object)
    } finally {
        $archive.Dispose()
    }
    $wanted = @('CONTRIBUTING.md', 'LICENSE', 'README.md', 'fennec-server.exe', 'fennec.exe' | ForEach-Object { "$folder/$_" } | Sort-Object)
    Expect "$name holds" ($files -join ' ') ($wanted -join ' ')

    $unpacked = Join-Path $work 'unpacked'
    [IO.Compression.ZipFile]::ExtractToDirectory($Zip, $unpacked)
    foreach ($b in @('fennec.exe', 'fennec-server.exe')) {
        $imports = @(Get-Imports (Join-Path $unpacked "$folder\$b") | Sort-Object)
        Write-Host "      $b imports $($imports -join ', ')"
        $runtime = @($imports | Where-Object { $_ -match '^(vcruntime|msvcp|ucrtbase|api-ms-win-crt-)' })
        Expect "$b imports from the Visual C++ runtime" ($runtime -join ', ') ''
    }

    # The release as install.ps1 finds it in a folder: the zip, and a
    # SHA256SUMS naming it as publish's `sha256sum` writes it, a tarball's
    # line before it.
    $from = Join-Path $work 'from'
    New-Item -ItemType Directory -Path $from | Out-Null
    Copy-Item -LiteralPath $Zip -Destination $from
    $sum = (Get-FileHash -LiteralPath $Zip -Algorithm SHA256).Hash.ToLowerInvariant()
    [IO.File]::WriteAllText((Join-Path $from 'SHA256SUMS'), "$('0' * 64)  fennec-v$Version-x86_64-unknown-linux-musl.tar.gz`n$sum  $name`n")

    $installer = Join-Path (Split-Path -Parent $PSScriptRoot) 'site\install.ps1'
    $shells = @()
    foreach ($s in @('powershell', 'pwsh')) {
        $found = Get-Command $s -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
        if ($found) { $shells += $found.Path }
    }
    Expect 'Windows PowerShell and PowerShell 7 on the PATH' ([string]$shells.Count) '2'

    foreach ($shell in $shells) {
        $tag = [IO.Path]::GetFileNameWithoutExtension($shell)
        $inst = Join-Path $work "inst-$tag"
        $env:FENNEC_FROM = $from
        $env:FENNEC_INSTALL = $inst
        # One run finds the version in SHA256SUMS, the other is told it.
        if ($tag -eq 'pwsh') { $env:FENNEC_VERSION = $Version } else { $env:FENNEC_VERSION = $null }
        $out = Join-Path $work "install-$tag.out"
        $p = Start-Process -FilePath $shell -ArgumentList '-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-File', "`"$installer`"" `
            -NoNewWindow -PassThru -RedirectStandardOutput $out -RedirectStandardError "$out.err"
        $null = $p.Handle
        $p.WaitForExit()
        $said = [IO.File]::ReadAllText($out) + [IO.File]::ReadAllText("$out.err")
        Write-Host ($said.TrimEnd() -replace '(?m)^', "      $tag| ")
        Expect "install.ps1 under $tag, its exit" ([string]$p.ExitCode) '0'
        Expect "install.ps1 under $tag checked the sum" ([string]$said.Contains("Checksum ok ($sum)")) 'True'
        Remove-Item Env:FENNEC_FROM, Env:FENNEC_INSTALL -ErrorAction SilentlyContinue

        $bin = Join-Path $inst 'bin'
        Expect "$tag`: fennec-server.exe --version" ([string](@(& (Join-Path $bin 'fennec-server.exe') --version) -join "`n")) "fennec-server $Version"
        Expect "$tag`: fennec.exe --version" ([string](@(& (Join-Path $bin 'fennec.exe') --version) -join "`n")) "fennec $Version"

        # A row written into a file by one shell and read by another: the
        # file made, mapped and locked by the programs the zip holds.
        $db = Join-Path $work "$tag.fennec"
        $wrote = @("create collection t (n int)`nput t {id: 1, n: 7}" | & (Join-Path $bin 'fennec.exe') $db) -join "`n"
        Expect "$tag`: fennec.exe wrote, its exit" ([string]$LASTEXITCODE) '0'
        $read = @("get t" | & (Join-Path $bin 'fennec.exe') $db) -join "`n"
        Expect "$tag`: fennec.exe read back, its exit" ([string]$LASTEXITCODE) '0'
        $row = @($read -split "`r?`n" | Where-Object { $_ -match '^1\s+7\s*$' }).Count
        Expect "$tag`: the row read back (wrote [$wrote], read [$read])" ([string]$row) '1'
    }
} finally {
    Remove-Item Env:FENNEC_FROM, Env:FENNEC_INSTALL, Env:FENNEC_VERSION -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $work -Recurse -Force -ErrorAction SilentlyContinue
}

if ($script:failed -ne 0) {
    Write-Host "$name`: $($script:failed) FAILED"
    exit 1
}
Write-Host "$name`: installed, run and kept"
