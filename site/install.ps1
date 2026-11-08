# Install fennecdb on Windows: `fennec.exe`, the shell, and
# `fennec-server.exe`, from a release.
#
#   irm https://fennecdb.com/install.ps1 | iex
#
# Read it first: open https://fennecdb.com/install.ps1 in a browser. It
#
#   - takes the release's zip for this machine: x86_64, which an arm64
#     Windows 11 runs too;
#   - downloads the zip and the release's SHA256SUMS from GitHub, over https,
#     and checks the zip against the sum before it unpacks it;
#   - puts fennec.exe and fennec-server.exe in a bin folder of your own: no
#     administrator, and no profile and no PATH is touched. It says how to
#     put the folder on your PATH, and that is the only thing left to you.
#
# Settings, all optional, as environment variables set before the line
# above ($env:FENNEC_VERSION = '0.1.13'):
#
#   FENNEC_VERSION  X.Y.Z (or vX.Y.Z); the latest release when unset
#   FENNEC_INSTALL  where it goes: $env:FENNEC_INSTALL\bin; by default
#                   $env:LOCALAPPDATA\fennecdb
#   FENNEC_BASE_URL where releases are fetched from, for a mirror or a test:
#                   https://github.com/fennec-db/fennecdb/releases by default,
#                   and it must be https (http only for localhost)
#   FENNEC_FROM     a folder holding a release's zip and its SHA256SUMS,
#                   installed from there with nothing downloaded: a release
#                   checked before it is published, or one carried to a
#                   machine off the network
#
# The checksums come from the same release as the zip: they catch a damaged
# or cut-short download, not a release someone replaced. The programs are not
# signed: SmartScreen warns of them where a browser downloaded them, and a
# file this script checked carries no download mark.
#
# The whole script is functions, called on the last line: a download cut
# short stops before that line and runs nothing. Run through `iex`, it runs
# in your session, so it changes none of the session's settings: its own are
# set inside its functions, and the one process-wide one, the TLS versions
# Windows PowerShell 5.1 may use, is put back as it was.

function Write-FennecUsage {
    Write-Host 'Install fennecdb (fennec.exe, the shell, and fennec-server.exe) from a release.'
    Write-Host ''
    Write-Host '    irm https://fennecdb.com/install.ps1 | iex'
    Write-Host ''
    Write-Host '  FENNEC_VERSION  X.Y.Z; the latest release when unset'
    Write-Host '  FENNEC_INSTALL  installs into $env:FENNEC_INSTALL\bin; default $env:LOCALAPPDATA\fennecdb'
    Write-Host '  FENNEC_FROM     a folder holding a release''s zip and SHA256SUMS, nothing downloaded'
    Write-Host ''
    Write-Host 'Windows 10 or Windows Server 2016 and later, on x86_64, and on arm64 under'
    Write-Host 'Windows 11''s emulation. On macOS and Linux: curl -fsSL https://fennecdb.com/install.sh | sh'
}

# The machine's processor, not this process's: an x64 PowerShell on an arm64
# Windows runs emulated and says AMD64 in its own environment, where the
# system's names the machine's.
function Get-FennecArch {
    $arch = $null
    try {
        $arch = (Get-ItemProperty -LiteralPath 'HKLM:\SYSTEM\CurrentControlSet\Control\Session Manager\Environment' -Name PROCESSOR_ARCHITECTURE).PROCESSOR_ARCHITECTURE
    } catch {
        $arch = $null
    }
    if (-not $arch) { $arch = $env:PROCESSOR_ARCHITEW6432 }
    if (-not $arch) { $arch = $env:PROCESSOR_ARCHITECTURE }
    switch ($arch) {
        'AMD64' { return 'x86_64' }
        'ARM64' { return 'aarch64' }
    }
    throw "no fennecdb release for the processor '$arch' (x86_64 only, which an arm64 Windows 11 runs too). The Docker image, ghcr.io/fennec-db/fennec-server, is another way."
}

# The base URL releases come from: https, or http to this machine for a test.
# The host is read up to its port, and a user or a password (an `@`) is
# refused, since http://localhost:x@example.com is a request to example.com.
function Get-FennecBase {
    $base = $env:FENNEC_BASE_URL
    if (-not $base) { $base = 'https://github.com/fennec-db/fennecdb/releases' }
    $base = $base.TrimEnd('/')
    if ($base -match '[@\s]') { throw "FENNEC_BASE_URL holds a user, a password or a space: $base" }
    if ($base -cmatch '^https://.') { return $base }
    if ($base -cmatch '^http://(localhost|127\.0\.0\.1)(:[0-9]+)?(/|$)') { return $base }
    throw "FENNEC_BASE_URL must be https (http only for localhost): $base"
}

