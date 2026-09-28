use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::combat::data_storage::{active_ms, DataStorage, TargetCombatData};
use crate::combat::ping_tracker::PingTracker;
use crate::entity::details_context::*;
use crate::entity::dps_data::DpsData;
use crate::entity::fight_record::FightRecord;
use crate::entity::job_class::JobClass;
use crate::entity::personal_data::PersonalData;
use crate::entity::summon_resolver;
use crate::i18n::lookup::{NpcLookup, SkillLookup};

/// Train mob NPC type codes.
const TRAIN_MOB_CODES: &[i32] = &[
    2300229, 2300919, 2310229, 2310919, 2320229, 2320919,
    2400032, 2400392, 2500075, 2500076, 2701376,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetSelectionMode {
    BossTargets,
    MostDamage,
    MostRecent,
    LastHitByMe,
    AllTargets,
    TrainTargets,
}

impl TargetSelectionMode {
    pub fn from_id(id: &str) -> Self {
        match id {
            "bossTargets" => Self::BossTargets,
            "mostDamage" => Self::MostDamage,
            "mostRecent" => Self::MostRecent,
            "lastHitByMe" => Self::LastHitByMe,
            "allTargets" => Self::AllTargets,
            "trainTargets" => Self::TrainTargets,
            _ => Self::LastHitByMe,
        }
    }

    pub fn id(&self) -> &'static str {
        match self {
            Self::BossTargets => "bossTargets",
            Self::MostDamage => "mostDamage",
            Self::MostRecent => "mostRecent",
            Self::LastHitByMe => "lastHitByMe",
            Self::AllTargets => "allTargets",
            Self::TrainTargets => "trainTargets",
        }
    }
}

/// Whose damage the meter lists. Target selection decides *which mob* is shown;
/// this decides *which players* hitting it are shown — without it, every
/// stranger on a world boss fills the meter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActorFilterMode {
    All,
    Party,
    SelfOnly,
}

impl ActorFilterMode {
    pub const SETTING_KEY: &'static str = "dpsMeter.actorFilter";

    pub fn from_id(id: &str) -> Self {
        match id {
            "all" => Self::All,
            "self" => Self::SelfOnly,
            _ => Self::Party,
        }
    }
}

/// Resolved allow-list for one calculation. Players are matched by canonical
/// entity id (you and your summons) or by character name: the party roster
/// carries an account dbid, not the session entity id, so the name is the only
/// join to the entities in the damage log.
struct ActorFilter {
    ids: HashSet<i32>,
    names: HashSet<String>,
}

impl ActorFilter {
    fn allows(&self, uid: i32, nickname: &str) -> bool {
        self.ids.contains(&uid) || self.names.contains(nickname.trim())
    }
}

pub struct DpsCalculator {
    data_storage: Arc<DataStorage>,
    skill_lookup: Arc<SkillLookup>,
    npc_lookup: Arc<NpcLookup>,
    ping_tracker: Arc<PingTracker>,
    current_target: i32,
    last_dps_snapshot: Option<DpsData>,
    last_damage_gen: i64,
    target_selection_mode: TargetSelectionMode,
    last_known_local_id: Option<i64>,
    all_targets_window_ms: i64,
    nickname_job_cache: HashMap<String, String>,
    saved_boss_targets: HashSet<i32>,
    actor_filter_mode: ActorFilterMode,
    last_filter_log: String,
}

impl DpsCalculator {
    pub fn new(
        data_storage: Arc<DataStorage>,
        skill_lookup: Arc<SkillLookup>,
        npc_lookup: Arc<NpcLookup>,
        ping_tracker: Arc<PingTracker>,
    ) -> Self {
        Self {
            data_storage,
            skill_lookup,
            npc_lookup,
            ping_tracker,
            current_target: 0,
            last_dps_snapshot: None,
            last_damage_gen: -1,
            target_selection_mode: TargetSelectionMode::BossTargets,
            last_known_local_id: None,
            all_targets_window_ms: 120_000,
            nickname_job_cache: HashMap::new(),
            saved_boss_targets: HashSet::new(),
            actor_filter_mode: ActorFilterMode::Party,
            last_filter_log: String::new(),
        }
    }

    pub fn set_target_selection_mode(&mut self, id: &str) {
        self.target_selection_mode = TargetSelectionMode::from_id(id);
    }

    pub fn set_actor_filter_mode(&mut self, id: &str) {
        let mode = ActorFilterMode::from_id(id);
        if mode != self.actor_filter_mode {
            tracing::info!("Actor filter mode: {:?} -> {:?}", self.actor_filter_mode, mode);
            self.actor_filter_mode = mode;
            // The cached snapshot was built under the old filter.
            self.last_damage_gen = -1;
        }
    }

    /// Build the allow-list for the current filter mode, or `None` to show
    /// everyone. Also `None` until we can actually recognise you in the damage
    /// log — by your entity id, or by an entity carrying your character name.
    /// A configured name alone is not enough: when the session started before
    /// the meter did, every row can still be a bare `#id`, and filtering then
    /// would blank the meter instead of just waiting for detection.
    fn actor_filter(&self, summon_data: &HashMap<i32, i32>) -> Option<ActorFilter> {
        if self.actor_filter_mode == ActorFilterMode::All {
            return None;
        }
        let mut ids = self.resolve_local_ids(summon_data).unwrap_or_default();
        let mut names = HashSet::new();
        if let Some(name) = self.data_storage.local_character_name() {
            let name = name.trim();
            if !name.is_empty() {
                names.insert(name.to_string());
            }
        }
        let recognised = !ids.is_empty()
            || self
                .data_storage
                .get_nicknames()
                .values()
                .any(|n| names.contains(n.trim()));
        if !recognised {
            return None;
        }
        if self.actor_filter_mode == ActorFilterMode::Party {
            names.extend(
                self.data_storage
                    .get_party_members()
                    .into_keys()
                    .map(|n| n.trim().to_string())
                    .filter(|n| !n.is_empty()),
            );
        }
        if ids.is_empty() && names.is_empty() {
            return None;
        }
        ids.retain(|&id| id > 0);
        Some(ActorFilter { ids, names })
    }

    /// Setting for the ALL mode's look-back window, in ms (Settings → "All
    /// Targets time range"). Applied on startup and on every change in lib.rs.
    pub const ALL_TARGETS_WINDOW_KEY: &'static str = "dpsMeter.allTargetsWindowMs";

    pub fn set_all_targets_window_ms(&mut self, ms: i64) {
        let ms = ms.clamp(10_000, 900_000);
        if ms != self.all_targets_window_ms {
            self.all_targets_window_ms = ms;
            self.last_damage_gen = -1;
        }
    }

