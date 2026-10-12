# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

A real-time DPS meter overlay for AION 2 (Windows only). It sniffs the game's
network traffic with Npcap/pcap, reverse-engineers the game's binary protocol
to reconstruct combat events, and renders a live overlay plus several tool
windows (Details, Settings, Battle History, per-fight views). Built with
**Tauri 2**: a Rust backend does capture/parsing/state, a plain HTML/CSS/JS
frontend (no framework, no bundler beyond Vite for dev-serving) renders the UI.

Requires Administrator privileges and Npcap installed in "WinPcap API-compatible
Mode" to capture raw packets.

This repo is a **fork** of `taengu/A2Tools-DPS-Meter` (remote `upstream`;
`origin` is the fork). Upstream changes are merged in periodically
(`git fetch upstream && git merge upstream/main`); the fork's own features
(actor filter, active-time DPS, buffs, personal bests, …) must survive those
merges. The upstream update check is disabled (`core.js`, see the comment where
`ReleaseChecker.start` used to be called) because accepting it installs
upstream's MSI over the fork — in a dev run, into `src-tauri/target/debug`.

## Commands

```bash
npm install              # install JS deps
npm run tauri dev        # full dev run: Rust backend + Vite frontend
npm run dev              # frontend-only (Vite), no Rust backend — rarely useful alone
npm run tauri build      # production build; MSI lands in src-tauri/target/release/bundle/msi/
npm run rename-msi       # strip spaces from the built MSI filename (used post-build)
```

Rust tests live under `src-tauri/`:

```bash
cd src-tauri
cargo test                                   # all tests
cargo test --test actor_filter               # one test file
cargo test --lib mob_cache                   # unit tests inside one module
```

`cargo test` fails with "failed to remove file …a2tools-dps-meter.exe" while
`tauri dev` is running (the exe is locked). Use a separate target dir instead
of stopping the app: `CARGO_TARGET_DIR=<some temp dir> cargo test`.

Several tests replay real packet captures and **skip silently** when the env
var is unset (captures are user traffic and are not committed):

| Test | Env var | Checks |
|---|---|---|
| `capture_replay` | `A2_REPLAY_CAPTURE`, `A2_REPLAY_AURA_CAPTURE` | party names, summon owners, roster |
| `self_status` | `A2_SELF_STATUS_CAPTURE` | local player bound from `4A 36` before any `33 36` |
| `buffs_capture` | `A2_BUFF_CAPTURE` | buff uptimes vs the damage-multiplier steps |
| `radar_capture` | `A2_RADAR_CAPTURE`, `A2_RADAR_LIVE_CAPTURE` | Sapphire on the radar next to the player, removed when depleted; spawn points saved and the map recognised in a later mid-zone session |

There is no JS test suite and no linter — `node --check public/src/js/<file>.js`
catches syntax errors; behaviour has to be checked in the running app.

### Dev-loop gotchas

- **Frontend changes need an app restart.** The frontend is classic
  `<script>` tags under `public/src/js/` (not ES modules), so Vite does not
  hot-reload them; restart `npm run tauri dev`. Rust edits anywhere under
  `src-tauri/` (including `tests/`) make `tauri dev` rebuild and relaunch the
  app on its own. Killing a dev run can orphan `a2tools-dps-meter.exe` and the
  Vite server on port 1420 — kill those before relaunching.
- The global hotkey **Ctrl+Alt+R resets combat data**; it does not reload windows.
- `src-tauri/Cargo.toml` routinely shows as modified with only line-ending
  differences (`git diff --ignore-cr-at-eol` is empty). Don't commit it;
  `git checkout -- src-tauri/Cargo.toml` clears it.
- JSON locale files mix CRLF/LF; edit them with a JSON-aware script that keeps
  the file's line endings, and run `JSON.parse` on the result.

### Protocol research tools

Raw captures come from Settings → Diagnostics → "Enable packet logging", which
writes `%APPDATA%\com.a2tools.dps-meter\packets_<timestamp>.txt`
(`TIMESTAMP|Client:<port>|HEX` per TCP payload; a new file per app start).
Client→server payloads go to `packets_<timestamp>_out.txt`; they are
encrypted, so every protocol fact has to come from the server side.
Two examples decode them with the app's own framing (varint length − 3, LZ4
`FF FF` bundles):