# Get-FennecFile URL FILE. Over https it is held to TLS 1.2 (or 1.3, where
# PowerShell 7 has it), and a redirect that ends anywhere but https is
# refused, its bytes removed: Windows PowerShell 5.1 follows a redirect from
# https to http, which PowerShell 7 refuses.
function Get-FennecFile([string] $url, [string] $out) {
    $ask = @{ Uri = $url; OutFile = $out; UseBasicParsing = $true; PassThru = $true }
    if ($PSVersionTable.PSVersion.Major -ge 6 -and $url.StartsWith('https://')) {
        $versions = @('Tls12')
        if ([enum]::GetNames([Microsoft.PowerShell.Commands.WebSslProtocol]) -contains 'Tls13') { $versions += 'Tls13' }
        $ask['SslProtocol'] = $versions
    }
    $answer = Invoke-WebRequest @ask
    $at = $null
    $response = $answer.BaseResponse
    if ($response.PSObject.Properties['ResponseUri']) {
        $at = $response.ResponseUri
    } elseif ($response.PSObject.Properties['RequestMessage'] -and $response.RequestMessage) {
        $at = $response.RequestMessage.RequestUri
    }
    if ($url.StartsWith('https://') -and $at -and $at.Scheme -ne 'https') {
        Remove-Item -LiteralPath $out -Force -ErrorAction SilentlyContinue
        throw "$url was redirected to $at, which is not https: nothing was installed"
    }
}

# The sum SHA256SUMS gives a file, as `sha256sum` writes it (a `*` before the
# name is binary mode), or nothing.
function Get-FennecSum([string[]] $sums, [string] $name) {
    foreach ($line in $sums) {
        if ($line -cmatch '^([0-9a-f]{64}) [ *](\S+)\s*$' -and $Matches[2] -ceq $name) { return $Matches[1] }
    }
    return $null
}

# The version of the newest zip SHA256SUMS lists for the target, or nothing.
function Get-FennecListed([string[]] $sums, [string] $target) {
    foreach ($line in $sums) {
        if ($line -cmatch "^[0-9a-f]{64} [ *]fennec-v(\S+)-$([regex]::Escape($target))\.zip\s*$") { return $Matches[1] }
    }
    return $null
}

