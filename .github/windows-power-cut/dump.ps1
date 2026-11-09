# What a server that could not open C:\d\t.fennec after a cut found there,
# for a run that keeps no file (vm.sh's file_dump): the file's length, and
# its last 64 KiB and the 4 KiB around byte $At -- where the server's error
# says it failed -- copied into the home directory for the host to fetch.
# Run 42533808035 said "unknown record kind 0" and nothing else.
param([long]$At = -1)
$f = [IO.File]::Open('C:\d\t.fennec', 'Open', 'Read', 'ReadWrite')
try {
    "length $($f.Length)"
    function Take([long]$From, [long]$Count, [string]$Name) {
        $From = [Math]::Max(0, $From)
        $Count = [Math]::Max(0, [Math]::Min($Count, $f.Length - $From))
        $b = New-Object byte[] $Count
        $f.Position = $From
        $got = 0
        while ($got -lt $Count) {
            $k = $f.Read($b, $got, $Count - $got)
            if ($k -le 0) { break }
            $got += $k
        }
        [IO.File]::WriteAllBytes((Join-Path $HOME $Name), $b)
        "$Name from $From, $got bytes"
    }
    Take ($f.Length - 65536) 65536 'tail.bin'
    if ($At -ge 0) { Take ($At - 2048) 4096 'at.bin' }
} finally {
    $f.Close()
}
