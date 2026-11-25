//! The one register checker every answer `ycsb` is given is held to:
//! `--verify`'s cells (`check`, in `bin/ycsb.rs`) and the soak's hours
//! (`soak.rs`) both ask [`Registers::may_hold`] whether a read could have
//! been answered so, and nothing else decides it.
//!
//! A register is one field of one record, the record's being there (a
//! delete writes 0, an insert 1, a row's `@ttl` passing 0) or a counter
//! ([`Counters`]). Its rule: a read over `[s, e]` may answer the value of a
//! write beside it (begun before the read ended, ended after it began), or
//! of one that could have been the last before it -- ended before the read
//! began, with no certain write begun after it ended that also ended before
//! the read began -- or, where no certain write ended before the read, a
//! value the register held before its kept writes (`held`, or what the
//! caller says it was loaded with).
//!
//! A write is certain unless it may not have landed: one not answered (the
//! connection cut, or a 5xx) may or may not have, at any time from its
//! start; and one answered within [`Kills::loss`] before a kill may be gone
//! for a read ending after that kill (the server answered it from a buffer
//! the kill took: `--sync 250`'s, see `soak.rs` for the bound). An
//! uncertain write is a value a read may answer; it never makes an earlier
//! value impossible.
//!
//! The register is atomic, not merely regular ([`Registers::read`]): reads
//! do not go back. Once a read has answered `v`, a read that began after it
//! ended may answer another value only from a write that can come after
//! that read -- one that ended after it began, or one never answered --
//! until a kill, which may take back what the first read saw: one sent
//! before the later read ended, the server not yet back when the first
//! began (it dies somewhere in that window). So two reads
//! during one write cannot answer its new value and then the old one, and
//! after two overlapping writes the reads cannot flip between their values:
//! the first read that settles on one holds every later read to it.
//! [`Registers::may_hold`] is the rule for a read alone.
//!
//! The registers are let go of as the reads move on ([`Registers::settle`]):
//! once no read to come can begin before `cutoff`, a register keeps only the
//! writes such a read could still be answered from, and one whose kept
//! writes are all certain and ended before it is folded into the values it
//! may hold (`held`), dropped altogether where that is the value the caller
//! says it was loaded with. `ycsb`'s cells settle at their end, the soak
//! each minute behind the earliest operation still in flight.

#![allow(dead_code)]

use std::collections::HashMap;

/// A write as a register keeps it: its span in ns and the value it wrote
/// (a digest, 0 or 1 for a record's being there, 1 for an increment).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct W {
    pub s: u64,
    pub e: u64,
    pub d: u64,
    /// Not answered -- the connection cut, or a 5xx -- so it may or may not
    /// have landed, at any time from its start.
    pub unanswered: bool,
}

impl W {
    pub fn answered(s: u64, e: u64, d: u64) -> W {
        W {
            s,
            e,
            d,
            unanswered: false,
        }
    }
}

/// The kills a run made -- each when it was sent and when the server
/// answered again (`u64::MAX` until it has) -- and how long before a kill an
/// answered write may still have been in the process alone: 0 where every
/// answered write was in the kernel (`--sync always`, and no kill at all).
#[derive(Clone, Default, Debug)]
pub struct Kills {
    pub at: Vec<(u64, u64)>,
    pub loss: u64,
}

impl Kills {
    /// Whether the answered write `w` may be gone for a read ending at `e`:
    /// answered within `loss` before a kill sent by then, or after the kill
    /// was sent and before the server answered again (the kill lands a
    /// moment after it is sent).
    fn lost_for(&self, w: &W, e: u64) -> bool {
        self.loss > 0
            && self
                .at
                .iter()
                .any(|&(k, back)| k <= e && w.e.saturating_add(self.loss) >= k && w.e <= back)
    }

    /// Whether it may be gone for a read still to come.
    fn lost_ever(&self, w: &W) -> bool {
        self.loss > 0
            && self
                .at
                .iter()
                .any(|&(k, back)| w.e.saturating_add(self.loss) >= k && w.e <= back)
    }

    /// Whether a read ending at `e` must see `w` once it ended.
    pub fn certain_for(&self, w: &W, e: u64) -> bool {
        !w.unanswered && !self.lost_for(w, e)
    }

