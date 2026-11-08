//! A secret's file, its owner's alone, on Windows (docs/design/windows.md,
//! 7h): `fennec key`'s key file, a tenant's `<t>.hooks`, and the TLS key a
//! server is told of. A Unix makes such a file with mode 0600; on Windows a
//! file inherits its directory's ACL, which `C:\ProgramData` opens to every
//! user of the machine, and a mode is nothing. So the file is made through
//! a handle shared with no reader ([`options`]) -- no other open may read
//! it meanwhile -- given a protected DACL granting this
//! process's user alone ([`restrict`]), advapi32's `SetSecurityInfo` on
//! that handle, and only then written. A reader whose handle was open
//! before keeps what it was granted, as a Unix descriptor keeps its file:
//! the secret goes in once none can be. The TLS key, which an operator
//! makes, is read rather than made: its DACL is read for the groups that
//! hold every user ([`open_to`]), and the server warns of it as a Unix
//! server warns of a mode.
//!
//! advapi32's calls are declared here as `fennec_abi::lock` declares
//! kernel32's -- the crates take no `windows-sys` -- and the library loads
//! no window library: the server already loaded advapi32 (fennec-server's
//! `the_server_loads_no_window_library` prints its modules), and one that
//! loaded user32 or gdi32 would no longer be told of a logoff or the
//! system's shutdown (docs/design/windows.md, decision 4).

use std::ffi::c_void;
use std::fs::{File, OpenOptions};
use std::io;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::path::Path;

/// winnt.h's access rights.
const GENERIC_WRITE: u32 = 0x4000_0000;
const WRITE_DAC: u32 = 0x0004_0000;
const READ_CONTROL: u32 = 0x0002_0000;
/// A file's every right: `icacls`' `(F)`.
const FILE_ALL_ACCESS: u32 = 0x001F_01FF;
/// What lets an ACE's holder read a file's bytes: `FILE_READ_DATA` and the
/// generic rights that map onto it.
const READS: u32 = 0x0000_0001 | 0x8000_0000 | 0x1000_0000;
const FILE_SHARE_WRITE: u32 = 0x0000_0002;
const FILE_SHARE_DELETE: u32 = 0x0000_0004;
const TOKEN_QUERY: u32 = 0x0008;
/// `TOKEN_INFORMATION_CLASS::TokenUser`.
const TOKEN_USER: u32 = 1;
const ACL_REVISION: u32 = 2;
/// `SE_OBJECT_TYPE::SE_FILE_OBJECT`.
const SE_FILE_OBJECT: u32 = 1;
const DACL_SECURITY_INFORMATION: u32 = 0x0000_0004;
/// The DACL inherits nothing from the directory, now or at a later change
/// of the directory's.
const PROTECTED_DACL_SECURITY_INFORMATION: u32 = 0x8000_0000;
/// `SECURITY_DESCRIPTOR_CONTROL`'s `SE_DACL_PROTECTED`.
const SE_DACL_PROTECTED: u16 = 0x1000;
const ACCESS_ALLOWED_ACE_TYPE: u8 = 0;
const ACCESS_DENIED_ACE_TYPE: u8 = 1;
/// An ACE that applies to what the object holds, not to the object.
const INHERIT_ONLY_ACE: u8 = 0x08;

/// The well-known SIDs that hold every user of a machine, as their bytes
/// are laid out -- revision 1, the count of sub-authorities, the authority
/// big-endian, each sub-authority little-endian -- and as a warning names
/// them, widest first.
const OPEN: [(&[u8], &str); 3] = [
    // S-1-1-0, Everyone.
    (
        &[1, 1, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0],
        "every user (Everyone)",
    ),
    // S-1-5-11, Authenticated Users.
    (
        &[1, 1, 0, 0, 0, 0, 0, 5, 11, 0, 0, 0],
        "every signed-in user (Authenticated Users)",
    ),
    // S-1-5-32-545, BUILTIN\Users.
    (
        &[1, 2, 0, 0, 0, 0, 0, 5, 32, 0, 0, 0, 0x21, 0x02, 0, 0],
        "the machine's users (Users)",
    ),
];

#[link(name = "kernel32")]
extern "system" {
    fn GetCurrentProcess() -> *mut c_void;
    fn CloseHandle(handle: *mut c_void) -> i32;
    fn LocalFree(memory: *mut c_void) -> *mut c_void;
}

