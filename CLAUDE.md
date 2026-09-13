# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

A real-time DPS meter overlay for AION 2 (Windows only). It sniffs the game's
network traffic with Npcap/pcap, reverse-engineers the game's binary protocol
to reconstruct combat events, and renders a live overlay plus several tool
windows (Details, Settings, Battle History, per-fight views). Built with
**Tauri 2**: a Rust backend does capture/parsing/state, a plain HTML/CSS/JS
frontend (no framework, no bundler beyond Vite for dev-serving) renders the UI.

Requires Administrator privileges and Npcap (WinPcap-compatible mode) to
capture raw packets.

## Commands

```bash
npm install              # install JS deps
npm run tauri dev        # full dev run: Rust backend + Vite frontend, hot reload
npm run dev               # frontend-only (Vite), no Rust backend — rarely useful alone
npm run tauri build       # production build; MSI lands in src-tauri/target/release/bundle/msi/
npm run rename-msi        # strip spaces from the built MSI filename (used post-build)
```

Rust tests live under `src-tauri/`:

```bash
cd src-tauri
cargo test                                   # all tests
cargo test --test spawn_mask_width           # one test file
cargo test --test capture_replay -- --nocapture   # skipped unless real captures are supplied, see below
```

`capture_replay.rs` is a ground-truth regression test driven by real packet
captures and is skipped when they're absent:

```bash
A2_REPLAY_CAPTURE=/path/to/packets_..._183732.txt \
A2_REPLAY_AURA_CAPTURE=/path/to/packets_..._112931.txt \
cargo test --test capture_replay -- --nocapture
```

There is no JS test suite and no linter configured — validate frontend changes
by running `npm run tauri dev` and exercising the affected window.

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
4. **`capture/stream_processor.rs`** (~2600 lines, the core of the app) —
   parses the game's binary protocol: entity spawns (`41 36` records),
   damage/skill records (`04 38` / `05 38`), party membership, etc. This is
   where new protocol knowledge goes when the game updates and breaks parsing.
5. **`combat/data_storage.rs`** — shared mutable state: known entities,
   nicknames, party membership, power-scalar observations, damage log.
6. **`combat/dps_calculator.rs`** — turns the raw damage log into the
   DPS/skill-breakdown data the UI consumes; also owns target-selection mode
   (Boss / Last Hit / All Targets / Train) and boss-fight snapshotting for
   auto-save.
7. **`lib.rs`** — Tauri command surface (`#[tauri::command]` functions) and
   multi-window management. The frontend talks to the backend exclusively
   through these commands plus `emit`/`listen` events; there is no other API
   boundary.

Supporting modules: `entity/` (shared data structs), `history/` (JSON-file
battle history persistence), `i18n/` (NPC/skill name + UI string lookups,
loaded from `src/data/i18n/**`), `config/settings.rs` (flat key-value settings
persisted to disk, broadcast to all windows on change via a `setting-changed`
event), `platform/` (Windows-only: admin check, global hotkeys, window
detection for AION 2, screenshot-to-clipboard), `logging/` (tracing setup,
optional raw-packet logging to disk for later replay/debugging).

**Summon/pet damage attribution** is a deliberately layered heuristic (spawn
`parent_key` → spawn inline name → power-scalar matching against known actors)
because summons don't always announce themselves and don't carry an explicit
owner field on their damage records. See `docs/summon-attribution.md` before
touching anything related to owner resolution, orphan merging, or the power
scalar — it documents the protocol layout, the ground-truth captures the logic
was measured against, and *why* each fallback exists.

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

### Protocol reverse-engineering conventions

- Entity/session state (entity ids, ports) is **session-scoped** — it resets
  on zone change or reconnect, so caching across sessions is invalid.
- When the game updates and packet parsing silently stops working (no errors,
  just missing data), suspect an offset/field-width change in
  `stream_processor.rs` first — see `src-tauri/tests/spawn_mask_width.rs` for
  a precedent (a mask field widened from u16 to u32 across a game patch,
  handled by trying both layouts).
- `combat_dispatcher.rs`'s record-terminator signature also changed across a
  June 2026 patch; both old and new magic bytes are matched for the same
  reason — protocol constants drift and old sessions/captures may still use
  the previous layout.
