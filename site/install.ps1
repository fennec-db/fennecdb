# Install fennecdb on Windows: `fennec.exe`, the shell, and
# `fennec-server.exe`, from a release.
#
#   irm https://fennecdb.com/install.ps1 | iex
#
# Read it first: open https://fennecdb.com/install.ps1 in a browser. It
#
#   - takes the release's zip for this machine, x86_64 or arm64 (an arm64
#     Windows 11 runs the x86_64 one where a release has no other);
#   - downloads the zip and the release's SHA256SUMS from GitHub, over https,
#     and checks the zip against the sum before it unpacks it;
#   - puts fennec.exe and fennec-server.exe in a bin folder of your own: no
#     administrator, and no profile and no PATH is touched. It says how to
#     put the folder on your PATH, and that is the only thing left to you.
#
# Or the fennec studio app (beta) instead, x64 or arm64, with -App:
#
#   & ([scriptblock]::Create((irm https://fennecdb.com/install.ps1))) -App
#
# It downloads the release's zip of the app for this machine, checks it the
# same way before it unpacks it, puts the app in
# $env:LOCALAPPDATA\dev.fennecdb.studio\stable\app (where the app's own
# installer would) and a shortcut to it in the Start menu: no administrator,
# no PATH, and nothing it downloaded is run.
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
    Write-Host '  -App            the fennec studio app (beta) instead, into'
    Write-Host '                  $env:LOCALAPPDATA\dev.fennecdb.studio, with a Start-menu shortcut:'
    Write-Host '                  & ([scriptblock]::Create((irm https://fennecdb.com/install.ps1))) -App'
    Write-Host ''
    Write-Host 'Windows 10 or Windows Server 2016 and later, on x86_64 or arm64.'
    Write-Host 'On macOS and Linux: curl -fsSL https://fennecdb.com/install.sh | sh'
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
    throw "no fennecdb release for the processor '$arch' (x86_64 and arm64 only). The Docker image, ghcr.io/fennec-db/fennec-server, is another way."
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

