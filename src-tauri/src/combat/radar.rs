//! Radar: world objects (gather nodes, Hidden Cubes, …) around the local player.
//!
//! Everything here comes from server->client traffic; the client's own packets
//! are encrypted. Coordinates are `f32` LE in centimetres, `z` being height.
//!
//! ```text
//! 34 36 <id varint> <kind u16> <code u32> <x> <y> <z> <heading f32> 01 01 <state> …   object enters view
//! 35 36 <id varint> <reason> <?> <?>                                                 object leaves
//! 2B 38 / 2A 38 <target varint> … <caster varint> <?> 02 <x> <y> <z>                 (buff tick/add) position
//! 23 36 00 <00×5> <x> <y> <z> …                                                      self teleport / zone load
//! 02 38 <self> 00 <u32> <u16 counter> <self> <heading f32> …                          self facing, ~1 Hz
//! ```
//!
//! Heading is Unreal yaw in degrees: 0° = +X, 90° = +Y, seen anywhere in
//! -180..360. On the 2026-10-12 walk it matched the direction of travel
//! between consecutive positions (323.2 vs 323.0, 219.9 vs 219.9, …).
//!
//! Evidence (capture of 2026-10-12, a Sapphire gathered three times): the
//! Sapphire was `34 36` code 1550020; standing on it put the player 0.8 m from
//! its coordinates. `35 36` reason `04` followed the third gather (depleted),
//! reason `01` objects walking out of view. The gather itself is
//! `38 36 <obj> <self> 01` (start) and `3A 36 <obj> <self> <n>` (n-th done).
//!
//! The local player's own position is never sent as such. The buff-family
//! records `2B 38` (tick) and `2A 38` (add) end with the position of the
//! entity they concern; they also carry other players, so a record is only
//! taken as ours when both its target and its caster are the local player.
//! A skill cast is a longer `02 38` with the position after the facing. All
//! three put us within ~1–2 m of the mantra positions (2026-10-12): exact,
//! for any class, whenever we buff ourselves, dodge or use a skill. The
//! once-a-second cadence (and the short `02 38` facing) is the Chanter's
//! *Undefeated Mantra* ticking; in between, for everyone else, the position
//! is estimated from what comes into view (`position_estimate`).
//!
//! The server only sends objects within ~60 m (spawned at 62 m, left view at
//! 62–64 m in that capture), so the radar also remembers every spawn point it
//! has seen, per map, in `radar_points.json`. Gather nodes respawn on the same
//! coordinates (a depleted one came back 2 s later, new id, same position), so
//! a point is (map, code, position) and survives meter restarts.
//!
//! The map id comes from the `33 36` self record (zone load):
//! `<map u32> <map u32> <x y z> <u32> 00 00 <u16> <z f32> 01 DE 02` — 1001 for
//! both loads of 2026-09-27, 1013 for the 2026-10-12 field map. A meter started
//! mid-zone never sees that record; it adopts the map of the first saved point
//! a live object lands on instead.
//!
//! That id is not purely the map: the same field later came back as 1052 (the
//! same 1100001 nodes, to the centimetre). So when a live object lands on a
//! point saved under another id, the two ids are one world: they are merged
//! (`aliases`, saved with the points) and everything is kept under the first.
//!
//! A spawn is sent once, on entering view. A meter restarted next to an object
//! never hears of it again until you walk away and back (a Hidden Cube 3–6 m
//! away stayed a remembered outline for minutes). So the objects in view are
//! also kept in `radar_live.json` with the session fingerprint, as the mob
//! cache does, and restored into the same session (see `LiveCache`).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::position_estimate::PositionEstimator;

/// Two sightings of the same code this close (cm) are the same spawn point.
const SAME_POINT_CM: f32 = 200.0;

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Vec3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

impl Vec3 {
    fn read(b: &[u8], at: usize) -> Option<Self> {
        let f = |o: usize| -> Option<f32> {
            Some(f32::from_le_bytes(b.get(at + o..at + o + 4)?.try_into().ok()?))
        };
        let v = Vec3 { x: f(0)?, y: f(4)?, z: f(8)? };
        // World coordinates sit in the ±100k..±300k range; z is height. A zero
        // pair or anything non-finite is a misread, not a place.
        let plausible = |c: f32, max: f32| c.is_finite() && c.abs() < max;
        (plausible(v.x, 2.0e6)
            && plausible(v.y, 2.0e6)
            && plausible(v.z, 2.0e5)
            && (v.x.abs() > 1.0 || v.y.abs() > 1.0))
            .then_some(v)
    }

