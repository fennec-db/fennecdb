# site/install.ps1 against a release of its own laying out, served from
# localhost and read from a folder: nothing is fetched from a published
# release. Windows only; CI's `windows (release zip)` job runs it on the zip
# tools/windows-zip.sh made there, with Python for the local server:
#
#   pwsh site/test_install.ps1 -Zip dist\fennec-v0.1.13-x86_64-pc-windows-msvc.zip -Version 0.1.13
#
# What it holds, under Windows PowerShell 5.1 and PowerShell 7 alike: the
# installer puts both programs where it was told, from a URL and from a
# folder, and says how to reach them, touching no profile and no PATH; it
# refuses a zip whose checksum is not the release's, a release with no
# Windows build, a version with no release and a base URL that is not https
# or this machine's, each installing nothing; it installs over a running
# server; a zip carrying the mark of the web installs programs carrying none;
# run through iex it leaves the session's settings as they were, and a
# failure there fails the line; a copy cut short at any point runs
# nothing; and tools/install-ps1-check.ps1 passes the script and fails a copy
# made to run a command. With -App it installs the app from the release's
# zip of it (a stand-in: a folder holding a copy of ping.exe as its
# launcher) into LOCALAPPDATA with a Start-menu shortcut in APPDATA, each the
# test's own, again over it keeping the app's own files beside it, from a
# folder whose marked zip leaves no mark, and through the scriptblock the
# docs give; and refuses a tampered zip, a release with no app and an app
# running from its place, each installing nothing. It prints what Windows does with the mark: whether
# Invoke-WebRequest marks what it downloads, and whether Expand-Archive
# carries a zip's mark onto what it unpacks.

param(
    [Parameter(Mandatory = $true)] [string] $Zip,
    [Parameter(Mandatory = $true)] [string] $Version
)

Set-StrictMode -Version 2.0
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$script = Join-Path $PSScriptRoot 'install.ps1'
$Zip = (Resolve-Path -LiteralPath $Zip).Path
$name = Split-Path -Leaf $Zip
if (-not ($name -match "^fennec-v$([regex]::Escape($Version))-(.+)\.zip$")) {
    throw "$name is not a zip of release $Version"
}
$target = $Matches[1]

$shells = @()
foreach ($s in @('powershell', 'pwsh')) {
    $found = Get-Command $s -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($found) { $shells += $found.Path }
}
if ($shells.Count -eq 0) { throw 'neither Windows PowerShell nor PowerShell 7 is on the PATH' }

