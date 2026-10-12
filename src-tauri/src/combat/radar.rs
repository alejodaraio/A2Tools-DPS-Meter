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
//! records `2B 38`/`2A 38` end with the position of the entity they concern,
//! about once a second; they also carry other players, so a record is only
//! taken as ours when both its target and its caster are the local player.
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
//! A spawn is sent once, on entering view. A meter restarted next to an object
//! never hears of it again until you walk away and back (a Hidden Cube 3–6 m
//! away stayed a remembered outline for minutes). So the objects in view are
//! also kept in `radar_live.json` with the session fingerprint, as the mob
//! cache does, and restored into the same session (see `LiveCache`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

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

    fn dist2d(&self, o: &Vec3) -> f32 {
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
    /// Objects in view right now.
    pub objects: Vec<RadarObject>,
    /// Every remembered spawn point on the current map.
    pub points: Vec<PointView>,
}

#[derive(Default)]
pub struct RadarTracker {
    objects: HashMap<i32, RadarObject>,
    self_pos: Option<(Vec3, i64)>,
    self_heading: Option<f32>,
    map: Option<u32>,
    points: Vec<SpawnPoint>,
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
                Ok(saved) => self.points = saved.points,
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
        let saved = SavedPoints { version: 1, points: self.points.clone() };
        serde_json::to_string(&saved).ok().map(|json| (path, json))
    }

    pub fn set_map(&mut self, map: u32) {
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
                self.map = Some(map);
                self.record_pending();
            }
            return;
        }
        self.record_point(kind, code, pos, now_ms);
    }

    /// `depleted`: `35 36` reason 04 — gathered out, rather than out of view.
    pub fn object_removed(&mut self, id: i32, depleted: bool, now_ms: i64) {
        let Some(obj) = self.objects.remove(&id) else { return };
        if !depleted {
            return;
        }
        if let Some(i) = self.find_point(obj.code, &obj.pos) {
            self.points[i].last_depleted_ms = now_ms;
            self.dirty = true;
        }
    }

    pub fn set_self_pos(&mut self, pos: Vec3, now_ms: i64) {
        self.self_pos = Some((pos, now_ms));
    }

    pub fn map(&self) -> Option<u32> {
        self.map
    }

    pub fn self_pos(&self) -> Option<Vec3> {
        self.self_pos.map(|(p, _)| p)
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
                self.objects.insert(o.id, o);
                n += 1;
            }
        }
        n
    }

    pub fn set_self_heading(&mut self, degrees: f32) {
        self.self_heading = Some(degrees.rem_euclid(360.0));
    }

    /// `23 36`: the local player teleported, possibly to another map. Sightings
    /// still waiting for a map can no longer be placed safely.
    pub fn self_teleported(&mut self, pos: Vec3, now_ms: i64) {
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
        RadarSnapshot {
            map: self.map,
            self_pos: self.self_pos.map(|(p, _)| p),
            self_heading: self.self_heading,
            self_pos_ms: self.self_pos.map(|(_, t)| t).unwrap_or(0),
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
        let (Some(local_id), Some(port)) = (storage.local_player_id(), port) else { return };
        if !self.restore_attempted {
            let Some(me) = storage.radar().self_pos() else { return };
            self.restore_attempted = true;
            let n = self.restore(storage, local_id, port, me, now_ms);
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

    fn restore(&self, storage: &crate::combat::data_storage::DataStorage, local_id: i64, port: u16, me: Vec3, now: i64) -> usize {
        let Ok(text) = std::fs::read_to_string(&self.path) else { return 0 };
        let Ok(saved) = serde_json::from_str::<SavedLive>(&text) else { return 0 };
        if saved.local_id != local_id || saved.port != port || now - saved.saved_at_ms > LIVE_MAX_AGE_MS {
            return 0;
        }
        let near: Vec<RadarObject> = saved
            .objects
            .into_iter()
            .filter(|o| o.pos.dist2d(&me) < LIVE_RESTORE_RADIUS_CM)
            .collect();
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

/// `2B 38` / `2A 38` body: the position at the end, if this record is the
/// local player's own (target and caster both `self_id`).
pub fn parse_self_position(body: &[u8], self_id: u32) -> Option<Vec3> {
    let (target, _) = varint(body, 0)?;
    if target != self_id || body.len() < 13 + 8 {
        return None;
    }
    let pos_at = body.len() - 12;
    if body[pos_at - 1] != 0x02 {
        return None;
    }
    // The caster sits a few bytes before the position (after the epoch-ms
    // timestamp); its exact offset varies with the record's shape.
    let needle = enc_varint(self_id);
    let caster_area = &body[pos_at.saturating_sub(10)..pos_at - 1];
    if !caster_area.windows(needle.len()).any(|w| w == needle.as_slice()) {
        return None;
    }
    Vec3::read(body, pos_at)
}

/// `02 38` body: the local player's facing, when the record is about them
/// (`<self> 00 <u32> <u16> <self> <f32>`). The same opcode carries other
/// shapes and other entities; the doubled self id is the guard.
///
/// The u16 counts up once per record from zone load. Read as a varint it
/// happens to be two bytes too once it passes 127 — which hid the bug on the
/// first capture and broke facing for the first two minutes of every zone.
pub fn parse_self_heading(body: &[u8], self_id: u32) -> Option<f32> {
    let (id, n) = varint(body, 0)?;
    if id != self_id || *body.get(n)? != 0 {
        return None;
    }
    let (id2, n2) = varint(body, n + 7)?;
    if id2 != self_id {
        return None;
    }
    let at = n + 7 + n2;
    // Exactly four bytes follow the facing. A longer variant (other u32, three
    // floats of what looks like a vector) shares the doubled id but no heading.
    if body.len() != at + 8 {
        return None;
    }
    let deg = f32::from_le_bytes(body.get(at..at + 4)?.try_into().ok()?);
    (deg.is_finite() && (-360.0..=360.0).contains(&deg)).then_some(deg)
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
        t.set_self_heading(-39.6);
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
