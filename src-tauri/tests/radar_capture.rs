//! Radar against a live capture (2026-10-12): walk up to a Sapphire, stand on
//! it, gather it three times. While standing there the radar must hold the
//! Sapphire (`34 36` code 1550020) within a couple of metres of the player's
//! own position; after the third gather (`35 36` reason 04) it must be gone.
//!
//!   A2_RADAR_CAPTURE=/path/to/packets_20261012_142010.txt \
//!   cargo test --test radar_capture -- --nocapture

use std::collections::HashMap;
use std::sync::Arc;

use a2tools_dps_meter_lib::capture::packet_accumulator::PacketAccumulator;
use a2tools_dps_meter_lib::capture::stream_processor::StreamProcessor;
use a2tools_dps_meter_lib::combat::data_storage::DataStorage;
use a2tools_dps_meter_lib::i18n::lookup::{NpcLookup, SkillLookup};

const SAPPHIRE: u32 = 1_550_020;

#[test]
fn sapphire_is_tracked_then_removed_when_depleted() {
    let Ok(path) = std::env::var("A2_RADAR_CAPTURE") else {
        eprintln!("A2_RADAR_CAPTURE unset — skipping");
        return;
    };
    let storage = Arc::new(DataStorage::new());
    let points_file = std::env::temp_dir().join("radar_capture_points.json");
    let _ = std::fs::remove_file(&points_file);
    storage.radar().load_points(&points_file);
    let mut while_gathering = None;
    replay(&path, &storage, |ts| {
        // Between arriving at the Sapphire and the third gather.
        if while_gathering.is_none() && ts >= "2026-10-12T14:22:10" {
            while_gathering = Some(storage.radar().snapshot());
        }
    });

    let snap = while_gathering.expect("capture reaches 14:22:10");
    for o in &snap.objects {
        println!("object {:>6} kind {:>2} code {} at ({:.0}, {:.0}, {:.0})", o.id, o.kind, o.code, o.pos.x, o.pos.y, o.pos.z);
    }
    let me = snap.self_pos.expect("self position");
    println!("self at ({:.0}, {:.0}, {:.0})", me.x, me.y, me.z);
    let sapphire = snap.objects.iter().find(|o| o.id == 33298).expect("Sapphire on radar");
    assert_eq!(sapphire.code, SAPPHIRE);
    let dist_m = ((sapphire.pos.x - me.x).powi(2) + (sapphire.pos.y - me.y).powi(2)).sqrt() / 100.0;
    assert!(dist_m < 2.0, "standing on the Sapphire, got {dist_m:.1} m");

    let end = storage.radar().snapshot();
    assert!(end.objects.iter().all(|o| o.id != 33298), "depleted Sapphire removed");

    // …but its spawn point is remembered, on the map from the self record,
    // along with the other Sapphire that walked out of view.
    assert_eq!(end.map, Some(1013));
    for p in &end.points {
        println!("point code {} at ({:.0}, {:.0}) seen {}x live={} depleted={}", p.point.code, p.point.pos.x, p.point.pos.y, p.point.seen_count, p.live, p.point.last_depleted_ms > 0);
    }
    let sapphires: Vec<_> = end.points.iter().filter(|p| p.point.code == SAPPHIRE).collect();
    assert_eq!(sapphires.len(), 2);
    assert!(sapphires.iter().all(|p| !p.live));
    assert_eq!(sapphires.iter().filter(|p| p.point.last_depleted_ms > 0).count(), 1);

    // A later session started mid-zone (no self record, so no map id) still
    // finds the map: live objects land on the points saved above.
    let Ok(live_path) = std::env::var("A2_RADAR_LIVE_CAPTURE") else { return };
    let (file, json) = storage.radar().take_points_to_save().expect("points to save");
    std::fs::write(&file, json).unwrap();
    let next = Arc::new(DataStorage::new());
    next.radar().load_points(&file);
    replay(&live_path, &next, |_| {});
    let snap = next.radar().snapshot();
    println!("live session: map {:?}, {} points, heading {:?}", snap.map, snap.points.len(), snap.self_heading);
    assert!(snap.self_heading.is_some(), "facing from 02 38");
    assert_eq!(snap.map, Some(1013));
    assert!(snap.points.len() > end.points.len(), "new sightings were added");
}