    /// Whether every read to come must.
    pub fn certain_ever(&self, w: &W) -> bool {
        !w.unanswered && !self.lost_ever(w)
    }
}

/// One register's kept writes.
#[derive(Default)]
struct Reg {
    /// By when they ended, once `ready`.
    ws: Vec<W>,
    /// The latest start among `ws[..=i]`.
    smax: Vec<u64>,
    ready: bool,
    /// The longest span among them: how far past a read's end a write
    /// beside it can have ended.
    longest: u64,
    unanswered: bool,
}

impl Reg {
    fn ready(&mut self) {
        if self.ready {
            return;
        }
        self.ws.sort_by_key(|w| (w.e, w.s));
        let mut m = 0;
        self.smax.clear();
        self.smax.extend(self.ws.iter().map(|w| {
            m = m.max(w.s);
            m
        }));
        self.longest = self
            .ws
            .iter()
            .map(|w| w.e - w.s.min(w.e))
            .max()
            .unwrap_or(0);
        self.unanswered = self.ws.iter().any(|w| w.unanswered);
        self.ready = true;
    }
}

/// A read a register answered, which reads after it may not go back on.
#[derive(Clone, Copy, Debug)]
struct Pin {
    s: u64,
    e: u64,
    d: u64,
}

/// At most this many reads kept a register: dropping one only weakens the
/// check, and a hot key is read thousands of times a minute.
const PINS: usize = 8;

/// The registers a run's writes made, and what each may hold before them.
#[derive(Default)]
pub struct Registers {
    regs: HashMap<(u64, u8), Reg>,
    /// A register settled to one value; `held_many` where the writes that
    /// could have been the last overlapped. Neither holds a register whose
    /// value is the one it was loaded with.
    held_one: HashMap<(u64, u8), u64>,
    held_many: HashMap<(u64, u8), Vec<u64>>,
    /// The reads of a register more than one value could be read from --
    /// one with writes kept, or settled to several -- that later reads are
    /// held to ([`Registers::read`]).
    pins: HashMap<(u64, u8), Vec<Pin>>,
    pub kills: Kills,
}

impl Registers {
    pub fn new(kills: Kills) -> Registers {
        Registers {
            kills,
            ..Registers::default()
        }
    }

    pub fn write(&mut self, key: u64, f: u8, w: W) {
        let r = self.regs.entry((key, f)).or_default();
        r.ws.push(w);
        r.ready = false;
    }

    /// Whether the register held `d` before its kept writes, `base` being
    /// what it was loaded with.
    fn initially(&self, key: u64, f: u8, d: u64, base: u64) -> bool {
        if let Some(&v) = self.held_one.get(&(key, f)) {
            return v == d;
        }
        match self.held_many.get(&(key, f)) {
            Some(v) => v.contains(&d),
            None => base == d,
        }
    }

    /// Every value the register may hold before its kept writes, and every
    /// value they wrote: what a scan's row may hold of it, in any order.
    pub fn candidates(&self, key: u64, f: u8, base: u64) -> Vec<u64> {
        let mut c = match (self.held_one.get(&(key, f)), self.held_many.get(&(key, f))) {
            (Some(&v), _) => vec![v],
            (None, Some(v)) => v.clone(),
            (None, None) => vec![base],
        };
        if let Some(r) = self.regs.get(&(key, f)) {
            c.extend(r.ws.iter().map(|w| w.d));
        }
        c
    }

