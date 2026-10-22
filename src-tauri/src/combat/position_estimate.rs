//! Where the local player probably is, when the server has not said.
//!
//! The only records carrying the player's own position are the buff family
//! (`2B 38`/`2A 38`, see `radar`). A Chanter gets one every second from
//! *Undefeated Mantra* ticking; other classes only when a buff lands on them
//! (a skill, a kill proc), so their radar would move in steps.
//!
//! What every class does get is what the server decides to show them, and it
//! decides by distance. Measured against the mantra positions of 2026-10-12:
//!
//! - world objects (`34 36`/`35 36`) enter view at a radius that depends on
//!   their code and leave at a larger one (hysteresis) — see `view_radii`;
//! - mobs and other players spawn (`41 36`/`45 36`) at most ~45 m away;
//! - their movement (`1A`–`1E`, `28`, `29`, `2E`, `2F 37`) is only sent while
//!   they are within ~50 m.
//!
//! Each of those is a ring or a disc the player must be in. The estimate is
//! the centre of the region that breaks the fewest of them, starting from the
//! previous estimate and moving no faster than a run. On three captures that
//! put the player within ~13 m half of the time and ~30–35 m nine times in
//! ten — enough to point at a node 60 m off, not to find one underfoot.
//! (Saved points *not* in view were tried as "you are not near these" and
//! made it worse: gathered nodes are absent too.)
//!
//! Units are metres here; the radar's are centimetres.

use std::collections::HashMap;

/// Run speed allowance between clues (m/s). Mounts go faster; the estimate
/// then lags and re-acquires.
const SPEED_MPS: f32 = 8.0;
/// Fastest plausible travel, for discarding clues that cannot be near us:
/// a mount ~23 m/s, flying ~50 m/s between two exact positions (2026-10-22).
const MOUNT_MPS: f32 = 60.0;
/// Two exact positions at most this far apart in time (ms) give a speed.
const VELOCITY_SPAN_MS: i64 = 20_000;
/// Mob/player movement is only sent within this distance (m).
const MOVER_RANGE_M: f32 = 46.0;
/// Mob/player spawns arrive within this distance (m).
const SPAWN_RANGE_M: f32 = 42.0;
/// How long each kind of clue is used (ms).
const MOVER_TTL_MS: i64 = 2_000;
const SPAWN_TTL_MS: i64 = 3_000;
const EDGE_TTL_MS: i64 = 6_000;
/// After a zone load everything in range spawns at once, not at the edge.
const SETTLE_MS: i64 = 3_000;
/// After gathering, how long (ms) the estimate holds on the node, and how
/// much worse (cost units, ~1 per metre² of broken clue) the node may fit
/// before it moves off anyway.
const HOLD_MS: i64 = 15_000;
const HOLD_COST: f32 = 1.0;
/// At most this often (ms).
const SOLVE_EVERY_MS: i64 = 500;
/// A server position this recent is used as is. The mantra ticks every
/// 0.98–1.1 s; below ~2 s a late tick would flicker to the estimate.
const FRESH_MS: i64 = 2_500;

/// Codes seen entering view at ~105 m and leaving at ~125 m: Odyle, the
/// Hidden Cube and a few unnamed objects.
const MID_RANGE: &[u32] = &[
    1_550_000, 1_301_199, 1_300_056, 1_300_891, 1_300_908, 1_300_920, 1_300_958, 1_301_077, 1_400_248,
    1_400_285, 1_400_330, 1_400_707,
];
/// Codes seen entering at ~245 m and leaving at ~277 m.
const LONG_RANGE: &[u32] = &[1_100_001, 1_300_096, 1_300_889, 1_400_138];

/// (enter, leave) view radii in metres for an object code.
pub fn view_radii(code: u32) -> (f32, f32) {
    if LONG_RANGE.contains(&code) {
        (245.0, 277.0)
    } else if MID_RANGE.contains(&code) {
        (105.0, 125.0)
    } else {
        (57.0, 70.0)
    }
}

/// The player is between `lo` and `hi` metres from (x, y).
struct Ring {
    x: f32,
    y: f32,
    lo: f32,
    hi: f32,
    w: f32,
}

struct Edge {
    x: f32,
    y: f32,
    code: u32,
    entered: bool,
    t: i64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Estimate {
    pub x: f32,
    pub y: f32,
    /// Rough size of the region it could be in (m).
    pub err_m: f32,
    pub t: i64,
}

#[derive(Default)]
pub struct PositionEstimator {
    est: Option<Estimate>,
    /// The last position the server gave (m, ms): the anchor after a buff.
    anchor: Option<(f32, f32, i64)>,
    edges: Vec<Edge>,
    movers: HashMap<u32, (f32, f32, i64)>,
    spawns: Vec<(f32, f32, i64)>,
    settle_until: i64,
    /// Until when to stay on a node we gathered at.
    hold_until: i64,
    last_solve: i64,
    /// Speed between the last two server positions (m/s), when faster than a
    /// run: riding or flying. Kept until a server position says we slowed.
    velocity: Option<f32>,
}

impl PositionEstimator {
    pub fn estimate(&self) -> Option<Estimate> {
        self.est
    }