    pub fn dist2d(&self, o: &Vec3) -> f32 {
        ((self.x - o.x).powi(2) + (self.y - o.y).powi(2)).sqrt()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RadarObject {
    pub id: i32,
    pub kind: u16,
    pub code: u32,
    pub pos: Vec3,
    pub seen_ms: i64,
}

/// A remembered spawn point: where an object of `code` has been seen on `map`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpawnPoint {
    pub map: u32,
    pub code: u32,
    pub kind: u16,
    pub pos: Vec3,
    pub first_seen_ms: i64,
    pub last_seen_ms: i64,
    pub seen_count: u32,
    /// Last time it was seen depleted (`35 36` reason 04); 0 = never.
    #[serde(default)]
    pub last_depleted_ms: i64,
}

#[derive(Serialize, Deserialize, Default)]
struct SavedPoints {
    version: u32,
    points: Vec<SpawnPoint>,
    /// Map ids found to be the same world as another: id -> the one kept.
    #[serde(default)]
    aliases: HashMap<u32, u32>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PointView {
    #[serde(flatten)]
    pub point: SpawnPoint,
    /// A live object currently stands on this point.
    pub live: bool,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RadarSnapshot {
    pub map: Option<u32>,
    pub self_pos: Option<Vec3>,
    /// Facing, degrees 0..360 (0 = +X, 90 = +Y).
    pub self_heading: Option<f32>,
    /// When `self_pos` was last updated (epoch ms), so the UI can show staleness.
    pub self_pos_ms: i64,
    /// `self_pos` is an estimate (no position from the server lately), give
    /// or take `self_pos_error_m` metres.
    pub self_pos_estimated: bool,
    pub self_pos_error_m: Option<f32>,
    /// Objects in view right now.
    pub objects: Vec<RadarObject>,
    /// Every remembered spawn point on the current map.
    pub points: Vec<PointView>,
}

#[derive(Default)]
pub struct RadarTracker {
    objects: HashMap<i32, RadarObject>,
    self_pos: Option<(Vec3, i64)>,
    self_heading: Option<(f32, i64)>,
    estimator: PositionEstimator,
    /// Evaluation only (`estimate_only`): server positions kept as ground
    /// truth instead of being used, past the first.
    estimate_only: bool,
    truth: Option<(Vec3, i64)>,
    /// Entities seen moving or spawning: never us, since the server does not
    /// send our own movement. Tells a gatherer apart from us without our id.
    others: HashSet<u32>,
    /// Our id as worked out from our own buffs, skills or gathering while
    /// the meter has none (see `is_self`).
    guessed_self: Option<u32>,
    map: Option<u32>,
    points: Vec<SpawnPoint>,
    aliases: HashMap<u32, u32>,
    points_path: Option<PathBuf>,
    dirty: bool,
    /// Sightings while the map is unknown, recorded once it is known. Never
    /// saved as is: a teleport drops them, so they cannot land on the wrong map.
    pending: Vec<RadarObject>,
}

impl RadarTracker {
    /// Load remembered points; from now on they are saved to the same file.
    pub fn load_points(&mut self, path: &Path) {
        if let Ok(text) = std::fs::read_to_string(path) {
            match serde_json::from_str::<SavedPoints>(&text) {
                Ok(saved) => {
                    self.points = saved.points;
                    self.aliases = saved.aliases;
                }
                Err(e) => tracing::warn!("radar: ignoring unreadable {}: {}", path.display(), e),
            }
        }
        self.points_path = Some(path.to_path_buf());
    }

    /// The file to write and its contents, if anything changed since last time.
    /// Serialised under the lock, written by the caller outside it.
    pub fn take_points_to_save(&mut self) -> Option<(PathBuf, String)> {
        if !self.dirty {
            return None;
        }
        let path = self.points_path.clone()?;
        self.dirty = false;
        let saved = SavedPoints { version: 1, points: self.points.clone(), aliases: self.aliases.clone() };
        serde_json::to_string(&saved).ok().map(|json| (path, json))
    }

    /// The id a map is kept under, following merges.
    fn canonical(&self, mut map: u32) -> u32 {
        // Bounded: a cycle can only come from a hand-edited file.
        for _ in 0..16 {
            match self.aliases.get(&map) {
                Some(&next) if next != map => map = next,
                _ => break,
            }
        }
        map
    }

    /// `from` turned out to be the same world as `into`: move its points over
    /// (folding any that were seen under both) and keep `into` from now on.
    fn merge_maps(&mut self, from: u32, into: u32) {
        if from == into {
            return;
        }
        tracing::info!("radar: map {} is the same world as {} — merging", from, into);
        for target in self.aliases.values_mut() {
            if *target == from {
                *target = into;
            }
        }
        self.aliases.insert(from, into);
        let moved: Vec<SpawnPoint> = self.points.iter().filter(|p| p.map == from).cloned().collect();
        self.points.retain(|p| p.map != from);
        for mut p in moved {
            let existing = self.points.iter_mut().find(|q| {
                q.map == into && q.code == p.code && q.pos.dist2d(&p.pos) < SAME_POINT_CM
            });
            match existing {
                Some(q) => {
                    q.first_seen_ms = q.first_seen_ms.min(p.first_seen_ms);
                    q.last_seen_ms = q.last_seen_ms.max(p.last_seen_ms);
                    q.last_depleted_ms = q.last_depleted_ms.max(p.last_depleted_ms);
                    q.seen_count += p.seen_count;
                }
                None => {
                    p.map = into;
                    self.points.push(p);
                }
            }
        }
        if self.map == Some(from) {
            self.map = Some(into);
        }
        self.dirty = true;
    }

    /// A live object landing on a point saved under another map id: same world.
    fn merge_if_same_world(&mut self, code: u32, pos: &Vec3) {
        let Some(cur) = self.map else { return };
        let other = self
            .points
            .iter()
            .find(|p| p.map != cur && p.code == code && p.pos.dist2d(pos) < SAME_POINT_CM)
            .map(|p| p.map);
        if let Some(other) = other {
            self.merge_maps(cur, other);
        }
    }

    pub fn set_map(&mut self, map: u32) {
        let map = self.canonical(map);
        if self.map == Some(map) {
            return;
        }
        if self.map.is_some() {
            // Live objects belong to the map we just left.
            self.objects.clear();
        }
        self.map = Some(map);
        self.record_pending();
    }

    pub fn object_spawned(&mut self, id: i32, kind: u16, code: u32, pos: Vec3, now_ms: i64) {
        let obj = RadarObject { id, kind, code, pos, seen_ms: now_ms };
        self.objects.insert(id, obj.clone());
        self.estimator.object_entered(pos.x, pos.y, code, now_ms);
        self.update_estimate(now_ms);
        if self.map.is_none() {
            self.pending.push(obj);
            // Started mid-zone: a live object on a remembered point tells us the map.
            let found = self
                .points
                .iter()
                .find(|p| p.code == code && p.pos.dist2d(&pos) < SAME_POINT_CM)
                .map(|p| p.map);
            if let Some(map) = found {
                tracing::info!("radar: map {} inferred from a remembered point", map);
                self.map = Some(self.canonical(map));
                self.record_pending();
            }
            return;
        }
        self.merge_if_same_world(code, &pos);
        self.record_point(kind, code, pos, now_ms);
    }

    /// `depleted`: `35 36` reason 04 — gathered out, rather than out of view.
    pub fn object_removed(&mut self, id: i32, depleted: bool, now_ms: i64) {
        let Some(obj) = self.objects.remove(&id) else { return };
        if !depleted {
            self.estimator.object_left(obj.pos.x, obj.pos.y, obj.code, now_ms);
            self.update_estimate(now_ms);
            return;
        }
        if let Some(i) = self.find_point(obj.code, &obj.pos) {
            self.points[i].last_depleted_ms = now_ms;
            self.dirty = true;
        }
    }

    pub fn set_self_pos(&mut self, pos: Vec3, now_ms: i64) {
        if self.estimate_only {
            self.truth = Some((pos, now_ms));
            if self.self_pos.is_some() {
                return;
            }
        }
        self.self_pos = Some((pos, now_ms));
        self.estimator.anchor(pos.x, pos.y, now_ms);
    }

    /// A mob, NPC or other player moved (`1A`–`1E`/`28`/`29`/`2E`/`2F 37`):
    /// they are only sent while near, so they say roughly where we are.
    pub fn entity_moved(&mut self, id: u32, pos: Vec3, now_ms: i64) {
        self.others.insert(id);
        if self.guessed_self == Some(id) {
            // It moves, so it is someone else: a wrong guess.
            self.guessed_self = None;
        }
        self.estimator.entity_moved(id, pos.x, pos.y, now_ms);
        self.update_estimate(now_ms);
    }

    /// A mob, NPC or other player came into view (`41 36`/`45 36`).
    pub fn entity_spawned(&mut self, id: u32, pos: Vec3, now_ms: i64) {
        self.others.insert(id);
        self.estimator.entity_spawned(pos.x, pos.y, now_ms);
        self.update_estimate(now_ms);
    }

    fn update_estimate(&mut self, now_ms: i64) {
        let live = self.objects.values().map(|o| (o.pos.x, o.pos.y, o.code));
        self.estimator.update(live, now_ms);
    }

    /// Ignore server positions after the first and keep them as ground truth,
    /// to measure the estimator on a capture as if playing another class.
    pub fn estimate_only(&mut self) {
        self.estimate_only = true;
    }

    pub fn truth(&self) -> Option<(Vec3, i64)> {
        self.truth
    }

    /// Where we are: the server's position, or the estimate when it is newer.
    /// (position, when, estimate error in m)
    fn position(&self) -> Option<(Vec3, i64, Option<f32>)> {
        match (self.self_pos, self.estimator.estimate()) {
            (Some((p, t)), Some(e)) if e.t > t => {
                Some((Vec3 { x: e.x * 100.0, y: e.y * 100.0, z: p.z }, e.t, Some(e.err_m)))
            }
            (Some((p, t)), _) => Some((p, t, None)),
            // Started mid-zone, no buff yet: height from the nearest object.
            (None, Some(e)) => {
                let (x, y) = (e.x * 100.0, e.y * 100.0);
                let z = self
                    .objects
                    .values()
                    .map(|o| ((o.pos.x - x).hypot(o.pos.y - y), o.pos.z))
                    .min_by(|a, b| a.0.total_cmp(&b.0))
                    .map_or(0.0, |(_, z)| z);
                Some((Vec3 { x, y, z }, e.t, Some(e.err_m)))
            }
            (None, None) => None,
        }
    }

    /// A buff we put on ourselves (`2A 38`/`2B 38`, caster = target = `id`)
    /// or a skill we cast (long `02 38`, with `heading`): `id` is at `pos`,
    /// exactly. Ours when `id` is us — see `is_self` while our id is unknown.
    pub fn self_cast(&mut self, id: u32, self_id: Option<u32>, pos: Vec3, heading: Option<f32>, now_ms: i64) {
        if !self.is_self(id, self_id, Some(pos)) {
            return;
        }
        self.set_self_pos(pos, now_ms);
        if let Some(h) = heading {
            self.set_self_heading(h, now_ms);
        }
    }

    /// Short `02 38` (the mantra tick): `id` faces `degrees`.
    pub fn self_facing(&mut self, id: u32, self_id: Option<u32>, degrees: f32, now_ms: i64) {
        if self.is_self(id, self_id, None) {
            self.set_self_heading(degrees, now_ms);
        }
    }

    /// Whether `id` is the local player. The meter's own binding (`self_id`)
    /// decides when it has one; it comes from `33 36`/`4A 36`, and without
    /// the mantra `4A 36` only shows when HP/MP change. Until then an id
    /// counts as us when it was never seen moving or spawning (the server
    /// sends everyone's movement but ours) and the record puts it `at` where
    /// we think we are; it is then remembered, and dropped if it ever moves.
    fn is_self(&mut self, id: u32, self_id: Option<u32>, at: Option<Vec3>) -> bool {
        if let Some(s) = self_id.or(self.guessed_self) {
            return s == id;
        }
        let (Some(at), Some((me, _, err))) = (at, self.position()) else { return false };
        let reach = 1_000.0 + 2.0 * err.unwrap_or(0.0) * 100.0;
        if self.others.contains(&id) || at.dist2d(&me) > reach {
            return false;
        }
        tracing::info!("radar: entity {} taken as the local player", id);
        self.guessed_self = Some(id);
        true
    }

    pub fn map(&self) -> Option<u32> {
        self.map
    }

    pub fn self_pos(&self) -> Option<Vec3> {
        self.position().map(|(p, _, _)| p)
    }

    /// Where we are, and the estimate's error in metres when estimated.
    pub fn self_pos_with_error(&self) -> Option<(Vec3, Option<f32>)> {
        self.position().map(|(p, _, e)| (p, e))
    }

    /// `38 36` / `3A 36`: `gatherer` is working `object_id`. When that is us,
    /// we stand at the object (0.8 m from the Sapphire's coordinates on
    /// 2026-10-12) — as good as a position from the server.
    ///
    /// Without our id (`self_id`; without the mantra it can take minutes to
    /// arrive) a gatherer is taken to be us when it was never seen moving
    /// (the server sends everyone's movement but ours) and the object is
    /// where we think we are. An object that was already in view when the
    /// radar started is placed on the nearest remembered point of its kind
    /// and becomes live, so it shows in colour and its depletion is caught.
    pub fn gathering(&mut self, object_id: i32, gatherer: u32, self_id: Option<u32>, now_ms: i64) {
        let me = self.position();
        let known = self.objects.get(&object_id).map(|o| o.pos);
        let near = known.or(me.map(|m| m.0));
        if !self.is_self(gatherer, self_id, near) {
            return;
        }
        let pos = match known {
            Some(p) => p,
            None => {
                // Already in view at startup: the nearest gatherable point.
                let Some((here, _, err)) = me else { return };
                let reach = 2_500.0f32.max(2.0 * err.unwrap_or(0.0) * 100.0);
                let Some(map) = self.map else { return };
                let Some(p) = self
                    .points
                    .iter()
                    .filter(|p| p.map == map && p.kind == 4 && p.pos.dist2d(&here) < reach)
                    .min_by(|a, b| a.pos.dist2d(&here).total_cmp(&b.pos.dist2d(&here)))
                    .cloned()
                else {
                    return;
                };
                tracing::info!("radar: gathering {} placed on a remembered {} point", object_id, p.code);
                self.objects.insert(
                    object_id,
                    RadarObject { id: object_id, kind: p.kind, code: p.code, pos: p.pos, seen_ms: now_ms },
                );
                p.pos
            }
        };
        if self.estimate_only {
            return;
        }
        self.self_pos = Some((pos, now_ms));
        self.estimator.anchor_gathering(pos.x, pos.y, now_ms);
    }

    pub fn live_objects(&self) -> Vec<RadarObject> {
        let mut v: Vec<RadarObject> = self.objects.values().cloned().collect();
        v.sort_by_key(|o| o.id);
        v
    }

    /// Objects carried over from a previous run of the meter in this session.
    /// They were recorded as points then, so they only rejoin the live set.
    pub fn restore_live(&mut self, map: Option<u32>, objects: Vec<RadarObject>) -> usize {
        if let (None, Some(map)) = (self.map, map) {
            self.set_map(map);
        }
        let mut n = 0;
        for o in objects {
            if !self.objects.contains_key(&o.id) {
                // Real objects in view, so they can tell two map ids apart too.
                self.merge_if_same_world(o.code, &o.pos);
                self.objects.insert(o.id, o);
                n += 1;
            }
        }
        n
    }

    pub fn set_self_heading(&mut self, degrees: f32, now_ms: i64) {
        self.self_heading = Some((degrees.rem_euclid(360.0), now_ms));
    }

    /// `23 36`: the local player teleported, possibly to another map. Sightings
    /// still waiting for a map can no longer be placed safely.
    pub fn self_teleported(&mut self, pos: Vec3, now_ms: i64) {
        self.estimator.reset(now_ms);
        // Even when evaluating: a zone load tells every class where it is.
        self.self_pos = None;
        self.set_self_pos(pos, now_ms);
        if self.map.is_none() {
            self.pending.clear();
        }
    }

    /// Entity ids are session-scoped: live objects from the previous zone must
    /// go. The map is left alone: the self id is bound by a scan that runs
    /// *after* the same bundle's `33 36` already set the new map, and every zone
    /// load carries a self record that moves it. Remembered points stay.
    pub fn clear(&mut self) {
        self.objects.clear();
        self.self_pos = None;
        self.self_heading = None;
        self.estimator = PositionEstimator::default();
        self.others.clear();
        self.guessed_self = None;
    }

    pub fn snapshot(&self) -> RadarSnapshot {
        let mut objects: Vec<RadarObject> = self.objects.values().cloned().collect();
        objects.sort_by_key(|o| o.id);
        let points = match self.map {
            Some(map) => self
                .points
                .iter()
                .filter(|p| p.map == map)
                .map(|p| PointView {
                    point: p.clone(),
                    live: objects
                        .iter()
                        .any(|o| o.code == p.code && o.pos.dist2d(&p.pos) < SAME_POINT_CM),
                })
                .collect(),
            None => Vec::new(),
        };
        let position = self.position();
        let estimated = position.is_some_and(|p| p.2.is_some());
        // A facing that stopped coming (mantra off) would point the disc the
        // wrong way while the position is estimated: drop it, north-up it is.
        let heading = self
            .self_heading
            .filter(|&(_, t)| !position.is_some_and(|p| p.2.is_some() && p.1 - t > 3_000))
            .map(|(h, _)| h);
        RadarSnapshot {
            map: self.map,
            self_pos: position.map(|p| p.0),
            self_heading: heading,
            self_pos_ms: position.map(|p| p.1).unwrap_or(0),
            self_pos_estimated: estimated,
            self_pos_error_m: position.and_then(|p| p.2),
            objects,
            points,
        }
    }

    fn find_point(&self, code: u32, pos: &Vec3) -> Option<usize> {
        let map = self.map?;
        self.points
            .iter()
            .position(|p| p.map == map && p.code == code && p.pos.dist2d(pos) < SAME_POINT_CM)
    }

    fn record_point(&mut self, kind: u16, code: u32, pos: Vec3, now_ms: i64) {
        let Some(map) = self.map else { return };
        match self.find_point(code, &pos) {
            Some(i) => {
                let p = &mut self.points[i];
                p.last_seen_ms = now_ms;
                p.seen_count += 1;
            }
            None => self.points.push(SpawnPoint {
                map,
                code,
                kind,
                pos,
                first_seen_ms: now_ms,
                last_seen_ms: now_ms,
                seen_count: 1,
                last_depleted_ms: 0,
            }),
        }
        self.dirty = true;
    }

    fn record_pending(&mut self) {
        for o in std::mem::take(&mut self.pending) {
            self.record_point(o.kind, o.code, o.pos, o.seen_ms);
        }
    }
}

/// A name the user gave an object code from the radar (`radar_labels.json`).
/// The packets carry no names, and the project has no gather-node data, so
/// this is how Diamond, Asvata, … get onto the radar.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RadarLabel {
    pub name: String,
    /// CSS colour, e.g. `#ff3b3b`.
    pub color: String,
    /// `dot` or `square`.
    pub shape: String,
}

pub fn load_labels(path: &Path) -> HashMap<u32, RadarLabel> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

/// Set (`Some`) or remove (`None`) the label of one code, and save.
pub fn set_label(path: &Path, code: u32, label: Option<RadarLabel>) -> std::io::Result<HashMap<u32, RadarLabel>> {
    let mut labels = load_labels(path);
    match label {
        Some(mut l) => {
            l.name = l.name.trim().chars().take(40).collect();
            if l.shape != "square" {
                l.shape = "dot".into();
            }
            labels.insert(code, l);
        }
        None => {
            labels.remove(&code);
        }
    }
    let json = serde_json::to_string_pretty(&labels).map_err(std::io::Error::other)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, path)?;
    Ok(labels)
}

/// Saved live objects older than this are not trusted, even in the same session.
const LIVE_MAX_AGE_MS: i64 = 30 * 60 * 1000;
/// Only objects this close to where you are now come back. The server keeps
/// an object in view up to ~62–64 m; anything further would have been
/// removed while the meter was not listening.
const LIVE_RESTORE_RADIUS_CM: f32 = 7_000.0;

#[derive(Serialize, Deserialize)]
struct SavedLive {
    local_id: i64,
    port: u16,
    saved_at_ms: i64,
    map: Option<u32>,
    objects: Vec<RadarObject>,
}

/// Keeps the objects in view across meter restarts within one game session.
/// The fingerprint is the local entity id plus the game port, both reissued on
/// a new session (same rule as `mob_cache`).
pub struct LiveCache {
    path: PathBuf,
    restore_attempted: bool,
    last_saved: Option<(i64, u16, Option<u32>, Vec<i32>)>,
}

impl LiveCache {
    pub fn new(app_data_dir: &Path) -> Self {
        Self { path: app_data_dir.join("radar_live.json"), restore_attempted: false, last_saved: None }
    }