    /// Whether a read over `[s, e]` may answer `d` of the register
    /// `(key, f)`, loaded with `base` (the module's rule).
    pub fn may_hold(&mut self, key: u64, f: u8, s: u64, e: u64, d: u64, base: u64) -> bool {
        let initially = self.initially(key, f, d, base);
        let Some(reg) = self.regs.get_mut(&(key, f)) else {
            return initially;
        };
        reg.ready();
        let kills = &self.kills;
        let ws = &reg.ws;
        let beside = |from: usize| {
            ws[from..]
                .iter()
                .take_while(|w| w.e <= e.saturating_add(reg.longest))
                .any(|w| w.d == d && w.s <= e)
        };
        // A write that may not have landed for this read makes the
        // latest-start index wrong: the slow way, over a register a kill
        // or a cut connection touched.
        let doubtful = reg.unanswered
            || (kills.loss > 0
                && kills.at.iter().any(|&(k, back)| {
                    let lo = ws.partition_point(|w| w.e.saturating_add(kills.loss) < k);
                    k <= e && lo < ws.len() && ws[lo].e <= back
                }));
        if doubtful {
            let m = ws
                .iter()
                .filter(|w| w.e < s && kills.certain_for(w, e))
                .map(|w| w.s)
                .max();
            if m.is_none() && initially {
                return true;
            }
            return ws.iter().any(|w| {
                w.d == d && ((w.s <= e && w.e >= s) || (w.e < s && m.is_none_or(|m| m <= w.e)))
            });
        }
        // The writes that ended before the read, and the latest start
        // among them: the last of them is any that ended after it.
        let n = ws.partition_point(|w| w.e < s);
        match n.checked_sub(1).map(|i| reg.smax[i]) {
            None => initially || beside(0),
            Some(m) => {
                let from = ws.partition_point(|w| w.e < m);
                ws[from..n].iter().any(|w| w.d == d) || beside(n)
            }
        }
    }

    /// [`Registers::may_hold`], for a read held as one of a history: reads
    /// do not go back (the module's account), and the read is kept for the
    /// reads after it. A history's reads are given in the order they ended,
    /// so that every read ended before another began is in when it comes.
    pub fn read(&mut self, key: u64, f: u8, s: u64, e: u64, d: u64, base: u64) -> bool {
        if !self.may_hold(key, f, s, e, d, base) || !self.after_reads(key, f, s, e, d) {
            return false;
        }
        if self.regs.contains_key(&(key, f)) || self.held_many.contains_key(&(key, f)) {
            let v = self.pins.entry((key, f)).or_default();
            // A read of the same value begun no later and ended no later
            // holds the reads after it to less than this one.
            v.retain(|p| !(p.d == d && p.s <= s && p.e >= e));
            v.push(Pin { s, e, d });
            if v.len() > PINS {
                let oldest = (0..v.len()).min_by_key(|&i| v[i].e).unwrap_or(0);
                v.swap_remove(oldest);
            }
        }
        true
    }

    /// Whether a read over `[s, e]` may answer `d` after the reads kept of
    /// the register: where one ended before it began and answered another
    /// value, with no kill between them, `d` must come from a write that
    /// can be ordered after that read -- ended after it began, or never
    /// answered, which may land at any time from its start. A value the
    /// register held before its kept writes cannot: those writes ended
    /// before any read still kept began (`settle`).
    ///
    /// A kill is between two reads where the server may have died after
    /// the first began and before the second ended: it dies somewhere in
    /// the kill's window, from when the kill was sent -- `docker kill` takes
    /// a moment to land, and a death is held as a kill from the last look
    /// before it -- to when the server answered again. So a kill sent by
    /// the second read's end lets go of a read begun before the server was
    /// back, one begun after the kill was sent among them: that read may
    /// have seen a write the kill then took.
    fn after_reads(&self, key: u64, f: u8, s: u64, e: u64, d: u64) -> bool {
        let Some(pins) = self.pins.get(&(key, f)) else {
            return true;
        };
        let kills = &self.kills.at;
        let floor = pins
            .iter()
            .filter(|p| {
                p.e < s && p.d != d && !kills.iter().any(|&(k, back)| k <= e && p.s <= back)
            })
            .map(|p| p.s)
            .max();
        let Some(floor) = floor else {
            return true;
        };
        self.regs.get(&(key, f)).is_some_and(|r| {
            r.ws.iter()
                .any(|w| w.d == d && w.s <= e && (w.unanswered || w.e > floor))
        })
    }

