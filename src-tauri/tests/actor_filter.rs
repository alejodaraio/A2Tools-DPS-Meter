//! The actor filter decides whose damage the meter lists: everyone hitting the
//! target, you plus your party, or only you. Without it, every stranger on a
//! world boss shows up on the meter.

use std::sync::Arc;

use a2tools_dps_meter_lib::combat::data_storage::{DataStorage, PartyMember};
use a2tools_dps_meter_lib::combat::dps_calculator::DpsCalculator;
use a2tools_dps_meter_lib::combat::ping_tracker::PingTracker;
use a2tools_dps_meter_lib::entity::damage_packet::ParsedDamagePacket;
use a2tools_dps_meter_lib::i18n::lookup::{NpcLookup, SkillLookup};

const MOB: i32 = 9000;
const ME: i32 = 101;
const FRIEND: i32 = 102;
const STRANGER: i32 = 103;
/// A class-band player skill, so each actor gets a job and survives the
/// "no job → not a player" pruning.
const PLAYER_SKILL: i32 = 11_010_000;

fn hit(storage: &DataStorage, actor: i32, damage: i32) {
    let mut p = ParsedDamagePacket::new();
    p.set_actor_id(actor);
    p.set_target_id(MOB);
    p.set_skill_code(PLAYER_SKILL);
    p.set_damage(damage);
    storage.append_damage(p);
}

fn setup(with_party: bool) -> Arc<DataStorage> {
    let storage = Arc::new(DataStorage::new());
    storage.set_permanent_nickname(ME, "Me");
    storage.set_permanent_nickname(FRIEND, "Friend");
    storage.set_permanent_nickname(STRANGER, "Stranger");
    storage.set_local_player_id(Some(ME as i64));
    if with_party {
        storage.set_party_roster(
            vec![
                ("Me".to_string(), PartyMember { slot: 1, ..Default::default() }),
                ("Friend".to_string(), PartyMember { slot: 2, ..Default::default() }),
            ],
            true,
        );
    }
    hit(&storage, ME, 1_000);
    hit(&storage, FRIEND, 2_000);
    hit(&storage, STRANGER, 7_000);
    storage
}

fn rows(storage: Arc<DataStorage>, mode: Option<&str>) -> (Vec<String>, i64, Vec<f64>) {
    let mut calc = DpsCalculator::new(
        storage,
        Arc::new(SkillLookup::new()),
        Arc::new(NpcLookup::new()),
        Arc::new(PingTracker::new()),
    );
    calc.set_target_selection_mode("allTargets");
    if let Some(mode) = mode {
        calc.set_actor_filter_mode(mode);
    }
    let dps = calc.get_dps();
    let mut names: Vec<String> = dps.map.values().map(|d| d.nickname.clone()).collect();
    names.sort();
    let pcts = dps.map.values().map(|d| d.damage_contribution).collect();
    (names, dps.target_total_damage, pcts)
}

#[test]
fn default_shows_me_and_my_party_only() {
    let (names, total, pcts) = rows(setup(true), None);
    assert_eq!(names, ["Friend", "Me"]);
    // The boss HP bar still counts the stranger's hits.
    assert_eq!(total, 10_000);
    // Contribution is over the rows shown, so the party sums to 100%.
    assert!((pcts.iter().sum::<f64>() - 100.0).abs() < 1e-6);
}

#[test]
fn self_mode_shows_only_me() {
    let (names, _, _) = rows(setup(true), Some("self"));
    assert_eq!(names, ["Me"]);
}

#[test]
fn all_mode_shows_everyone() {
    let (names, _, _) = rows(setup(true), Some("all"));
    assert_eq!(names, ["Friend", "Me", "Stranger"]);
}

#[test]
fn party_mode_without_a_roster_shows_only_me() {
    let (names, _, _) = rows(setup(false), Some("party"));
    assert_eq!(names, ["Me"]);
}

/// Seen live: the meter started mid-session, so the self record hadn't arrived
/// and every row was a bare `#id`. A configured character name that matches no
/// entity must not blank the meter — show everyone until you're recognised.
#[test]
fn unrecognised_self_shows_everyone_instead_of_nothing() {
    let storage = Arc::new(DataStorage::new());
    storage.set_local_character_name(Some("Me".to_string()));
    hit(&storage, 193, 1_000);
    hit(&storage, 37831, 2_000);
    let (names, _, _) = rows(storage, Some("self"));
    assert_eq!(names, ["193", "37831"]);
}

#[test]
fn details_context_is_filtered_too() {
    let mut calc = DpsCalculator::new(
        setup(true),
        Arc::new(SkillLookup::new()),
        Arc::new(NpcLookup::new()),
        Arc::new(PingTracker::new()),
    );
    let ctx = calc.get_details_context();
    let target = ctx.targets.iter().find(|t| t.target_id == MOB).unwrap();
    assert!(!target.actor_damage.contains_key(&STRANGER));
    assert!(target.actor_damage.contains_key(&ME) && target.actor_damage.contains_key(&FRIEND));
    assert!(ctx.actors.iter().all(|a| a.actor_id != STRANGER));

    let details = calc.get_target_details(MOB, None);
    assert!(details.skills.iter().all(|s| s.actor_id != STRANGER));

    calc.set_actor_filter_mode("all");
    let ctx = calc.get_details_context();
    let target = ctx.targets.iter().find(|t| t.target_id == MOB).unwrap();
    assert!(target.actor_damage.contains_key(&STRANGER));
}