    /// Call periodically. Restores once, as soon as the session and your
    /// position are known (before the first save, so the old file isn't
    /// clobbered), then saves whenever the set of objects in view changes.
    pub fn tick(&mut self, storage: &crate::combat::data_storage::DataStorage, port: Option<u16>, now_ms: i64) {
        let Some(port) = port else { return };
        // 0 = not known yet: the self id comes from `4A 36`, which without the
        // Chanter's mantra only shows up when HP/MP change.
        let local_id = storage.local_player_id().unwrap_or(0);
        if !self.restore_attempted {
            let Some((me, err_m)) = storage.radar().self_pos_with_error() else { return };
            self.restore_attempted = true;
            let n = self.restore(storage, local_id, port, me, err_m, now_ms);
            if n > 0 {
                tracing::info!("radar: restored {} objects in view from the previous run", n);
            }
        }
        let (map, objects) = {
            let radar = storage.radar();
            (radar.map(), radar.live_objects())
        };
        let sig = (local_id, port, map, objects.iter().map(|o| o.id).collect::<Vec<_>>());
        if self.last_saved.as_ref() == Some(&sig) {
            return;
        }
        let saved = SavedLive { local_id, port, saved_at_ms: now_ms, map, objects };
        if let Ok(json) = serde_json::to_string(&saved) {
            let tmp = self.path.with_extension("json.tmp");
            if std::fs::write(&tmp, json).is_ok() && std::fs::rename(&tmp, &self.path).is_ok() {
                self.last_saved = Some(sig);
            }
        }
    }

