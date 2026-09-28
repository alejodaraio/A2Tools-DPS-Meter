//! Buff intervals per entity, from the buff add (`2A 38`) / remove (`2C 38`)
//! records, for "how long was each buff up while I was attacking".
//!
//! Layout, measured on a live Chanter capture (2026-09-27):
//!
//! ```text
//! 2A 38 <target varint> <varint> <varint> <seq varint> <effect u32 LE>
//!       <duration_ms u32 LE> <00 00 00 00> <epoch ms u64 LE> <caster varint> ...
//! 2C 38 <target varint> 01 00 <seq varint> 01
//! ```
//!
//! The effect id is a skill (or item) code times ten plus one digit: 182500101
//! is Power of the Storm (18250010), 22101051 Courage Scroll (2210105). Its
//! 12 s Power of the Storm add and the matching remove 12 s later bracket the
//! +20% step in the damage multiplier exactly. Most buffs are never removed
//! explicitly (36 of 134 adds in that capture): they lapse after their
//! duration or are refreshed by a new add — Protection Circle was re-added
//! every ~0.5 s with 5 s duration — so an add counts until its duration ends,
//! a remove cuts it short, and refreshes merge into one interval.

use std::collections::HashMap;

/// Intervals older than this are dropped; no fight looks back further.
const KEEP_MS: i64 = 2 * 60 * 60 * 1000;

#[derive(Default, Debug, Clone)]
pub struct BuffTracker {
    /// target -> effect -> merged `(start, end)` intervals.
    intervals: HashMap<i32, HashMap<u32, Vec<(i64, i64)>>>,
    /// (target, seq) -> (effect, start, duration) for adds a remove may close.
    open: HashMap<(i32, u32), (u32, i64, u32)>,
}

impl BuffTracker {
    pub fn add(&mut self, target: i32, seq: u32, effect: u32, duration_ms: u32, ts: i64) {
        self.open.insert((target, seq), (effect, ts, duration_ms));
        // No duration: lasts until removed (see `remove`). Many of these are
        // instant proc markers removed in the same instant.
        if duration_ms == 0 {
            return;
        }
        self.push(target, effect, ts, ts + duration_ms as i64);
    }

    pub fn remove(&mut self, target: i32, seq: u32, ts: i64) {
        let Some((effect, start, duration_ms)) = self.open.remove(&(target, seq)) else { return };
        if duration_ms == 0 {
            if ts > start {
                self.push(target, effect, start, ts);
            }
            return;
        }
        // Removed early: cut the interval that holds this add short.
        if let Some(list) = self.intervals.get_mut(&target).and_then(|m| m.get_mut(&effect)) {
            if let Some(last) = list.last_mut() {
                if last.0 <= ts && ts < last.1 {
                    last.1 = ts;
                }
            }
        }
    }

    fn push(&mut self, target: i32, effect: u32, start: i64, end: i64) {
        let list = self.intervals.entry(target).or_default().entry(effect).or_default();
        match list.last_mut() {
            // A refresh while still up extends it rather than adding a new one.
            Some(last) if start <= last.1 => last.1 = last.1.max(end),
            _ => list.push((start, end)),
        }
        let cutoff = end - KEEP_MS;
        list.retain(|&(_, e)| e >= cutoff);
        self.open.retain(|_, &mut (_, s, _)| s >= cutoff);
    }

    /// How long each buff on `target` overlapped `spans` (the target's active
    /// spans), as `effect -> ms`, longest first.
    pub fn uptime(&self, target: i32, spans: &[(i64, i64)]) -> Vec<(u32, i64)> {
        let Some(effects) = self.intervals.get(&target) else { return Vec::new() };
        let spans = union(spans.to_vec());
        let mut out: Vec<(u32, i64)> = effects
            .iter()
            .map(|(&effect, list)| (effect, overlap(&union(list.clone()), &spans)))
            .filter(|&(_, ms)| ms > 0)
            .collect();
        out.sort_by_key(|&(effect, ms)| (std::cmp::Reverse(ms), effect));
        out
    }
}

/// The skill/item code an effect id names (see the module docs), for name
/// lookups: `effect / 10`.
pub fn effect_code(effect: u32) -> i32 {
    (effect / 10) as i32
}

fn union(mut list: Vec<(i64, i64)>) -> Vec<(i64, i64)> {
    list.sort_unstable();
    let mut out: Vec<(i64, i64)> = Vec::new();
    for (s, e) in list {
        match out.last_mut() {
            Some(last) if s <= last.1 => last.1 = last.1.max(e),
            _ => out.push((s, e)),
        }
    }
    out
}

/// Total overlap of two sorted, disjoint interval lists.
fn overlap(a: &[(i64, i64)], b: &[(i64, i64)]) -> i64 {
    let (mut i, mut j, mut total) = (0, 0, 0);
    while i < a.len() && j < b.len() {
        let s = a[i].0.max(b[j].0);
        let e = a[i].1.min(b[j].1);
        if e > s {
            total += e - s;
        }
        if a[i].1 < b[j].1 { i += 1 } else { j += 1 }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duration_remove_and_refresh() {
        let mut t = BuffTracker::default();
        // Power of the Storm: 12 s add, explicit remove after 12 s.
        t.add(1, 10, 182500101, 12_000, 0);
        t.remove(1, 10, 12_000);
        // Protection Circle: 5 s adds every 2 s from 0 to 20 s -> up 0..25 s.
        for (k, at) in (0..=20_000).step_by(2_000).enumerate() {
            t.add(1, 100 + k as u32, 187300011, 5_000, at);
        }
        // Cut short: a 10 s buff removed after 4 s.
        t.add(1, 50, 999, 10_000, 30_000);
        t.remove(1, 50, 34_000);
        // Instant marker (no duration, removed at once) counts for nothing.
        t.add(1, 60, 183300073, 0, 1_000);
        t.remove(1, 60, 1_000);

        let up = t.uptime(1, &[(0, 40_000)]);
        assert_eq!(up, vec![(187300011, 25_000), (182500101, 12_000), (999, 4_000)]);

        // Only time inside the active spans counts (ties: lower effect id first).
        let up = t.uptime(1, &[(0, 6_000), (30_000, 31_000)]);
        assert_eq!(up, vec![(182500101, 6_000), (187300011, 6_000), (999, 1_000)]);
    }

    #[test]
    fn effect_ids_name_their_skill() {
        assert_eq!(effect_code(182500101), 18250010);
        assert_eq!(effect_code(22101051), 2210105);
    }
}