$work = Join-Path ([IO.Path]::GetTempPath()) ('fennecdb-install-test-' + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $work | Out-Null
$server = $null
$running = $null
$script:failed = 0

function Pass([string] $what) { Write-Host "ok    $what" }
function Fail([string] $what) {
    Write-Host "FAIL  $what"
    $script:failed++
}
function Expect([string] $what, [bool] $holds, [string] $saw = '') {
    if ($holds) { Pass $what } elseif ($saw) { Fail "$what (saw: $saw)" } else { Fail $what }
}

function Sha256([string] $path) { (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLowerInvariant() }

function Quote([string] $a) {
    if ($a -match '[\s"]') { return '"' + $a.Replace('"', '\"') + '"' }
    return $a
}

# Run FILE ARGS..., its stdout and stderr in OUT, with the variables of ENV
# set for it alone (a $null value unsets one); its exit code.
function Run([string] $exe, [string[]] $argv, [string] $out, [hashtable] $vars = @{}, [string] $cwd = $work) {
    # Windows PowerShell started by Start-Process from PowerShell 7 inherits
    # 7's module path and cannot load its own Get-FileHash: it is given the
    # system's, as a Windows PowerShell window has it.
    if ([IO.Path]::GetFileNameWithoutExtension($exe) -eq 'powershell' -and -not $vars.ContainsKey('PSModulePath')) {
        $vars = $vars.Clone()
        $vars['PSModulePath'] = [Environment]::GetEnvironmentVariable('PSModulePath', 'Machine')
    }
    $saved = @{}
    foreach ($k in $vars.Keys) {
        $saved[$k] = [Environment]::GetEnvironmentVariable($k, 'Process')
        [Environment]::SetEnvironmentVariable($k, $vars[$k], 'Process')
    }
    try {
        $err = "$out.err"
        $p = Start-Process -FilePath $exe -ArgumentList (@($argv | ForEach-Object { Quote $_ }) -join ' ') `
            -WorkingDirectory $cwd -NoNewWindow -PassThru `
            -RedirectStandardOutput $out -RedirectStandardError $err
        $null = $p.Handle
        $p.WaitForExit()
        [IO.File]::WriteAllText($out, (Text $out) + (Text $err))
        return $p.ExitCode
    } finally {
        foreach ($k in $saved.Keys) { [Environment]::SetEnvironmentVariable($k, $saved[$k], 'Process') }
    }
}

function Text([string] $path) {
    # Opened shared for writing: a running server holds its own stderr file
    # open for writing, and File.ReadAllText refuses to share with a writer.
    if (-not (Test-Path -LiteralPath $path)) { return '' }
    $f = [IO.File]::Open($path, 'Open', 'Read', 'ReadWrite')
    try { return (New-Object IO.StreamReader($f)).ReadToEnd() } finally { $f.Dispose() }
}

function Marked([string] $path) {
    return [bool](Get-Item -LiteralPath $path -Stream Zone.Identifier -ErrorAction SilentlyContinue)
}

function Mark([string] $path) {
    Set-Content -LiteralPath $path -Stream Zone.Identifier -Value "[ZoneTransfer]`r`nZoneId=3`r`nHostUrl=https://example.invalid/$((Split-Path -Leaf $path))"
}

# What the account keeps that an installer might touch: its environment's
# values, PATH among them, and each PowerShell profile.
function Snapshot {
    $lines = @()
    $environment = Get-Item -LiteralPath 'HKCU:\Environment'
    foreach ($v in @($environment.GetValueNames() | Sort-Object)) {
        $lines += "$v=$($environment.GetValue($v, $null, 'DoNotExpandEnvironmentNames'))"
    }
    $documents = [Environment]::GetFolderPath('MyDocuments')
    foreach ($dir in @('WindowsPowerShell', 'PowerShell')) {
        foreach ($f in @(Get-ChildItem -LiteralPath (Join-Path $documents $dir) -Filter '*profile.ps1' -File -ErrorAction SilentlyContinue)) {
            $lines += "$($f.FullName) $($f.Length) $($f.LastWriteTimeUtc.Ticks)"
        }
    }
    return ($lines -join "`n")
}

try {
    # The release: v$Version with this zip, laid out as GitHub serves one --
    # download/v<version>/ and latest/download/ -- and as a folder.
    $site = Join-Path $work 'site'
    $rel = Join-Path $site "download\v$Version"
    New-Item -ItemType Directory -Path $rel, (Join-Path $site 'latest\download') | Out-Null
    Copy-Item -LiteralPath $Zip -Destination $rel
    $sum = Sha256 (Join-Path $rel $name)
    # The app's zip for this processor, as desktop.yml attaches it: a
    # stand-in, the folder fennec-studio holding a launcher (a copy of
    # ping.exe, which runs from anywhere and can be kept running) and the
    # main process's file.
    $appArch = if ($target -eq 'aarch64-pc-windows-msvc') { 'arm64' } else { 'x64' }
    $appName = "fennec-studio-v$Version-windows-$appArch.zip"
    $standIn = Join-Path $work 'appsrc\fennec-studio'
    New-Item -ItemType Directory -Path (Join-Path $standIn 'bin'), (Join-Path $standIn 'Resources\app\bun') | Out-Null
    Copy-Item -LiteralPath (Join-Path $env:SystemRoot 'System32\PING.EXE') -Destination (Join-Path $standIn 'bin\launcher.exe')
    [IO.File]::WriteAllText((Join-Path $standIn 'Resources\app\bun\index.js'), '// the stand-in app')
    Compress-Archive -Path $standIn -DestinationPath (Join-Path $rel $appName)
    $appSum = Sha256 (Join-Path $rel $appName)
    # A tarball's line before the zip's, as release.yml's SHA256SUMS has them,
    # and the app's appended after them, as desktop.yml appends it.
    $sums = "$('0' * 64)  fennec-v$Version-x86_64-unknown-linux-musl.tar.gz`n$sum  $name`n$appSum  $appName`n"
    [IO.File]::WriteAllText((Join-Path $rel 'SHA256SUMS'), $sums)
    Copy-Item -LiteralPath (Join-Path $rel 'SHA256SUMS') -Destination (Join-Path $site 'latest\download')
    $from = Join-Path $work 'from'
    New-Item -ItemType Directory -Path $from | Out-Null
    Copy-Item -LiteralPath (Join-Path $rel $name), (Join-Path $rel $appName), (Join-Path $rel 'SHA256SUMS') -Destination $from
    # A release with the programs and no app.
    $noApp = Join-Path $site 'download\v0.0.1'
    New-Item -ItemType Directory -Path $noApp | Out-Null
    [IO.File]::WriteAllText((Join-Path $noApp 'SHA256SUMS'), "$sum  fennec-v0.0.1-$target.zip`n")
    # A release with no Windows build, as every one before it.
    $old = Join-Path $work 'oldsite'
    New-Item -ItemType Directory -Path (Join-Path $old 'latest\download') | Out-Null
    [IO.File]::WriteAllText((Join-Path $old 'latest\download\SHA256SUMS'), "$('0' * 64)  fennec-v0.1.12-x86_64-unknown-linux-musl.tar.gz`n")
    # A release with the other processor's Windows zip alone: an arm64 machine
    # takes no x86_64 build in place of its own, and an x86_64 one no arm64.
    $other = if ($target -eq 'x86_64-pc-windows-msvc') { 'aarch64-pc-windows-msvc' } else { 'x86_64-pc-windows-msvc' }
    $wrong = Join-Path $work 'wrongsite'
    New-Item -ItemType Directory -Path (Join-Path $wrong 'latest\download') | Out-Null
    [IO.File]::WriteAllText((Join-Path $wrong 'latest\download\SHA256SUMS'), "$('0' * 64)  fennec-v0.1.12-x86_64-unknown-linux-musl.tar.gz`n$('0' * 64)  fennec-v0.1.12-$other.zip`n")

    # A server on a port the system picks, found by the file it writes.
    $python = Get-Command python, py -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
    if (-not $python) { throw 'the test serves its release with Python, and there is none on the PATH' }
    $serve = Join-Path $work 'serve.py'
    [IO.File]::WriteAllText($serve, @"
import functools, http.server, sys


class Quiet(http.server.SimpleHTTPRequestHandler):
    def log_message(self, *args):
        pass


server = http.server.ThreadingHTTPServer(
    ("127.0.0.1", 0), functools.partial(Quiet, directory=sys.argv[1])
)
open(sys.argv[2] + ".part", "w").write(str(server.server_address[1]))
import os
os.replace(sys.argv[2] + ".part", sys.argv[2])
server.serve_forever()
"@)
    $portFile = Join-Path $work 'port'
    $server = Start-Process -FilePath $python.Path -ArgumentList (Quote $serve), (Quote $work), (Quote $portFile) -NoNewWindow -PassThru
    $deadline = (Get-Date).AddSeconds(30)
    while (-not (Test-Path -LiteralPath $portFile)) {
        if ($server.HasExited) { throw 'the test server exited' }
        if ((Get-Date) -gt $deadline) { throw 'the test server wrote no port in 30 s' }
        Start-Sleep -Milliseconds 100
    }
    $port = (Get-Content -LiteralPath $portFile -Raw).Trim()
    $base = "http://127.0.0.1:$port/site"
    $oldBase = "http://127.0.0.1:$port/oldsite"
    $wrongBase = "http://127.0.0.1:$port/wrongsite"

    $before = Snapshot

    foreach ($shell in $shells) {
        $tag = [IO.Path]::GetFileNameWithoutExtension($shell)
        Write-Host "--- $shell"

        # install NAME VARS [ARGS]: a run into a folder of its own, from the
        # local release unless VARS says otherwise; its exit code, and its
        # output in $work\<tag>-<NAME>.out.
        function Install([string] $n, [hashtable] $vars = @{}, [string[]] $more = @(), [string] $cwd = $work) {
            $v = @{ FENNEC_BASE_URL = $base; FENNEC_INSTALL = (Join-Path $work "$tag-$n-inst"); FENNEC_VERSION = $null; FENNEC_FROM = $null }
            foreach ($k in $vars.Keys) { $v[$k] = $vars[$k] }
            return (Run $shell (@('-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-File', $script) + $more) (Join-Path $work "$tag-$n.out") $v $cwd)
        }
        function Out([string] $n) { Text (Join-Path $work "$tag-$n.out") }
        function Inst([string] $n) { Join-Path $work "$tag-$n-inst" }
        function Files([string] $dir) { @(Get-ChildItem -LiteralPath $dir -Force -File -ErrorAction SilentlyContinue) }
        function Says([string] $exe, [string] $wanted) {
            if (-not (Test-Path -LiteralPath $exe)) { return $false }
            $said = @(& $exe --version) | Select-Object -First 1
            return ($said -ceq $wanted)
        }

        # The latest release, from a URL.
        $rc = Install latest
        Expect "$tag latest: exit 0" ($rc -eq 0) (Out latest)
        Expect "$tag latest: found $Version" ((Out latest).Contains("Installed fennecdb $Version ($target)")) (Out latest)
        Expect "$tag latest: checked the sum" ((Out latest).Contains("Checksum ok ($sum)"))
        $bin = Join-Path (Inst latest) 'bin'
        Expect "$tag latest: fennec-server.exe runs" (Says (Join-Path $bin 'fennec-server.exe') "fennec-server $Version")
        Expect "$tag latest: fennec.exe runs" (Says (Join-Path $bin 'fennec.exe') "fennec $Version")
        Expect "$tag latest: the two programs and nothing else" (@(Files $bin).Count -eq 2) (@(Files $bin) -join ', ')
        Expect "$tag latest: says how to reach it" ((Out latest).Contains('$env:Path = '))
        Expect "$tag latest: says it ran" ((Out latest).Contains("(fennec-server $Version)"))

        # Again over it, as an upgrade.
        $rc = Install latest
        Expect "$tag again, over the first: exit 0" ($rc -eq 0) (Out latest)
        Expect "$tag again: still two files" (@(Files $bin).Count -eq 2) (@(Files $bin) -join ', ')

        # Over a server running from the folder: the running program's file is
        # renamed aside, kept while it runs, and removed by the next install.
        $db = Join-Path $work "$tag-running.fennec"
        $runErr = Join-Path $work "$tag-running.err"
        $running = Start-Process -FilePath (Join-Path $bin 'fennec-server.exe') -ArgumentList '--file', (Quote $db), '--http', '127.0.0.1:0' `
            -NoNewWindow -PassThru -RedirectStandardOutput (Join-Path $work "$tag-running.out") -RedirectStandardError $runErr
        $deadline = (Get-Date).AddSeconds(60)
        while (-not (Text $runErr).Contains('listening on')) {
            if ($running.HasExited) { break }
            if ((Get-Date) -gt $deadline) { break }
            Start-Sleep -Milliseconds 100
        }
        Expect "$tag a server runs from the folder" ((Text $runErr).Contains('listening on')) (Text $runErr)
        $rc = Install latest
        Expect "$tag over a running server: exit 0" ($rc -eq 0) (Out latest)
        Expect "$tag over a running server: the new one in its place" (Says (Join-Path $bin 'fennec-server.exe') "fennec-server $Version")
        # Windows refuses to delete a running program's file: the old one
        # stays aside while it runs, said so, or is gone had Windows let it go.
        $aside = @(Files $bin | Where-Object { $_.Name -like '.fennec-server.exe.old.*' }).Count
        Write-Host "    a running server's old file: $(if ($aside) { 'kept aside while it runs' } else { 'removed at once' })"
        Expect "$tag over a running server: at most the old file beside the two" ($aside -le 1 -and @(Files $bin).Count -eq 2 + $aside) (@(Files $bin) -join ', ')
        Expect "$tag over a running server: says so where it kept it" (($aside -eq 1) -eq (Out latest).Contains('keeps its old file')) (Out latest)
        Stop-Process -Id $running.Id -Force
        $running.WaitForExit()
        $running = $null
        $rc = Install latest
        Expect "$tag once it stopped: exit 0" ($rc -eq 0) (Out latest)
        Expect "$tag once it stopped: the old file removed" (@(Files $bin).Count -eq 2) (@(Files $bin) -join ', ')

        # The second program cannot be replaced -- its file held open without
        # the right to rename it, as a backup agent or an antivirus can: the
        # first, already replaced, is put back, and the message says so.
        $rc = Install rollback
        Expect "$tag rollback: a first install" ($rc -eq 0) (Out rollback)
        $rbin = Join-Path (Inst rollback) 'bin'
        [IO.File]::WriteAllText((Join-Path $rbin 'fennec.exe'), 'the old fennec.exe')
        $held = [IO.File]::Open((Join-Path $rbin 'fennec-server.exe'), 'Open', 'Read', 'Read')
        try {
            $rc = Install rollbackagain @{ FENNEC_INSTALL = (Inst rollback) }
        } finally {
            $held.Dispose()
        }
        Expect "$tag rollback: fails" ($rc -ne 0) (Out rollbackagain)
        Expect "$tag rollback: names the file it could not write" ((Out rollbackagain).Contains('could not write')) (Out rollbackagain)
        Expect "$tag rollback: says the first is back" ((Out rollbackagain).Contains('fennec.exe is back as it was')) (Out rollbackagain)
        Expect "$tag rollback: the first is the old one" ((Text (Join-Path $rbin 'fennec.exe')) -ceq 'the old fennec.exe') (Text (Join-Path $rbin 'fennec.exe'))
        Expect "$tag rollback: the second is the one that was there" (Says (Join-Path $rbin 'fennec-server.exe') "fennec-server $Version")
        Expect "$tag rollback: the two programs and nothing else" (@(Files $rbin).Count -eq 2) (@(Files $rbin) -join ', ')

        # Already on the PATH: nothing to say about it.
        $onpath = Join-Path (Inst onpath) 'bin'
        $rc = Install onpath @{ Path = "$onpath;$env:Path" }
        Expect "$tag on PATH: exit 0" ($rc -eq 0) (Out onpath)
        Expect "$tag on PATH: no hint" (-not (Out onpath).Contains('is not on your PATH'))

        # A version asked for, with and without the v, and one with no release.
        $rc = Install pinned @{ FENNEC_VERSION = "v$Version" }
        Expect "$tag pinned v: exit 0" ($rc -eq 0) (Out pinned)
        $rc = Install pinned2 @{ FENNEC_VERSION = $Version }
        Expect "$tag pinned: exit 0" ($rc -eq 0) (Out pinned2)
        $rc = Install nosuch @{ FENNEC_VERSION = '1.2.3' }
        Expect "$tag no such release: fails" ($rc -ne 0)
        Expect "$tag no such release: says so" ((Out nosuch).Contains('is there a release v1.2.3')) (Out nosuch)
        Expect "$tag no such release: installs nothing" (-not (Test-Path (Inst nosuch)))
        $rc = Install oddversion @{ FENNEC_VERSION = '1.0/../x' }
        Expect "$tag odd version: fails" ($rc -ne 0)
        Expect "$tag odd version: installs nothing" (-not (Test-Path (Inst oddversion)))

        # The latest release has no Windows build, as none before slice 5's.
        $rc = Install nowindows @{ FENNEC_BASE_URL = $oldBase }
        Expect "$tag no Windows build: fails" ($rc -ne 0)
        Expect "$tag no Windows build: names the target" ((Out nowindows).Contains("lists no build for $target")) (Out nowindows)
        Expect "$tag no Windows build: installs nothing" (-not (Test-Path (Inst nowindows)))
        Expect "$tag no Windows build: names the release" ((Out nowindows).Contains('the latest release (v0.1.12)')) (Out nowindows)

        # The other processor's zip is listed, this one's is not: refused, the
        # release named, nothing taken in its place.
        $rc = Install wrongarch @{ FENNEC_BASE_URL = $wrongBase }
        Expect "$tag only the other processor's zip: fails" ($rc -ne 0)
        Expect "$tag only the other processor's zip: names the target and the release" ((Out wrongarch).Contains("the latest release (v0.1.12) lists no build for $target")) (Out wrongarch)
        Expect "$tag only the other processor's zip: installs nothing" (-not (Test-Path (Inst wrongarch)))

        # A zip that is not the one the sums name: a byte added.
        $served = Join-Path $rel $name
        Copy-Item -LiteralPath $served -Destination "$served.good"
        [IO.File]::AppendAllText($served, 'x')
        $rc = Install tampered
        Copy-Item -LiteralPath "$served.good" -Destination $served -Force
        Remove-Item -LiteralPath "$served.good"
        Expect "$tag tampered: fails" ($rc -ne 0)
        Expect "$tag tampered: names the checksum" ((Out tampered).Contains('checksum')) (Out tampered)
        Expect "$tag tampered: installs nothing" (-not (Test-Path (Inst tampered)))

        # From a folder, nothing downloaded; the zip there marked as a
        # browser marks a download, which nothing installed carries.
        $fromZip = Join-Path $from $name
        Mark $fromZip
        $rc = Install from @{ FENNEC_BASE_URL = $null; FENNEC_FROM = $from }
        Expect "$tag from a folder: exit 0" ($rc -eq 0) (Out from)
        Expect "$tag from a folder: checked the sum" ((Out from).Contains("Checksum ok ($sum)"))
        Expect "$tag from a folder: the zip there was marked" (Marked $fromZip)
        foreach ($b in @('fennec.exe', 'fennec-server.exe')) {
            $f = Join-Path (Inst from) "bin\$b"
            Expect "$tag from a folder: $b installed" (Test-Path -LiteralPath $f)
            Expect "$tag from a folder: $b carries no mark" (-not (Marked $f))
        }
        $rc = Install frompinned @{ FENNEC_BASE_URL = $null; FENNEC_FROM = $from; FENNEC_VERSION = $Version }
        Expect "$tag from a folder, pinned: exit 0" ($rc -eq 0) (Out frompinned)
        $rc = Install fromboth @{ FENNEC_FROM = $from }
        Expect "$tag a folder and a URL: refused" ($rc -ne 0)
        Expect "$tag a folder and a URL: installs nothing" (-not (Test-Path (Inst fromboth)))
        Copy-Item -LiteralPath $fromZip -Destination "$fromZip.good"
        [IO.File]::AppendAllText($fromZip, 'x')
        $rc = Install fromtampered @{ FENNEC_BASE_URL = $null; FENNEC_FROM = $from }
        Copy-Item -LiteralPath "$fromZip.good" -Destination $fromZip -Force
        Remove-Item -LiteralPath "$fromZip.good"
        Expect "$tag from a folder, tampered: fails" ($rc -ne 0)
        Expect "$tag from a folder, tampered: names the checksum" ((Out fromtampered).Contains('checksum')) (Out fromtampered)
        Expect "$tag from a folder, tampered: installs nothing" (-not (Test-Path (Inst fromtampered)))
        $rc = Install fromnothing @{ FENNEC_BASE_URL = $null; FENNEC_FROM = (Join-Path $work 'nowhere') }
        Expect "$tag from a folder with no SHA256SUMS: fails" ($rc -ne 0)
        Expect "$tag from a folder with no SHA256SUMS: installs nothing" (-not (Test-Path (Inst fromnothing)))

        # The app, -App: into a LOCALAPPDATA and an APPDATA of the test's own.
        function AppInstall([string] $n, [hashtable] $vars = @{}, [string[]] $flag = @('-App')) {
            $local = Join-Path $work "$tag-$n-local"
            $roaming = Join-Path $work "$tag-$n-roaming"
            New-Item -ItemType Directory -Force -Path $local, $roaming | Out-Null
            $v = @{ LOCALAPPDATA = $local; APPDATA = $roaming }
            foreach ($k in $vars.Keys) { $v[$k] = $vars[$k] }
            return (Install $n $v $flag)
        }
        function Stable([string] $n) { Join-Path $work "$tag-$n-local\dev.fennecdb.studio\stable" }
        function AppDir([string] $n) { Join-Path (Stable $n) 'app' }
        function Lnk([string] $n) { Join-Path $work "$tag-$n-roaming\Microsoft\Windows\Start Menu\Programs\fennec studio.lnk" }
        function Target([string] $lnk) {
            $shell = New-Object -ComObject WScript.Shell
            try { return $shell.CreateShortcut($lnk).TargetPath } finally { [void][Runtime.InteropServices.Marshal]::ReleaseComObject($shell) }
        }
        function Names([string] $dir) { (@(Get-ChildItem -LiteralPath $dir -Force -ErrorAction SilentlyContinue | ForEach-Object { $_.Name } | Sort-Object) -join ' ') }

        $rc = AppInstall app
        $launcher = Join-Path (AppDir app) 'bin\launcher.exe'
        Expect "$tag -App: exit 0" ($rc -eq 0) (Out app)
        Expect "$tag -App: checked the sum" ((Out app).Contains("Checksum ok ($appSum)")) (Out app)
        Expect "$tag -App: says where it installed the app" ((Out app).Contains("Installed fennec studio $Version (beta) in $(AppDir app).")) (Out app)
        Expect "$tag -App: the app is there" ((Test-Path -LiteralPath $launcher) -and (Test-Path -LiteralPath (Join-Path (AppDir app) 'Resources\app\bun\index.js')))
        Expect "$tag -App: nothing beside it" ((Names (Stable app)) -eq 'app') (Names (Stable app))
        Expect "$tag -App: a Start-menu shortcut" (Test-Path -LiteralPath (Lnk app))
        Expect "$tag -App: the shortcut starts the app" ((Target (Lnk app)) -eq $launcher) (Target (Lnk app))
        Expect "$tag -App: says how to start it" ((Out app).Contains("& '$launcher'")) (Out app)
        Expect "$tag -App: the programs not installed" (-not (Test-Path (Inst app)))

        # Again over it: the app's own files beside it are kept.
        [IO.File]::WriteAllText((Join-Path (Stable app) 'state.json'), '{}')
        $rc = AppInstall app
        Expect "$tag -App again, over the first: exit 0" ($rc -eq 0) (Out app)
        Expect "$tag -App again: the app's state beside it kept, no staging left" ((Names (Stable app)) -eq 'app state.json') (Names (Stable app))
        $rc = AppInstall appflag @{} @('--app')
        Expect "$tag --app, spelled so: exit 0" ($rc -eq 0) (Out appflag)

        # The app running from its place is not replaced under it.
        $runningApp = Start-Process -FilePath $launcher -ArgumentList '-n', '120', '127.0.0.1' -NoNewWindow -PassThru `
            -RedirectStandardOutput (Join-Path $work "$tag-ping.out") -RedirectStandardError (Join-Path $work "$tag-ping.err")
        try {
            $deadline = (Get-Date).AddSeconds(30)
            while (-not (Get-Process -Id $runningApp.Id -ErrorAction SilentlyContinue) -and (Get-Date) -lt $deadline) { Start-Sleep -Milliseconds 100 }
            $before = Names (Join-Path (AppDir app) 'bin')
            $rc = AppInstall app
            Expect "$tag -App while it runs: refused" ($rc -ne 0) (Out app)
            Expect "$tag -App while it runs: says to quit it" ((Out app).Contains('is running from')) (Out app)
            Expect "$tag -App while it runs: the app left as it was" ((Names (Join-Path (AppDir app) 'bin')) -eq $before -and (Test-Path -LiteralPath $launcher))
        } finally {
            Stop-Process -Id $runningApp.Id -Force -ErrorAction SilentlyContinue
            $runningApp.WaitForExit()
        }

        # A zip that is not the one the sums name, a release with no app, and
        # a latest release with no Windows build.
        $servedApp = Join-Path $rel $appName
        Copy-Item -LiteralPath $servedApp -Destination "$servedApp.good"
        [IO.File]::AppendAllText($servedApp, 'x')
        $rc = AppInstall apptampered
        Copy-Item -LiteralPath "$servedApp.good" -Destination $servedApp -Force
        Remove-Item -LiteralPath "$servedApp.good"
        Expect "$tag -App, a tampered zip: refused" ($rc -ne 0)
        Expect "$tag -App, a tampered zip: names the checksum" ((Out apptampered).Contains('checksum')) (Out apptampered)
        Expect "$tag -App, a tampered zip: installs nothing" (-not (Test-Path (Stable apptampered)) -and -not (Test-Path (Lnk apptampered)))
        $rc = AppInstall appnone @{ FENNEC_VERSION = '0.0.1' }
        Expect "$tag -App, a release with no app: refused" ($rc -ne 0)
        Expect "$tag -App, a release with no app: says so" ((Out appnone).Contains("release v0.0.1 lists no fennec-studio-v0.0.1-windows-$appArch.zip")) (Out appnone)
        Expect "$tag -App, a release with no app: installs nothing" (-not (Test-Path (Stable appnone)))
        $rc = AppInstall appold @{ FENNEC_BASE_URL = $oldBase }
        Expect "$tag -App, a latest release with no Windows build: refused" ($rc -ne 0)
        Expect "$tag -App, a latest release with no Windows build: installs nothing" (-not (Test-Path (Stable appold)))

        # From a folder whose zip a browser marked: nothing installed carries the mark.
        $fromApp = Join-Path $from $appName
        Mark $fromApp
        $rc = AppInstall appfrom @{ FENNEC_BASE_URL = $null; FENNEC_FROM = $from }
        Expect "$tag -App from a folder: exit 0" ($rc -eq 0) (Out appfrom)
        Expect "$tag -App from a folder: the zip there was marked" (Marked $fromApp)
        Expect "$tag -App from a folder: the launcher carries no mark" ((Test-Path -LiteralPath (Join-Path (AppDir appfrom) 'bin\launcher.exe')) -and -not (Marked (Join-Path (AppDir appfrom) 'bin\launcher.exe')))

        # As the docs give it: the script made a script block and called with -App.
        $sbDriver = Join-Path $work "$tag-sb.ps1"
        [IO.File]::WriteAllText($sbDriver, @'
$path = $args[0]
$before = "$ErrorActionPreference $ProgressPreference $([Net.ServicePointManager]::SecurityProtocol) $(Get-Location)"
& ([scriptblock]::Create([IO.File]::ReadAllText($path))) -App
$after = "$ErrorActionPreference $ProgressPreference $([Net.ServicePointManager]::SecurityProtocol) $(Get-Location)"
if ($before -ne $after) { Write-Host "session changed: [$before] -> [$after]"; exit 3 }
if (Get-Command Install-Fennec -ErrorAction SilentlyContinue) { Write-Host 'a function of the script is left defined'; exit 4 }
Write-Host 'session unchanged'
'@)
        $sbLocal = Join-Path $work "$tag-sb-local"
        $sbRoaming = Join-Path $work "$tag-sb-roaming"
        New-Item -ItemType Directory -Force -Path $sbLocal, $sbRoaming | Out-Null
        $sbOut = Join-Path $work "$tag-sb.out"
        $rc = Run $shell @('-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-File', $sbDriver, $script) $sbOut @{ FENNEC_BASE_URL = $base; LOCALAPPDATA = $sbLocal; APPDATA = $sbRoaming }
        Expect "$tag -App through a script block: exit 0" ($rc -eq 0) (Text $sbOut)
        Expect "$tag -App through a script block: installed" (Test-Path -LiteralPath (Join-Path $sbLocal 'dev.fennecdb.studio\stable\app\bin\launcher.exe')) (Text $sbOut)
        Expect "$tag -App through a script block: the session as it was" ((Text $sbOut).Contains('session unchanged')) (Text $sbOut)

        # A base URL that is not https, or not this machine's.
        $n = 0
        foreach ($url in @(
                'http://example.com/releases',
                'http://localhost:x@example.invalid',
                'http://localhost:80@example.invalid/releases',
                'http://127.0.0.1@example.invalid',
                'http://localhost.example.invalid/releases',
                'http://127.0.0.1.example.invalid',
                'http://localhostx',
                'https://user:secret@example.invalid/releases',
                'https://example.invalid/a b',
                'ftp://example.invalid/releases',
                'file:///C:/Windows')) {
            $n++
            $rc = Install "url$n" @{ FENNEC_BASE_URL = $url }
            Expect "$tag base URL $url`: refused" ($rc -ne 0)
            Expect "$tag base URL $url`: installs nothing" (-not (Test-Path (Inst "url$n")))
            Expect "$tag base URL $url`: says why" ((Out "url$n").Contains('FENNEC_BASE_URL')) (Out "url$n")
        }
        $rc = Install localhost @{ FENNEC_BASE_URL = "http://localhost:$port/site" }
        Expect "$tag http://localhost:port: installs" ($rc -eq 0) (Out localhost)
        $rc = Install localhostslash @{ FENNEC_BASE_URL = "http://127.0.0.1:$port/site/" }
        Expect "$tag http://127.0.0.1:port/: installs" ($rc -eq 0) (Out localhostslash)

        # A relative FENNEC_INSTALL is told back as an absolute folder.
        $relcwd = Join-Path $work "$tag-relcwd"
        New-Item -ItemType Directory -Path $relcwd | Out-Null
        $rc = Install rel @{ FENNEC_INSTALL = 'rel\dir' } @() $relcwd
        Expect "$tag relative FENNEC_INSTALL: installs" ($rc -eq 0) (Out rel)
        Expect "$tag relative FENNEC_INSTALL: tells the absolute folder" ((Out rel).Contains("$relcwd\rel\dir\bin")) (Out rel)
        Expect "$tag relative FENNEC_INSTALL: where it said" (Test-Path -LiteralPath "$relcwd\rel\dir\bin\fennec.exe")

        # A folder that cannot be made: its parent is a file.
        $blocker = Join-Path $work "$tag-blocker"
        Set-Content -LiteralPath $blocker -Value 'a file'
        $rc = Install blocked @{ FENNEC_INSTALL = (Join-Path $blocker 'x') }
        Expect "$tag a folder that cannot be made: fails" ($rc -ne 0)
        Expect "$tag a folder that cannot be made: says where" ((Out blocked).Contains('FENNEC_INSTALL')) (Out blocked)

        # An argument it does not know is refused, and --help installs nothing.
        $rc = Install arg @{} @('--nonsense')
        Expect "$tag unknown argument: refused" ($rc -ne 0)
        Expect "$tag unknown argument: installs nothing" (-not (Test-Path (Inst arg)))
        $rc = Install help @{} @('--help')
        Expect "$tag --help: exit 0" ($rc -eq 0) (Out help)
        Expect "$tag --help: usage" ((Out help).Contains('FENNEC_VERSION'))
        Expect "$tag --help: installs nothing" (-not (Test-Path (Inst help)))

        # Through iex, as `irm ... | iex` runs it: it installs, leaves the
        # session's settings as they were, and a failure fails the line.
        $driver = Join-Path $work "$tag-iex.ps1"
        [IO.File]::WriteAllText($driver, @'
$path = $args[0]
# What `irm ... | iex` at a prompt has: no arguments.
$args = @()
$before = "$ErrorActionPreference $ProgressPreference $([Net.ServicePointManager]::SecurityProtocol) $(Get-Location)"
Invoke-Expression ([IO.File]::ReadAllText($path))
$after = "$ErrorActionPreference $ProgressPreference $([Net.ServicePointManager]::SecurityProtocol) $(Get-Location)"
if ($before -ne $after) { Write-Host "session changed: [$before] -> [$after]"; exit 3 }
Write-Host 'session unchanged'
'@)
        $iex = Join-Path $work "$tag-iex.out"
        $rc = Run $shell @('-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-File', $driver, $script) $iex @{ FENNEC_BASE_URL = $base; FENNEC_INSTALL = (Inst iex) }
        Expect "$tag through iex: exit 0" ($rc -eq 0) (Text $iex)
        Expect "$tag through iex: installed" (Says (Join-Path (Inst iex) 'bin\fennec-server.exe') "fennec-server $Version")
        Expect "$tag through iex: the session's settings as they were" ((Text $iex).Contains('session unchanged')) (Text $iex)
        $iexFail = Join-Path $work "$tag-iexfail.out"
        $rc = Run $shell @('-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-Command', "Invoke-Expression ([IO.File]::ReadAllText('$script'))") $iexFail @{ FENNEC_BASE_URL = $base; FENNEC_INSTALL = (Inst iexfail); FENNEC_VERSION = '1.2.3' }
        Expect "$tag through iex, failing: the line fails" ($rc -ne 0) (Text $iexFail)
        Expect "$tag through iex, failing: says why" ((Text $iexFail).Contains('fennecdb install: could not download')) (Text $iexFail)

        # The script cut short at any point runs nothing: every line's end,
        # bytes across the file, and each prefix of the last line short of
        # the call's name -- with the name whole the script is whole, and it
        # runs as it would.
        $text = [IO.File]::ReadAllText($script)
        $body = $text.TrimEnd()
        $last = $body.LastIndexOf("`n") + 1
        $cuts = New-Object System.Collections.Generic.List[int]
        for ($i = 0; $i -lt $last; $i++) { if ($text[$i] -eq "`n") { $cuts.Add($i + 1) } }
        $step = [int]($text.Length / 60) + 1
        for ($i = 0; $i -lt $last; $i += $step) { $cuts.Add($i) }
        for ($i = 0; $i -lt 'Install-Fennec'.Length; $i++) { $cuts.Add($last + $i) }
        $cutList = Join-Path $work "$tag-cuts.txt"
        [IO.File]::WriteAllText($cutList, ($cuts -join "`n"))
        $cutDriver = Join-Path $work "$tag-cut.ps1"
        [IO.File]::WriteAllText($cutDriver, @'
$text = [IO.File]::ReadAllText($args[0])
$count = 0
foreach ($c in [IO.File]::ReadAllLines($args[1])) {
    try { & { Invoke-Expression $args[0] } $text.Substring(0, [int]$c) } catch { }
    $count++
}
Write-Host "cuts run: $count"
'@)
        $cutOut = Join-Path $work "$tag-cut.out"
        $rc = Run $shell @('-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-File', $cutDriver, $script, $cutList) $cutOut @{ FENNEC_BASE_URL = $base; FENNEC_INSTALL = (Inst cut) }
        Expect "$tag cut short ($($cuts.Count) cuts): every cut tried" ((Text $cutOut).Contains("cuts run: $($cuts.Count)")) (Text $cutOut)
        Expect "$tag cut short: nothing runs" (-not (Test-Path (Inst cut)) -and -not (Text $cutOut).Contains('Looking for')) (Text $cutOut)

        # The check that holds the file safe to run through iex
        # (tools/install-ps1-check.ps1, also run by site/build.py before the
        # site ships it) passes the file and fails each copy made to run
        # something: a command invoked inside Install-Fennec, one chained
        # after a function's closing brace on the same line -- both of which a
        # reading of the file's lines let through -- and Add-Type.
        $checker = (Resolve-Path -LiteralPath (Join-Path (Join-Path $PSScriptRoot '..') 'tools\install-ps1-check.ps1')).Path
        $call = 'Invoke-FennecInstall $args $tmp'
        Expect "$tag the check's anchor is in the script" ($text.Contains($call))
        $mutants = [ordered]@{
            'invoke-expression in Install-Fennec' = $text.Replace($call, "Invoke-Expression (irm 'https://example.invalid/x')`n        $call")
            'add-type in Install-Fennec'          = $text.Replace($call, "Add-Type -TypeDefinition 'public class FennecX {}'`n        $call")
            'a command after a closing brace'     = ([regex]'(?m)^}(?=\r?$)').Replace($text, '}; New-Item -ItemType Directory -Path pwned', 1)
            'a command before the call'           = $text.Replace('Install-Fennec @args', "New-Item -ItemType Directory -Path pwned`nInstall-Fennec @args")
            'the shell object running a program'  = $text.Replace('$s.Save()', "`$s.Save()`n        `$shell.Run('cmd.exe')")
        }
        $checked = Join-Path $work "$tag-check.ps1"
        [IO.File]::WriteAllText($checked, $text)
        $checkOut = Join-Path $work "$tag-check.out"
        $rc = Run $shell @('-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-File', $checker, $checked) $checkOut
        Expect "$tag the check passes the script" ($rc -eq 0) (Text $checkOut)
        $m = 0
        foreach ($what in $mutants.Keys) {
            $m++
            $copy = Join-Path $work "$tag-mutant$m.ps1"
            Expect "$tag the check's copy with $what differs" ($mutants[$what] -cne $text)
            [IO.File]::WriteAllText($copy, $mutants[$what])
            $copyOut = Join-Path $work "$tag-mutant$m.out"
            $rc = Run $shell @('-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-File', $checker, $copy) $copyOut
            Expect "$tag the check fails a copy with $what" ($rc -ne 0) (Text $copyOut)
        }

        # What Windows does with the mark, printed: whether Invoke-WebRequest
        # marks a download, from the internet and from this machine, and
        # whether Expand-Archive carries a zip's mark onto what it unpacks.
        $probe = Join-Path $work "$tag-mark.ps1"
        [IO.File]::WriteAllText($probe, @'
$ProgressPreference = 'SilentlyContinue'
$dir = $args[0]
function Said($f) { if (Get-Item -LiteralPath $f -Stream Zone.Identifier -ErrorAction SilentlyContinue) { 'marked' } else { 'not marked' } }
try {
    Invoke-WebRequest -UseBasicParsing -Uri 'https://github.com/robots.txt' -OutFile (Join-Path $dir 'internet.txt')
    Write-Host "Invoke-WebRequest, from https://github.com: $(Said (Join-Path $dir 'internet.txt'))"
} catch { Write-Host "Invoke-WebRequest, from https://github.com: not reached ($($_.Exception.Message))" }
Invoke-WebRequest -UseBasicParsing -Uri $args[1] -OutFile (Join-Path $dir 'local.zip')
Write-Host "Invoke-WebRequest, from this machine: $(Said (Join-Path $dir 'local.zip'))"
Copy-Item -LiteralPath $args[2] -Destination (Join-Path $dir 'marked.zip')
Set-Content -LiteralPath (Join-Path $dir 'marked.zip') -Stream Zone.Identifier -Value "[ZoneTransfer]`r`nZoneId=3"
Expand-Archive -LiteralPath (Join-Path $dir 'marked.zip') -DestinationPath (Join-Path $dir 'unpacked')
$exe = Get-ChildItem -LiteralPath (Join-Path $dir 'unpacked') -Recurse -Filter 'fennec.exe' | Select-Object -First 1
Write-Host "Expand-Archive, a marked zip's program: $(Said $exe.FullName)"
'@)
        $probeDir = Join-Path $work "$tag-mark"
        New-Item -ItemType Directory -Path $probeDir | Out-Null
        $probeOut = Join-Path $work "$tag-mark.out"
        $rc = Run $shell @('-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-File', $probe, $probeDir, "$base/download/v$Version/$name", $Zip) $probeOut
        Write-Host "    what $tag does with the mark of the web:"
        foreach ($l in (Text $probeOut).Split("`n")) { if ($l.Trim()) { Write-Host "      $($l.Trim())" } }
        Expect "$tag the mark's probe ran" ($rc -eq 0) (Text $probeOut)
    }

    $after = Snapshot
    Expect 'no profile and no user environment value touched' ($before -ceq $after) "before [$before] after [$after]"
} finally {
    if ($running -and -not $running.HasExited) { Stop-Process -Id $running.Id -Force -ErrorAction SilentlyContinue }
    if ($server -and -not $server.HasExited) { Stop-Process -Id $server.Id -Force -ErrorAction SilentlyContinue }
    if ($running) { $running.WaitForExit() }
    if ($server) { $server.WaitForExit() }
    Remove-Item -LiteralPath $work -Recurse -Force -ErrorAction SilentlyContinue
}

if ($script:failed -ne 0) {
    Write-Host "install.ps1: $($script:failed) FAILED"
    exit 1
}
Write-Host 'install.ps1: all passed'