```bash
cd src-tauri
cargo run --example self_id_probe -- <capture> <mode> ...
#   probe <id> [from] [to]   opcodes whose first varint is <id>, with cadence
#   vals <op>...             distinct first-varint values per opcode (e.g. 4A36 2B38)
#   shapes <op>              distinct packet shapes for one opcode
#   dealt <id> [from] [to]   04 38 damage records dealt by <id>, per skill
#   findval <value> [id]     where a value (e.g. an NPC code) appears, varint or u32
#   scalars <id>             damage-multiplier timeline and per-skill split
#   around <id> <before_ms> <after_ms> <HH:MM:SS.mmm>...   non-combat packets near times
#   buffs <id>               decoded 2A 38 / 2C 38 buff adds/removes with names
#   selfrecords              every 33 36 self record
#   ops [from] [to]          opcode histogram: count, sizes, first/last time
#   dump <op>|all [from] [to]   hex of every packet with an opcode (e.g. 3436)
cargo run --example replay_dps -- <capture> [from_ts] [to_ts]
#   runs the real parser + calculator: per-target damage, gaps, DPS at several
#   gap thresholds, meter rows, multiplier split, buff uptimes, name check
```

The method that found most protocol facts below: know a ground-truth value
(an entity id from a `self record` log line, a timestamp where the multiplier
changed, the game's own combat-analysis number), then search captures for it.

## Architecture

### Backend: capture → parse → state → commands

Pipeline, roughly in dependency order (`src-tauri/src/`):

1. **`capture/pcap_capturer.rs`** — opens Npcap devices, emits raw
   `CapturedPayload`s (device, ports, bytes) onto an mpsc channel.
2. **`combat/capture_dispatcher.rs`** — the router. Before a game port is
   known, it gates candidate packets on a byte-signature ("record terminator")
   and requires a *sustained rate* of matches within a sliding window before
   locking onto a `(src_port, dst_port)` flow — a single coincidental match
   (e.g. a local service on the wrong port) must not steal the lock. It also
   polls whether the AION 2 window is running/foreground and resets capture
   state when the game exits or a locked connection goes stale.
3. **`capture/stream_assembler.rs`** — reassembles a locked flow's byte stream
   from possibly-fragmented TCP segments per `(port, port)` key.
4. **`capture/stream_processor.rs`** (the core of the app) — parses the game's
   binary protocol. Every framed packet (top-level or inside an LZ4 bundle)
   goes through `parse_perfect_packet`, which tries each record parser in
   turn; some records are also found by scanning raw bytes. This is where new
   protocol knowledge goes when the game updates and breaks parsing.
5. **`combat/data_storage.rs`** — shared mutable state behind one `RwLock`:
   entities, nicknames, party roster, mob codes/HP, per-target per-actor combat
   aggregates (`TargetCombatData` → `ActorCombatData`), buffs.
6. **`combat/dps_calculator.rs`** — turns the aggregates into what the UI
   consumes: `get_dps` (overlay, emitted as `dps-update` every 500 ms),
   `get_details_context` / `get_target_details` (Details), and
   `snapshot_boss_fights` (auto-saved history every 30 s). Owns target
   selection (Boss / Last Hit / All Targets / Train) and the actor filter.
7. **`lib.rs`** — Tauri command surface (`#[tauri::command]` functions),
   multi-window management, and the periodic loops (DPS emit, mob cache,
   auto-save). The frontend talks to the backend exclusively through these
   commands plus `emit`/`listen` events.

Supporting modules: `entity/` (shared data structs), `history/` (JSON-file
battle history; `list_fights` summaries are cached until the directory
changes), `i18n/` (NPC/skill names loaded from `src/data/i18n/**`, reloaded on
`set_language`), `config/settings.rs` (flat key-value settings in
`%APPDATA%\com.a2tools.dps-meter\settings.json`), `combat/buffs.rs`,
`combat/mob_cache.rs`, `combat/radar.rs` (world objects + self position for
the Radar window, `public/radar.html`; spawn points seen per map persist in
`radar_points.json`, because the server only sends objects within ~60 m; the
objects in view go to `radar_live.json` with a session fingerprint, like the
mob cache, since a spawn is never re-sent to a meter restarted next to it), `platform/` (Windows-only: admin check, global hotkeys,
AION 2 window detection, screenshot-to-clipboard), `logging/` (tracing setup,
optional raw-packet logging).

### Protocol map (as of the 2026-09 game version)

Opcodes are the two bytes after the packet's varint length. The `36` family
shifted by +1 in a June 2026 patch; parsers accept both values where noted.

| Record | Meaning |
|---|---|
| `04 38` | damage/heal record: target, switch, flag, actor, skill u32, …, `[0 pad] <power scalar> <damage>` |
| `05 38` | compact skill/damage record |
| `33 36` | **self record**: your entity id + name. Only on zone load. Also the map id: `<map u32> <map u32> <x y z> <u32> 00 00 <u16> <z f32> 01 DE 02` |
| `45 36` / `44 36` | another player's id + name (mask-gated name field) |
| `40 36` / `41 36` | mob/summon spawn: NPC code, max HP, summon `parent_key`. Only when the entity enters view. |
| `41 36` / `42 36` | death (flag 3 = combat death; only seen for mobs so far) |
| `4A 36` | **self status tick** (~1 Hz), first varint = your entity id; never carries anyone else's |
| `23 36` (entity 0) | zone change / self teleport |
| `02 97` | party roster: names, level, gear score, combat power, server (account dbid, not entity id) |
| `2A 38` | buff add: target, ?, ?, seq, effect u32, duration_ms u32, `00×4`, epoch ms u64, caster |
| `2C 38` | buff remove (short form): target, `01 00`, seq, `01` |
| `2B 38` / `2A 38` | end with `02 <x y z>`: the position of the entity concerned, ~1 Hz — the only source of the local player's position |
| `02 38` | `<self> 00 <u32> <u16 counter> <self> <heading f32> <4 bytes>`: your facing, ~1 Hz, Unreal yaw in degrees (0° = +X, 90° = +Y); other shapes/entities share the opcode |
| `34 36` | world object (gather node, …) enters view: id, kind u16, code u32, `x y z` f32 (cm), heading. Known codes (`radar.js` `KNOWN`, all kind 4): 1301199 Hidden Cube; gatherables 1550000 Odyle, 1550004 Orichalcum, 1550012 Asvata, 1550020 Sapphire, 1550021 Diamond, 1550033 Calendula, 1550045 Ohkra. Any other code is named from the radar's list (`radar_labels.json`) |
| `35 36` | object leaves: id, reason (`04` depleted, `01` out of view), 2 bytes |
| `38 36` / `3A 36` | gathering: `<obj> <self> 01` start, `<obj> <self> <n>` n-th gather done |

Consequences baked into the code:

- **Who is "me"**: bound from `33 36`; when the meter starts mid-zone, `4A 36`
  fills the gap within a second (`parse_self_status_packet`). It never
  overrides an existing binding.
- **Party members are joined by character name**, never entity id, because
  the roster only carries an account id.
- **Mob names**: the NPC code exists only in the spawn record — a 4-minute
  capture without it had the code in no other packet. `combat/mob_cache.rs`
  persists entity → NPC code/HP to `mob_cache.json` and restores it on startup
  only when the session fingerprint (local entity id + game port) matches and
  the file is < 30 min old. Otherwise the title shows "Unknown target".
- **Entity ids are session-scoped** (they change on zone change/reconnect);
  anything persisted across app runs needs a fingerprint like the mob cache's.
- **Power scalar** (`04 38`, hundredths of a percent): actor state, not per
  skill — it steps by 10% as damage buffs come and go (Chanter 125.2/135.2/
  145.2%, Gladiator 156/176/146%). Summons inherit their owner's value, which
  summon attribution relies on (`docs/summon-attribution.md`). Hits carry it
  (`ParsedDamagePacket::power_scalar`); `scalar_time_ms` splits active time
  across levels.
- **Buffs** (`combat/buffs.rs`): effect id = skill/item code × 10 + one digit
  (182500101 → 18250010 "Power of the Storm"), so names come from the skill
  data. Most buffs are never removed explicitly — they lapse after their
  duration or are refreshed — so an add lasts its duration, a remove cuts it
  short, refreshes merge.

**Summon/pet damage attribution** is a deliberately layered heuristic (spawn
`parent_key` → spawn inline name → power-scalar matching against known actors)
because summons don't always announce themselves. Read
`docs/summon-attribution.md` before touching owner resolution, orphan merging,
or the power scalar.

### How DPS is computed

- **Active time, not the fight window.** Each actor keeps `active_spans`; a gap
  of `ACTIVE_GAP_MS` (2 s) or more between hits ends a span. DPS = damage /
  union of the row's spans (owner + summons, overlaps counted once). This is
  how the game's own combat analysis measures it: on a calibration fight
  first-to-last read 16.2k, active time 21.6k, the game ~22k.
  `public/src/js/activeTime.js` mirrors the rule for Details/history, which
  derive active time from per-hit timestamps.
- **Actor filter** (`dpsMeter.actorFilter`: `party` default / `self` / `all`)
  is applied after the orphan merge, so summon damage stays with its owner. It
  stays inactive (shows everyone) until the local player is recognised, rather
  than blanking the meter. Contribution % is over shown rows; the boss HP bar
  always uses everyone's damage.
- **ALL mode** only counts targets hit within `dpsMeter.allTargetsWindowMs` of
  the latest hit.
- **Personal bests** (`personalBests.js`): best active-time DPS per NPC code and
  character, from `FightSummary.local*` (computed in `FightRecord::local_stats`;
  records store `local_actor_id`, older ones are matched by the one unmasked
  nickname). A fight needs ≥ 10 s active time to count.

### Multi-window frontend

There is **one** `index.html`/JS bundle shared by every window. Which "view"
a window renders is decided at window-creation time in `lib.rs`, which injects
`window.__A2_VIEW__ = '<view>'` via `initialization_script` (`main`, `details`,
`settings`, `history`; per-fight windows are `details-<id>` and also render as
`details`). `public/src/js/tauriBridge.js` reads that value first thing and
sets `window.A2_VIEW`, which the rest of the frontend (`core.js`, `meter.js`,
`details.js`, `history.js`, ...) branches on to show/hide the right panel.

Windows are: the always-on-top transparent overlay (`main`), a single live
Details window (optionally pinned to a monitor), one History browser window,
and any number of per-fight Details windows (`details-<fight_id>`, cascaded on
open so they don't stack exactly on top of each other). `lib.rs`'s
`request_details_view` command decides which of these a click should target
and, if the target window doesn't exist yet, parks the request in
`PENDING_DETAILS_REQUEST` for that window to pull once its own listener is
ready (a freshly-built webview has no listener at emit time).

Tauri command handlers that build a `WebviewWindowBuilder` window on Windows
**must be `async`**: `build()` deadlocks on the main thread there (see the doc
comment on `open_settings_window` in `lib.rs` for why). Windows are always
built visible with an explicit background color rather than hidden-then-shown,
because a hidden WebView2 host may never finish loading its content.

**Settings across windows.** Settings is its own window. A change goes through
`javaBridge.setSetting` → `update_settings`, which persists it and emits
`setting-changed` to every window; each window applies it in
`DpsApp.applyRemoteSettingChange` (`core.js`) — either via
`REMOTE_APPLIED_SETTING_CONTROLS` (checkbox/input controls) or the
`directHandlers` map there (theme, opacity, slim mode, language, …). A new
setting that changes how another window looks must be added there, or it only
applies after a restart. A backend-relevant setting is also handled by key in
`update_settings` and read once at startup in `run()` (see `actorFilter`,
`allTargetsWindowMs`).

**i18n**: UI strings live in `src/data/i18n/ui/<lang>.json` for ten locales
(en, de, es, fr, ja, ko, pt, ru, zh-Hans, zh-Hant); every new key goes into all
ten. JS reads them with `i18n.t(key, fallback)`; markup uses `data-i18n`.

**Static assets**: class icons are `public/assets/<Korean class name>.png`
(`검성.png`, …), committed via a `.gitignore` exception — upstream adds them at
release time from outside git. Skill icons are fetched from the game's CDN
(`assets.playnccdn.com/static-aion2-gamedata/resources/<ICON_NAME>.png`) using
`src/data/skill_icons.json`.
