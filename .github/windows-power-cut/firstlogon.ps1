# Run once at the first logon, from the config CD (its letter is $args[0]):
# makes C:\ci, where the host puts fennec-server.exe and its token, and
# starts sshd -- installed with Windows Server 2025, off by default; added
# from Windows Update if not.
function Say($m) { cmd /c "echo GUEST $m >COM1" }
Say "firstlogon.ps1 from $($args[0])"
New-Item -ItemType Directory -Force C:\ci | Out-Null
netsh advfirewall set allprofiles state off | Out-Null
if (-not (Get-Service sshd -ErrorAction SilentlyContinue)) {
    Say "sshd absent, adding the capability"
    $r = Add-WindowsCapability -Online -Name OpenSSH.Server~~~~0.0.1.0
    Say "capability restart=$($r.RestartNeeded)"
}
Set-Service sshd -StartupType Automatic
Start-Service sshd
Say "sshd $((Get-Service sshd).Status)"
$ip = (Get-NetIPAddress -AddressFamily IPv4 | Where-Object { $_.IPAddress -ne '127.0.0.1' } | Select-Object -First 1).IPAddress
Say "ip $ip"
# A cut before Windows marks a boot good makes the next boot the second
# failed one in a row, and that one waits for a key: WinRE's "Recovery"
# screen (the third run's cut 2, 40 s after its boot), or with recovery off
# the boot manager's "failed to turn off properly" (the fourth run's). A rig
# that cuts again and again boots on. Set here, not in specialize: set there,
# bootstatuspolicy was gone by the first logon.
bcdedit /set '{default}' bootstatuspolicy ignoreallfailures | Out-Null
bcdedit /set '{default}' recoveryenabled no | Out-Null
bcdedit /enum '{default}' | Select-String 'bootstatuspolicy|recoveryenabled' | ForEach-Object { Say "bcd $($_.Line -replace '\s+', ' ')" }
Say "firstlogon done"