    fn restore(
        &self,
        storage: &crate::combat::data_storage::DataStorage,
        local_id: i64,
        port: u16,
        me: Vec3,
        err_m: Option<f32>,
        now: i64,
    ) -> usize {
        let Ok(text) = std::fs::read_to_string(&self.path) else { return 0 };
        let Ok(saved) = serde_json::from_str::<SavedLive>(&text) else { return 0 };
        // Same connection (port) is the same session; the ids only have to
        // agree when both are known.
        let other_player = local_id != 0 && saved.local_id != 0 && saved.local_id != local_id;
        if other_player || saved.port != port || now - saved.saved_at_ms > LIVE_MAX_AGE_MS {
            return 0;
        }
        // An estimated position can be off by its error, twice to be safe.
        let radius = LIVE_RESTORE_RADIUS_CM + err_m.map_or(0.0, |e| (2.0 * e * 100.0).min(10_000.0));
        let near: Vec<RadarObject> =
            saved.objects.into_iter().filter(|o| o.pos.dist2d(&me) < radius).collect();
        storage.radar().restore_live(saved.map, near)
    }
}

fn varint(b: &[u8], at: usize) -> Option<(u32, usize)> {
    let (mut v, mut shift, mut n) = (0u32, 0, 0);
    loop {
        let x = *b.get(at + n)? as u32;
        n += 1;
        v |= (x & 0x7F) << shift;
        if x & 0x80 == 0 {
            return Some((v, n));
        }
        shift += 7;
        if shift >= 32 {
            return None;
        }
    }
}

fn enc_varint(mut v: u32) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let b = (v & 0x7F) as u8;
        v >>= 7;
        if v == 0 {
            out.push(b);
            return out;
        }
        out.push(b | 0x80);
    }
}

