//! A test build's clock (`--features test-clock`, which `make chaos`'s image
//! is built with and no release is): the file `FENNEC_TEST_CLOCK` names
//! shifts every open tenant's `now()` by the milliseconds it holds. Built
//! without the feature -- every build CI tests and every release -- the
//! variable is nothing: the node says no word of it, and `now()` is the
//! system's.

use crate::support::{column, start_env, tmp, Http};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const ADMIN: &str = "admin-token";

/// A hundred years of 365.25 days: a year no clock here reads.
const SHIFT: i64 = 3_155_760_000_000;

/// The year the system's clock reads, a day either side of New Year's
/// aside (`within_a_year`).
fn this_year() -> i64 {
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    1970 + ms / 31_557_600_000
}

fn within_a_year(a: i64, b: i64) -> bool {
    (a - b).abs() <= 1
}

/// The year of `now()` as the tenant answers it, in the row the `put` wrote.
fn year_of_now(c: &mut Http, id: u32) -> i64 {
    let body = c
        .query_at(
            "/t/acme",
            &format!("put t {{id: {id}, at: now()}} returning at"),
        )
        .unwrap_or_else(|e| panic!("put: {e:?}"));
    let at = column(&body, "at");
    let at = at.first().unwrap_or_else(|| panic!("no `at` in {body}"));
    at.get(..4)
        .and_then(|y| y.parse().ok())
        .unwrap_or_else(|| panic!("no year in {at}"))
}

#[test]
fn a_test_clock_shifts_now_only_in_a_test_build() {
    let dir = tmp("clock", "node");
    std::fs::create_dir_all(&dir).unwrap();
    let file = tmp("clock", "shift");
    std::fs::write(&file, SHIFT.to_string()).unwrap();
    let node = start_env(
        &["--admin-token", ADMIN, "--dir", dir.to_str().unwrap()],
        &[("FENNEC_TEST_CLOCK", file.to_str().unwrap())],
    );
    let made = node
        .http()
        .with_token(ADMIN)
        .ask("PUT", "/_admin/tenants/acme", "");
    assert_eq!(made.status, 201, "{}", made.body);
    let mut c = node.http();
    c.query_at("/t/acme", "create collection t (at timestamp)")
        .unwrap();
    let year = this_year();
    // The line a test build's node says as it starts.
    let said = node.log.lock().unwrap().contains("test-clock: ");
    if cfg!(feature = "test-clock") {
        assert!(said, "{}", node.log.lock().unwrap());
        // The shift reaches the open tenant within the clock's step: the
        // year looked for, not a time slept.
        let mut id = 0;
        let mut until = |want: i64, what: &str| {
            let end = Instant::now() + Duration::from_secs(30);
            loop {
                id += 1;
                let y = year_of_now(&mut c, id);
                if within_a_year(y, want) {
                    return;
                }
                assert!(Instant::now() < end, "now() never {what}: the year {y}");
                std::thread::sleep(Duration::from_millis(10));
            }
        };
        until(year + 100, "moved a hundred years on");
        // Emptied, the file shifts nothing: the system's clock again.
        std::fs::write(&file, "").unwrap();
        until(year, "came back");
    } else {
        // No thread follows the file, so there is no moment to wait for.
        assert!(!said, "{}", node.log.lock().unwrap());
        let y = year_of_now(&mut c, 1);
        assert!(
            within_a_year(y, year),
            "now() is in {y}, the system's in {year}"
        );
    }
    drop(node);
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_file(&file);
}