# Where the programs go: $env:FENNEC_INSTALL\bin, else
# $env:LOCALAPPDATA\fennecdb\bin, made absolute against this session's
# folder, so the PATH line it prints is good from any other.
function Get-FennecBin {
    $root = $env:FENNEC_INSTALL
    if (-not $root) {
        if (-not $env:LOCALAPPDATA) { throw 'LOCALAPPDATA is not set: say where to install with FENNEC_INSTALL' }
        $root = Join-Path $env:LOCALAPPDATA 'fennecdb'
    }
    $root = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($root)
    return (Join-Path $root.TrimEnd('\') 'bin')
}

# Each program written beside its place and renamed into it. A running
# program's file cannot be written over or deleted, but it can be renamed, so
# one there already is first renamed aside, and removed once nothing runs it:
# now, or at the next install.
function Install-FennecFiles([string] $src, [string] $bindir) {
    New-Item -ItemType Directory -Force -Path $bindir -ErrorAction SilentlyContinue | Out-Null
    if (-not (Test-Path -LiteralPath $bindir -PathType Container)) {
        throw "could not make $bindir`: say where to install with FENNEC_INSTALL"
    }
    $kept = @()
    foreach ($b in @('fennec.exe', 'fennec-server.exe')) {
        foreach ($left in @(Get-ChildItem -LiteralPath $bindir -Force -File -Filter ".$b.old.*")) {
            Remove-Item -LiteralPath $left.FullName -Force -ErrorAction SilentlyContinue
        }
        $dest = Join-Path $bindir $b
        $staged = Join-Path $bindir ".$b.new.$PID"
        try {
            Copy-Item -LiteralPath (Join-Path $src $b) -Destination $staged -Force
            if (Test-Path -LiteralPath $dest) {
                $old = Join-Path $bindir (".$b.old." + [Guid]::NewGuid().ToString('N').Substring(0, 8))
                Move-Item -LiteralPath $dest -Destination $old -Force
                try {
                    Move-Item -LiteralPath $staged -Destination $dest
                } catch {
                    Move-Item -LiteralPath $old -Destination $dest -Force
                    throw
                }
                try {
                    Remove-Item -LiteralPath $old -Force
                } catch {
                    $kept += $old
                }
            } else {
                Move-Item -LiteralPath $staged -Destination $dest
            }
        } catch {
            Remove-Item -LiteralPath $staged -Force -ErrorAction SilentlyContinue
            throw "could not write $dest`: $($_.Exception.Message)"
        }
    }
    return , $kept
}

function Test-FennecOnPath([string] $dir, [string] $path) {
    if (-not $path) { return $false }
    foreach ($p in $path.Split(';')) {
        if ($p -and $p.Trim().TrimEnd('\') -eq $dir.TrimEnd('\')) { return $true }
    }
    return $false
}

function Invoke-FennecInstall([object[]] $arguments, [string] $tmp) {
    foreach ($a in $arguments) {
        if (@('-h', '--help', '-Help', '/?') -contains $a) {
            Write-FennecUsage
            return
        }
        Write-FennecUsage
        throw "unknown argument '$a'"
    }

    if ($PSVersionTable.PSVersion.Major -ge 6 -and -not $IsWindows) {
        throw 'this installer is for Windows; on macOS and Linux: curl -fsSL https://fennecdb.com/install.sh | sh'
    }
    if (-not [Environment]::Is64BitOperatingSystem) {
        throw 'no fennecdb release for 32-bit Windows (x86_64 only)'
    }

    $from = $env:FENNEC_FROM
    if ($from -and $env:FENNEC_BASE_URL) {
        throw 'FENNEC_FROM and FENNEC_BASE_URL each name where the release is: set one'
    }
    if ($from) {
        $from = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($from)
        if (-not (Test-Path -LiteralPath (Join-Path $from 'SHA256SUMS') -PathType Leaf)) {
            throw "FENNEC_FROM names $from, which holds no SHA256SUMS"
        }
    } else {
        $base = Get-FennecBase
    }
    $bindir = Get-FennecBin

    # x86_64 where the release has no build for an arm64 machine: Windows 11
    # runs it under its emulation.
    $arch = Get-FennecArch
    $targets = @("$arch-pc-windows-msvc")
    if ($arch -ne 'x86_64') { $targets += 'x86_64-pc-windows-msvc' }

    $sumsFile = Join-Path $tmp 'SHA256SUMS'
    $version = $null
    if ($env:FENNEC_VERSION) {
        $version = $env:FENNEC_VERSION
        if ($version.StartsWith('v')) { $version = $version.Substring(1) }
        if ($version -cnotmatch '^[0-9A-Za-z.+-]+$') {
            throw "FENNEC_VERSION '$($env:FENNEC_VERSION)' is not a version like 0.1.13"
        }
        Write-Host "Looking for fennecdb $version"
        if ($from) {
            Copy-Item -LiteralPath (Join-Path $from 'SHA256SUMS') -Destination $sumsFile
        } else {
            try {
                Get-FennecFile "$base/download/v$version/SHA256SUMS" $sumsFile
            } catch {
                throw "could not download $base/download/v$version/SHA256SUMS: is there a release v$version? ($($_.Exception.Message))"
            }
        }
    } else {
        Write-Host 'Looking for the latest fennecdb release'
        if ($from) {
            Copy-Item -LiteralPath (Join-Path $from 'SHA256SUMS') -Destination $sumsFile
        } else {
            try {
                Get-FennecFile "$base/latest/download/SHA256SUMS" $sumsFile
            } catch {
                throw "could not download $base/latest/download/SHA256SUMS ($($_.Exception.Message))"
            }
        }
    }
    $sums = @(Get-Content -LiteralPath $sumsFile)

    $target = $null
    $name = $null
    $want = $null
    foreach ($t in $targets) {
        $v = $version
        if (-not $v) { $v = Get-FennecListed $sums $t }
        if (-not $v -or $v -cnotmatch '^[0-9A-Za-z.+-]+$') { continue }
        $n = "fennec-v$v-$t.zip"
        $s = Get-FennecSum $sums $n
        if ($s) {
            $version = $v
            $target = $t
            $name = $n
            $want = $s
            break
        }
    }
    if (-not $want) {
        if ($version) { throw "release v$version lists no fennec-v$version-$($targets[0]).zip: it has no build for this machine" }
        throw "the latest release lists no build for $($targets[0])"
    }
    if ($target -ne $targets[0]) {
        Write-Host "No build for $($targets[0]) in v$version`: taking $target, which Windows 11 runs under its emulation"
    }

    $zip = Join-Path $tmp $name
    if ($from) {
        Write-Host "Taking $name from $from"
        $source = Join-Path $from $name
        if (-not (Test-Path -LiteralPath $source -PathType Leaf)) { throw "$from holds no $name" }
        Copy-Item -LiteralPath $source -Destination $zip
    } else {
        Write-Host "Downloading $name"
        try {
            Get-FennecFile "$base/download/v$version/$name" $zip
        } catch {
            throw "could not download $base/download/v$version/$name ($($_.Exception.Message))"
        }
    }

    $got = (Get-FileHash -LiteralPath $zip -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($got -ne $want) {
        throw "the checksum of $name is $got, SHA256SUMS says $want`: nothing was installed"
    }
    Write-Host "Checksum ok ($want)"
    # The mark of the web, where the download carries one (a copy of a zip a
    # browser saved does): the zip is the release's, by its sum, and an
    # unpacked program would carry the mark on.
    Unblock-File -LiteralPath $zip

    $unpacked = Join-Path $tmp 'unpacked'
    try {
        Expand-Archive -LiteralPath $zip -DestinationPath $unpacked -Force
    } catch {
        throw "could not unpack $name ($($_.Exception.Message))"
    }
    $src = Join-Path $unpacked "fennec-v$version-$target"
    foreach ($b in @('fennec.exe', 'fennec-server.exe')) {
        if (-not (Test-Path -LiteralPath (Join-Path $src $b) -PathType Leaf)) { throw "$name holds no $b" }
    }

    $kept = Install-FennecFiles $src $bindir

    Write-Host ''
    Write-Host "Installed fennecdb $version ($target) in $bindir`:"
    Write-Host '  fennec.exe         the shell:  fennec scratch.fennec'
    Write-Host '  fennec-server.exe  the server: fennec-server --file notes.fennec'

    # Does it start? It says its version and nothing else.
    $line = $null
    try {
        $line = @(& (Join-Path $bindir 'fennec-server.exe') --version) | Select-Object -First 1
    } catch {
        $line = $null
    }
    if ($line) {
        Write-Host "  ($line)"
    } else {
        $Host.UI.WriteErrorLine("fennecdb install: installed, but $bindir\fennec-server.exe did not start on this machine")
    }
    foreach ($old in $kept) {
        Write-Host ''
        Write-Host "The one running keeps its old file, $old, until it stops; the next install removes it."
    }

    if (-not (Test-FennecOnPath $bindir $env:Path)) {
        # Written as typed: under LOCALAPPDATA by its name, anywhere else
        # quoted as it is, a quote in it doubled.
        $add = '$env:Path = ''' + $bindir.Replace("'", "''") + ';'' + $env:Path'
        $local = $null
        if ($env:LOCALAPPDATA) { $local = $env:LOCALAPPDATA.TrimEnd('\') + '\' }
        if ($local -and $bindir.StartsWith($local, [StringComparison]::OrdinalIgnoreCase) -and $bindir.IndexOfAny([char[]]'"`$') -lt 0) {
            $add = '$env:Path = "$env:LOCALAPPDATA\' + $bindir.Substring($local.Length) + ';$env:Path"'
        }
        Write-Host ''
        Write-Host "$bindir is not on your PATH. Add it for this session:"
        Write-Host ''
        Write-Host "  $add"
        Write-Host ''
        if (Test-FennecOnPath $bindir ([Environment]::GetEnvironmentVariable('Path', 'User'))) {
            Write-Host 'It is on your user''s Path already, which a new window reads.'
        } else {
            Write-Host 'and to keep it, to your user''s Path, not the machine''s: Start, "Edit'
            Write-Host 'environment variables for your account", Path, Edit, New, and the folder above.'
        }
    }
    $other = Get-Command fennec -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($other -and $other.Path -ne (Join-Path $bindir 'fennec.exe')) {
        Write-Host ''
        Write-Host "Note: 'fennec' on your PATH is $($other.Path), not this one."
    }

    Write-Host ''
    Write-Host 'A first query: https://fennecdb.com/docs/quickstart'
}

# The entry: settings that are this run's alone, a folder of its own for the
# downloads, removed whatever happens, and a failure said in one line. Run
# from a file it exits 1 on a failure; through `iex` it throws, which ends
# the line it was run from and not the session.
function Install-Fennec {
    Set-StrictMode -Version 2.0
    $ErrorActionPreference = 'Stop'
    $ProgressPreference = 'SilentlyContinue'
    $protocols = [Net.ServicePointManager]::SecurityProtocol
    $tmp = Join-Path ([IO.Path]::GetTempPath()) ('fennecdb-install-' + [Guid]::NewGuid().ToString('N'))
    $failed = $null
    try {
        if ($PSVersionTable.PSVersion.Major -lt 6) {
            [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
        }
        New-Item -ItemType Directory -Path $tmp | Out-Null
        Invoke-FennecInstall $args $tmp
    } catch {
        $failed = $_.Exception.Message
    } finally {
        [Net.ServicePointManager]::SecurityProtocol = $protocols
        Remove-Item -LiteralPath $tmp -Recurse -Force -ErrorAction SilentlyContinue
    }
    if ($null -ne $failed) {
        if ($MyInvocation.ScriptName) {
            $Host.UI.WriteErrorLine("fennecdb install: $failed")
            exit 1
        }
        throw "fennecdb install: $failed"
    }
}

Install-Fennec @args
