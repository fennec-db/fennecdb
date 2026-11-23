//! The mode of the files a server makes: its owner's alone whatever the
//! umask, a file there keeping the mode an operator gave it through every
//! replacement. Up to 0.2.0 each took the umask's -- 0644 under 022, 0666
//! under 000 -- and every user of the machine read a node's tenants, their
//! archives, backups and the audit log.

#![cfg(unix)]

use fennec_core::prelude::*;
use fennec_http::archive::{self, Archive, Target};
use fennec_http::replication::{self, fresh_id, Replication, Upstream};
use fennec_http::seal::Key;
use fennec_http::{Config, Server};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

/// The umask the child runs under, in octal; unset in the test run itself.
const UMASK: &str = "FENNEC_TEST_UMASK";
const TOKEN: &str = "t";

/// `child`'s output once it exits, or once `within` has passed, killed: a
/// child that hangs fails the test with what it printed rather than hold
/// it, and the run, for ever.
fn bounded(child: Child, within: Duration) -> Output {
    let pid = child.id().to_string();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || tx.send(child.wait_with_output().unwrap()));
    match rx.recv_timeout(within) {
        Ok(out) => out,
        Err(_) => {
            // Not reaped until its output is read, so the id is still its.
            let _ = Command::new("kill").args(["-KILL", &pid]).status();
            let out = rx.recv().unwrap();
            panic!(
                "the child ran past {within:?} and was killed:\n{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }
}

fn mode(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

fn chmod(path: &Path, mode: u32) {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

/// Waits, 30 s at most, for a file in `dir` named `prefix`...`suffix`:
/// not the side file it is written into.
fn appears(dir: &Path, prefix: &str, suffix: &str) -> std::path::PathBuf {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let found = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .find(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                name.starts_with(prefix) && name.ends_with(suffix)
            });
        if let Some(e) = found {
            return e.path();
        }
        assert!(
            Instant::now() < deadline,
            "no {prefix} file in {}",
            dir.display()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// A primary serving `file` in this process, writes sent to its archives.
fn primary(file: &Path) -> (Arc<RwLock<Database>>, String) {
    let (mut db, feed) =
        replication::open(file.to_str().unwrap(), replication::DEFAULT_BUFFER).unwrap();
    db.fork(fresh_id()).unwrap();
    let db = Arc::new(RwLock::new(db));
    let cfg = Config {
        addr: "127.0.0.1:0".into(),
        sync_on_write: true,
        ..Config::default()
    };
    let server = Server::new(Arc::clone(&db), cfg).with_replication(Replication::new(
        Some(TOKEN.into()),
        Some(feed),
        None,
    ));
    let listener = server.bind().unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        let _ = server.serve_on(listener);
    });
    (db, url)
}

/// A write as a client makes one: executed, then fsynced, which sends it.
fn exec(db: &RwLock<Database>, sql: &str) {
    let durable = {
        let mut g = db.write().unwrap();
        g.execute(&fennec_ql::parse_one(sql).unwrap()).unwrap();
        g.flush().unwrap()
    };
    if let Some(d) = durable {
        d().unwrap();
    }
}

/// Every file the server's library makes -- the audit log; a node's
/// directory, a tenant's file, lock and identity, its quota, an installed
/// tenant, the schedule hint; an archive's directory, images and segments;
/// a restore, a backup into a file, sealed or not, and an unseal -- is its
/// owner's alone under a umask of 022 and of 000, and a file there keeps
/// its mode through a quota written again, the hint written again, a
/// restore, a backup and an unseal over it (what `--force` runs). The
/// umask is the process's: set here it would change every file the tests
/// beside this one make, so it runs again in a child, set by `sh` before
/// its exec, which runs this test alone.
#[test]
fn files_are_made_their_owners_alone_and_keep_their_mode() {
    let Ok(mask) = std::env::var(UMASK) else {
        for mask in ["022", "000"] {
            let child = std::process::Command::new("/bin/sh")
                .args(["-c", "umask \"$0\" && exec \"$@\"", mask])
                .arg(std::env::current_exe().unwrap())
                .args([
                    "modes::files_are_made_their_owners_alone_and_keep_their_mode",
                    "--exact",
                    "--test-threads=1",
                ])
                .env(UMASK, mask)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            let out = bounded(child, Duration::from_secs(300));
            let said = String::from_utf8_lossy(&out.stdout);
            assert!(
                out.status.success() && said.contains("1 passed"),
                "under umask {mask}:\n{said}{}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        return;
    };
    let mask = u32::from_str_radix(&mask, 8).unwrap();
    let d = std::env::temp_dir().join(format!("fennec-modes-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    // The umask is the one asked: a plain create takes it.
    std::fs::write(d.join("probe"), b"").unwrap();
    assert_eq!(mode(&d.join("probe")), 0o666 & !mask, "the child's umask");

    // The audit log, and one an operator made.
    fennec_http::audit::open(&d.join("audit.log")).unwrap();
    assert_eq!(mode(&d.join("audit.log")), 0o600, "the audit log");
    std::fs::write(d.join("kept.log"), b"").unwrap();
    chmod(&d.join("kept.log"), 0o640);
    fennec_http::audit::open(&d.join("kept.log")).unwrap();
    assert_eq!(mode(&d.join("kept.log")), 0o640, "an audit log there");

    // A node's directory and what it keeps of a tenant.
    let dir = d.join("tenants");
    let tenants = fennec_http::tenants::Tenants::new(&dir).unwrap();
    assert_eq!(mode(&dir), 0o700, "a node's directory");
    tenants.create("acme").unwrap();
    for name in ["acme.fennec", "acme.fennec.lock", "acme.instance"] {
        assert_eq!(mode(&dir.join(name)), 0o600, "a tenant's {name}");
    }
    let set = |text: &str| fennec_http::quota::Set::parse(text).unwrap();
    tenants.set_quota("acme", set(r#"{"rows": 10}"#)).unwrap();
    assert_eq!(mode(&dir.join("acme.quota")), 0o600, "a tenant's quota");
    chmod(&dir.join("acme.quota"), 0o640);
    tenants.set_quota("acme", set(r#"{"rows": 20}"#)).unwrap();
    assert_eq!(
        mode(&dir.join("acme.quota")),
        0o640,
        "a quota written again"
    );
    let image = tenants.export("acme").unwrap();
    tenants.import("beta", &image).unwrap();
    assert_eq!(mode(&dir.join("beta.fennec")), 0o600, "a tenant installed");
    fennec_http::schedule::hint::write(&dir, "").unwrap();
    let hint = dir.join(fennec_http::schedule::hint::FILE);
    assert_eq!(mode(&hint), 0o600, "the schedule hint");
    chmod(&hint, 0o640);
    fennec_http::schedule::hint::write(&dir, "").unwrap();
    assert_eq!(mode(&hint), 0o640, "the hint written again");
    // Let go as a node lets a tenant go, each closing its file: never
    // `shutdown`, which leaks every open tenant's write lock for the
    // process's exit, so a drop after it waits on that lock for ever.
    drop(tenants);
    let root = d.join("node-archive");
    fennec_http::node_archive::NodeArchive::new(&root, None).unwrap();
    assert_eq!(mode(&root), 0o700, "a node's archive");

    // An archive's directory, images and segments.
    let (db, url) = primary(&d.join("p.fennec"));
    exec(&db, "create collection c (x int)");
    let arch = d.join("archive");
    let stop = Arc::new(AtomicBool::new(false));
    let archiver = {
        let (stop, arch, url) = (Arc::clone(&stop), arch.clone(), url.clone());
        std::thread::spawn(move || {
            let upstream = Upstream::new(&url, TOKEN.into()).unwrap();
            Archive::new(&arch)?.follow(&upstream, &stop, &|_| {})
        })
    };
    let image = appears(&arch, "image-", ".fennec");
    exec(&db, "put c {x: 1}");
    let segment = appears(&arch, "writes-", ".log");
    assert_eq!(mode(&arch), 0o700, "an archive's directory");
    assert_eq!(mode(&image), 0o600, "an archive's image");
    assert_eq!(mode(&segment), 0o600, "an archive's segment");

    // A restore, and one over a file an operator gave a group.
    let a = Archive::new(&arch).unwrap();
    let out = d.join("restored.fennec");
    let restore = |out: &Path| {
        let deadline = Instant::now() + Duration::from_secs(30);
        let end = db.read().unwrap().change_seq();
        while a.restore(out, Target::End).ok().map(|r| r.seq) != Some(end) {
            assert!(
                Instant::now() < deadline,
                "the archive did not reach the end"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    };
    restore(&out);
    assert_eq!(mode(&out), 0o600, "a restore");
    chmod(&out, 0o640);
    restore(&out);
    assert_eq!(mode(&out), 0o640, "a restore over a file");
    stop.store(true, Ordering::SeqCst);
    archiver.join().unwrap().unwrap();
    // From an image an earlier release made with the umask's mode.
    chmod(&image, 0o644);
    let wide = d.join("from-wide.fennec");
    a.restore(&wide, Target::End).unwrap();
    assert_eq!(mode(&wide), 0o600, "a restore from an image wider");

    // A backup into a file, sealed or not, and its unseal.
    let upstream = Upstream::new(&url, TOKEN.into()).unwrap();
    let key = Key::from_hex(&"ab".repeat(32)).unwrap();
    let plain = d.join("backup.fennec");
    let sealed = d.join("backup.sealed");
    let opened = d.join("unsealed.fennec");
    archive::backup(&upstream, &plain, None).unwrap();
    archive::backup(&upstream, &sealed, Some(&key)).unwrap();
    archive::unseal(&sealed, &key, &opened).unwrap();
    for (file, what) in [
        (&plain, "a backup"),
        (&sealed, "a sealed backup"),
        (&opened, "an unseal"),
    ] {
        assert_eq!(mode(file), 0o600, "{what}");
        chmod(file, 0o640);
    }
    archive::backup(&upstream, &plain, None).unwrap();
    archive::backup(&upstream, &sealed, Some(&key)).unwrap();
    archive::unseal(&sealed, &key, &opened).unwrap();
    for (file, what) in [
        (&plain, "a backup"),
        (&sealed, "a sealed backup"),
        (&opened, "an unseal"),
    ] {
        assert_eq!(mode(file), 0o640, "{what} over a file");
    }
    std::fs::remove_dir_all(&d).unwrap();
}

/// The directory a child of [`verify_writes_nothing_another_user_reaches`]
/// is given as its temp directory; unset in the test run itself.
const TEMP: &str = "FENNEC_TEST_TEMP";

/// `fennec verify` restores an archive's end where no other user of the
/// machine reaches it, and writes nothing through a name another user put
/// first in the temp directory. Up to this fix it restored into
/// `<temp>/fennec-verify-<pid>-<n>.fennec`: another user put that name
/// first as a link to a `0644` file of theirs, in a directory of theirs,
/// and the restore followed the link, took the file's mode and renamed the
/// archive's plaintext over it -- under root's `FENNEC_KEY_FILE`, a sealed
/// archive's rows -- the cleanup removing the link alone. Here the links
/// name files of the test's own, `0644`, in a directory of its own. It
/// runs in a child of its own, alone, whose temp directory (`TMPDIR`) is
/// one this test made: the name an earlier verify took counted the
/// process's verifies, so the child's first is the one planted, and no
/// other test's files are in the directory it lists.
#[test]
fn verify_writes_nothing_another_user_reaches() {
    let Ok(temp) = std::env::var(TEMP) else {
        let temp = std::env::temp_dir().join(format!("fennec-verify-temp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&temp);
        std::fs::create_dir_all(&temp).unwrap();
        let child = Command::new(std::env::current_exe().unwrap())
            .args([
                "modes::verify_writes_nothing_another_user_reaches",
                "--exact",
                "--test-threads=1",
            ])
            .env(TEMP, &temp)
            .env("TMPDIR", &temp)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let out = bounded(child, Duration::from_secs(300));
        let said = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success() && said.contains("1 passed"),
            "{said}{}",
            String::from_utf8_lossy(&out.stderr)
        );
        std::fs::remove_dir_all(&temp).unwrap();
        return;
    };
    let temp = Path::new(&temp);
    assert_eq!(std::env::temp_dir(), temp, "the child's temp directory");
    let d = temp.join("work");
    std::fs::create_dir_all(&d).unwrap();

    // An archive of a database holding a row.
    let (db, url) = primary(&d.join("p.fennec"));
    exec(&db, "create collection c (x text)");
    let arch = d.join("archive");
    let stop = Arc::new(AtomicBool::new(false));
    let archiver = {
        let (stop, arch, url) = (Arc::clone(&stop), arch.clone(), url.clone());
        std::thread::spawn(move || {
            let upstream = Upstream::new(&url, TOKEN.into()).unwrap();
            Archive::new(&arch)?.follow(&upstream, &stop, &|_| {})
        })
    };
    appears(&arch, "image-", ".fennec");
    exec(&db, "put c {x: \"the archive's row\"}");
    let end = db.read().unwrap().change_seq();
    let deadline = Instant::now() + Duration::from_secs(30);
    let out = d.join("reached.fennec");
    while Archive::new(&arch)
        .and_then(|a| a.restore(&out, Target::End))
        .ok()
        .map(|r| r.seq)
        != Some(end)
    {
        assert!(
            Instant::now() < deadline,
            "the archive did not reach the end"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    stop.store(true, Ordering::SeqCst);
    archiver.join().unwrap().unwrap();

    // Another user's files, as they would be put: `0644`, in a directory
    // of theirs, and a link at each name a verify restored into.
    let theirs = d.join("theirs");
    std::fs::create_dir(&theirs).unwrap();
    let planted: Vec<(std::path::PathBuf, u64)> = (0..4)
        .map(|n| {
            let target = theirs.join(format!("x-{n}.fennec"));
            std::fs::write(&target, b"theirs").unwrap();
            chmod(&target, 0o644);
            let name = format!("fennec-verify-{}-{n}.fennec", std::process::id());
            std::os::unix::fs::symlink(&target, temp.join(name)).unwrap();
            let ino = std::os::unix::fs::MetadataExt::ino(&std::fs::metadata(&target).unwrap());
            (target, ino)
        })
        .collect();
    let listed = |dir: &Path| -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    };
    let (temp_before, theirs_before) = (listed(temp), listed(&theirs));

    let verified = Archive::new(&arch).unwrap().verify().unwrap();
    assert_eq!(verified.last, end);
    for (target, ino) in &planted {
        assert_eq!(
            std::fs::read(target).unwrap(),
            b"theirs",
            "verify wrote over {}, which a link at its name named",
            target.display()
        );
        let m = std::fs::metadata(target).unwrap();
        assert_eq!(std::os::unix::fs::MetadataExt::ino(&m), *ino);
        assert_eq!(mode(target), 0o644);
    }
    assert_eq!(listed(&theirs), theirs_before, "verify wrote beside them");
    // What verify wrote went with its directory, and it removed nothing
    // it did not make.
    assert_eq!(listed(temp), temp_before, "the temp directory after verify");
    std::fs::remove_dir_all(&d).unwrap();
}