/// The position estimator against the server's own positions: the capture
/// is replayed as if the player had no mantra (server positions past the
/// first are only ground truth) and the error is measured every time a real
/// position arrives. `A2_RADAR_ESTIMATE_CAPTURES` is a `;`-separated list.
/// On the 2026-10-12 captures (Python prototype) the median was ~13 m.
#[test]
fn position_is_estimated_without_the_mantra() {
    let Ok(paths) = std::env::var("A2_RADAR_ESTIMATE_CAPTURES") else {
        eprintln!("A2_RADAR_ESTIMATE_CAPTURES unset — skipping");
        return;
    };
    for path in paths.split(';').filter(|p| !p.is_empty()) {
        let storage = Arc::new(DataStorage::new());
        storage.radar().estimate_only();
        let mut errs: Vec<f32> = Vec::new();
        let mut last_truth = 0;
        replay(path, &storage, |_| {
            let r = storage.radar();
            let Some((truth, t)) = r.truth() else { return };
            if t == last_truth {
                return;
            }
            last_truth = t;
            let snap = r.snapshot();
            if let (true, Some(me)) = (snap.self_pos_estimated, snap.self_pos) {
                let e = me.dist2d(&truth) / 100.0;
                if e > 200.0 && std::env::var("A2_DEBUG").is_ok() {
                    println!("  t={t} err={e:.0} me=({:.0},{:.0}) truth=({:.0},{:.0})", me.x / 100.0, me.y / 100.0, truth.x / 100.0, truth.y / 100.0);
                }
                errs.push(e);
            }
        });
        errs.sort_by(f32::total_cmp);
        assert!(!errs.is_empty(), "{path}: never estimated");
        let q = |f: f32| errs[((errs.len() - 1) as f32 * f) as usize];
        println!("{path}: n={} p50={:.1} m p75={:.1} m p90={:.1} m max={:.0} m", errs.len(), q(0.5), q(0.75), q(0.9), q(1.0));
        assert!(q(0.5) < 25.0, "{path}: median error {:.1} m", q(0.5));
    }
}

/// Other players on the radar (`45 36` in, movement records, `47 36` out):
/// whenever our position is exact and fresh, each player shown must be
/// within view range of us (~50 m; 80 m allowed for movement between
/// records) — party members excepted, they are sent from anywhere — and
/// most must have a name.
///
///   A2_RADAR_PLAYERS_CAPTURE=/path/to/packets_20261012_152328.txt
#[test]
fn players_in_view_are_near_us() {
    let Ok(path) = std::env::var("A2_RADAR_PLAYERS_CAPTURE") else {
        eprintln!("A2_RADAR_PLAYERS_CAPTURE unset — skipping");
        return;
    };
    let storage = Arc::new(DataStorage::new());
    let mut dists: Vec<f32> = Vec::new();
    let (mut named, mut total, mut peak) = (0usize, 0usize, 0usize);
    let mut servers: HashMap<u32, Option<u16>> = HashMap::new();
    let mut last = 0;
    replay(&path, &storage, |_| {
        let snap = storage.radar().snapshot();
        for pl in &snap.players {
            servers.insert(pl.id, pl.server);
        }
        let (Some(me), false) = (snap.self_pos, snap.self_pos_estimated) else { return };
        if snap.self_pos_ms == last {
            return;
        }
        last = snap.self_pos_ms;
        peak = peak.max(snap.players.len());
        let party = storage.get_party_members();
        for pl in &snap.players {
            let name = storage.get_nickname(pl.id as i32).unwrap_or_default();
            if party.contains_key(&name) {
                continue;
            }
            let d = pl.pos.dist2d(&me) / 100.0;
            if d > 80.0 && std::env::var("A2_DEBUG").is_ok() {
                println!("  far: player {} {name:?} at {d:.0} m, seen {} ms ago", pl.id, snap.self_pos_ms - pl.seen_ms);
            }
            dists.push(d);
            total += 1;
            if storage.get_nickname(pl.id as i32).is_some() {
                named += 1;
            }
        }
    });
    let mut by_server: HashMap<Option<u16>, usize> = HashMap::new();
    for s in servers.values() {
        *by_server.entry(*s).or_default() += 1;
    }
    println!("players per server id: {by_server:?}");
    println!("party at the end: {:?}", storage.get_party_members().keys().collect::<Vec<_>>());
    let known = servers.values().filter(|s| s.is_some()).count();
    assert!(known * 10 >= servers.len() * 9, "server id read for only {known} of {} players", servers.len());
    dists.sort_by(f32::total_cmp);
    assert!(!dists.is_empty(), "no players seen");
    let q = |f: f32| dists[((dists.len() - 1) as f32 * f) as usize];
    println!(
        "players: samples={} peak={} p50={:.0} m p95={:.0} m max={:.0} m named={:.0}%",
        dists.len(), peak, q(0.5), q(0.95), q(1.0), 100.0 * named as f32 / total as f32
    );
    assert!(q(0.95) < 80.0, "p95 distance {:.0} m", q(0.95));
}

fn replay(path: &str, storage: &Arc<DataStorage>, mut on_line: impl FnMut(&str)) {
    let mut p = StreamProcessor::new(storage.clone(), Arc::new(SkillLookup::new()), Arc::new(NpcLookup::new()));
    let mut streams: HashMap<String, PacketAccumulator> = HashMap::new();
    for line in std::fs::read_to_string(path).unwrap().lines() {
        let mut it = line.splitn(3, '|');
        let (Some(ts), Some(key), Some(hex)) = (it.next(), it.next(), it.next()) else { continue };
        if line.starts_with('#') {
            continue;
        }
        on_line(ts);
        p.set_override_timestamp(chrono::DateTime::parse_from_rfc3339(ts).ok().map(|d| d.timestamp_millis()));
        let Some(bytes) = (0..hex.len()).step_by(2).map(|i| u8::from_str_radix(&hex[i..i + 2], 16).ok()).collect::<Option<Vec<u8>>>() else { continue };
        let acc = streams.entry(key.to_string()).or_insert_with(PacketAccumulator::new);
        acc.append(&bytes);
        let used = p.consume_stream(acc.snapshot());
        if used > 0 {
            acc.discard_bytes(used);
        }
    }
}