    /// The writes a read over `[s, e]` that answered `d` shows gone with a
    /// kill: each answered and ended before the read began, begun after
    /// every write of `d` it could have answered ended -- so had it landed,
    /// the register could not hold `d` -- and with a kill sent before the
    /// read ended and before the server answered again after the write.
    /// For each, how long before that kill it was answered in ns (negative:
    /// after the kill was sent), and whether a kill may take it
    /// ([`Kills::loss`]): one it may not is a write lost past the bound,
    /// and the read a mismatch.
    pub fn gone(&self, key: u64, f: u8, s: u64, e: u64, d: u64) -> Vec<(i64, bool, u64)> {
        let mut out = Vec::new();
        let Some(r) = self.regs.get(&(key, f)) else {
            return out;
        };
        if !self.kills.at.iter().any(|&(k, _)| k <= e) {
            return out;
        }
        let d_end =
            r.ws.iter()
                .filter(|w| w.d == d && w.s <= e)
                .map(|w| w.e)
                .max()
                .unwrap_or(0);
        for w in
            r.ws.iter()
                .filter(|w| !w.unanswered && w.e < s && w.d != d && w.s > d_end)
        {
            // The kill that took it: the first one the server had not come
            // back from when it answered.
            let kill = self
                .kills
                .at
                .iter()
                .filter(|&&(k, back)| k <= e && w.e <= back)
                .min_by_key(|x| x.1);
            if let Some(&(k, _)) = kill {
                let lost = self.kills.lost_for(w, e);
                out.push((k as i64 - w.e as i64, !lost, w.s));
            }
        }
        out
    }

    /// Lets go of what no read beginning at `cutoff` or later can be
    /// answered from (the module's account); `base` says what a register
    /// was loaded with. Every kill sent before `cutoff + loss` must be in
    /// `kills` by then.
    pub fn settle(&mut self, cutoff: u64, base: impl Fn(u64, u8) -> u64) {
        // A read kept that began before `cutoff` could be one a write let
        // go of below ended after: such a write's end is lost with it, so
        // the read can no longer say which values came after it.
        self.pins.retain(|_, v| {
            v.retain(|p| p.s >= cutoff);
            !v.is_empty()
        });
        let kills = &self.kills;
        let mut settled: Vec<((u64, u8), Vec<u64>)> = Vec::new();
        self.regs.retain(|&r, reg| {
            let sure = |w: &W| w.e < cutoff && kills.certain_ever(w);
            let m = reg.ws.iter().filter(|w| sure(w)).map(|w| w.s).max();
            if let Some(m) = m {
                let before = reg.ws.len();
                reg.ws.retain(|w| w.e >= m);
                if reg.ws.len() != before {
                    reg.ready = false;
                }
            }
            if m.is_some() && reg.ws.iter().all(sure) {
                let mut v: Vec<u64> = reg.ws.iter().map(|w| w.d).collect();
                v.sort_unstable();
                v.dedup();
                settled.push((r, v));
                return false;
            }
            true
        });
        for (r, v) in settled {
            self.held_one.remove(&r);
            self.held_many.remove(&r);
            if v.len() == 1 {
                // One value left: every read answers it, or none may.
                self.pins.remove(&r);
                if v[0] != base(r.0, r.1) {
                    self.held_one.insert(r, v[0]);
                }
            } else {
                self.held_many.insert(r, v);
            }
        }
    }

    /// The registers with writes kept, and those settled off their load.
    pub fn sizes(&self) -> (usize, usize) {
        (self.regs.len(), self.held_one.len() + self.held_many.len())
    }
}

/// A counter's increments (`set counters {n: n + 1}`): not idempotent, so
/// a read must answer at least the certain increments that ended before it
/// and at most every one begun by its end -- one that landed twice, or a
/// count that went back past what a kill may take, is outside both.
#[derive(Default)]
struct Counter {
    /// Increments let go of: certain ones, and ones that may not have
    /// landed.
    sure: u64,
    doubt: u64,
    recent: Vec<W>,
}

#[derive(Default)]
pub struct Counters {
    c: HashMap<u64, Counter>,
}

impl Counters {
    pub fn add(&mut self, key: u64, w: W) {
        self.c.entry(key).or_default().recent.push(w);
    }

    /// The counts a read over `[s, e]` may answer: from the certain
    /// increments that ended before it to every one begun by its end, the
    /// counter loaded with `base`.
    pub fn range(&self, key: u64, s: u64, e: u64, base: u64, kills: &Kills) -> (u64, u64) {
        let Some(c) = self.c.get(&key) else {
            return (base, base);
        };
        let lo = c
            .recent
            .iter()
            .filter(|w| w.e < s && kills.certain_for(w, e))
            .count() as u64;
        let hi = c.recent.iter().filter(|w| w.s <= e).count() as u64;
        (base + c.sure + lo, base + c.sure + c.doubt + hi)
    }