    /// A real position from the server: start from it.
    pub fn anchor(&mut self, x_cm: f32, y_cm: f32, t: i64) {
        let (x, y) = (x_cm / 100.0, y_cm / 100.0);
        if let Some((px, py, pt)) = self.anchor {
            let dt = (t - pt) as f32 / 1000.0;
            if (0.3..=VELOCITY_SPAN_MS as f32 / 1000.0).contains(&dt) {
                let v = (x - px).hypot(y - py) / dt;
                self.velocity = (v > SPEED_MPS && v < MOUNT_MPS).then_some(v);
            }
        }
        self.anchor = Some((x, y, t));
        self.est = None;
    }

    /// We are gathering at (x, y): an anchor that also holds a while, since
    /// we stand there for the whole gather.
    pub fn anchor_gathering(&mut self, x_cm: f32, y_cm: f32, t: i64) {
        self.anchor(x_cm, y_cm, t);
        self.hold_until = t + HOLD_MS;
    }

    /// Zone load or teleport: everything known is about the old place.
    pub fn reset(&mut self, now: i64) {
        *self = Self { settle_until: now + SETTLE_MS, ..Self::default() };
    }

    pub fn object_entered(&mut self, x_cm: f32, y_cm: f32, code: u32, now: i64) {
        // Objects arriving while settling were already in range, not at the edge.
        if now >= self.settle_until {
            self.edges.push(Edge { x: x_cm / 100.0, y: y_cm / 100.0, code, entered: true, t: now });
        }
    }

    pub fn object_left(&mut self, x_cm: f32, y_cm: f32, code: u32, now: i64) {
        self.edges.push(Edge { x: x_cm / 100.0, y: y_cm / 100.0, code, entered: false, t: now });
    }

    pub fn entity_moved(&mut self, id: u32, x_cm: f32, y_cm: f32, now: i64) {
        self.movers.insert(id, (x_cm / 100.0, y_cm / 100.0, now));
    }

    pub fn entity_spawned(&mut self, x_cm: f32, y_cm: f32, now: i64) {
        self.spawns.push((x_cm / 100.0, y_cm / 100.0, now));
    }

