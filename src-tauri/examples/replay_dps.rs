//! Investigation tool: replay a raw packet log through the real parser and
//! print what the meter would compute, per target, for the local player.
//!
//!   cargo run --example replay_dps -- <packets.txt> [from_ts] [to_ts]

use std::collections::HashMap;
use std::sync::Arc;

use a2tools_dps_meter_lib::capture::packet_accumulator::PacketAccumulator;
use a2tools_dps_meter_lib::capture::stream_processor::StreamProcessor;
use a2tools_dps_meter_lib::combat::data_storage::{DataStorage, TargetCombatData};
use a2tools_dps_meter_lib::combat::dps_calculator::DpsCalculator;
use a2tools_dps_meter_lib::combat::ping_tracker::PingTracker;
use a2tools_dps_meter_lib::entity::summon_resolver;
use a2tools_dps_meter_lib::i18n::lookup::{NpcLookup, SkillLookup};

fn decode_hex(hex: &str) -> Option<Vec<u8>> {
    (0..hex.len()).step_by(2).map(|i| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok()).collect()
}

fn fmt_ms(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|d| d.with_timezone(&chrono::Local).format("%H:%M:%S%.3f").to_string())
        .unwrap_or_default()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path = &args[1];
    let from = args.get(2).cloned().unwrap_or_default();
    let to = args.get(3).cloned().unwrap_or_else(|| "~".into());

    let storage = Arc::new(DataStorage::new());
    let skills = Arc::new(SkillLookup::new());
    if let Ok(t) = std::fs::read_to_string("../src/data/i18n/skills/en.json") {
        skills.load_from_json(&t);
    }
    let mut p = StreamProcessor::new(storage.clone(), skills.clone(), Arc::new(NpcLookup::new()));
    let mut streams: HashMap<String, PacketAccumulator> = HashMap::new();

    // Fights get wiped by zone changes / idle resets, so keep the last seen
    // state of every (target, fight start) as the replay goes.
    let mut fights: HashMap<(i32, i64), TargetCombatData> = HashMap::new();
    let mut n = 0usize;
    let text = std::fs::read_to_string(path).expect("read capture");
    for line in text.lines() {
        n += 1;
        if n % 50 == 0 {
            for (id, t) in storage.get_combat_snapshot() {
                fights.insert((id, t.first_damage_time), t);
            }
        }
        if line.is_empty() || line.starts_with('#') { continue; }
        let mut it = line.splitn(3, '|');
        let (Some(ts), Some(key), Some(hex)) = (it.next(), it.next(), it.next()) else { continue };
        if ts < from.as_str() || ts >= to.as_str() { continue; }
        let Some(bytes) = decode_hex(hex) else { continue };
        let ms = chrono::DateTime::parse_from_rfc3339(ts).map(|d| d.timestamp_millis()).unwrap_or(0);
        p.set_override_timestamp(Some(ms));
        let acc = streams.entry(key.to_string()).or_insert_with(PacketAccumulator::new);
        acc.append(&bytes);
        let used = p.consume_stream(acc.snapshot());
        if used > 0 { acc.discard_bytes(used); }
    }

    // What the meter itself would show at the end of the replay window.
    for mode in ["lastHitByMe", "allTargets"] {
        let mut calc = DpsCalculator::new(storage.clone(), skills.clone(), Arc::new(NpcLookup::new()), Arc::new(PingTracker::new()));
        calc.set_target_selection_mode(mode);
        calc.set_actor_filter_mode("all");
        let dps = calc.get_dps();
        println!("meter [{mode}] battle_time={:.1}s", dps.battle_time as f64 / 1000.0);
        for (id, d) in &dps.map {
            println!("  #{id} {:<12} dmg={:.0} dps={:.0}", d.nickname, d.amount, d.dps);
        }
    }

    // Title check: every target that took damage, whether its NPC code is
    // known (spawn seen) and whether that code has a name.
    let npcs = NpcLookup::new();
    if let Ok(t) = std::fs::read_to_string("../src/data/i18n/npcs/en.json") {
        npcs.load_from_json(&t);
    }
    let mobs_now = storage.get_mob_data();
    let mut all_targets: Vec<_> = fights.keys().map(|(id, _)| *id).collect::<std::collections::BTreeSet<_>>().into_iter().collect();
    all_targets.sort();
    for id in all_targets {
        let code = mobs_now.get(&id);
        let name = code.map(|&c| npcs.get_npc_name(c)).unwrap_or_default();
        let dmg: i64 = fights.iter().filter(|((t, _), _)| *t == id).map(|(_, f)| f.total_damage).sum();
        println!("title-check target {id:>6} code={:<10} name={:<30} dmg={dmg}",
            code.map(|c| c.to_string()).unwrap_or("-".into()), format!("{name:?}"));
    }

    let local = storage.local_player_id().map(|v| v as i32);
    println!("local player: {:?}  name: {:?}", local, storage.local_character_name());
    let Some(local) = local else { return };
    let summons = storage.get_summon_data();
    let mobs = storage.get_mob_data();

    for (id, t) in storage.get_combat_snapshot() {
        fights.insert((id, t.first_damage_time), t);
    }
    let mut targets: Vec<_> = fights.values().collect();
    targets.sort_by_key(|t| t.first_damage_time);
    for t in targets {
        let mut mine = 0i64;
        let (mut first, mut last) = (i64::MAX, 0i64);
        let mut per_skill: Vec<(String, bool, i32, i32)> = Vec::new();
        for (&aid, ad) in &t.actors {
            if summon_resolver::resolve(aid, &summons) != local { continue; }
            mine += ad.total_damage;
            for (&(code, dot), s) in &ad.skills {
                for &ts in &s.hit_timestamps {
                    first = first.min(ts);
                    last = last.max(ts);
                }
                let name = skills.lookup_skill_name(code);
                let tag = if aid == local { String::new() } else { format!(" [summon {aid}]") };
                per_skill.push((format!("{code} {name}{tag}"), dot, s.hit_count, s.total_damage));
            }
        }
        if mine == 0 { continue; }
        let dur = (t.last_damage_time - t.first_damage_time).max(0);
        let own = (last - first).max(0);
        println!(
            "\ntarget {} mob={:?}  target window {}..{} ({:.1}s)  total={}",
            t.target_id, mobs.get(&t.target_id), fmt_ms(t.first_damage_time), fmt_ms(t.last_damage_time),
            dur as f64 / 1000.0, t.total_damage
        );
        println!(
            "  you: dmg={}  your hits {}..{} ({:.1}s)  DPS over target window={:.0}  over your window={:.0}",
            mine, fmt_ms(first), fmt_ms(last), own as f64 / 1000.0,
            mine as f64 / (dur.max(1000) as f64 / 1000.0),
            mine as f64 / (own.max(1000) as f64 / 1000.0)
        );
        // Gaps between your consecutive hits, to see whether idle stretches
        // inflate the window the DPS is divided by.
        let mut hits: Vec<i64> = t.actors.iter()
            .filter(|(aid, _)| summon_resolver::resolve(**aid, &summons) == local)
            .flat_map(|(_, ad)| ad.skills.values().flat_map(|s| s.hit_timestamps.iter().copied()))
            .collect();
        hits.sort();
        let gaps: Vec<(i64, i64)> = hits.windows(2).filter(|w| w[1] - w[0] >= 2_000).map(|w| (w[0], w[1] - w[0])).collect();
        for (at, g) in &gaps {
            println!("  gap {:.1}s after {}", *g as f64 / 1000.0, fmt_ms(*at));
        }
        for thr in [1_000i64, 1_500, 2_000, 3_000, 5_000, 10_000] {
            let idle: i64 = hits.windows(2).map(|w| w[1] - w[0]).filter(|&g| g >= thr).sum();
            println!("  gaps>={:.1}s removed: active {:.1}s -> DPS {:.0}", thr as f64 / 1000.0,
                (own - idle) as f64 / 1000.0, mine as f64 / ((own - idle).max(1000) as f64 / 1000.0));
        }
        per_skill.sort_by_key(|s| std::cmp::Reverse(s.3));
        for (name, dot, hits, dmg) in per_skill {
            println!("    {:>9} {:>4} hits {}{}", dmg, hits, name, if dot { " (DOT)" } else { "" });
        }
    }
}