#[derive(Debug, PartialEq)]
pub struct ObjectSpawn {
    pub id: i32,
    pub kind: u16,
    pub code: u32,
    pub pos: Vec3,
}

/// `34 36` body (bytes after the opcode).
pub fn parse_object_spawn(body: &[u8]) -> Option<ObjectSpawn> {
    let (id, n) = varint(body, 0)?;
    if !(1..10_000_000).contains(&id) {
        return None;
    }
    let kind = u16::from_le_bytes(body.get(n..n + 2)?.try_into().ok()?);
    let code = u32::from_le_bytes(body.get(n + 2..n + 6)?.try_into().ok()?);
    if kind > 0xFF || !(1_000_000..10_000_000).contains(&code) {
        return None;
    }
    let pos = Vec3::read(body, n + 6)?;
    Some(ObjectSpawn { id: id as i32, kind, code, pos })
}

/// `35 36` body: `<id varint> <reason> <?> <?>` — exactly three bytes after the id.
pub fn parse_object_despawn(body: &[u8]) -> Option<(i32, u8)> {
    let (id, n) = varint(body, 0)?;
    (body.len() == n + 3 && (1..10_000_000).contains(&id)).then(|| (id as i32, body[n]))
}

/// `2B 38` / `2A 38` body: (target, position at the end) when the caster is
/// the target itself — a buff someone put on themselves. The caster sits a
/// few bytes before the position (after the epoch-ms timestamp), followed by
/// `<1–2 bytes> 02` in a tick and `01 00` in an add.
pub fn parse_self_cast_position(body: &[u8]) -> Option<(u32, Vec3)> {
    let (target, n) = varint(body, 0)?;
    if body.len() < 30 {
        return None;
    }
    let pos_at = body.len() - 12;
    let needle = enc_varint(target);
    let caster_area = &body[(pos_at - 10).max(n)..pos_at];
    if !caster_area.windows(needle.len()).any(|w| w == needle.as_slice()) {
        return None;
    }
    let pos = Vec3::read(body, pos_at).filter(far_from_origin)?;
    Some((target, pos))
}