    pub fn may_count(&self, key: u64, s: u64, e: u64, n: u64, base: u64, kills: &Kills) -> bool {
        let (lo, hi) = self.range(key, s, e, base, kills);
        lo <= n && n <= hi
    }

    /// Folds the increments that ended before `cutoff` into the counts.
    pub fn settle(&mut self, cutoff: u64, kills: &Kills) {
        for c in self.c.values_mut() {
            let (sure, doubt) = (&mut c.sure, &mut c.doubt);
            c.recent.retain(|w| {
                if w.e >= cutoff {
                    return true;
                }
                match kills.certain_ever(w) {
                    true => *sure += 1,
                    false => *doubt += 1,
                }
                false
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A history and the verdict on each of its reads: the table both of
    /// `ycsb`'s ways of using the registers -- a cell at once, the soak a
    /// minute at a time -- are held to.
    struct Case {
        name: &'static str,
        loss: u64,
        kills: &'static [(u64, u64)],
        writes: &'static [(u8, u64, u64, u64, bool)],
        /// (field, s, e, value, base, allowed)
        reads: &'static [(u8, u64, u64, u64, u64, bool)],
    }

    const CASES: &[Case] = &[
        Case {
            name: "the last write before the read, not the one before it",
            loss: 0,
            kills: &[],
            writes: &[(0, 10, 20, 41, false), (0, 30, 40, 42, false)],
            reads: &[
                (0, 50, 60, 42, 7, true),
                (0, 50, 60, 41, 7, false),
                (0, 50, 60, 7, 7, false),
                (0, 1, 5, 7, 7, true),
            ],
        },
        Case {
            name: "writes beside the read, and two that overlapped before it",
            loss: 0,
            kills: &[],
            writes: &[
                (0, 10, 40, 41, false),
                (0, 20, 30, 42, false),
                (0, 55, 70, 43, false),
            ],
            reads: &[
                (0, 50, 60, 41, 7, true),
                (0, 50, 60, 42, 7, true),
                (0, 50, 60, 43, 7, true),
                (0, 50, 60, 7, 7, false),
                (0, 15, 25, 7, 7, true),
                (0, 15, 25, 41, 7, true),
                (0, 15, 25, 99, 7, false),
            ],
        },
        Case {
            name: "a write not answered may or may not have landed",
            loss: 0,
            kills: &[],
            writes: &[(0, 100, 900, 42, true)],
            reads: &[
                (0, 1_000, 1_100, 7, 7, true),
                (0, 1_200, 1_300, 42, 7, true),
                (0, 1_200, 1_300, 43, 7, false),
            ],
        },
        Case {
            name: "a kill takes back a write answered within the loss before it",
            loss: 1_000,
            kills: &[(5_500, 9_000)],
            writes: &[
                (0, 100, 200, 41, false),
                (0, 5_000, 5_100, 42, false),
                (0, 11_000, 11_100, 43, false),
            ],
            reads: &[
                // Before the kill the write holds; after it either value,
                // until a write after the restart ended.
                (0, 5_200, 5_300, 41, 7, false),
                (0, 10_000, 10_100, 41, 7, true),
                (0, 10_200, 10_300, 42, 7, true),
                (0, 12_000, 12_100, 41, 7, false),
                (0, 12_000, 12_100, 43, 7, true),
            ],
        },
        Case {
            name: "a write answered before the loss is held across a kill",
            loss: 1_000,
            kills: &[(5_500, 9_000)],
            writes: &[(0, 100, 200, 41, false), (0, 3_000, 4_000, 42, false)],
            reads: &[
                (0, 10_000, 10_100, 41, 7, false),
                (0, 10_000, 10_100, 42, 7, true),
            ],
        },
        Case {
            name: "an insert, a delete, and a row a @ttl expired",
            loss: 0,
            kills: &[],
            // The record's being there: absent as loaded; inserted; its
            // expiry a write of 0 over the span its time can fall in.
            writes: &[(10, 10, 20, 1, false), (10, 1_000, 2_000, 0, false)],
            reads: &[
                (10, 1, 2, 0, 0, true),
                (10, 1, 2, 1, 0, false),
                (10, 30, 40, 1, 0, true),
                (10, 30, 40, 0, 0, false),
                (10, 1_500, 1_600, 1, 0, true),
                (10, 1_500, 1_600, 0, 0, true),
                (10, 2_100, 2_200, 1, 0, false),
                (10, 2_100, 2_200, 0, 0, true),
            ],
        },
    ];

    fn with_writes(c: &Case) -> Registers {
        let mut r = Registers::new(Kills {
            at: c.kills.to_vec(),
            loss: c.loss,
        });
        for &(f, s, e, d, unanswered) in c.writes {
            r.write(
                1,
                f,
                W {
                    s,
                    e,
                    d,
                    unanswered,
                },
            );
        }
        r
    }

    fn run(c: &Case, settle_every: Option<u64>) {
        let mut r = with_writes(c);
        // Each read alone, as a history's first: the same verdict.
        for &(f, s, e, d, base, allowed) in c.reads {
            assert_eq!(
                with_writes(c).read(1, f, s, e, d, base),
                allowed,
                "{}: a read over [{s}, {e}] answering {d}, alone",
                c.name
            );
        }
        let mut reads = c.reads.to_vec();
        reads.sort_by_key(|x| x.1);
        for &(f, s, e, d, base, allowed) in &reads {
            if let Some(margin) = settle_every {
                // As the soak lets go: behind the read, past the loss.
                r.settle(s.saturating_sub(margin), |_, _| base);
            }
            assert_eq!(
                r.may_hold(1, f, s, e, d, base),
                allowed,
                "{}: a read over [{s}, {e}] answering {d}",
                c.name
            );
        }
    }

    #[test]
    fn every_case_at_once_and_settled_as_it_goes() {
        for c in CASES {
            run(c, None);
            // The soak settles behind the reads by the loss and a margin:
            // the same verdicts.
            run(c, Some(c.loss + 1));
            run(c, Some(c.loss + 10_000));
        }
    }

    /// Histories whose reads are one history each, held as the soak and
    /// `--verify` hold them (`Registers::read`): what a register being
    /// atomic, not merely regular, refuses.
    const HISTORIES: &[Case] = &[
        Case {
            name: "two reads during one write: the new value, then the old",
            loss: 0,
            kills: &[],
            writes: &[(0, 10, 100, 42, false)],
            reads: &[(0, 20, 30, 42, 7, true), (0, 40, 50, 7, 7, false)],
        },
        Case {
            name: "two reads during one write: the old value, then the new",
            loss: 0,
            kills: &[],
            writes: &[(0, 10, 100, 42, false)],
            reads: &[(0, 20, 30, 7, 7, true), (0, 40, 50, 42, 7, true)],
        },
        Case {
            name: "two overlapping writes: the reads after them settle on one",
            loss: 0,
            kills: &[],
            writes: &[(0, 10, 50, 41, false), (0, 20, 40, 42, false)],
            reads: &[
                (0, 60, 70, 41, 7, true),
                (0, 80, 90, 42, 7, false),
                (0, 100, 110, 41, 7, true),
            ],
        },
        Case {
            name: "a read beside a write, then one after it",
            loss: 0,
            kills: &[],
            writes: &[(0, 10, 50, 41, false), (0, 40, 80, 42, false)],
            reads: &[
                (0, 60, 70, 42, 7, true),
                // 41's write ended before the first read began.
                (0, 90, 95, 41, 7, false),
                (0, 96, 99, 42, 7, true),
            ],
        },
        Case {
            name: "a kill lets a read go back, once",
            loss: 1_000,
            kills: &[(5_500, 9_000)],
            writes: &[(0, 100, 200, 41, false), (0, 5_000, 5_100, 42, false)],
            reads: &[
                (0, 5_200, 5_300, 42, 7, true),
                // The kill took 42: the read after it may go back...
                (0, 10_000, 10_100, 41, 7, true),
                // ...and the one after that may not come forward again.
                (0, 10_200, 10_300, 42, 7, false),
                (0, 10_400, 10_500, 41, 7, true),
            ],
        },
        Case {
            name: "a read begun after the kill was sent, before the server died",
            loss: 1_000,
            kills: &[(5_500, 9_000)],
            writes: &[(0, 100, 200, 41, false), (0, 5_000, 5_100, 42, false)],
            reads: &[
                // `docker kill` lands a moment after it is sent: this read
                // still reached the server the kill then took 42 from...
                (0, 5_600, 5_700, 42, 7, true),
                // ...so one begun before the server was back, and one
                // after, may answer the older value...
                (0, 8_900, 9_100, 41, 7, true),
                (0, 10_000, 10_100, 41, 7, true),
                // ...and a read begun after it came back holds the rest.
                (0, 10_200, 10_300, 42, 7, false),
            ],
        },
        Case {
            name: "a death, held as a kill from the last look before it",
            loss: 1_000,
            kills: &[(2_000, 9_000)],
            writes: &[(0, 100, 200, 41, false), (0, 5_000, 5_100, 42, false)],
            reads: &[
                (0, 5_200, 5_300, 42, 7, true),
                (0, 10_000, 10_100, 41, 7, true),
                (0, 10_200, 10_300, 42, 7, false),
            ],
        },
        Case {
            name: "a write not answered may land late, and then it stays",
            loss: 0,
            kills: &[],
            writes: &[(0, 100, 900, 42, true)],
            reads: &[
                (0, 1_000, 1_100, 7, 7, true),
                (0, 1_200, 1_300, 42, 7, true),
                (0, 1_400, 1_500, 7, 7, false),
            ],
        },
    ];

    /// The reads of a history its verdicts are wrong on, each said.
    fn history(c: &Case, settle_margin: Option<u64>) -> Vec<String> {
        let mut r = with_writes(c);
        let mut reads = c.reads.to_vec();
        reads.sort_by_key(|x| x.2);
        let mut wrong = Vec::new();
        for &(f, s, e, d, base, allowed) in &reads {
            if let Some(margin) = settle_margin {
                r.settle(s.saturating_sub(margin), |_, _| base);
            }
            if r.read(1, f, s, e, d, base) != allowed {
                wrong.push(format!(
                    "{}: a read over [{s}, {e}] answering {d} {}",
                    c.name,
                    if allowed { "refused" } else { "allowed" }
                ));
            }
        }
        wrong
    }

    #[test]
    fn a_register_is_atomic_its_reads_never_going_back() {
        // Every history's wrong verdicts, not only the first.
        let mut wrong = Vec::new();
        for c in HISTORIES {
            wrong.extend(history(c, None));
            // The soak settles a minute and the loss behind its reads, far
            // past a read's span.
            wrong.extend(history(c, Some(c.loss + 10_000)));
        }
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
        // Two overlapping writes settled into the values that could be
        // last: the reads still settle on one of them.
        let mut r = Registers::default();
        r.write(1, 0, W::answered(10, 50, 41));
        r.write(1, 0, W::answered(20, 40, 42));
        r.settle(55, |_, _| 7);
        assert_eq!(r.sizes(), (0, 1));
        assert!(r.read(1, 0, 60, 70, 41, 7));
        assert!(!r.read(1, 0, 80, 90, 42, 7));
        assert!(r.read(1, 0, 100, 110, 41, 7));
        // Reads let go of (begun before the cutoff) hold nothing more: the
        // check weakens with time, it never refuses more.
        r.settle(105, |_, _| 7);
        assert!(r.read(1, 0, 120, 130, 42, 7));
        // A register settled to one value keeps no read.
        let mut r = Registers::default();
        r.write(1, 0, W::answered(10, 20, 41));
        assert!(r.read(1, 0, 30, 40, 41, 7));
        assert_eq!(r.pins.len(), 1);
        r.settle(25, |_, _| 7);
        assert!(r.pins.is_empty());
    }

    #[test]
    fn a_read_after_a_kill_names_the_writes_it_shows_gone() {
        let kills = Kills {
            at: vec![(5_500, 9_000)],
            loss: 1_000,
        };
        // Answered 400 before the kill, within the loss: gone and allowed.
        let mut r = Registers::new(kills.clone());
        r.write(1, 0, W::answered(100, 200, 41));
        r.write(1, 0, W::answered(5_000, 5_100, 42));
        assert!(r.may_hold(1, 0, 10_000, 10_100, 41, 7));
        assert_eq!(r.gone(1, 0, 10_000, 10_100, 41), vec![(400, false, 5_000)]);
        // Nothing gone for a read that answered the last write, or one
        // before the kill.
        assert!(r.gone(1, 0, 10_000, 10_100, 42).is_empty());
        assert!(r.gone(1, 0, 5_200, 5_300, 41).is_empty());
        // Answered 1 500 before it, past the loss: gone, and a mismatch.
        let mut r = Registers::new(kills);
        r.write(1, 0, W::answered(100, 200, 41));
        r.write(1, 0, W::answered(3_000, 4_000, 42));
        assert!(!r.may_hold(1, 0, 10_000, 10_100, 41, 7));
        assert_eq!(r.gone(1, 0, 10_000, 10_100, 41), vec![(1_500, true, 3_000)]);
        // The value the register was loaded with: every write is after it.
        assert_eq!(
            r.gone(1, 0, 10_000, 10_100, 7),
            vec![(5_300, true, 100), (1_500, true, 3_000)]
        );
    }

    #[test]
    fn a_settled_register_keeps_the_values_that_could_be_last() {
        let mut r = Registers::default();
        r.write(1, 0, W::answered(10, 40, 41));
        r.write(1, 0, W::answered(20, 30, 42));
        r.settle(100, |_, _| 7);
        assert_eq!(r.sizes(), (0, 1));
        assert!(r.may_hold(1, 0, 200, 210, 41, 7));
        assert!(r.may_hold(1, 0, 200, 210, 42, 7));
        assert!(!r.may_hold(1, 0, 200, 210, 7, 7));
        // One settled to what it was loaded with keeps nothing.
        r.write(2, 0, W::answered(10, 20, 7));
        r.settle(100, |_, _| 7);
        assert_eq!(r.sizes(), (0, 1));
        // An uncertain write is kept until a certain one ended after it.
        let mut r = Registers::default();
        r.write(
            1,
            0,
            W {
                s: 10,
                e: 20,
                d: 41,
                unanswered: true,
            },
        );
        r.settle(100, |_, _| 7);
        assert_eq!(r.sizes(), (1, 0));
        r.write(1, 0, W::answered(30, 40, 42));
        r.settle(100, |_, _| 7);
        assert_eq!(r.sizes(), (0, 1));
        assert!(!r.may_hold(1, 0, 200, 210, 41, 7));
    }

    #[test]
    fn a_counter_answers_between_its_certain_and_its_begun_increments() {
        let kills = Kills {
            at: vec![(5_500, 9_000)],
            loss: 1_000,
        };
        let mut c = Counters::default();
        c.add(1, W::answered(100, 200, 1));
        c.add(1, W::answered(300, 400, 1));
        // Answered within the loss before the kill: gone or not after it.
        c.add(1, W::answered(5_000, 5_100, 1));
        // Not answered: once or not at all.
        c.add(
            1,
            W {
                s: 5_200,
                e: 9_500,
                d: 1,
                unanswered: true,
            },
        );
        assert!(c.may_count(1, 5_200, 5_300, 3, 0, &kills));
        assert!(!c.may_count(1, 5_200, 5_300, 2, 0, &kills));
        assert!(c.may_count(1, 10_000, 10_100, 2, 0, &kills));
        assert!(c.may_count(1, 10_000, 10_100, 4, 0, &kills));
        // Landed twice, or less than the certain ones.
        assert!(!c.may_count(1, 10_000, 10_100, 5, 0, &kills));
        assert!(!c.may_count(1, 10_000, 10_100, 1, 0, &kills));
        c.settle(20_000, &kills);
        assert!(c.may_count(1, 30_000, 30_100, 2, 0, &kills));
        assert!(c.may_count(1, 30_000, 30_100, 4, 0, &kills));
        assert!(!c.may_count(1, 30_000, 30_100, 5, 0, &kills));
    }
}