    pub fn mark_all_targets_saved(&mut self) {
        let combat = self.data_storage.get_combat_snapshot_light();
        for &tid in combat.keys() {
            self.saved_boss_targets.insert(tid);
        }
    }

    pub fn restart_target_selection(&mut self, clear_damage: bool) {
        self.current_target = 0;
        self.last_dps_snapshot = None;
        self.saved_boss_targets.clear();
        self.last_damage_gen = -1;
        if clear_damage {
            self.data_storage.flush();
        }
        self.data_storage.set_current_target(0);
    }

    pub fn get_dps(&mut self) -> DpsData {
        // A zone change flushed combat data; drop our cached snapshot and saved-target
        // state so the meter resets this cycle instead of returning the stale snapshot.
        if self.data_storage.take_combat_reset_requested() {
            self.last_dps_snapshot = None;
            self.saved_boss_targets.clear();
            self.last_damage_gen = -1;
            self.current_target = 0;
            self.data_storage.set_current_target(0);
        }

        let current_local_id = self.data_storage.local_player_id();
        if current_local_id != self.last_known_local_id {
            // The local player can churn entity ids several times per fight, so
            // this fires repeatedly. Only invalidate the snapshot so the "you"
            // highlight follows the new id — do NOT restart target selection,
            // which would drop the current target/segment on every change.
            self.last_known_local_id = current_local_id;
            self.last_damage_gen = -1;
        }

        // If no new damage since last cycle, return cached result
        let current_gen = self.data_storage.damage_generation();
        if current_gen == self.last_damage_gen && self.last_dps_snapshot.is_some() {
            return self.last_dps_snapshot.as_ref().unwrap().clone();
        }
        self.last_damage_gen = current_gen;

        // Get pre-computed aggregates (cheap — small map, not 17K packets).
        // Light snapshot: skips per-hit timestamps (unused here, grows unbounded).
        let combat_data = self.data_storage.get_combat_snapshot_light();
        let nickname_data = self.data_storage.get_nicknames();
        let summon_data = self.data_storage.get_summon_data();

        let mut dps_data = DpsData::new();
        dps_data.local_player_id = current_local_id;

        // Decide target
        let (target_ids, target_name, tracking_id) = self.decide_target(&combat_data, &nickname_data, &summon_data);
        dps_data.target_name = target_name;
        dps_data.target_mode = self.target_selection_mode.id().to_string();
        self.current_target = tracking_id;
        dps_data.target_id = self.current_target;
        self.data_storage.set_current_target(self.current_target);

        // Boss HP bar source: spawn-time max HP of the single boss target. Only
        // meaningful for a single target (multi-target HP can't be summed sanely),
        // so leave it 0 otherwise and let the frontend hide the bar.
        let target_max_hp = if self.current_target != 0 {
            self.data_storage.get_mob_hp(self.current_target).unwrap_or(0) as i64
        } else {
            0
        };
        dps_data.target_max_hp = target_max_hp;

        // Real current HP from the live feed (-1 if none seen). Preferred over the
        // derived max-minus-damage bar when available.
        let target_current_hp = if self.current_target != 0 {
            self.data_storage
                .get_mob_current_hp(self.current_target)
                .map(|h| h as i64)
                .unwrap_or(-1)
        } else {
            -1
        };
        dps_data.target_current_hp = target_current_hp;
        dps_data.target_mob_code = if self.current_target != 0 {
            self.data_storage.get_mob_data().get(&self.current_target).copied().unwrap_or(0)
        } else {
            0
        };

        // Collect actors from selected targets
        let mut combined_actors: HashMap<i32, i64> = HashMap::new();
        let mut combined_jobs: HashMap<i32, Option<JobClass>> = HashMap::new();
        let mut actor_spans: HashMap<i32, Vec<(i64, i64)>> = HashMap::new();
        for &tid in &target_ids {
            if let Some(target_data) = combat_data.get(&tid) {
                for (&actor_id, actor_data) in &target_data.actors {
                    *combined_actors.entry(actor_id).or_insert(0) += actor_data.total_damage;
                    actor_spans.entry(actor_id).or_default().extend_from_slice(&actor_data.active_spans);
                    if actor_data.job.is_some() && combined_jobs.get(&actor_id).and_then(|j| j.as_ref()).is_none() {
                        combined_jobs.insert(actor_id, actor_data.job);
                    }
                }
            }
        }

        // Calculate battle time
        let battle_time = if self.current_target != 0 {
            combat_data.get(&self.current_target)
                .map(|td| (td.last_damage_time - td.first_damage_time).max(0))
                .unwrap_or(0)
        } else if !target_ids.is_empty() {
            // Multi-target: first hit to last hit across the selected targets.
            // The longest single target's window understated the fight when
            // mobs were killed one after another.
            let selected = target_ids.iter().filter_map(|tid| combat_data.get(tid));
            let first = selected.clone().map(|td| td.first_damage_time).min().unwrap_or(0);
            let last = selected.map(|td| td.last_damage_time).max().unwrap_or(0);
            (last - first).max(0)
        } else {
            0
        };

        if (battle_time == 0 && combined_actors.is_empty()) || combined_actors.is_empty() {
            if let Some(ref mut snapshot) = self.last_dps_snapshot {
                snapshot.target_name = dps_data.target_name.clone();
                snapshot.target_mode = dps_data.target_mode.clone();
                snapshot.target_id = dps_data.target_id;
                snapshot.target_max_hp = target_max_hp;
                snapshot.target_total_damage = 0;
                snapshot.target_current_hp = target_current_hp;
                return snapshot.clone();
            }
            self.last_dps_snapshot = Some(dps_data.clone());
            return dps_data;
        }

        // Build canonical nickname map from aggregates
        let canonical = build_nickname_canonical_map_from_aggregates(&combined_actors, &summon_data, &nickname_data, current_local_id.map(|v| v as i32));

        let mut total_damage: f64 = 0.0;
        // Active spans per row: a row is a player plus their summons, so their
        // spans are pooled and `active_ms` counts overlapping stretches once.
        let mut row_spans: HashMap<i32, Vec<(i64, i64)>> = HashMap::new();

        // Build PersonalData from aggregates (no packet iteration!)
        for (&actor_id, &damage) in &combined_actors {
            let raw_uid = summon_resolver::resolve(actor_id, &summon_data);
            if raw_uid <= 0 { continue; }
            let nickname = resolve_nickname(raw_uid, &nickname_data, &summon_data);
            let uid = *canonical.get(&nickname).unwrap_or(&raw_uid);

            total_damage += damage as f64;
            if let Some(spans) = actor_spans.get(&actor_id) {
                row_spans.entry(uid).or_default().extend_from_slice(spans);
            }

            let entry = dps_data.map.entry(uid).or_insert_with(|| {
                let cached_job = self.cached_job(&nickname);
                if let Some(job) = cached_job {
                    PersonalData::with_job(nickname.clone(), job)
                } else {
                    PersonalData::new(nickname.clone())
                }
            });

            if entry.nickname != nickname {
                entry.nickname = nickname.clone();
            }

            entry.amount += damage as f64;

            if entry.job.is_empty() {
                if let Some(job) = combined_jobs.get(&actor_id).and_then(|j| *j) {
                    entry.job = job.class_name().to_string();
                    self.cache_job(&nickname, job.class_name());
                }
            }
        }

        // Orphan summon inference: attribute an entity that is really a summon to
        // the player who owns it, for the summons the spawn packet never covered.
        //
        // Two things used to make this miss the case it exists for. It required the
        // owner to be NAMED, and it skipped anything in `known_player_ids` — but a
        // summon lands in that set automatically, because skills like Divine Aura
        // (17153450) sit in the player band and `append_damage` classifies any
        // actor using one as a player. So a Cleric's aura was filed as a player and
        // then never reconsidered, which is why it showed as its own `#id` row next
        // to an equally-unnamed Cleric.
        //
        // The discriminator is the power scalar carried in every damage record: a
        // summon inherits its owner's (see `DataStorage::actor_power_scalars`).
        // Matched against ground truth from a capture where the spawn packets DID
        // arrive — 81 known summon/owner pairs — scalar + class + "the owner has a
        // real rotation" decided 43 of them with **zero** wrong answers and never
        // merged a real player into another.
        let known_players = self.data_storage.get_known_player_ids();
        let scalars = self.data_storage.get_power_scalars();
        // Distinct skills per actor, taken from the combat aggregates — this fast
        // path builds PersonalData from totals and leaves `analyzed_data` empty,
        // so counting that instead would silently read zero for everyone.
        let mut skill_counts: HashMap<i32, HashSet<i32>> = HashMap::new();
        for &tid in &target_ids {
            if let Some(target_data) = combat_data.get(&tid) {
                for (&actor_id, actor_data) in &target_data.actors {
                    let e = skill_counts.entry(actor_id).or_default();
                    for &(code, _) in actor_data.skills.keys() {
                        e.insert(code);
                    }
                }
            }
        }
        let skill_counts: HashMap<i32, usize> =
            skill_counts.into_iter().map(|(k, v)| (k, v.len())).collect();
        let mut orphan_merges: Vec<(i32, i32)> = Vec::new();
        for (&uid, data) in &dps_data.map {
            if summon_data.contains_key(&uid) { continue; }
            if nickname_data.contains_key(&uid) { continue; }
            let job = &data.job;
            if job.is_empty() { continue; }
            let my_skills = skill_counts.get(&uid).copied().unwrap_or(0);

            // Original path, unchanged: an entity never classified as a player,
            // attributed to the one NAMED same-class player on the meter. The
            // "named" test is what makes "exactly one candidate" meaningful here —
            // without it, other unnamed orphans of the same class count as
            // candidates and the rule stops firing at all.
            if !known_players.contains(&uid) {
                let same_job: Vec<_> = dps_data.map.iter()
                    .filter(|(oid, od)| **oid != uid && od.job == *job && nickname_data.contains_key(oid))
                    .map(|(&oid, _)| oid)
                    .collect();
                if same_job.len() == 1 {
                    orphan_merges.push((uid, same_job[0]));
                    continue;
                }
            }

            // Scalar path, for a summon that skill band alone made look like a
            // player. A summon spams one or two abilities; a real player runs a
            // rotation, so requiring the candidate to show at least three times as
            // many distinct skills keeps two genuine players apart even when their
            // scalars happen to coincide.
            let Some(my_scalars) = scalars.get(&uid) else { continue };
            if my_scalars.is_empty() || my_skills == 0 {
                continue;
            }
            let owners: Vec<i32> = dps_data.map.iter()
                .filter(|(oid, od)| {
                    **oid != uid
                        && od.job == *job
                        && skill_counts.get(*oid).copied().unwrap_or(0) >= 3 * my_skills
                        && scalars.get(*oid).is_some_and(|s| !s.is_disjoint(my_scalars))
                })
                .map(|(&oid, _)| oid)
                .collect();
            if owners.len() == 1 {
                tracing::debug!(
                    "Summon {} attributed to owner {} by power scalar {:?}",
                    uid, owners[0], my_scalars
                );
                orphan_merges.push((uid, owners[0]));
            }
        }
        for (orphan, owner) in orphan_merges {
            if let Some(orphan_data) = dps_data.map.remove(&orphan) {
                if let Some(owner_data) = dps_data.map.get_mut(&owner) {
                    owner_data.merge_from(&orphan_data);
                }
                if let Some(spans) = row_spans.remove(&orphan) {
                    row_spans.entry(owner).or_default().extend(spans);
                }
            }
        }

        // Drop players outside the actor filter (you / your party). Done after the
        // orphan merge so a summon's damage has already joined its owner's row.
        // `total_damage` stays the unfiltered sum: it feeds the boss HP bar, which
        // must count everyone's hits. Contribution % uses the shown rows only.
        let filter = self.actor_filter(&summon_data);
        let filter_state = format!(
            "mode={:?} local_id={:?} char_name={:?} party={:?} active={} rows={:?}",
            self.actor_filter_mode,
            self.data_storage.local_player_id(),
            self.data_storage.local_character_name(),
            self.data_storage.get_party_members().keys().collect::<Vec<_>>(),
            filter.is_some(),
            {
                let mut r: Vec<_> = dps_data.map.iter().map(|(id, d)| format!("{}={}", id, d.nickname)).collect();
                r.sort();
                r
            },
        );
        if filter_state != self.last_filter_log {
            tracing::debug!("Actor filter: {}", filter_state);
            self.last_filter_log = filter_state;
        }
        if let Some(filter) = filter {
            dps_data.map.retain(|&uid, data| filter.allows(uid, &data.nickname));
        }
        let shown_damage: f64 = dps_data.map.values().map(|d| d.amount).sum();

        // Filter and compute DPS
        let local_ids = self.resolve_local_ids(&summon_data);
        let party_members = self.data_storage.get_party_members();
        let bt = battle_time.max(1000);
        let mut to_remove = Vec::new();
        for (&uid, data) in &mut dps_data.map {
            // Combat power joins on the character name: the roster carries an
            // account-level dbid, not the session entity id keyed here.
            data.combat_power = party_members
                .get(&data.nickname)
                .map(|m| m.combat_power)
                .unwrap_or(0);
            if data.job.is_empty() {
                if local_ids.as_ref().is_some_and(|ids| ids.contains(&uid)) {
                    data.job = "Unknown".to_string();
                } else {
                    to_remove.push(uid);
                    continue;
                }
            }
            // Each row over its own active time; the target window is only the
            // fallback for a row with no spans recorded.
            let active = row_spans
                .get(&uid)
                .map(|s| active_ms(s))
                .filter(|&ms| ms > 0)
                .unwrap_or(bt);
            data.dps = data.amount / active.max(1000) as f64 * 1000.0;
            data.damage_contribution = data.amount / shown_damage.max(1.0) * 100.0;
        }
        for uid in to_remove {
            dps_data.map.remove(&uid);
        }

        dps_data.battle_time = battle_time;
        // total_damage here is the cumulative damage to the selected target(s).
        // Paired with target_max_hp it yields remaining = max(0, max_hp - dealt).
        dps_data.target_total_damage = total_damage as i64;
        // If the boss is dead, force the bar to empty. The derived remaining can
        // leave a sliver because the meter never observes every last hit (there is
        // no live boss HP packet), so a kill should still read 0%.
        if target_max_hp > 0
            && self.current_target != 0
            && self.data_storage.is_entity_dead(self.current_target)
        {
            dps_data.target_total_damage = target_max_hp;
            dps_data.target_current_hp = 0;
        }
        self.last_dps_snapshot = Some(dps_data.clone());
        dps_data
    }