#[link(name = "advapi32")]
extern "system" {
    fn OpenProcessToken(process: *mut c_void, access: u32, token: *mut *mut c_void) -> i32;
    fn GetTokenInformation(
        token: *mut c_void,
        class: u32,
        info: *mut c_void,
        len: u32,
        needed: *mut u32,
    ) -> i32;
    fn GetLengthSid(sid: *const c_void) -> u32;
    fn InitializeAcl(acl: *mut c_void, len: u32, revision: u32) -> i32;
    fn AddAccessAllowedAce(acl: *mut c_void, revision: u32, mask: u32, sid: *const c_void) -> i32;
    fn SetSecurityInfo(
        handle: *mut c_void,
        kind: u32,
        what: u32,
        owner: *const c_void,
        group: *const c_void,
        dacl: *const c_void,
        sacl: *const c_void,
    ) -> u32;
    fn GetSecurityInfo(
        handle: *mut c_void,
        kind: u32,
        what: u32,
        owner: *mut *mut c_void,
        group: *mut *mut c_void,
        dacl: *mut *mut c_void,
        sacl: *mut *mut c_void,
        descriptor: *mut *mut c_void,
    ) -> u32;
    fn GetSecurityDescriptorControl(
        descriptor: *const c_void,
        control: *mut u16,
        revision: *mut u32,
    ) -> i32;
}

/// `opts` set to open a secret's file for [`restrict`]: written, its DACL
/// set through the handle (`WRITE_DAC`), and shared with no reader, so no
/// other open reads it before its DACL is set. Shared for deletion and
/// writing, which the hooks' sidecar asks with its handle still open: its
/// rename into its name, and `fs::sync_dir`'s flush of it after, which
/// opens it to write -- refused, that flush would be tried again for a
/// second and then passed over. A writer is no reader, and one the
/// directory lets write a file it did not make could make the file itself
/// before the server, there as on a Unix.
pub fn options(opts: &mut OpenOptions) -> &mut OpenOptions {
    opts.access_mode(GENERIC_WRITE | WRITE_DAC | READ_CONTROL)
        .share_mode(FILE_SHARE_WRITE | FILE_SHARE_DELETE)
}

/// Gives `file`, opened through [`options`], a protected DACL with one
/// entry: this process's user, every right. Nothing inherited from the
/// directory is left, nor taken from it later.
pub fn restrict(file: &File) -> io::Result<()> {
    let user = user_sid()?;
    let sid = user.sid();
    // SAFETY: a SID the token's answer holds, alive while `user` is.
    let sid_len = unsafe { GetLengthSid(sid) };
    // `ACL`, 8 bytes, then `ACCESS_ALLOWED_ACE` -- its header, its mask and
    // the SID where its last `DWORD` begins -- in `DWORD`s.
    let len = 8 + 8 + sid_len;
    let mut acl = vec![0u32; len.div_ceil(4) as usize];
    // SAFETY: a buffer of at least `len` bytes, aligned to a `DWORD`, and a
    // valid SID; neither call keeps a pointer.
    unsafe {
        if InitializeAcl(acl.as_mut_ptr().cast(), len, ACL_REVISION) == 0
            || AddAccessAllowedAce(acl.as_mut_ptr().cast(), ACL_REVISION, FILE_ALL_ACCESS, sid) == 0
        {
            return Err(io::Error::last_os_error());
        }
    }
    // SAFETY: the file's own handle, open while `file` is borrowed, and an
    // ACL the call copies.
    let set = unsafe {
        SetSecurityInfo(
            file.as_raw_handle(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            std::ptr::null(),
            std::ptr::null(),
            acl.as_ptr().cast(),
            std::ptr::null(),
        )
    };
    match set {
        0 => Ok(()),
        e => Err(io::Error::from_raw_os_error(e as i32)),
    }
}

/// Who besides its owner may read the file at `path`, by its DACL: one of
/// the groups that hold every user, named, where an entry allows it to
/// read and none before it denies that; a file with no DACL at all is
/// every user's. `None` where none of them may.
pub fn open_to(path: &Path) -> io::Result<Option<&'static str>> {
    let dacl = Dacl::of(path)?;
    let Some(acl) = dacl.bytes() else {
        return Ok(Some(OPEN[0].1));
    };
    Ok(open_in(acl))
}

