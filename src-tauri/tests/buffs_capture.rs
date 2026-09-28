//! Buff uptimes against a live Chanter capture (2026-09-27, the 82 s
//! Training Scarecrow fight). Two independent signals must agree: Power of the
//! Storm's uptime equals the time the damage multiplier sat at +20%.
//!
//!   A2_BUFF_CAPTURE=/path/to/packets_20260927_212316.txt \
//!   cargo test --test buffs_capture -- --nocapture

use std::collections::HashMap;
use std::sync::Arc;

use a2tools_dps_meter_lib::capture::packet_accumulator::PacketAccumulator;
use a2tools_dps_meter_lib::capture::stream_processor::StreamProcessor;
use a2tools_dps_meter_lib::combat::data_storage::DataStorage;
use a2tools_dps_meter_lib::combat::dps_calculator::DpsCalculator;
use a2tools_dps_meter_lib::combat::ping_tracker::PingTracker;
use a2tools_dps_meter_lib::i18n::lookup::{NpcLookup, SkillLookup};

#[test]
fn power_of_the_storm_matches_the_multiplier_step() {
    let Ok(path) = std::env::var("A2_BUFF_CAPTURE") else {
        eprintln!("A2_BUFF_CAPTURE unset — skipping");
        return;
    };
    let storage = Arc::new(DataStorage::new());
    let skills = Arc::new(SkillLookup::new());
    skills.load_from_json(&std::fs::read_to_string("../src/data/i18n/skills/en.json").unwrap());
    let mut p = StreamProcessor::new(storage.clone(), skills.clone(), Arc::new(NpcLookup::new()));
    let mut streams: HashMap<String, PacketAccumulator> = HashMap::new();
    for line in std::fs::read_to_string(&path).unwrap().lines() {
        let mut it = line.splitn(3, '|');
        let (Some(ts), Some(key), Some(hex)) = (it.next(), it.next(), it.next()) else { continue };
        if line.starts_with('#') || ts < "2026-09-27T21:24:30" || ts >= "2026-09-27T21:26:02" {
            continue;
        }
        let Some(bytes) = (0..hex.len()).step_by(2).map(|i| u8::from_str_radix(&hex[i..i + 2], 16).ok()).collect::<Option<Vec<u8>>>() else { continue };
        p.set_override_timestamp(chrono::DateTime::parse_from_rfc3339(ts).ok().map(|d| d.timestamp_millis()));
        let acc = streams.entry(key.to_string()).or_insert_with(PacketAccumulator::new);
        acc.append(&bytes);
        let used = p.consume_stream(acc.snapshot());
        if used > 0 {
            acc.discard_bytes(used);
        }
    }

    let calc = DpsCalculator::new(storage.clone(), skills, Arc::new(NpcLookup::new()), Arc::new(PingTracker::new()));
    let me = storage.local_player_id().expect("local player") as i32;
    let ctx = calc.get_details_context();
    let actor = ctx.actors.iter().find(|a| a.actor_id == me).expect("local actor");
    for b in &actor.buffs {
        println!("{:>5.1}% {}", b.uptime_pct, b.name);
    }
    let storm = actor.buffs.iter().find(|b| b.name == "Power of the Storm").expect("Power of the Storm");
    // Multiplier at 145.20% for 9.6 s of 61.6 s active = 15.6%.
    assert!((storm.uptime_pct - 15.6).abs() < 1.0, "Power of the Storm {:.1}%", storm.uptime_pct);
    let circle = actor.buffs.iter().find(|b| b.name == "Protection Circle").expect("Protection Circle");
    assert!(circle.uptime_pct > 95.0, "refreshed every ~0.5 s, so up nearly all the time");
}