    fn decide_target(
        &mut self,
        combat_data: &HashMap<i32, TargetCombatData>,
        _nickname_data: &HashMap<i32, String>,
        summon_data: &HashMap<i32, i32>,
    ) -> (HashSet<i32>, String, i32) {
        let mob_data = self.data_storage.get_mob_data();

        match self.target_selection_mode {
            TargetSelectionMode::MostDamage => {
                let best = combat_data.iter()
                    .max_by_key(|(_, td)| td.total_damage);
                match best {
                    Some((&id, _)) => {
                        let name = self.resolve_target_name(id);
                        (HashSet::from([id]), name, id)
                    }
                    None => (HashSet::new(), String::new(), 0),
                }
            }
            TargetSelectionMode::MostRecent => {
                let best = combat_data.iter()
                    .max_by_key(|(_, td)| td.last_damage_time);
                match best {
                    Some((&id, _)) => {
                        let name = self.resolve_target_name(id);
                        (HashSet::from([id]), name, id)
                    }
                    None => (HashSet::new(), String::new(), 0),
                }
            }
            TargetSelectionMode::BossTargets => {
                let boss_targets: Vec<_> = combat_data.keys()
                    .filter(|&&tid| {
                        if let Some(&mob_code) = mob_data.get(&tid) {
                            self.npc_lookup.is_boss(mob_code)
                        } else {
                            false
                        }
                    })
                    .cloned()
                    .collect();

                if let Some(&best) = boss_targets.iter()
                    .max_by_key(|&&tid| combat_data.get(&tid).map(|td| td.last_damage_time).unwrap_or(0))
                {
                    let name = self.resolve_target_name(best);
                    (HashSet::from([best]), name, best)
                } else {
                    // Fall back to most damage
                    let best = combat_data.iter()
                        .max_by_key(|(_, td)| td.total_damage);
                    match best {
                        Some((&id, _)) => {
                            let name = self.resolve_target_name(id);
                            (HashSet::from([id]), name, id)
                        }
                        None => (HashSet::new(), String::new(), 0),
                    }
                }
            }
            TargetSelectionMode::AllTargets => {
                // Only targets hit within the configured window ("All Targets time
                // range"), anchored on the latest hit rather than the wall clock so
                // the meter keeps the last fight on screen once combat stops.
                // Without this, ALL summed every target since the last zone change
                // or idle reset — stale fights included.
                let latest = combat_data.values().map(|td| td.last_damage_time).max().unwrap_or(0);
                let since = latest - self.all_targets_window_ms;
                let all: HashSet<i32> = combat_data
                    .iter()
                    .filter(|(_, td)| td.last_damage_time >= since)
                    .map(|(&id, _)| id)
                    .collect();
                (all, "All Targets".to_string(), 0)
            }
            TargetSelectionMode::TrainTargets => {
                let trains: HashSet<i32> = combat_data.keys()
                    .filter(|&&tid| {
                        mob_data.get(&tid).is_some_and(|code| TRAIN_MOB_CODES.contains(code))
                    })
                    .cloned()
                    .collect();
                (trains, "Train".to_string(), 0)
            }
            TargetSelectionMode::LastHitByMe => {
                let local_ids = self.resolve_local_ids(summon_data);
                if let Some(ref ids) = local_ids {
                    // Find the target most recently damaged by the local player
                    let mut best_target: Option<(i32, i64)> = None;
                    for (&target_id, target_data) in combat_data {
                        for (&actor_id, actor_data) in &target_data.actors {
                            let resolved = summon_resolver::resolve(actor_id, summon_data);
                            if ids.contains(&resolved) {
                                let ts = actor_data.last_damage_time;
                                if best_target.is_none() || ts > best_target.unwrap().1 {
                                    best_target = Some((target_id, ts));
                                }
                            }
                        }
                    }
                    match best_target {
                        Some((id, _)) => {
                            let name = self.resolve_target_name(id);
                            (HashSet::from([id]), name, id)
                        }
                        None => (HashSet::new(), String::new(), 0),
                    }
                } else {
                    // Not identified — fall back to most recently damaged target
                    let best = combat_data.iter()
                        .max_by_key(|(_, td)| td.last_damage_time);
                    match best {
                        Some((&id, _)) => {
                            let name = self.resolve_target_name(id);
                            (HashSet::from([id]), name, id)
                        }
                        None => (HashSet::new(), String::new(), 0),
                    }
                }
            }
        }
    }