    /// Re-estimate from the clues so far, unless the server's own position is
    /// fresh or it ran less than `SOLVE_EVERY_MS` ago. `live` are the world
    /// objects in view: (x cm, y cm, code).
    pub fn update(&mut self, live: impl Iterator<Item = (f32, f32, u32)>, now: i64) {
        if self.anchor.is_some_and(|a| now - a.2 < FRESH_MS) || now - self.last_solve < SOLVE_EVERY_MS {
            return;
        }
        self.last_solve = now;
        self.edges.retain(|e| now - e.t < EDGE_TTL_MS);
        self.spawns.retain(|s| now - s.2 < SPAWN_TTL_MS);
        self.movers.retain(|_, m| now - m.2 < MOVER_TTL_MS);

        let age = |t: i64| (now - t).max(0) as f32 / 1000.0;
        // Riding or flying (measured between two server positions): allow that
        // speed. Which way we go is not known — turns are not sent — so no
        // dead reckoning: tried on 2026-10-22, a flight that turned came out
        // no better (147 / 227 m off either way).
        let fast = self.velocity;
        let speed = fast.map_or(SPEED_MPS, |v| (1.3 * v).min(MOUNT_MPS));
        let mut rings: Vec<Ring> = Vec::new();
        for (x, y, code) in live {
            let (_, leave) = view_radii(code);
            rings.push(Ring { x: x / 100.0, y: y / 100.0, lo: 0.0, hi: leave + 3.0, w: 1.0 });
        }
        for e in &self.edges {
            let (enter, leave) = view_radii(e.code);
            let s = speed * age(e.t);
            rings.push(if e.entered {
                Ring { x: e.x, y: e.y, lo: enter - 12.0 - s, hi: enter + 4.0 + s, w: 0.5 }
            } else {
                Ring { x: e.x, y: e.y, lo: leave - 6.0 - s, hi: leave + 12.0 + s, w: 1.0 }
            });
        }
        for &(x, y, t) in self.movers.values() {
            rings.push(Ring { x, y, lo: 0.0, hi: MOVER_RANGE_M + speed * age(t), w: 0.3 });
        }
        for &(x, y, t) in &self.spawns {
            rings.push(Ring { x, y, lo: 0.0, hi: SPAWN_RANGE_M + speed * age(t), w: 0.5 });
        }
        // Where we last were: the estimate, or the server's position if newer.
        // Neither when the meter started mid-zone with no buff since.
        let prev = match (self.est, self.anchor) {
            (Some(e), Some(a)) if a.2 >= e.t => Some(a),
            (Some(e), _) => Some((e.x, e.y, e.t)),
            (None, a) => a,
        };
        if let Some((px, py, pt)) = prev {
            // Clues that could not be in view from anywhere we can have got
            // to since (riding, with margin) are leftovers of another place.
            let far = MOUNT_MPS * age(pt) + 150.0;
            rings.retain(|r| (r.x - px).hypot(r.y - py) < far + r.hi);
        } else if rings.len() < 3 {
            // From scratch: wait for a few clues before guessing.
            return;
        }
        if rings.is_empty() {
            return;
        }
        let median = || {
            let mut xs: Vec<f32> = rings.iter().map(|r| r.x).collect();
            let mut ys: Vec<f32> = rings.iter().map(|r| r.y).collect();
            xs.sort_by(f32::total_cmp);
            ys.sort_by(f32::total_cmp);
            (xs[xs.len() / 2], ys[ys.len() / 2])
        };
        let mut found = match prev {
            Some((px, py, pt)) => solve(&rings, Some((px, py, speed * age(pt) + 2.0)), (px, py)),
            None => solve(&rings, None, median()),
        };
        // Lost (most clues broken, e.g. after a fast mount ride): start over
        // from the middle of the clues, without the motion limit.
        let total: f32 = rings.iter().map(|r| r.w).sum();
        if prev.is_some() && cost(found.0, found.1, &rings, None) > 0.5 * total * 100.0 {
            found = solve(&rings, None, median());
        } else if let (Some((px, py, _)), true) = (prev, now < self.hold_until) {
            // Just gathered: we stand at the node. Stay there while it still
            // fits the clues about as well, rather than drift to the middle
            // of the possible region. (Always holding lagged behind walks.)
            if cost(px, py, &rings, None) <= cost(found.0, found.1, &rings, None) + HOLD_COST {
                found = (px, py, found.2);
            }
        }
        // Moving fast, the clues lag and the error grows with the time since
        // the last server position: say so rather than claim a few metres.
        let err = match (fast, self.anchor) {
            (Some(_), Some(a)) => found.2.max((0.3 * speed * age(a.2)).min(300.0)),
            _ => found.2,
        };
        self.est = Some(Estimate { x: found.0, y: found.1, err_m: err, t: now });
    }
}

/// How badly (x, y) breaks the rings, plus running further than `prior`
/// allows: (x, y, reach in m). Each ring's share is capped, so one wrong clue
/// (a respawn mid-view, a mob that was teleported) cannot dominate.
fn cost(x: f32, y: f32, rings: &[Ring], prior: Option<(f32, f32, f32)>) -> f32 {
    let mut c = 0.0;
    for r in rings {
        let d = (x - r.x).hypot(y - r.y);
        let v = if d < r.lo { r.lo - d } else if d > r.hi { d - r.hi } else { 0.0 };
        if v > 0.0 {
            c += r.w * (v * v).min(400.0);
        }
    }
    if let Some((px, py, reach)) = prior {
        let over = (x - px).hypot(y - py) - reach;
        if over > 0.0 {
            c += 0.5 * over * over;
        }
    }
    c
}

/// Grid search around `start`: a coarse pass finds the near-best region and
/// its centre, a fine pass refines it when the region is small. The answer
/// is the region's centre — not its first cell, which sits on an edge — and
/// its spread is the error.
fn solve(rings: &[Ring], prior: Option<(f32, f32, f32)>, start: (f32, f32)) -> (f32, f32, f32) {
    let (cx, cy, spread) = region(rings, prior, start, 8.0, 240.0);
    if spread > 12.0 {
        return (cx, cy, spread);
    }
    let (fx, fy, fine) = region(rings, prior, (cx, cy), 2.0, 24.0);
    (fx, fy, fine.max(5.0))
}

/// Centre and RMS spread of the cells within 1 of the lowest cost.
fn region(rings: &[Ring], prior: Option<(f32, f32, f32)>, (cx, cy): (f32, f32), step: f32, span: f32) -> (f32, f32, f32) {
    let n = (span / step) as i32;
    let mut cells: Vec<(f32, f32, f32)> = Vec::with_capacity(((2 * n + 1) * (2 * n + 1)) as usize);
    let mut best = f32::MAX;
    for i in -n..=n {
        for j in -n..=n {
            let (x, y) = (cx + i as f32 * step, cy + j as f32 * step);
            let c = cost(x, y, rings, prior);
            best = best.min(c);
            cells.push((c, x, y));
        }
    }
    let good: Vec<(f32, f32)> = cells.iter().filter(|c| c.0 <= best + 1.0).map(|c| (c.1, c.2)).collect();
    let k = good.len() as f32;
    let (mx, my) = good.iter().fold((0.0, 0.0), |a, p| (a.0 + p.0 / k, a.1 + p.1 / k));
    let spread = (good.iter().map(|p| (p.0 - mx).powi(2) + (p.1 - my).powi(2)).sum::<f32>() / k).sqrt();
    (mx, my, spread)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Walking east along y = 0 past nodes 57 m north of the path: each one
    /// entering view is a ring; mobs around add discs. The estimate has to
    /// follow without any real position after the first.
    #[test]
    fn follows_a_walk_from_view_edges_alone() {
        let mut e = PositionEstimator::default();
        e.anchor(0.0, 0.0, 0);
        let mut live: Vec<(f32, f32, u32)> = Vec::new();
        let mut worst: f32 = 0.0;
        for step in 1..=40i64 {
            let t = step * 500;
            let me_x = step as f32 * 2.0; // 4 m/s
            // A node every 10 m along x, 50 m north: enters when 57 m away.
            for k in 0..20 {
                let (nx, ny) = (k as f32 * 10.0, 50.0);
                let d = (nx - me_x).hypot(ny);
                let in_view = live.iter().any(|l| l.0 == nx * 100.0);
                if d <= 57.0 && !in_view {
                    live.push((nx * 100.0, ny * 100.0, 1_550_020));
                    e.object_entered(nx * 100.0, ny * 100.0, 1_550_020, t);
                } else if d > 70.0 && in_view {
                    live.retain(|l| l.0 != nx * 100.0);
                    e.object_left(nx * 100.0, ny * 100.0, 1_550_020, t);
                }
            }
            // Two mobs 30 m away, south-east and south-west.
            e.entity_moved(1, (me_x + 21.0) * 100.0, -2100.0, t);
            e.entity_moved(2, (me_x - 21.0) * 100.0, -2100.0, t);
            e.update(live.iter().copied(), t);
            if let Some(est) = e.estimate() {
                worst = worst.max((est.x - me_x).hypot(est.y));
            }
        }
        let est = e.estimate().expect("an estimate");
        assert!((est.x - 80.0).abs() < 15.0 && est.y.abs() < 15.0, "{est:?}");
        assert!(worst < 25.0, "worst error {worst}");
    }

    #[test]
    fn from_scratch_needs_a_few_clues_and_a_fresh_anchor_wins() {
        let mut e = PositionEstimator::default();
        e.entity_moved(1, 1000.0, 0.0, 4_900);
        e.update(std::iter::empty(), 5_000);
        assert_eq!(e.estimate(), None, "one clue is not enough to start from nothing");
        e.entity_moved(2, -1000.0, 0.0, 5_500);
        e.entity_moved(3, 0.0, 1000.0, 5_500);
        e.update(std::iter::empty(), 5_600);
        let first = e.estimate().expect("three clues");
        assert!(first.x.abs() < 10.0 && first.y.abs() < 10.0, "{first:?}");
        e.anchor(0.0, 0.0, 5_600);
        e.update(std::iter::empty(), 6_200);
        assert_eq!(e.estimate(), None, "the server's position is still fresh");
        e.entity_moved(1, 1000.0, 0.0, 8_200);
        e.update(std::iter::empty(), 8_200);
        assert!(e.estimate().is_some());
    }

    /// Gathering at (0, 0): mobs 40 m east keep the possible region centred
    /// further east, but the node still fits, so the estimate stays on it.
    #[test]
    fn stays_on_the_node_after_gathering() {
        let mut e = PositionEstimator::default();
        e.anchor_gathering(0.0, 0.0, 0);
        for k in 0..20i64 {
            let t = 3_000 + k * 500;
            e.entity_moved(1, 4_000.0, 1_000.0, t);
            e.entity_moved(2, 4_000.0, -1_000.0, t);
            e.update(std::iter::empty(), t);
        }
        let est = e.estimate().expect("estimated");
        assert!(est.x.hypot(est.y) < 1.0, "drifted to {est:?}");
        // Without the hold (a plain anchor) it drifts towards the mobs.
        let mut f = PositionEstimator::default();
        f.anchor(0.0, 0.0, 0);
        for k in 0..20i64 {
            let t = 3_000 + k * 500;
            f.entity_moved(1, 4_000.0, 1_000.0, t);
            f.entity_moved(2, 4_000.0, -1_000.0, t);
            f.update(std::iter::empty(), t);
        }
        assert!(f.estimate().unwrap().x > 5.0);
    }

    #[test]
    fn radii_by_code() {
        assert_eq!(view_radii(1_550_020), (57.0, 70.0));
        assert_eq!(view_radii(1_301_199), (105.0, 125.0));
        assert_eq!(view_radii(1_100_001), (245.0, 277.0));
    }
}