/// Whether the file at `path` is its owner's alone as [`restrict`] makes
/// it: a protected DACL of one entry, allowing this process's user every
/// right. A test's question.
#[doc(hidden)]
pub fn owner_alone(path: &Path) -> io::Result<bool> {
    let dacl = Dacl::of(path)?;
    let user = user_sid()?;
    // SAFETY: as in `restrict`.
    let len = unsafe { GetLengthSid(user.sid()) } as usize;
    // SAFETY: a SID of `len` bytes the token's answer holds.
    let mine = unsafe { std::slice::from_raw_parts(user.sid().cast::<u8>(), len) };
    let Some(acl) = dacl.bytes() else {
        return Ok(false);
    };
    let aces = aces(acl);
    Ok(dacl.protected()
        && aces.len() == 1
        && aces[0].0 == ACCESS_ALLOWED_ACE_TYPE
        && aces[0].2 == FILE_ALL_ACCESS
        && aces[0].3 == mine)
}

/// The groups' entries in `acl`, in order: an allowing one that grants a
/// read open the file to its group, unless a denying one before denied it
/// -- Everyone's denial is every group's.
fn open_in(acl: &[u8]) -> Option<&'static str> {
    let mut denied = [false; OPEN.len()];
    let mut open = [false; OPEN.len()];
    for (kind, flags, mask, sid) in aces(acl) {
        if flags & INHERIT_ONLY_ACE != 0 || mask & READS == 0 {
            continue;
        }
        let Some(i) = OPEN.iter().position(|(s, _)| *s == sid) else {
            continue;
        };
        match kind {
            ACCESS_DENIED_ACE_TYPE if i == 0 => denied = [true; OPEN.len()],
            ACCESS_DENIED_ACE_TYPE => denied[i] = true,
            ACCESS_ALLOWED_ACE_TYPE if !denied[i] => open[i] = true,
            _ => {}
        }
    }
    OPEN.iter()
        .zip(open)
        .find(|(_, o)| *o)
        .map(|((_, who), _)| *who)
}