    fn resolve_target_name(&self, target_id: i32) -> String {
        let mob_data = self.data_storage.get_mob_data();
        if let Some(&code) = mob_data.get(&target_id) {
            let name = self.npc_lookup.get_npc_name(code);
            if !name.is_empty() {
                return name;
            }
        }
        String::new()
    }

    fn resolve_local_ids(&self, summon_data: &HashMap<i32, i32>) -> Option<HashSet<i32>> {
        let local_id = self.data_storage.local_player_id()? as i32;
        let mut ids = HashSet::new();
        ids.insert(local_id);
        for (&summon, &owner) in summon_data {
            if summon_resolver::resolve(owner, summon_data) == local_id {
                ids.insert(summon);
            }
        }
        Some(ids)
    }

    fn cached_job(&self, nickname: &str) -> Option<String> {
        let key = nickname.trim().to_lowercase();
        if key.is_empty() || key.chars().all(|c| c.is_ascii_digit()) { return None; }
        self.nickname_job_cache.get(&key)
            .filter(|j| !j.is_empty() && *j != "Unknown")
            .cloned()
    }

    fn cache_job(&mut self, nickname: &str, job: &str) {
        if job.is_empty() || job == "Unknown" { return; }
        let key = nickname.trim().to_lowercase();
        if key.is_empty() || key.chars().all(|c| c.is_ascii_digit()) { return; }
        self.nickname_job_cache.insert(key, job.to_string());
    }

