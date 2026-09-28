use serde::{Deserialize, Serialize};

use super::details_context::{DetailsActorSummary, TargetDetailsResponse};

pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FightRecord {
    pub id: String,
    /// Display name (for backward compat). New files also have mob_code for i18n resolution.
    pub boss_name: String,
    pub target_id: i32,
    pub start_time_ms: i64,
    pub duration_ms: i64,
    pub total_damage: i32,
    /// Job class prefix IDs (e.g. [11, 14, 17]) for language-independent storage.
    pub jobs: Vec<String>,
    /// Job class prefix IDs for i18n resolution (new field).
    #[serde(default)]
    pub job_ids: Vec<i32>,
    pub details: TargetDetailsResponse,
    pub actors: Vec<DetailsActorSummary>,
    #[serde(default)]
    pub is_train: bool,
    #[serde(default)]
    pub app_version: String,
    /// NPC mob type code for i18n boss name resolution (new field).
    #[serde(default)]
    pub mob_code: i32,
    /// Entity id of the local player in this fight (0 in fights saved before
    /// this field; see `FightRecord::local_stats` for how those are handled).
    #[serde(default)]
    pub local_actor_id: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FightSummary {
    pub id: String,
    pub boss_name: String,
    pub target_id: i32,
    pub start_time_ms: i64,
    pub duration_ms: i64,
    pub total_damage: i32,
    pub jobs: Vec<String>,
    #[serde(default)]
    pub job_ids: Vec<i32>,
    #[serde(default)]
    pub is_train: bool,
    #[serde(default)]
    pub is_live: bool,
    #[serde(default)]
    pub app_version: String,
    #[serde(default)]
    pub mob_code: i32,
    /// The local player in this fight: name, damage and active-time DPS. Empty
    /// / 0 when they could not be identified. Drives personal bests per NPC.
    #[serde(default)]
    pub local_name: String,
    #[serde(default)]
    pub local_damage: i64,
    /// Active time behind `local_dps`. A couple of hits inside one second read
    /// as their whole damage per second, so personal bests require a minimum.
    #[serde(default)]
    pub local_active_ms: i64,
    #[serde(default)]
    pub local_dps: f64,
}

impl FightRecord {
    /// The local player's (name, damage, active ms, active-time DPS) in this fight.
    ///
    /// Newer records name the local actor outright. Older ones don't, but the
    /// local player is the one name `obscure_nickname` leaves intact, so the
    /// only actor without a `*` in its name is taken — and nothing when that is
    /// ambiguous. Active time follows the same rule as the live meter
    /// (`data_storage::ACTIVE_GAP_MS`), from the per-hit timestamps.
    pub fn local_stats(&self) -> Option<(String, i64, i64, f64)> {
        let id = if self.local_actor_id > 0 {
            self.local_actor_id
        } else {
            let clear: Vec<_> = self
                .actors
                .iter()
                .filter(|a| a.nickname.chars().count() > 1 && !a.nickname.contains('*'))
                .collect();
            if clear.len() != 1 {
                return None;
            }
            clear[0].actor_id
        };
        let name = self.actors.iter().find(|a| a.actor_id == id)?.nickname.clone();

        let mut damage = 0i64;
        let mut hits: Vec<i64> = Vec::new();
        for skill in self.details.skills.iter().filter(|s| s.actor_id == id) {
            damage += skill.dmg as i64;
            hits.extend_from_slice(&skill.hit_timestamps);
        }
        if damage <= 0 {
            return None;
        }
        hits.sort_unstable();
        let mut spans = Vec::new();
        for ts in hits {
            crate::combat::data_storage::note_active_hit(&mut spans, ts);
        }
        let active = crate::combat::data_storage::active_ms(&spans);
        Some((name, damage, active, damage as f64 / (active.max(1000) as f64 / 1000.0)))
    }
}

/// Obscure a nickname for privacy: keep first char and last char, mask the middle.
/// For CJK names (2-3 chars), keep first char, mask rest.
/// The local player's name is NOT obscured.
pub fn obscure_nickname(name: &str) -> String {
    let chars: Vec<char> = name.chars().collect();
    if chars.len() <= 1 {
        return name.to_string();
    }
    if chars.len() == 2 {
        return format!("{}*", chars[0]);
    }
    if chars.len() == 3 {
        return format!("{}*{}", chars[0], chars[2]);
    }
    // For longer names: first 2 chars + asterisks + last char
    let mask_len = (chars.len() - 3).min(4);
    let mask: String = std::iter::repeat_n('*', mask_len).collect();
    format!("{}{}{}{}", chars[0], chars[1], mask, chars[chars.len() - 1])
}