/// An ACL's entries of the two kinds that name a SID after their mask --
/// allowing and denying -- as (kind, flags, mask, SID). Read by their
/// sizes, never past the ACL's.
fn aces(acl: &[u8]) -> Vec<(u8, u8, u32, &[u8])> {
    let u16_at = |at: usize| {
        acl.get(at..at + 2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
    };
    let size = u16_at(2).map_or(0, usize::from).min(acl.len());
    let count = u16_at(4).unwrap_or(0);
    let mut out = Vec::new();
    let mut at = 8;
    for _ in 0..count {
        let Some(&[kind, flags, s0, s1, ..]) = acl.get(at..size) else {
            break;
        };
        let ace_size = usize::from(u16::from_le_bytes([s0, s1]));
        let ace = &acl[at..size];
        let Some(ace) = ace.get(..ace_size).filter(|_| ace_size >= 8) else {
            break;
        };
        if matches!(kind, ACCESS_ALLOWED_ACE_TYPE | ACCESS_DENIED_ACE_TYPE) && ace.len() >= 12 {
            let mask = u32::from_le_bytes([ace[4], ace[5], ace[6], ace[7]]);
            let sid = &ace[8..];
            // A SID is 8 bytes and 4 a sub-authority.
            let sid_len = 8 + 4 * usize::from(sid[1]);
            if let Some(sid) = sid.get(..sid_len) {
                out.push((kind, flags, mask, sid));
            }
        }
        at += ace_size;
    }
    out
}

/// A file's DACL as `GetSecurityInfo` answers it, freed with it.
struct Dacl {
    descriptor: *mut c_void,
    dacl: *mut c_void,
}

impl Dacl {
    /// Read through a handle that may read the file's security alone, so
    /// no sharing a writer holds refuses it.
    fn of(path: &Path) -> io::Result<Dacl> {
        let file = OpenOptions::new().access_mode(READ_CONTROL).open(path)?;
        let mut d = Dacl {
            descriptor: std::ptr::null_mut(),
            dacl: std::ptr::null_mut(),
        };
        // SAFETY: the file's own handle, and out-pointers the call fills;
        // the descriptor it allocates is freed with `d`.
        let read = unsafe {
            GetSecurityInfo(
                file.as_raw_handle(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut d.dacl,
                std::ptr::null_mut(),
                &mut d.descriptor,
            )
        };
        match read {
            0 => Ok(d),
            e => Err(io::Error::from_raw_os_error(e as i32)),
        }
    }

    /// The ACL's bytes, `None` for no DACL: every right to everyone.
    fn bytes(&self) -> Option<&[u8]> {
        if self.dacl.is_null() {
            return None;
        }
        // SAFETY: an ACL inside the descriptor, which lives as long as
        // `self`; its header's `AclSize` is its length.
        unsafe {
            let size = u16::from_le_bytes(*self.dacl.cast::<u8>().add(2).cast::<[u8; 2]>());
            Some(std::slice::from_raw_parts(
                self.dacl.cast::<u8>(),
                usize::from(size),
            ))
        }
    }

    /// Whether the DACL inherits nothing.
    fn protected(&self) -> bool {
        let (mut control, mut revision) = (0u16, 0u32);
        // SAFETY: the descriptor the read allocated.
        let ok =
            unsafe { GetSecurityDescriptorControl(self.descriptor, &mut control, &mut revision) };
        ok != 0 && control & SE_DACL_PROTECTED != 0
    }
}

impl Drop for Dacl {
    fn drop(&mut self) {
        // SAFETY: allocated by `GetSecurityInfo` for the caller to free.
        unsafe { LocalFree(self.descriptor) };
    }
}

/// This process's user, as its token's `TOKEN_USER` answer holds it.
struct User(Vec<u64>);

impl User {
    /// The SID: `TOKEN_USER`'s first field points into the answer itself.
    fn sid(&self) -> *const c_void {
        // SAFETY: the answer begins with a `SID_AND_ATTRIBUTES`, whose
        // first field is the SID's address, aligned as the `u64`s are.
        unsafe { *self.0.as_ptr().cast::<*const c_void>() }
    }
}

fn user_sid() -> io::Result<User> {
    let mut token = std::ptr::null_mut();
    // SAFETY: the process's own pseudo-handle, and a token handle closed
    // below on every path.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut needed = 0u32;
    // SAFETY: a size asked with no buffer; then a buffer of that size.
    unsafe {
        GetTokenInformation(token, TOKEN_USER, std::ptr::null_mut(), 0, &mut needed);
        let mut buf = vec![0u64; (needed as usize).div_ceil(8)];
        let ok = GetTokenInformation(
            token,
            TOKEN_USER,
            buf.as_mut_ptr().cast(),
            needed,
            &mut needed,
        );
        let user = if ok == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(User(buf))
        };
        CloseHandle(token);
        user
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("fennec-owner-only-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(name);
        let _ = std::fs::remove_file(&p);
        p
    }

    /// A file made through `options` and `restrict` is its owner's alone,
    /// written and read back by its owner, renamed into another name as the
    /// hooks' sidecar is, and refused to a reader while it is being made.
    /// A file made as std makes one inherits the temporary directory's ACL,
    /// which `owner_alone` tells apart.
    #[test]
    fn a_secret_s_file_is_its_owner_s_alone() {
        let p = scratch("secret.key");
        let mut f = options(OpenOptions::new().write(true).create_new(true))
            .open(&p)
            .unwrap();
        // No other open reads it before its DACL is set.
        let reader = File::open(&p).unwrap_err();
        assert_eq!(reader.raw_os_error(), Some(32), "{reader}");
        restrict(&f).unwrap();
        f.write_all(b"secret").unwrap();
        drop(f);
        assert!(owner_alone(&p).unwrap());
        assert_eq!(open_to(&p).unwrap(), None);
        let mut back = String::new();
        File::open(&p).unwrap().read_to_string(&mut back).unwrap();
        assert_eq!(back, "secret");
        let renamed = scratch("secret.renamed");
        std::fs::rename(&p, &renamed).unwrap();
        assert!(owner_alone(&renamed).unwrap());
        // Made again over an older file, truncated, as the sidecar's
        // `.writing` is: the DACL is set over what the file had.
        let plain = scratch("plain.key");
        std::fs::write(&plain, b"old").unwrap();
        assert!(!owner_alone(&plain).unwrap());
        let f = options(OpenOptions::new().write(true).create(true).truncate(true))
            .open(&plain)
            .unwrap();
        restrict(&f).unwrap();
        drop(f);
        assert!(owner_alone(&plain).unwrap());
        assert_eq!(std::fs::metadata(&plain).unwrap().len(), 0);
        for q in [renamed, plain] {
            std::fs::remove_file(q).unwrap();
        }
    }

    /// The groups that hold every user, read from an ACL as its bytes lie:
    /// an entry allowing a read opens the file to its group, one denying it
    /// before closes it, one only inherited or allowing no read is nothing.
    #[test]
    fn a_dacl_says_whom_a_file_is_open_to() {
        fn ace(kind: u8, flags: u8, mask: u32, sid: &[u8]) -> Vec<u8> {
            let size = (8 + sid.len()) as u16;
            let mut a = vec![kind, flags];
            a.extend(size.to_le_bytes());
            a.extend(mask.to_le_bytes());
            a.extend(sid);
            a
        }
        fn acl(aces: &[Vec<u8>]) -> Vec<u8> {
            let body: Vec<u8> = aces.concat();
            let mut a = vec![2, 0];
            a.extend(((8 + body.len()) as u16).to_le_bytes());
            a.extend((aces.len() as u16).to_le_bytes());
            a.extend([0, 0]);
            a.extend(body);
            a
        }
        let (everyone, authenticated, users) = (OPEN[0].0, OPEN[1].0, OPEN[2].0);
        // S-1-5-18, SYSTEM; and a user's SID of five sub-authorities.
        let system: &[u8] = &[1, 1, 0, 0, 0, 0, 0, 5, 18, 0, 0, 0];
        let user: &[u8] = &[
            1, 5, 0, 0, 0, 0, 0, 5, 21, 0, 0, 0, 1, 0, 0, 0, 2, 0, 0, 0, 3, 0, 0, 0, 0xe9, 3, 0, 0,
        ];
        let owner = ace(0, 0, FILE_ALL_ACCESS, user);
        assert_eq!(open_in(&acl(std::slice::from_ref(&owner))), None);
        assert_eq!(
            open_in(&acl(&[owner.clone(), ace(0, 0, FILE_ALL_ACCESS, system)])),
            None
        );
        // `C:\ProgramData`'s inherited read and execute for Users.
        let users_read = ace(0, 0x10, 0x0012_00a9, users);
        assert_eq!(
            open_in(&acl(&[owner.clone(), users_read.clone()])),
            Some(OPEN[2].1)
        );
        assert_eq!(
            open_in(&acl(&[
                ace(0, 0, 0x8000_0000, everyone),
                users_read.clone()
            ])),
            Some(OPEN[0].1)
        );
        assert_eq!(
            open_in(&acl(&[ace(0, 0, 0x1000_0000, authenticated)])),
            Some(OPEN[1].1)
        );
        // Denied before, inherited only, or no read: none.
        assert_eq!(
            open_in(&acl(&[ace(1, 0, 1, users), users_read.clone()])),
            None
        );
        assert_eq!(
            open_in(&acl(&[ace(1, 0, 1, everyone), users_read.clone()])),
            None
        );
        assert_eq!(open_in(&acl(&[ace(0, INHERIT_ONLY_ACE, 1, users)])), None);
        assert_eq!(open_in(&acl(&[ace(0, 0, 0x0002_0000, everyone)])), None);
        // Cut short, or claiming more entries than it holds: what is whole.
        let whole = acl(&[owner.clone(), users_read.clone()]);
        assert_eq!(open_in(&whole[..whole.len() - 3]), None);
        let mut more = acl(&[users_read]);
        more[4] = 9;
        assert_eq!(open_in(&more), Some(OPEN[2].1));
    }

    /// A key made as an operator makes one, with whatever its directory
    /// gives, is read for whom it is open to -- the temporary directory's
    /// ACL printed as the warning would name it -- and one granted to
    /// Everyone is every user's.
    #[test]
    fn a_key_everyone_reads_is_open_to_every_user() {
        let p = scratch("key.pem");
        std::fs::write(&p, b"key").unwrap();
        println!(
            "a file in the temporary directory is open to: {:?}",
            open_to(&p).unwrap()
        );
        let f = options(OpenOptions::new().write(true)).open(&p).unwrap();
        restrict(&f).unwrap();
        drop(f);
        assert_eq!(open_to(&p).unwrap(), None);
        let granted = std::process::Command::new("icacls")
            .arg(&p)
            .args(["/grant", "*S-1-1-0:R"])
            .output()
            .unwrap();
        assert!(granted.status.success(), "{granted:?}");
        assert_eq!(open_to(&p).unwrap(), Some(OPEN[0].1));
        assert!(!owner_alone(&p).unwrap());
        std::fs::remove_file(&p).unwrap();
    }
}
