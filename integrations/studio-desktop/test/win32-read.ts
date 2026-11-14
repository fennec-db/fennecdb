// Run by the Bun the app runs on, never by `node --test` (bun:ffi is Bun's):
// what test/platform.ts asks of Windows, read by the app's own calls
// (src/bun/win32.ts) and written out as UTF-8, not by PowerShell, whose
// runtime crashed on the arm64 runner (platform.ts's `argvOf`). Not a test
// file of its own.
//   command-line <pid>: the process's command line (`processCommandLine`);
//   security <path>: this user's SID, a line, then the file's owner and DACL
//     as SDDL (`pipeSddl`: a pipe is a file object to GetNamedSecurityInfo).

import { pipeSddl, processCommandLine, userSid } from "../src/bun/win32.ts";

const [what, of] = process.argv.slice(2);
if (what === "command-line") process.stdout.write(processCommandLine(Number(of)).commandLine);
else if (what === "security" && of) process.stdout.write(`${userSid()}\n${pipeSddl(of)}`);
else throw new Error(`asked ${what}: command-line <pid> or security <path>`);