    pub fn snapshot_boss_fights(&mut self) -> Vec<FightRecord> {
        self.snapshot_boss_fights_inner(false)
    }

    pub fn snapshot_boss_fights_force(&mut self) -> Vec<FightRecord> {
        self.snapshot_boss_fights_inner(true)
    }

    fn snapshot_boss_fights_inner(&mut self, force: bool) -> Vec<FightRecord> {
        let mob_data = self.data_storage.get_mob_data();
        // Light snapshot: only used for target filtering + per-actor aggregate
        // stats here; the saved record's timestamps come from get_target_details.
        let combat_data = self.data_storage.get_combat_snapshot_light();
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);

        let mut records = Vec::new();

        let boss_target_ids: Vec<i32> = combat_data.keys()
            .filter(|&&tid| {
                if self.saved_boss_targets.contains(&tid) {
                    return false;
                }
                if let Some(&code) = mob_data.get(&tid) {
                    self.npc_lookup.is_boss(code) || TRAIN_MOB_CODES.contains(&code)
                } else {
                    false
                }
            })
            .cloned()
            .collect();

        if !boss_target_ids.is_empty() {
            tracing::trace!("snapshot_boss_fights: {} candidate targets", boss_target_ids.len());
        }

        for target_id in boss_target_ids {
            let target_data = match combat_data.get(&target_id) {
                Some(td) => td,
                None => continue,
            };

            let battle_time = (target_data.last_damage_time - target_data.first_damage_time).max(0);
            if battle_time < 5_000 || target_data.total_damage <= 0 {
                continue;
            }

            let idle_time = now_ms - target_data.last_damage_time;
            let is_ended = idle_time >= 10_000;
            let is_periodic = battle_time >= 15_000;
            if !force && !is_ended && !is_periodic {
                continue;
            }

            // Generate fight record
            let details = self.get_target_details(target_id, None);
            let nickname_data = self.data_storage.get_nicknames();
            let summon_data_snap = self.data_storage.get_summon_data();

            let mut record_actors: HashMap<i32, (String, String)> = HashMap::new();
            for skill in &details.skills {
                let uid = skill.actor_id;
                record_actors.entry(uid).or_insert_with(|| {
                    let nick = resolve_nickname(uid, &nickname_data, &summon_data_snap);
                    let job = if !skill.job.is_empty() { skill.job.clone() }
                        else { JobClass::convert_from_skill(skill.code).map(|j| j.class_name().to_string()).unwrap_or_default() };
                    (nick, job)
                });
                let entry = record_actors.get_mut(&uid).unwrap();
                if entry.1.is_empty() && !skill.job.is_empty() {
                    entry.1 = skill.job.clone();
                }
            }

            let local_id = self.data_storage.local_player_id().unwrap_or(-1) as i32;
            let party = self.data_storage.get_party_members();
            let actors: Vec<DetailsActorSummary> = record_actors.iter()
                .map(|(&id, (nick, job))| {
                    let display_nick = if id == local_id {
                        nick.clone()
                    } else {
                        crate::entity::fight_record::obscure_nickname(nick)
                    };
                    let job_class = JobClass::convert_from_skill(
                        details.skills.iter()
                            .find(|s| s.actor_id == id && !s.job.is_empty())
                            .map(|s| s.code)
                            .unwrap_or(0)
                    );
                    // Aggregate per-actor stats across all targets
                    let (mut party_heal, mut regen, mut dmg_recv, mut hits_recv) = (0i64, 0i64, 0i64, 0i32);
                    for td in combat_data.values() {
                        if let Some(ad) = td.actors.get(&id) {
                            party_heal += ad.party_heal;
                            regen += ad.regen;
                            dmg_recv += ad.damage_received;
                            hits_recv += ad.hits_received;
                        }
                    }
                    DetailsActorSummary {
                        actor_id: id,
                        nickname: display_nick,
                        job: job.clone(),
                        job_id: job_class.map(|j| j.class_prefix()).unwrap_or(0),
                        party_heal,
                        regen,
                        damage_received: dmg_recv,
                        hits_received: hits_recv,
                        // Looked up by the real name: display_nick is obscured.
                        level: party.get(nick).map(|m| m.level).unwrap_or(0),
                        gear_score: party.get(nick).map(|m| m.gear_score).unwrap_or(0),
                        combat_power: party.get(nick).map(|m| m.combat_power).unwrap_or(0),
                    }
                })
                .collect();

            let mob_code = mob_data.get(&target_id).copied().unwrap_or(0);
            let boss_name = self.resolve_target_name(target_id);

            let job_ids: Vec<i32> = actors.iter()
                .filter(|a| a.job_id > 0)
                .map(|a| a.job_id)
                .collect::<HashSet<_>>()
                .into_iter()
                .collect();
            let jobs: Vec<String> = actors.iter()
                .filter(|a| !a.job.is_empty() && a.job != "Unknown")
                .map(|a| a.job.clone())
                .collect::<HashSet<_>>()
                .into_iter()
                .collect();

            let id = format!("auto_{}_{}", target_id, target_data.first_damage_time);

            let is_train = TRAIN_MOB_CODES.contains(&mob_code);
            let record = FightRecord {
                id,
                boss_name,
                target_id,
                start_time_ms: target_data.first_damage_time,
                duration_ms: battle_time,
                total_damage: target_data.total_damage as i32,
                jobs,
                job_ids,
                details,
                actors,
                is_train,
                app_version: crate::entity::fight_record::APP_VERSION.to_string(),
                mob_code,
                local_actor_id: local_id,
            };

            if is_ended {
                self.saved_boss_targets.insert(target_id);
            }
            records.push(record);
        }

