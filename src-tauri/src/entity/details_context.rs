use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DetailsActorSummary {
    pub actor_id: i32,
    pub nickname: String,
    #[serde(default)]
    pub job: String,
    /// Job class prefix ID (e.g. 11=Gladiator) for language-independent storage
    #[serde(default)]
    pub job_id: i32,
    #[serde(default)]
    pub party_heal: i64,
    #[serde(default)]
    pub regen: i64,
    #[serde(default)]
    pub damage_received: i64,
    #[serde(default)]
    pub hits_received: i32,
    /// From the party roster (0x9702), joined by character name; 0 for anyone
    /// not in your party (the roster is the only packet carrying these).
    #[serde(default)]
    pub level: i32,
    #[serde(default)]
    pub gear_score: i32,
    #[serde(default)]
    pub combat_power: i64,
    /// Active time per damage-multiplier level, `[scalar, ms]` ascending by
    /// scalar (hundredths of a percent). See `data_storage::scalar_time_ms`.
    #[serde(default)]
    pub power_scalar_ms: Vec<(i32, i64)>,
    /// Buffs on this actor while they were attacking, longest first.
    #[serde(default)]
    pub buffs: Vec<BuffUptime>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BuffUptime {
    /// Raw effect id (skill/item code x10 + a digit; see `combat::buffs`).
    pub effect: u32,
    /// Localized skill/item name, empty when unknown.
    pub name: String,
    /// Share of the actor's active time the buff was up, 0-100.
    pub uptime_pct: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DetailsTargetSummary {
    pub target_id: i32,
    #[serde(default)]
    pub target_name: String,
    #[serde(default)]
    pub max_hp: i32,
    pub battle_time: i64,
    pub last_damage_time: i64,
    pub total_damage: i32,
    pub actor_damage: std::collections::HashMap<i32, i32>,
    /// Per actor (same keys as `actor_damage`), their active stretches as
    /// `[first_hit_ms, last_hit_ms]` — see `ActorCombatData::active_spans`.
    /// The frontend unions them across targets for active-time DPS.
    #[serde(default)]
    pub actor_active_spans: std::collections::HashMap<i32, Vec<(i64, i64)>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DetailsContext {
    pub current_target_id: i32,
    pub targets: Vec<DetailsTargetSummary>,
    pub actors: Vec<DetailsActorSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DetailSkillEntry {
    pub actor_id: i32,
    pub code: i32,
    pub name: String,
    pub time: i32,
    pub dmg: i32,
    pub multi_hit_count: i32,
    pub multi_hit_damage: i32,
    #[serde(default)]
    pub multi_hit_hits: i32,
    #[serde(default)]
    pub min_dmg: i32,
    #[serde(default)]
    pub max_dmg: i32,
    pub crit: i32,
    pub parry: i32,
    pub back: i32,
    #[serde(default)]
    pub frontal: i32,
    pub perfect: i32,
    pub double: i32,
    pub smite: i32,
    pub powershard: i32,
    pub regen: i32,
    #[serde(default)]
    pub job: String,
    #[serde(default)]
    pub is_dot: bool,
    #[serde(default)]
    pub hit_timestamps: Vec<i64>,
    #[serde(default)]
    pub specs: Vec<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PingPoint {
    pub ts_ms: i64,
    pub ping_ms: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetDetailsResponse {
    pub target_id: i32,
    #[serde(default)]
    pub max_hp: i32,
    pub total_target_damage: i32,
    pub battle_time: i64,
    #[serde(default)]
    pub start_time: i64,
    pub skills: Vec<DetailSkillEntry>,
    #[serde(default)]
    pub ping_history: Vec<PingPoint>,
    /// Per-actor / per-skill HEALING done in this fight. Reuses DetailSkillEntry:
    /// `dmg` = heal amount, `time` = tick count, `is_dot` = HoT. Empty for old files.
    #[serde(default)]
    pub heal_skills: Vec<DetailSkillEntry>,
}