# The version of the release SHA256SUMS belongs to, from any file it lists,
# or nothing: a release lists all its files, a tarball's among a zip's.
function Get-FennecRelease([string[]] $sums) {
    foreach ($line in $sums) {
        if ($line -cmatch '^[0-9a-f]{64} [ *]fennec-v([0-9A-Za-z.+-]+?)-(x86_64|aarch64)-\S+\s*$') { return $Matches[1] }
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
# now, or at the next install. Both programs are copied before either is
# replaced, and one that cannot be put in its place puts back the one before
# it, so a failure leaves the two programs the versions they were -- or says,
# exactly, which one it could not put back.
function Install-FennecFiles([string] $src, [string] $bindir) {
    New-Item -ItemType Directory -Force -Path $bindir -ErrorAction SilentlyContinue | Out-Null
    if (-not (Test-Path -LiteralPath $bindir -PathType Container)) {
        throw "could not make $bindir`: say where to install with FENNEC_INSTALL"
    }
    $names = @('fennec.exe', 'fennec-server.exe')
    $staged = @{}
    foreach ($b in $names) {
        foreach ($left in @(Get-ChildItem -LiteralPath $bindir -Force -File -Filter ".$b.old.*")) {
            Remove-Item -LiteralPath $left.FullName -Force -ErrorAction SilentlyContinue
        }
        $staged[$b] = Join-Path $bindir ".$b.new.$PID"
    }
    $done = @()
    $at = $null
    try {
        foreach ($b in $names) {
            $at = $b
            Copy-Item -LiteralPath (Join-Path $src $b) -Destination $staged[$b] -Force
        }
        foreach ($b in $names) {
            $at = $b
            $dest = Join-Path $bindir $b
            $old = $null
            if (Test-Path -LiteralPath $dest) {
                $old = Join-Path $bindir (".$b.old." + [Guid]::NewGuid().ToString('N').Substring(0, 8))
                Move-Item -LiteralPath $dest -Destination $old -Force
            }
            try {
                Move-Item -LiteralPath $staged[$b] -Destination $dest
            } catch {
                if ($old) { Move-Item -LiteralPath $old -Destination $dest -Force }
                throw
            }
            $done += @{ Name = $b; Dest = $dest; Old = $old }
        }
    } catch {
        $said = $_.Exception.Message
        foreach ($b in $names) { Remove-Item -LiteralPath $staged[$b] -Force -ErrorAction SilentlyContinue }
        $back = @()
        $stuck = @()
        for ($i = $done.Count - 1; $i -ge 0; $i--) {
            try {
                Remove-Item -LiteralPath $done[$i].Dest -Force
                if ($done[$i].Old) { Move-Item -LiteralPath $done[$i].Old -Destination $done[$i].Dest -Force }
                $back += $done[$i].Name
            } catch {
                $stuck += $done[$i].Name
            }
        }
        $tail = ''
        if ($back.Count) { $tail += " ($($back -join ', ') is back as it was)" }
        if ($stuck.Count) { $tail += " ($($stuck -join ', ') could not be put back and is the new version, the other the old)" }
        throw "could not write $(Join-Path $bindir $at)`: $said$tail"
    }
    $kept = @()
    foreach ($d in $done) {
        if ($d.Old) {
            try {
                Remove-Item -LiteralPath $d.Old -Force
            } catch {
                $kept += $d.Old
            }
        }
    }
    return , $kept
}

# The release's file NAME in TMP, from the folder FROM or downloaded from
# BASE, held to the sum SHA256SUMS gives it before anything reads it, and the
# mark of the web taken off it where it carries one (a copy of a zip a
# browser saved does): the file is the release's, by its sum, and what is
# unpacked from it would carry the mark on into SmartScreen's warning.
function Get-FennecAsset([string] $name, [string] $want, [string] $version, [string] $from, [string] $base, [string] $tmp) {
    $file = Join-Path $tmp $name
    if ($from) {
        Write-Host "Taking $name from $from"
        $source = Join-Path $from $name
        if (-not (Test-Path -LiteralPath $source -PathType Leaf)) { throw "$from holds no $name" }
        Copy-Item -LiteralPath $source -Destination $file
    } else {
        Write-Host "Downloading $name"
        try {
            Get-FennecFile "$base/download/v$version/$name" $file
        } catch {
            throw "could not download $base/download/v$version/$name ($($_.Exception.Message))"
        }
    }
    $got = (Get-FileHash -LiteralPath $file -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($got -ne $want) {
        throw "the checksum of $name is $got, SHA256SUMS says $want`: nothing was installed"
    }
    Write-Host "Checksum ok ($want)"
    Unblock-File -LiteralPath $file
    return $file
}

# Whether a program runs from the folder DIR: a running app's files cannot be
# replaced under it (Windows refuses to move a folder a running program's
# file is in). A process whose path cannot be read (another user's) is not
# this user's app.
function Test-FennecAppRunning([string] $dir) {
    $prefix = $dir.TrimEnd('\') + '\'
    foreach ($p in @(Get-Process -ErrorAction SilentlyContinue)) {
        $path = $null
        try { $path = $p.Path } catch { $path = $null }
        if ($path -and $path.StartsWith($prefix, [StringComparison]::OrdinalIgnoreCase)) { return $true }
    }
    return $false
}

# A shortcut LNK to the program TARGET, through the shell's own object: a
# .lnk is a binary format, and this is the one writer every Windows has.
function New-FennecShortcut([string] $lnk, [string] $target) {
    $shell = New-Object -ComObject WScript.Shell
    try {
        $s = $shell.CreateShortcut($lnk)
        $s.TargetPath = $target
        $s.WorkingDirectory = Split-Path -Parent $target
        $s.Description = 'fennec studio: make, open, serve and back up fennecdb databases'
        $s.Save()
    } finally {
        [void][Runtime.InteropServices.Marshal]::ReleaseComObject($shell)
    }
}

# The app (fennec studio, beta), from the release's zip of it for this
# machine's processor: checked against SHA256SUMS before it is unpacked,
# unpacked beside its place and renamed into it, over the one there by
# rename, so a failure leaves that one as it was. Its place is the one the
# app's own installer (Electrobun's) uses, so the two never make two copies,
# and the folder above it is the app's own (state.json, its pages' stores),
# never touched. Then a shortcut in the Start menu. Nothing it unpacked runs.
function Install-FennecApp([string[]] $sums, [string] $version, [string] $arch, [string] $from, [string] $base, [string] $tmp) {
    $appArch = 'x64'
    if ($arch -eq 'aarch64') { $appArch = 'arm64' }
    $name = "fennec-studio-v$version-windows-$appArch.zip"
    $want = Get-FennecSum $sums $name
    if (-not $want) { throw "release v$version lists no $name`: it has no Windows app yet" }
    if (-not $env:LOCALAPPDATA) { throw 'LOCALAPPDATA is not set: the app goes into it' }
    $root = Join-Path $env:LOCALAPPDATA 'dev.fennecdb.studio\stable'
    $dest = Join-Path $root 'app'
    if (Test-FennecAppRunning $dest) { throw "fennec studio is running from $dest`: quit it, then run this again" }

    $zip = Get-FennecAsset $name $want $version $from $base $tmp

    New-Item -ItemType Directory -Force -Path $root -ErrorAction SilentlyContinue | Out-Null
    if (-not (Test-Path -LiteralPath $root -PathType Container)) { throw "could not make $root" }
    # What an install that could not remove its staging folder left: a file
    # in it was held, by a scanner or the app the previous one replaced.
    foreach ($left in @(Get-ChildItem -LiteralPath $root -Force -Directory -Filter '.app-install.*')) {
        Remove-Item -LiteralPath $left.FullName -Recurse -Force -ErrorAction SilentlyContinue
    }
    $stage = Join-Path $root ".app-install.$PID"
    Remove-Item -LiteralPath $stage -Recurse -Force -ErrorAction SilentlyContinue
    try {
        Expand-Archive -LiteralPath $zip -DestinationPath $stage -Force
    } catch {
        Remove-Item -LiteralPath $stage -Recurse -Force -ErrorAction SilentlyContinue
        throw "could not unpack $name ($($_.Exception.Message)): nothing was installed"
    }
    $src = Join-Path $stage 'fennec-studio'
    if (-not (Test-Path -LiteralPath (Join-Path $src 'bin\launcher.exe') -PathType Leaf) -or -not (Test-Path -LiteralPath (Join-Path $src 'Resources\app\bun\index.js') -PathType Leaf)) {
        Remove-Item -LiteralPath $stage -Recurse -Force -ErrorAction SilentlyContinue
        throw "$name holds no app (fennec-studio\bin\launcher.exe): nothing was installed"
    }
    $previous = $null
    if (Test-Path -LiteralPath $dest) {
        $previous = Join-Path $stage 'previous'
        try {
            Move-Item -LiteralPath $dest -Destination $previous
        } catch {
            Remove-Item -LiteralPath $stage -Recurse -Force -ErrorAction SilentlyContinue
            throw "could not move the app at $dest aside ($($_.Exception.Message)): nothing was installed"
        }
    }
    try {
        Move-Item -LiteralPath $src -Destination $dest
    } catch {
        $said = $_.Exception.Message
        if ($previous) {
            try {
                Move-Item -LiteralPath $previous -Destination $dest
            } catch {
                # Kept, not removed with the rest: it is the only copy left.
                throw "could not write $dest ($said), nor put the one that was there back: it is $previous"
            }
        }
        Remove-Item -LiteralPath $stage -Recurse -Force -ErrorAction SilentlyContinue
        throw "could not write $dest ($said)"
    }
    Remove-Item -LiteralPath $stage -Recurse -Force -ErrorAction SilentlyContinue

    $launcher = Join-Path $dest 'bin\launcher.exe'
    $lnk = $null
    if ($env:APPDATA) {
        $programs = Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs'
        try {
            New-Item -ItemType Directory -Force -Path $programs -ErrorAction Stop | Out-Null
            $lnk = Join-Path $programs 'fennec studio.lnk'
            New-FennecShortcut $lnk $launcher
        } catch {
            $Host.UI.WriteErrorLine("fennecdb install: installed, but could not write the Start-menu shortcut ($($_.Exception.Message))")
            $lnk = $null
        }
    }

    Write-Host ''
    Write-Host "Installed fennec studio $version (beta) in $dest."
    if ($lnk) {
        Write-Host 'Open it from the Start menu, fennec studio, or:'
    } else {
        Write-Host 'Start it with:'
    }
    Write-Host ''
    Write-Host "  & '$($launcher.Replace("'", "''"))'"
    Write-Host ''
    Write-Host 'Not signed: what this script downloads carries no mark of the web, so SmartScreen'
    Write-Host 'does not ask; the same zip saved by a browser does, and asks once.'
    $remove = $dest
    if ($lnk) { $remove = "$dest and $lnk" }
    Write-Host "To remove it: delete $remove."
    Write-Host 'The app: https://fennecdb.com/docs/studio#desktop'
}

function Test-FennecOnPath([string] $dir, [string] $path) {
    if (-not $path) { return $false }
    foreach ($p in $path.Split(';')) {
        if ($p -and $p.Trim().TrimEnd('\') -eq $dir.TrimEnd('\')) { return $true }
    }
    return $false
}

function Invoke-FennecInstall([object[]] $arguments, [string] $tmp) {
    $app = $false
    foreach ($a in $arguments) {
        if (@('-h', '--help', '-Help', '/?') -contains $a) {
            Write-FennecUsage
            return
        }
        if (@('-App', '--app') -contains $a) {
            $app = $true
            continue
        }
        Write-FennecUsage
        throw "unknown argument '$a'"
    }

    if ($PSVersionTable.PSVersion.Major -ge 6 -and -not $IsWindows) {
        throw 'this installer is for Windows; on macOS and Linux: curl -fsSL https://fennecdb.com/install.sh | sh'
    }
    if (-not [Environment]::Is64BitOperatingSystem) {
        throw 'no fennecdb release for 32-bit Windows (x86_64 and arm64 only)'
    }

    $from = $env:FENNEC_FROM
    $base = $null
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

    # The build for this machine's processor, and no other: a release has
    # both Windows zips or neither (release.yml's publish), and an x86_64
    # program does not run on a Windows 10 for arm64, only a Windows 11's
    # emulation does, so a machine whose zip is not listed is refused.
    $arch = Get-FennecArch
    $target = "$arch-pc-windows-msvc"

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
                # ${version}: PowerShell reads `$version?` as a variable named "version?".
                throw "could not download $base/download/v$version/SHA256SUMS: is there a release v${version}? ($($_.Exception.Message))"
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

    $name = $null
    $want = $null
    $v = $version
    if (-not $v) { $v = Get-FennecListed $sums $target }
    if ($app -and $v -and $v -cmatch '^[0-9A-Za-z.+-]+$') {
        # The release the programs' zip names, or the one asked for: its app.
        Install-FennecApp $sums $v $arch $from $base $tmp
        return
    }
    if ($v -and $v -cmatch '^[0-9A-Za-z.+-]+$') {
        $name = "fennec-v$v-$target.zip"
        $want = Get-FennecSum $sums $name
    }
    if (-not $want) {
        $which = 'the latest release'
        if ($version) {
            $which = "release v$version"
        } else {
            $release = Get-FennecRelease $sums
            if ($release) { $which = "the latest release (v$release)" }
        }
        $why = ''
        if ($arch -ne 'x86_64') { $why = "; the x86_64 build is not taken in its place, a Windows 10 for arm64 does not run it" }
        throw "$which lists no build for $target (fennec-v*-$target.zip)$why"
    }
    $version = $v

    $zip = Get-FennecAsset $name $want $version $from $base $tmp

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
