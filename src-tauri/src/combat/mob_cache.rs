//! Remember which NPC each mob entity is across meter restarts.
//!
//! A mob's NPC code (and so its name, boss flag and max HP) arrives only in its
//! spawn record, sent once when it enters view. A meter (re)started while mobs
//! are already on screen never sees those records, so the title reads
//! `Mob #<id>` until the mob leaves view and comes back. Measured on live
//! captures (2026-09-27): across a 4-minute fight with no spawn in the capture,
//! the NPC code appeared in no other packet, in any encoding — there is nothing
//! to recover it from in-band.
//!
//! Entity ids are session-scoped, so the saved table is only valid for the same
//! session. The fingerprint is the local player's entity id plus the game
//! connection's port: both are reissued on a new session. A restore also needs
//! the file to be recent, and never overwrites a mob the live stream already
//! identified.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::combat::data_storage::DataStorage;
use crate::i18n::lookup::NpcLookup;

/// Older than this, the saved table is not trusted even with a matching
/// fingerprint.
const MAX_AGE_MS: i64 = 30 * 60 * 1000;

#[derive(Serialize, Deserialize, Default)]
struct Saved {
    local_id: i64,
    port: u16,
    saved_at_ms: i64,
    mobs: HashMap<i32, i32>,
    hp: HashMap<i32, i32>,
}

pub struct MobCache {
    path: PathBuf,
    restore_attempted: bool,
    last_saved: Option<(i64, u16, usize, usize)>,
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

impl MobCache {
    pub fn new(app_data_dir: &std::path::Path) -> Self {
        Self {
            path: app_data_dir.join("mob_cache.json"),
            restore_attempted: false,
            last_saved: None,
        }
    }

    /// Call periodically. Waits until the session fingerprint is known, then
    /// restores once (before the first save, so the old table isn't clobbered)
    /// and afterwards saves whenever the mob table or fingerprint changed.
    pub fn tick(&mut self, storage: &DataStorage, npcs: &NpcLookup, port: Option<u16>) {
        let (Some(local_id), Some(port)) = (storage.local_player_id(), port) else {
            return;
        };
        if !self.restore_attempted {
            self.restore_attempted = true;
            let restored = self.restore(storage, npcs, local_id, port, now_ms());
            if restored > 0 {
                tracing::info!("Mob cache: restored {} mob identities from the previous run", restored);
            }
        }

        let mobs = storage.get_mob_data();
        let hp = storage.get_mob_hp_data();
        let sig = (local_id, port, mobs.len(), hp.len());
        if self.last_saved == Some(sig) {
            return;
        }
        let saved = Saved { local_id, port, saved_at_ms: now_ms(), mobs, hp };
        if let Ok(json) = serde_json::to_string(&saved) {
            let tmp = self.path.with_extension("json.tmp");
            if std::fs::write(&tmp, json).is_ok() && std::fs::rename(&tmp, &self.path).is_ok() {
                self.last_saved = Some(sig);
            }
        }
    }

    /// Apply the saved table if it belongs to this session. Returns how many
    /// mobs were filled in.
    fn restore(&self, storage: &DataStorage, npcs: &NpcLookup, local_id: i64, port: u16, now: i64) -> usize {
        let Ok(text) = std::fs::read_to_string(&self.path) else { return 0 };
        let Ok(saved) = serde_json::from_str::<Saved>(&text) else { return 0 };
        if saved.local_id != local_id || saved.port != port || now - saved.saved_at_ms > MAX_AGE_MS {
            return 0;
        }
        let mut n = 0;
        for (&id, &code) in &saved.mobs {
            if storage.is_mob(id) {
                continue;
            }
            storage.append_mob(id, code);
            if npcs.is_boss(code) {
                storage.register_boss(id);
            }
            if let Some(&hp) = saved.hp.get(&id) {
                if storage.get_mob_hp(id).is_none() {
                    storage.append_mob_hp(id, hp);
                }
            }
            n += 1;
        }
        n
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh directory per test: they run in parallel and must not share a file.
    fn dir() -> PathBuf {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!("a2_mob_cache_{}_{}_{}", std::process::id(), now_ms(), n));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn restores_into_the_same_session_only() {
        let d = dir();
        // First run: mob 37404 spawned in view.
        let s1 = DataStorage::new();
        s1.set_local_player_id(Some(12978));
        s1.append_mob(37404, 2400032);
        s1.append_mob_hp(37404, 5_000_000);
        MobCache::new(&d).tick(&s1, &NpcLookup::new(), Some(62031));

        // Restart, same session: the mob is known again without a spawn.
        let s2 = DataStorage::new();
        s2.set_local_player_id(Some(12978));
        MobCache::new(&d).tick(&s2, &NpcLookup::new(), Some(62031));
        assert_eq!(s2.get_mob_data().get(&37404), Some(&2400032));
        assert_eq!(s2.get_mob_hp(37404), Some(5_000_000));

        // Restart into another session (new local id): nothing restored.
        let s3 = DataStorage::new();
        s3.set_local_player_id(Some(555));
        MobCache::new(&d).tick(&s3, &NpcLookup::new(), Some(62031));
        assert!(s3.get_mob_data().is_empty());
    }

    #[test]
    fn a_live_spawn_wins_over_the_saved_table() {
        let d = dir();
        let s1 = DataStorage::new();
        s1.set_local_player_id(Some(1));
        s1.append_mob(10, 111);
        MobCache::new(&d).tick(&s1, &NpcLookup::new(), Some(5));

        let s2 = DataStorage::new();
        s2.set_local_player_id(Some(1));
        s2.append_mob(10, 222);
        MobCache::new(&d).tick(&s2, &NpcLookup::new(), Some(5));
        assert_eq!(s2.get_mob_data().get(&10), Some(&222));
    }

    #[test]
    fn stale_file_is_ignored() {
        let d = dir();
        let saved = Saved { local_id: 1, port: 5, saved_at_ms: now_ms() - MAX_AGE_MS - 1, mobs: HashMap::from([(10, 111)]), hp: HashMap::new() };
        std::fs::write(d.join("mob_cache.json"), serde_json::to_string(&saved).unwrap()).unwrap();
        let s = DataStorage::new();
        s.set_local_player_id(Some(1));
        MobCache::new(&d).tick(&s, &NpcLookup::new(), Some(5));
        assert!(s.get_mob_data().is_empty());
    }
}
