//! DPS is divided by active time — pauses between hits don't count — which is
//! how the game's own combat analysis measures it. And the ALL mode only looks
//! at targets hit within its configured window.

use std::sync::Arc;

use a2tools_dps_meter_lib::combat::data_storage::{active_ms, note_active_hit, DataStorage};
use a2tools_dps_meter_lib::combat::dps_calculator::DpsCalculator;
use a2tools_dps_meter_lib::combat::ping_tracker::PingTracker;
use a2tools_dps_meter_lib::entity::damage_packet::ParsedDamagePacket;
use a2tools_dps_meter_lib::i18n::lookup::{NpcLookup, SkillLookup};

const ME: i32 = 101;
const PLAYER_SKILL: i32 = 11_010_000;
const T0: i64 = 1_800_000_000_000;

fn hit(storage: &DataStorage, target: i32, at_ms: i64, damage: i32) {
    let mut p = ParsedDamagePacket::new();
    p.set_actor_id(ME);
    p.set_target_id(target);
    p.set_skill_code(PLAYER_SKILL);
    p.set_damage(damage);
    p.set_timestamp(T0 + at_ms);
    storage.append_damage(p);
}

fn calc(storage: Arc<DataStorage>, mode: &str) -> DpsCalculator {
    let mut c = DpsCalculator::new(
        storage,
        Arc::new(SkillLookup::new()),
        Arc::new(NpcLookup::new()),
        Arc::new(PingTracker::new()),
    );
    c.set_target_selection_mode(mode);
    c.set_actor_filter_mode("all");
    c
}

#[test]
fn spans_split_on_pauses_and_union_overlaps() {
    let mut s = Vec::new();
    for t in [0, 900, 1_800, 5_000, 5_500] {
        note_active_hit(&mut s, t);
    }
    assert_eq!(s, vec![(0, 1_800), (5_000, 5_500)]);
    assert_eq!(active_ms(&s), 2_300);
    // A summon attacking alongside its owner doesn't add time twice.
    let summon = vec![(1_000, 2_500)];
    assert_eq!(active_ms(s.iter().chain(summon.iter())), 2_500 + 500);
}

#[test]
fn a_pause_mid_fight_does_not_dilute_dps() {
    let storage = Arc::new(DataStorage::new());
    storage.set_local_player_id(Some(ME as i64));
    // 10 s of hits, a 14 s pause, 10 more seconds: 22 hits of 1000.
    for s in 0..=10 {
        hit(&storage, 9000, s * 1_000, 1_000);
    }
    for s in 24..=34 {
        hit(&storage, 9000, s * 1_000, 1_000);
    }
    let dps = calc(storage, "lastHitByMe").get_dps();
    let me = &dps.map[&ME];
    assert_eq!(me.amount, 22_000.0);
    // 20 s active, not the 34 s first-to-last window.
    assert!((me.dps - 1_100.0).abs() < 1.0, "dps {}", me.dps);
    assert_eq!(dps.battle_time, 34_000);
}

#[test]
fn all_targets_only_counts_the_configured_window() {
    let storage = Arc::new(DataStorage::new());
    storage.set_local_player_id(Some(ME as i64));
    for s in 0..=5 {
        hit(&storage, 9001, s * 1_000, 5_000); // old fight
    }
    for s in 200..=205 {
        hit(&storage, 9002, s * 1_000, 1_000); // current fight
    }
    let mut c = calc(storage, "allTargets");
    c.set_all_targets_window_ms(120_000);
    let dps = c.get_dps();
    assert_eq!(dps.map[&ME].amount, 6_000.0, "the old fight is outside the window");

    c.set_all_targets_window_ms(900_000);
    let dps = c.get_dps();
    assert_eq!(dps.map[&ME].amount, 36_000.0);
    assert_eq!(dps.battle_time, 205_000, "first to last hit across targets");
}