/// `parse_self_cast_position` for a known self id.
pub fn parse_self_position(body: &[u8], self_id: u32) -> Option<Vec3> {
    parse_self_cast_position(body).filter(|(t, _)| *t == self_id).map(|(_, p)| p)
}

/// `02 38` body about one entity: `<id> 00 <u32> <u16 counter> <id>
/// <facing f32> [<x y z>] <4 bytes>` -> (id, facing, position). The short form
/// is the mantra tick; with a position it is a skill cast. A long form whose
/// three floats are not a place (a direction-like vector) is neither.
pub fn parse_facing(body: &[u8]) -> Option<(u32, f32, Option<Vec3>)> {
    let (id, n) = varint(body, 0)?;
    if *body.get(n)? != 0 {
        return None;
    }
    let (id2, n2) = varint(body, n + 7)?;
    if id2 != id {
        return None;
    }
    let at = n + 7 + n2;
    let deg = f32::from_le_bytes(body.get(at..at + 4)?.try_into().ok()?);
    if !(deg.is_finite() && (-360.0..=360.0).contains(&deg)) {
        return None;
    }
    let pos = if body.len() == at + 8 {
        None
    } else if body.len() == at + 20 {
        Some(Vec3::read(body, at + 4).filter(far_from_origin)?)
    } else {
        return None;
    };
    Some((id, deg, pos))
}

/// `02 38` body: the local player's facing, when the record is about them
/// (`<self> 00 <u32> <u16> <self> <f32>`). The same opcode carries other
/// shapes and other entities; the doubled self id is the guard.
///
/// The u16 counts up once per record from zone load. Read as a varint it
/// happens to be two bytes too once it passes 127 — which hid the bug on the
/// first capture and broke facing for the first two minutes of every zone.
pub fn parse_self_heading(body: &[u8], self_id: u32) -> Option<f32> {
    parse_facing(body).filter(|(id, _, pos)| *id == self_id && pos.is_none()).map(|(_, d, _)| d)
}

/// Movement of a mob, NPC or other player (`1A`–`1E`, `28`, `29`, `2E`, `2F
/// 37`): `<id varint> <1–2 flag bytes> <x y z> …`. The position is the first
/// plausible one within a few bytes of the id (the flags vary by opcode).
pub fn parse_entity_move(body: &[u8]) -> Option<(u32, Vec3)> {
    let (id, n) = varint(body, 0)?;
    let pos = (n..=n + 4).find_map(|i| Vec3::read(body, i).filter(far_from_origin))?;
    Some((id, pos))
}

/// `41 36` / `45 36` spawn of a mob, NPC or player: the first plausible
/// position in the record (it follows variable-length blocks).
pub fn parse_entity_spawn_pos(body: &[u8]) -> Option<(u32, Vec3)> {
    let (id, n) = varint(body, 0)?;
    let pos = (n..body.len().saturating_sub(11)).find_map(|i| Vec3::read(body, i).filter(far_from_origin))?;
    Some((id, pos))
}

/// Both horizontal coordinates past 10 m: a world position rather than small
/// floats (scales, directions) that happen to read as one.
fn far_from_origin(v: &Vec3) -> bool {
    v.x.abs() > 1_000.0 && v.y.abs() > 1_000.0
}

/// `38 36` (start) / `3A 36` (n-th done) body: `<object varint> <gatherer
/// varint> <n> …` -> (object, gatherer).
pub fn parse_gathering(body: &[u8]) -> Option<(i32, u32)> {
    let (obj, n) = varint(body, 0)?;
    let (who, _) = varint(body, n)?;
    ((1..10_000_000).contains(&obj) && (1..10_000_000).contains(&who)).then_some((obj as i32, who))
}

/// `23 36` body with entity 0: the local player teleported (zone load included).
pub fn parse_self_teleport(body: &[u8]) -> Option<Vec3> {
    if body.len() < 18 || body[..6].iter().any(|&b| b != 0) {
        return None;
    }
    Vec3::read(body, 6)
}