        records
    }

    pub fn get_details_context(&self) -> DetailsContext {
        // Light snapshot: this builds per-target/per-actor summaries only — the
        // per-hit timeline is fetched separately via get_target_details.
        let combat_data = self.data_storage.get_combat_snapshot_light();
        let nickname_data = self.data_storage.get_nicknames();
        let summon_data = self.data_storage.get_summon_data();
        let mob_hp_data = self.data_storage.get_mob_hp_data();
        let mob_data = self.data_storage.get_mob_data();

        let actor_filter = self.actor_filter(&summon_data);

        let mut actor_meta: HashMap<i32, (String, String)> = HashMap::new();
        let mut targets = Vec::new();

        for (&target_id, target_data) in &combat_data {
            let mut actor_damage: HashMap<i32, i32> = HashMap::new();
            let mut actor_spans: HashMap<i32, Vec<(i64, i64)>> = HashMap::new();
            let canonical = build_nickname_canonical_map_from_aggregates(
                &target_data.actors.iter().map(|(&id, ad)| (id, ad.total_damage)).collect(),
                &summon_data,
                &nickname_data,
                self.data_storage.local_player_id().map(|v| v as i32),
            );

            for (&actor_id, actor_data) in &target_data.actors {
                let raw_uid = summon_resolver::resolve(actor_id, &summon_data);
                if raw_uid <= 0 { continue; }
                let nickname = resolve_nickname(raw_uid, &nickname_data, &summon_data);
                let uid = *canonical.get(&nickname).unwrap_or(&raw_uid);
                *actor_damage.entry(uid).or_insert(0) += actor_data.total_damage as i32;
                actor_spans.entry(uid).or_default().extend_from_slice(&actor_data.active_spans);

                actor_meta.entry(uid).or_insert_with(|| {
                    (resolve_nickname(uid, &nickname_data, &summon_data), String::new())
                });

                if actor_meta.get(&uid).unwrap().1.is_empty() {
                    if let Some(job) = actor_data.job {
                        actor_meta.get_mut(&uid).unwrap().1 = job.class_name().to_string();
                    }
                }
            }

            // Orphan summon inference: merge true orphans (not known players) into
            // the same-class player when there is exactly one — an unambiguous
            // owner even if several such orphans share the class.
            let target_actor_ids: HashSet<i32> = actor_damage.keys().copied().collect();
            let known_players = self.data_storage.get_known_player_ids();
            let mut orphan_merges: Vec<(i32, i32)> = Vec::new();
            for (&uid, (_, job)) in &actor_meta {
                if !target_actor_ids.contains(&uid) { continue; }
                if summon_data.contains_key(&uid) { continue; }
                if nickname_data.contains_key(&uid) { continue; }
                if known_players.contains(&uid) { continue; }
                if job.is_empty() { continue; }
                let same_job: Vec<i32> = actor_meta.iter()
                    .filter(|(oid, (_, oj))| **oid != uid && *oj == *job
                        && nickname_data.contains_key(oid)
                        && target_actor_ids.contains(oid))
                    .map(|(oid, _)| *oid)
                    .collect();
                if same_job.len() == 1 {
                    orphan_merges.push((uid, same_job[0]));
                }
            }
            for (orphan, owner) in &orphan_merges {
                if let Some(dmg) = actor_damage.remove(orphan) {
                    *actor_damage.entry(*owner).or_insert(0) += dmg;
                }
                if let Some(spans) = actor_spans.remove(orphan) {
                    actor_spans.entry(*owner).or_default().extend(spans);
                }
                actor_meta.remove(orphan);
            }
            // Remove actors with no job and no nickname
            let remove_ids: Vec<i32> = actor_damage.keys()
                .filter(|id| {
                    actor_meta.get(id).is_some_and(|(_, job)| job.is_empty() && !nickname_data.contains_key(id))
                })
                .copied().collect();
            for id in remove_ids {
                actor_damage.remove(&id);
                actor_meta.remove(&id);
            }
            if let Some(ref filter) = actor_filter {
                actor_damage.retain(|id, _| {
                    let nick = actor_meta.get(id).map(|(n, _)| n.as_str()).unwrap_or("");
                    filter.allows(*id, nick)
                });
            }
            // Same rows as actor_damage after the merges, removals and filter.
            actor_spans.retain(|id, _| actor_damage.contains_key(id));

            let target_name = if let Some(&code) = mob_data.get(&target_id) {
                self.npc_lookup.get_npc_name(code)
            } else {
                String::new()
            };

            targets.push(DetailsTargetSummary {
                target_id,
                target_name,
                max_hp: mob_hp_data.get(&target_id).copied().unwrap_or(0),
                battle_time: (target_data.last_damage_time - target_data.first_damage_time).max(0),
                last_damage_time: target_data.last_damage_time,
                total_damage: target_data.total_damage as i32,
                actor_damage,
                actor_active_spans: actor_spans,
            });
        }

        let party = self.data_storage.get_party_members();
        let actors: Vec<DetailsActorSummary> = actor_meta.iter()
            .filter(|(id, (nick, _))| actor_filter.as_ref().is_none_or(|f| f.allows(**id, nick)))
            .map(|(&id, (nick, job))| {
                let job_id = if let Some(jc) = JobClass::convert_from_skill(
                    // Find a skill code from this actor's aggregate data
                    combat_data.values()
                        .flat_map(|td| td.actors.get(&id))
                        .flat_map(|ad| ad.skills.keys())
                        .find(|&&(sc, _)| JobClass::convert_from_skill(sc).is_some())
                        .map(|&(sc, _)| sc)
                        .unwrap_or(0)
                ) { jc.class_prefix() } else { 0 };
                // Aggregate per-actor stats
                let (mut party_heal, mut regen, mut dmg_recv, mut hits_recv) = (0i64, 0i64, 0i64, 0i32);
                for td in combat_data.values() {
                    if let Some(ad) = td.actors.get(&id) {
                        party_heal += ad.party_heal;
                        regen += ad.regen;
                        dmg_recv += ad.damage_received;
                        hits_recv += ad.hits_received;
                    }
                }
                DetailsActorSummary {
                    actor_id: id,
                    nickname: nick.clone(),
                    job: job.clone(),
                    job_id,
                    party_heal,
                    regen,
                    damage_received: dmg_recv,
                    hits_received: hits_recv,
                    level: party.get(nick).map(|m| m.level).unwrap_or(0),
                    gear_score: party.get(nick).map(|m| m.gear_score).unwrap_or(0),
                    combat_power: party.get(nick).map(|m| m.combat_power).unwrap_or(0),
                }
            })
            .collect();

        DetailsContext {
            current_target_id: self.current_target,
            targets,
            actors,
        }
    }

    pub fn get_target_details(&self, target_id: i32, actor_ids: Option<&[i32]>) -> TargetDetailsResponse {
        let combat_data = self.data_storage.get_combat_snapshot();
        let target_data = match combat_data.get(&target_id) {
            Some(td) => td,
            None => return TargetDetailsResponse {
                target_id,
                max_hp: 0,
                total_target_damage: 0,
                battle_time: 0,
                start_time: 0,
                skills: Vec::new(),
                ping_history: Vec::new(),
                heal_skills: Vec::new(),
            },
        };

        let summon_data = self.data_storage.get_summon_data();
        let nickname_data = self.data_storage.get_nicknames();
        let mob_hp_data = self.data_storage.get_mob_hp_data();

        let actor_damage_map: HashMap<i32, i64> = target_data.actors.iter()
            .map(|(&id, ad)| (id, ad.total_damage))
            .collect();
        let canonical = build_nickname_canonical_map_from_aggregates(&actor_damage_map, &summon_data, &nickname_data, self.data_storage.local_player_id().map(|v| v as i32));

        // Build orphan summon map
        let mut orphan_to_owner: HashMap<i32, i32> = HashMap::new();
        {
            let known_players = self.data_storage.get_known_player_ids();
            let mut actor_jobs: HashMap<i32, String> = HashMap::new();
            for (&actor_id, actor_data) in &target_data.actors {
                let raw_uid = summon_resolver::resolve(actor_id, &summon_data);
                if raw_uid <= 0 { continue; }
                let uid = *canonical.get(&resolve_nickname(raw_uid, &nickname_data, &summon_data)).unwrap_or(&raw_uid);
                if actor_jobs.contains_key(&uid) { continue; }
                if let Some(job) = actor_data.job {
                    actor_jobs.insert(uid, job.class_name().to_string());
                }
            }
            let mut seen = HashSet::new();
            for (&actor_id, actor_data) in &target_data.actors {
                let raw_uid = summon_resolver::resolve(actor_id, &summon_data);
                if raw_uid <= 0 { continue; }
                if summon_data.contains_key(&raw_uid) || nickname_data.contains_key(&raw_uid) { continue; }
                // Never merge known players — they have their own identity
                if known_players.contains(&raw_uid) { continue; }
                if !seen.insert(raw_uid) { continue; }
                // Use loose detection from any skill this actor used
                let job = actor_data.skills.keys()
                    .find_map(|&(sc, _)| JobClass::convert_from_skill_loose(sc))
                    .map(|j| j.class_name().to_string());
                let job = match job {
                    Some(j) => j,
                    None => continue,
                };
                let matching: Vec<i32> = actor_jobs.iter()
                    .filter(|(id, j)| **id != raw_uid && **j == job && nickname_data.contains_key(id))
                    .map(|(id, _)| *id)
                    .collect();
                if matching.len() == 1 {
                    orphan_to_owner.insert(raw_uid, matching[0]);
                }
            }
        }

        // Build expanded actor ID set for filtering
        let filter_uids: Option<HashSet<i32>> = actor_ids.map(|ids| {
            let canonical_ids: HashSet<i32> = ids.iter()
                .map(|&id| {
                    let nick = resolve_nickname(id, &nickname_data, &summon_data);
                    *canonical.get(&nick).unwrap_or(&id)
                })
                .collect();
            let mut expanded = HashSet::from_iter(ids.iter().copied());
            for (&actor_id, _) in &target_data.actors {
                let raw_uid = summon_resolver::resolve(actor_id, &summon_data);
                if raw_uid <= 0 { continue; }
                let remapped = *orphan_to_owner.get(&raw_uid).unwrap_or(&raw_uid);
                let nick = resolve_nickname(remapped, &nickname_data, &summon_data);
                let uid = *canonical.get(&nick).unwrap_or(&remapped);
                if canonical_ids.contains(&uid) {
                    expanded.insert(raw_uid);
                }
            }
            for (&orphan, &owner) in &orphan_to_owner {
                let nick = resolve_nickname(owner, &nickname_data, &summon_data);
                let uid = *canonical.get(&nick).unwrap_or(&owner);
                if canonical_ids.contains(&uid) {
                    expanded.insert(orphan);
                }
            }
            expanded
        });

        let actor_filter = self.actor_filter(&summon_data);

        // Build skill entries from aggregates (no packet iteration!)
        let mut skill_map: HashMap<(i32, i32), DetailSkillEntry> = HashMap::new();
        let fight_start = target_data.first_damage_time;

        for (&actor_id, actor_data) in &target_data.actors {
            let raw_uid = summon_resolver::resolve(actor_id, &summon_data);
            if raw_uid <= 0 { continue; }

            if let Some(ref filter) = filter_uids {
                if !filter.contains(&raw_uid) { continue; }
            }

            let remapped = *orphan_to_owner.get(&raw_uid).unwrap_or(&raw_uid);
            let nickname = resolve_nickname(remapped, &nickname_data, &summon_data);
            let uid = *canonical.get(&nickname).unwrap_or(&remapped);
            if actor_filter.as_ref().is_some_and(|f| !f.allows(uid, &nickname)) {
                continue;
            }

            for (&(raw_skill, is_dot), skill_data) in &actor_data.skills {
                // Normalize skill code
                let skill_code = {
                    let base = raw_skill - (raw_skill % 10000);
                    let base_name = self.skill_lookup.get_skill_name(base);
                    if !base_name.is_empty() {
                        let raw_name = self.skill_lookup.get_skill_name(raw_skill);
                        if raw_name.is_empty() || raw_name == base_name { base } else { raw_skill }
                    } else { raw_skill }
                };

                let dot_offset = if is_dot { 1_000_000_000 } else { 0 };
                let key = (uid, skill_code + dot_offset);
                let mut skill_name = self.skill_lookup.lookup_skill_name(skill_code);
                if is_dot && !skill_name.is_empty() {
                    skill_name = format!("{} - DOT", skill_name);
                }
                let job = JobClass::convert_from_skill(skill_code)
                    .map(|j| j.class_name().to_string())
                    .unwrap_or_default();

                let entry = skill_map.entry(key).or_insert_with(|| DetailSkillEntry {
                    actor_id: uid,
                    code: skill_code,
                    name: skill_name,
                    time: 0,
                    dmg: 0,
                    multi_hit_count: 0,
                    multi_hit_damage: 0,
                    multi_hit_hits: 0,
                    min_dmg: i32::MAX,
                    max_dmg: 0,
                    crit: 0,
                    parry: 0,
                    back: 0,
                    frontal: 0,
                    perfect: 0,
                    double: 0,
                    smite: 0,
                    powershard: 0,
                    regen: 0,
                    job,
                    is_dot,
                    hit_timestamps: Vec::new(),
                    specs: skill_data.spec_flags.to_vec(),
                });

                entry.time += skill_data.hit_count;
                // saturating: damage sums are i32 and can exceed i32::MAX across
                // a long fight / many actors — avoid overflow panic (debug) and
                // wrap-to-negative (release).
                entry.dmg = entry.dmg.saturating_add(skill_data.total_damage);
                entry.multi_hit_count += skill_data.multi_hit_count;
                entry.multi_hit_damage = entry.multi_hit_damage.saturating_add(skill_data.multi_hit_damage);
                entry.multi_hit_hits += skill_data.multi_hit_hits;
                if skill_data.min_damage < entry.min_dmg { entry.min_dmg = skill_data.min_damage; }
                if skill_data.max_damage > entry.max_dmg { entry.max_dmg = skill_data.max_damage; }
                entry.crit += skill_data.crit_count;
                entry.back += skill_data.back_count;
                entry.frontal += skill_data.frontal_count;
                entry.parry += skill_data.parry_count;
                entry.perfect += skill_data.perfect_count;
                entry.double += skill_data.double_count;
                entry.smite += skill_data.smite_count;
                entry.powershard += skill_data.powershard_count;
                entry.regen = entry.regen.saturating_add(skill_data.heal_amount);
                // Add timestamps relative to fight start
                for &ts in &skill_data.hit_timestamps {
                    entry.hit_timestamps.push(ts - fight_start);
                }
                // Merge spec flags
                for (i, &flag) in skill_data.spec_flags.iter().enumerate() {
                    if flag && i < entry.specs.len() { entry.specs[i] = true; }
                }
            }
        }

        // Fix min_dmg sentinel
        for entry in skill_map.values_mut() {
            if entry.min_dmg == i32::MAX { entry.min_dmg = 0; }
        }

        // Healing done this segment, per healer/skill. Keyed by the canonical actor
        // (same nickname/orphan resolution as damage). Reuses DetailSkillEntry:
        // dmg = heal amount, time = tick count, is_dot = HoT.
        let mut heal_map: HashMap<(i32, i32), DetailSkillEntry> = HashMap::new();
        for (&actor_id, skills) in &self.data_storage.get_heal_snapshot() {
            let raw_uid = summon_resolver::resolve(actor_id, &summon_data);
            if raw_uid <= 0 { continue; }
            let remapped = *orphan_to_owner.get(&raw_uid).unwrap_or(&raw_uid);
            let nickname = resolve_nickname(remapped, &nickname_data, &summon_data);
            let uid = *canonical.get(&nickname).unwrap_or(&remapped);
            if let Some(ref filter) = filter_uids {
                if !filter.contains(&uid) { continue; }
            }
            if actor_filter.as_ref().is_some_and(|f| !f.allows(uid, &nickname)) {
                continue;
            }
            for (&(skill_code, is_hot), hd) in skills {
                let mut skill_name = self.skill_lookup.lookup_skill_name(skill_code);
                if is_hot && !skill_name.is_empty() {
                    skill_name = format!("{} - HoT", skill_name);
                }
                let job = JobClass::convert_from_skill(skill_code)
                    .map(|j| j.class_name().to_string())
                    .unwrap_or_default();
                let key = (uid, skill_code + if is_hot { 1_000_000_000 } else { 0 });
                let entry = heal_map.entry(key).or_insert_with(|| DetailSkillEntry {
                    actor_id: uid,
                    code: skill_code,
                    name: skill_name,
                    time: 0,
                    dmg: 0,
                    multi_hit_count: 0,
                    multi_hit_damage: 0,
                    multi_hit_hits: 0,
                    min_dmg: 0,
                    max_dmg: 0,
                    crit: 0,
                    parry: 0,
                    back: 0,
                    frontal: 0,
                    perfect: 0,
                    double: 0,
                    smite: 0,
                    powershard: 0,
                    regen: 0,
                    job,
                    is_dot: is_hot,
                    hit_timestamps: Vec::new(),
                    specs: Vec::new(),
                });
                entry.dmg = entry.dmg.saturating_add(hd.total_heal.min(i32::MAX as i64) as i32);
                entry.time += hd.tick_count;
            }
        }

        let battle_time = (target_data.last_damage_time - target_data.first_damage_time).max(0);

        let ping_history = self.ping_tracker.get_ping_history(
            target_data.first_damage_time, target_data.last_damage_time
        ).into_iter()
            .map(|(ts, ping)| PingPoint { ts_ms: ts - fight_start, ping_ms: ping })
            .collect();

        TargetDetailsResponse {
            target_id,
            max_hp: mob_hp_data.get(&target_id).copied().unwrap_or(0),
            total_target_damage: target_data.total_damage as i32,
            battle_time,
            start_time: target_data.first_damage_time,
            skills: skill_map.into_values().collect(),
            ping_history,
            heal_skills: heal_map.into_values().collect(),
        }
    }
}

