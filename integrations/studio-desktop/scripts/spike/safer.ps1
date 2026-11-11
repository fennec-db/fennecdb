# TEMPORARY (spike): starts a command line with this user's token made a
# normal user's (SAFER NORMALUSER, Administrators deny-only) at Medium
# integrity, and prints its process id. The runners run elevated, and
# WebView2 ignores WEBVIEW2_* variables in an elevated host.
param([Parameter(Mandatory = $true)][string]$CommandLine)
$ErrorActionPreference = 'Stop'
Add-Type -TypeDefinition @"
using System;
using System.ComponentModel;
using System.Runtime.InteropServices;
public static class Safer {
  [DllImport("advapi32.dll", SetLastError=true)] static extern bool SaferCreateLevel(uint scope, uint level, uint flags, out IntPtr h, IntPtr reserved);
  [DllImport("advapi32.dll", SetLastError=true)] static extern bool SaferComputeTokenFromLevel(IntPtr level, IntPtr inToken, out IntPtr outToken, uint flags, IntPtr reserved);
  [DllImport("advapi32.dll", SetLastError=true)] static extern bool SaferCloseLevel(IntPtr h);
  [DllImport("advapi32.dll", SetLastError=true, CharSet=CharSet.Unicode)] static extern bool ConvertStringSidToSid(string s, out IntPtr sid);
  [DllImport("advapi32.dll", SetLastError=true)] static extern bool SetTokenInformation(IntPtr tok, int cls, ref TML info, int len);
  [DllImport("advapi32.dll", SetLastError=true)] static extern uint GetLengthSid(IntPtr sid);
  [StructLayout(LayoutKind.Sequential)] struct TML { public IntPtr Sid; public uint Attributes; }
  [StructLayout(LayoutKind.Sequential, CharSet=CharSet.Unicode)] struct SI { public int cb; public string r; public string desk; public string title; public int x, y, xs, ys, xc, yc, fill, flags; public short show, r2; public IntPtr r3, i, o, e; }
  [StructLayout(LayoutKind.Sequential)] struct PI { public IntPtr p, t; public int pid, tid; }
  [DllImport("advapi32.dll", SetLastError=true, CharSet=CharSet.Unicode)] static extern bool CreateProcessAsUser(IntPtr tok, string app, string cmd, IntPtr pa, IntPtr ta, bool inherit, uint flags, IntPtr env, string dir, ref SI si, out PI pi);
  public static int Start(string cmd) {
    IntPtr level;
    if (!SaferCreateLevel(2, 0x20000, 1, out level, IntPtr.Zero)) throw new Win32Exception(Marshal.GetLastWin32Error(), "SaferCreateLevel");
    IntPtr tok;
    if (!SaferComputeTokenFromLevel(level, IntPtr.Zero, out tok, 0, IntPtr.Zero)) throw new Win32Exception(Marshal.GetLastWin32Error(), "SaferComputeTokenFromLevel");
    SaferCloseLevel(level);
    IntPtr sid;
    if (!ConvertStringSidToSid("S-1-16-8192", out sid)) throw new Win32Exception(Marshal.GetLastWin32Error(), "ConvertStringSidToSid");
    TML tml = new TML(); tml.Sid = sid; tml.Attributes = 0x20;
    if (!SetTokenInformation(tok, 25, ref tml, Marshal.SizeOf(typeof(TML)) + (int)GetLengthSid(sid))) throw new Win32Exception(Marshal.GetLastWin32Error(), "SetTokenInformation");
    SI si = new SI(); si.cb = Marshal.SizeOf(typeof(SI)); si.desk = "winsta0\\default";
    PI pi;
    if (!CreateProcessAsUser(tok, null, cmd, IntPtr.Zero, IntPtr.Zero, false, 0x08000000, IntPtr.Zero, null, ref si, out pi)) throw new Win32Exception(Marshal.GetLastWin32Error(), "CreateProcessAsUser");
    return pi.pid;
  }
}
"@
[Safer]::Start($CommandLine)