/// `33 36` body: the map id, found by the shape around it (see module docs).
pub fn parse_map_from_self_record(body: &[u8]) -> Option<u32> {
    let u32_at = |i: usize| -> Option<u32> { Some(u32::from_le_bytes(body.get(i..i + 4)?.try_into().ok()?)) };
    (0..body.len().saturating_sub(35)).find_map(|i| {
        let map = u32_at(i)?;
        let shaped = (1..100_000).contains(&map)
            && u32_at(i + 4)? == map
            && body[i + 24..i + 26] == [0, 0]
            && body[i + 32..i + 35] == [0x01, 0xDE, 0x02]
            && Vec3::read(body, i + 8).is_some();
        shaped.then_some(map)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        s.split_whitespace().map(|b| u8::from_str_radix(b, 16).unwrap()).collect()
    }

    #[test]
    fn sapphire_spawn() {
        // 14:21:49.880, length + opcode stripped.
        let body = hex("92 84 02 04 00 C4 A6 17 00 40 9E 28 C8 00 A6 FD 46 00 00 D6 46 00 00 00 00 00 00 00 00 01 01 01 B2 D5 0A 31 A1 01 00 00 00 00 00 00 00 00 00 00 1E 31 D1 D0 07 00 00");
        let s = parse_object_spawn(&body).unwrap();
        assert_eq!((s.id, s.kind, s.code), (33298, 4, 1_550_020));
        assert_eq!((s.pos.x.round(), s.pos.y.round(), s.pos.z.round()), (-172665.0, 32467.0, 27392.0));
    }

    #[test]
    fn despawn_reasons() {
        assert_eq!(parse_object_despawn(&hex("92 84 02 04 03 00")), Some((33298, 4)));
        assert_eq!(parse_object_despawn(&hex("AF B9 03 01 01 00")), Some((56495, 1)));
        assert_eq!(parse_object_despawn(&hex("92 84 02 04 03")), None);
    }

    #[test]
    fn self_position_from_buff_tick() {
        let body = hex("D9 67 13 35 7B 94 D7 0A 60 09 00 00 00 00 00 00 C6 AA 21 31 A1 01 00 00 D9 67 14 D9 8E 15 01 02 43 D7 21 C8 15 87 C4 46 00 C0 D5 46");
        let p = parse_self_position(&body, 13273).unwrap();
        assert_eq!((p.x.round(), p.y.round()), (-165725.0, 25156.0));
        // Same record for someone else, or when only the target is us.
        assert_eq!(parse_self_position(&body, 11212), None);
        let short = hex("D9 67 11 13 84 4E 00 00 41 13 85 4C 01 00 00 00 B2 99 A6 7D A2 01 00 00 D9 67 07 02 43 D7 21 C8 15 87 C4 46 00 C0 D5 46");
        assert!(parse_self_position(&short, 13273).is_some());
        // A buff add (`2A 38`): `01 00` between the caster and the position.
        let add = hex("D9 67 01 11 01 EB 2E 00 00 FF FF FF FF FF FF FF FF 80 75 D5 2A BB 03 00 00 D9 67 01 00 43 D7 21 C8 15 87 C4 46 00 C0 D5 46");
        assert_eq!(parse_self_cast_position(&add).map(|(id, p)| (id, p.x.round())), Some((13273, -165725.0)));
    }

    #[test]
    fn skill_cast_gives_position_and_facing() {
        // 2026-10-12 14:21:35, 3.8 m from the mantra position a second off.
        let cast = hex("D9 67 00 E4 D0 12 01 30 02 D9 67 03 78 A5 42 3B C5 21 C8 28 F7 CB 46 E7 56 D5 46 99 6A 01 00");
        let (id, deg, pos) = parse_facing(&cast).unwrap();
        assert_eq!((id, deg.round()), (13273, 83.0));
        let pos = pos.unwrap();
        assert_eq!((pos.x.round(), pos.y.round()), (-165653.0, 26108.0));
        assert_eq!(parse_self_heading(&cast, 13273), None, "not the mantra tick");
    }

    #[test]
    fn our_own_buff_identifies_us_without_an_id() {
        let mut t = RadarTracker::default();
        let here = Vec3 { x: 100_000.0, y: 30_000.0, z: 0.0 };
        t.set_self_pos(here, 1);
        // Another player, seen moving, buffing themselves nearby: not us.
        t.entity_moved(44, here, 2);
        t.self_cast(44, None, Vec3 { x: here.x + 500.0, ..here }, None, 3);
        assert_eq!(t.snapshot().self_pos, Some(here));
        // An id never seen moving, buffing itself 3 m from us: us.
        let moved = Vec3 { x: here.x + 300.0, ..here };
        t.self_cast(9_878, None, moved, Some(90.0), 4);
        assert_eq!(t.snapshot().self_pos, Some(moved));
        assert_eq!(t.snapshot().self_heading, Some(90.0));
        // From then on only that id is us.
        t.self_cast(1_234, None, here, None, 5);
        assert_eq!(t.snapshot().self_pos, Some(moved));
    }

    #[test]
    fn map_from_self_record() {
        // Tails of the 2026-10-12 and 2026-09-27 self records.
        let today = hex("03 0E 00 56 04 00 00 00 00 00 00 56 04 00 00 F5 03 00 00 F5 03 00 00 91 03 22 C8 AC 2B C5 46 00 FC D5 46 8B 57 04 00 00 00 61 09 00 C0 D5 46 01 DE 02 0C 28 00 64 00 00 00");
        assert_eq!(parse_map_from_self_record(&today), Some(1013));
        let sept = hex("00 00 E9 03 00 00 E9 03 00 00 F3 F9 99 C7 2F 29 04 C8 00 ED 0D 47 E8 FE 03 00 00 00 E0 07 00 4E 05 47 01 DE 02 0B 64 64");
        assert_eq!(parse_map_from_self_record(&sept), Some(1001));
    }

    #[test]
    fn points_are_remembered_per_map_and_infer_the_map() {
        let pos = Vec3 { x: -172665.0, y: 32467.0, z: 27392.0 };
        let mut t = RadarTracker::default();
        t.set_map(1013);
        t.object_spawned(33298, 4, 1_550_020, pos, 1);
        // Respawned with a new id on the same spot: still one point.
        t.object_removed(33298, true, 2);
        t.object_spawned(40000, 4, 1_550_020, Vec3 { x: pos.x + 30.0, ..pos }, 3);
        let snap = t.snapshot();
        assert_eq!(snap.points.len(), 1);
        assert_eq!((snap.points[0].point.seen_count, snap.points[0].point.last_depleted_ms), (2, 2));
        assert!(snap.points[0].live);

        // New session, meter started mid-zone: map unknown until a live object
        // lands on a remembered point.
        let mut t = RadarTracker { points: t.points, ..Default::default() };
        t.object_spawned(50000, 1, 1_100_001, Vec3 { x: 0.0, y: 5000.0, z: 0.0 }, 4);
        assert_eq!(t.snapshot().map, None);
        t.object_spawned(50001, 4, 1_550_020, pos, 5);
        let snap = t.snapshot();
        assert_eq!(snap.map, Some(1013));
        assert_eq!(snap.points.len(), 2, "the object seen before the map was known is recorded too");
    }

    #[test]
    fn a_new_id_for_the_same_world_is_merged_and_remembered() {
        let node = Vec3 { x: -89228.0, y: 12189.0, z: 0.0 };
        let mut t = RadarTracker::default();
        t.set_map(1013);
        t.object_spawned(1, 1, 1_100_001, node, 1);
        // Later session: the self record says 1052; a new point is seen first,
        // then a node exactly on a 1013 point.
        let mut t = RadarTracker { points: t.points, ..Default::default() };
        t.set_map(1052);
        t.object_spawned(2, 4, 1_550_004, Vec3 { x: -92340.0, y: 10131.0, z: 0.0 }, 2);
        t.object_spawned(3, 1, 1_100_001, Vec3 { x: node.x + 20.0, ..node }, 3);
        let snap = t.snapshot();
        assert_eq!(snap.map, Some(1013));
        assert_eq!(snap.points.len(), 2, "the 1052 sighting moved over, the shared node folded");
        assert_eq!(snap.points.iter().find(|p| p.point.code == 1_100_001).unwrap().point.seen_count, 2);

        // Next time 1052 is announced it goes straight to 1013.
        let mut t = RadarTracker { points: t.points, aliases: t.aliases, ..Default::default() };
        t.set_map(1052);
        assert_eq!(t.snapshot().map, Some(1013));
        assert_eq!(t.snapshot().points.len(), 2);
    }

    #[test]
    fn gathering_without_our_id_places_us_and_lights_the_node() {
        let diamond = Vec3 { x: 117_717.0, y: 29_491.0, z: 1004.0 };
        let mut t = RadarTracker::default();
        t.set_map(1013);
        t.object_spawned(1, 4, 1_550_021, diamond, 1);
        t.object_removed(1, false, 2); // walked away: a remembered point now
        t.set_self_pos(Vec3 { x: diamond.x + 1_500.0, ..diamond }, 3);
        // Someone seen moving gathers a node: not us.
        t.entity_moved(500, Vec3 { x: 120_000.0, y: 30_000.0, z: 0.0 }, 4);
        t.gathering(77, 500, None, 5);
        assert!(t.snapshot().objects.is_empty());
        // An id never seen moving gathers next to us: us, on the Diamond.
        t.gathering(77, 9_878, None, 6);
        let snap = t.snapshot();
        assert_eq!(snap.objects.len(), 1, "the node is live again");
        assert_eq!(snap.objects[0].code, 1_550_021);
        assert_eq!(snap.self_pos, Some(diamond));
        assert!(snap.points.iter().any(|p| p.live));
    }

    #[test]
    fn a_teleport_drops_sightings_still_waiting_for_a_map() {
        let mut t = RadarTracker::default();
        t.object_spawned(1, 4, 1_550_020, Vec3 { x: 100.0, y: 100.0, z: 0.0 }, 1);
        t.self_teleported(Vec3 { x: 9000.0, y: 9000.0, z: 0.0 }, 2);
        t.clear();
        t.set_map(1001);
        assert!(t.snapshot().points.is_empty());
    }

    #[test]
    fn heading() {
        // 14:35:46.355, facing 91.2°.
        let body = hex("D9 67 00 D9 8E 15 01 98 00 D9 67 2A 72 B6 42 99 6A 01 00");
        assert_eq!(parse_self_heading(&body, 13273).map(|d| (d * 10.0).round()), Some(912.0));
        assert_eq!(parse_self_heading(&body, 11212), None);
        // Just after a zone load the counter is small: `01 00`, not a varint.
        let fresh = hex("AE 62 00 D9 8E 15 01 01 00 AE 62 02 EC A3 43 99 6A 01 00");
        assert_eq!(parse_self_heading(&fresh, 12590).map(|d| d.round()), Some(328.0));
        let vector = hex("AE 62 00 E5 A8 12 01 2C 02 AE 62 75 A6 C3 C1 B7 ED C7 42 E2 07 2B 40 00 00 00 00 90 4E 01 F4 03");
        assert_eq!(parse_self_heading(&vector, 12590), None);
        let mut t = RadarTracker::default();
        t.set_self_heading(-39.6, 0);
        assert_eq!(t.snapshot().self_heading.map(|d| d.round()), Some(320.0));
    }

    #[test]
    fn live_objects_come_back_after_a_meter_restart_in_the_same_session() {
        use crate::combat::data_storage::DataStorage;
        let dir = std::env::temp_dir().join(format!("a2_radar_live_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cube = Vec3 { x: -202560.0, y: 32926.0, z: 27488.0 };
        let far = Vec3 { x: cube.x + 20_000.0, ..cube };

        // First run: the cube and a far object are in view.
        let s1 = DataStorage::new();
        s1.set_local_player_id(Some(3211));
        {
            let mut r = s1.radar();
            r.set_map(1013);
            r.set_self_pos(cube, 1);
            r.object_spawned(55715, 4, 1_301_199, cube, 1);
            r.object_spawned(60000, 4, 1_550_000, far, 1);
        }
        LiveCache::new(&dir).tick(&s1, Some(61143), 10);

        // Restart in the same session, standing next to the cube.
        let s2 = DataStorage::new();
        s2.set_local_player_id(Some(3211));
        s2.radar().set_self_pos(Vec3 { x: cube.x + 300.0, ..cube }, 20);
        LiveCache::new(&dir).tick(&s2, Some(61143), 20);
        let snap = s2.radar().snapshot();
        assert_eq!(snap.map, Some(1013));
        assert_eq!(snap.objects.iter().map(|o| o.id).collect::<Vec<_>>(), vec![55715], "only what is still in range");

        // A new session (other port) restores nothing.
        let s3 = DataStorage::new();
        s3.set_local_player_id(Some(3211));
        s3.radar().set_self_pos(cube, 30);
        LiveCache::new(&dir).tick(&s3, Some(50000), 30);
        assert!(s3.radar().snapshot().objects.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn labels_round_trip() {
        let path = std::env::temp_dir().join(format!("a2_radar_labels_{}.json", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let diamond = RadarLabel { name: "  Diamond ".into(), color: "#e8f4ff".into(), shape: "star".into() };
        set_label(&path, 1_550_004, Some(diamond)).unwrap();
        let l = &load_labels(&path)[&1_550_004];
        assert_eq!((l.name.as_str(), l.shape.as_str()), ("Diamond", "dot"));
        set_label(&path, 1_550_004, None).unwrap();
        assert!(load_labels(&path).is_empty());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn teleport_position() {
        let body = hex("00 00 00 00 00 00 43 D7 21 C8 15 87 C4 46 00 C0 D5 46 05 00 00 00 91 7A");
        assert_eq!(parse_self_teleport(&body).map(|p| p.x.round()), Some(-165725.0));
    }
}