fn resolve_nickname(uid: i32, nicknames: &HashMap<i32, String>, summon_data: &HashMap<i32, i32>) -> String {
    if let Some(name) = nicknames.get(&uid) {
        return name.clone();
    }
    let resolved = summon_resolver::resolve(uid, summon_data);
    if let Some(name) = nicknames.get(&resolved) {
        return name.clone();
    }
    uid.to_string()
}

fn build_nickname_canonical_map_from_aggregates(
    actor_damage: &HashMap<i32, i64>,
    summon_data: &HashMap<i32, i32>,
    nickname_data: &HashMap<i32, String>,
    local_player_id: Option<i32>,
) -> HashMap<String, i32> {
    let mut nickname_damage: HashMap<String, HashMap<i32, i64>> = HashMap::new();

    for (&actor_id, &damage) in actor_damage {
        let uid = summon_resolver::resolve(actor_id, summon_data);
        if uid <= 0 { continue; }
        let nickname = resolve_nickname(uid, nickname_data, summon_data);
        *nickname_damage.entry(nickname).or_default().entry(uid).or_insert(0) += damage;
    }

    let mut result = HashMap::new();
    for (nickname, id_damage) in &nickname_damage {
        // Pin the local player's row to their bound id so it doesn't oscillate
        // between co-existing self-ids as damage accumulates (which made the
        // frontend re-bind and thrash the meter).
        if let Some(lid) = local_player_id {
            if id_damage.contains_key(&lid) {
                result.insert(nickname.clone(), lid);
                continue;
            }
        }
        let direct_owner = id_damage.keys().find(|&&id| nickname_data.get(&id).is_some_and(|n| n == nickname));
        let canonical = direct_owner.copied()
            .or_else(|| id_damage.iter().max_by_key(|(_, d)| *d).map(|(id, _)| *id));
        if let Some(id) = canonical {
            result.insert(nickname.clone(), id);
        }
    }
    result
}
