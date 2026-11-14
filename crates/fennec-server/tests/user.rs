//! `--user` and `FENNEC_USER` as a server not started as root meets them:
//! the image sets the variable for every container, so `docker run --user`,
//! a pod's `runAsUser` and systemd's `User=` start a server it must pass
//! over, and a `--user` it cannot become is refused before anything is
//! opened. What root does with them -- the volume handed over, root
//! dropped -- runs in a container (docs/design/integrations.md#the-images-user);
//! the rules it hands a directory over by are `fennec_server::user`'s own
//! tests.

use crate::support::{start_env, tmp, try_start_env};

/// The user and group this process runs as, or `None` as root, where
/// these tests would ask what the container's run asks.
#[cfg(unix)]
fn me() -> Option<fennec_server::user::User> {
    let (euid, me) = fennec_server::user::current();
    (euid != 0).then_some(me)
}

#[cfg(unix)]
#[test]
fn the_images_variable_is_passed_over_by_a_server_not_started_as_root() {
    let Some(me) = me() else { return };
    let file = tmp("user-env", "data.fennec");
    // Whatever it says: started as anyone but root, it is not even read.
    for value in ["65534:65534", "nonsense"] {
        let server = start_env(
            &["--file", file.to_str().unwrap()],
            &[("FENNEC_USER", value)],
        );
        let (code, log) = server.terminate();
        assert_eq!(code, 0, "{log}");
        assert!(!log.contains("running as"), "{log}");
    }
    // The flag naming who the process is already: nothing to become.
    let own = me.to_string();
    let server = start_env(&["--file", file.to_str().unwrap(), "--user", &own], &[]);
    let (code, log) = server.terminate();
    assert_eq!(code, 0, "{log}");
}

#[cfg(unix)]
#[test]
fn a_user_the_server_cannot_become_is_refused_before_anything_is_opened() {
    let Some(me) = me() else { return };
    let file = tmp("user-flag", "data.fennec");
    let other = format!("{}:{}", me.uid.wrapping_add(1), me.gid);
    for (args, said) in [
        (
            vec!["--user", other.as_str()],
            "only a server started as root",
        ),
        (vec!["--user", "0:0"], "names root"),
        (vec!["--user", "nobody"], "expects <uid>:<gid>"),
    ] {
        let mut all = vec!["--file", file.to_str().unwrap()];
        all.extend(args);
        let log = match try_start_env(&all, &[]) {
            Ok(_) => panic!("{all:?} started"),
            Err(log) => log,
        };
        assert!(log.contains(said), "{all:?}: {log}");
        assert!(!file.exists(), "{all:?} made the file");
        assert!(
            !file.with_extension("fennec.lock").exists(),
            "{all:?} took the lock"
        );
    }
}

#[cfg(windows)]
#[test]
fn a_windows_server_takes_no_user() {
    let file = tmp("user-windows", "data.fennec");
    let log = match try_start_env(&["--file", file.to_str().unwrap(), "--user", "1:1"], &[]) {
        Ok(_) => panic!("started with --user"),
        Err(log) => log,
    };
    assert!(log.contains("--user is a Unix server's"), "{log}");
    // The variable is no flag: passed over.
    let server = start_env(
        &["--file", file.to_str().unwrap()],
        &[("FENNEC_USER", "65534:65534")],
    );
    let (code, log) = server.terminate();
    assert_eq!(code, 0, "{log}");
}
