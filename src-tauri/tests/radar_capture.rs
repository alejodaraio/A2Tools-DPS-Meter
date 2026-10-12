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
        let Some(bytes) = (0..hex.len()).step_by(2).map(|i| u8::from_str_radix(&hex[i..i + 2], 16).ok()).collect::<Option<Vec<u8>>>() else { continue };
        let acc = streams.entry(key.to_string()).or_insert_with(PacketAccumulator::new);
        acc.append(&bytes);
        let used = p.consume_stream(acc.snapshot());
        if used > 0 {
            acc.discard_bytes(used);
        }
    }
}
