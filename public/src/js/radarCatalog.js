// What the Radar knows and how its view is set up, shared by the Radar window
// (radar.js) and the Hub's Radar tab (hub.js), which both edit the same
// prefs (radar_prefs.json through the backend).
window.A2RadarCatalog = (() => {
  // Objects worth showing, by `34 36` code. Anything else is only drawn with
  // "all" on, labelled with its code, which is how new codes get found.
  // `shape`: "dot" (default) or "square".
  const KNOWN = {
    1550020: { name: "Sapphire", color: "#5aa9ff" },
    // Named by the user from the radar (2026-10-12).
    1550000: { name: "Odyle", color: "#3ddc84" },
    1550004: { name: "Orichalcum", color: "#ffcc33" },
    1550012: { name: "Asvata", color: "#3ddc84" },
    1550021: { name: "Diamond", color: "#e8f4ff" },
    1550022: { name: "Ruby", color: "#ff3b3b" },
    1550040: { name: "Raydam", color: "#ff3b3b" },
    1550032: { name: "Azpha", color: "#c77dff" },
    1550033: { name: "Calendula", color: "#3ddc84" },
    1550041: { name: "Morfa", color: "#3ddc84" },
    1550045: { name: "Ohkra", color: "#ff8a3d" },
    // Confirmed 2026-10-12: stood 3–6 m from it for minutes (map 1013).
    1301199: { name: "Hidden Cube", color: "#ff3b3b", shape: "square" },
    // Named by the user in game (2026-10-22). Not a gather node.
    1100035: { name: "Elyos Kisk", color: "#5aa9ff", shape: "square" },
  };

  // headingMirror defaults on: in-game, facing-up only matched with the
  // mirror on. `v` bumps re-apply a changed default once over saved prefs.
  const PREFS_VERSION = 2;
  // `hidden`: codes unticked in the ⚙ panel.
  const DEFAULT_PREFS = { range: 100, rotate: 0, mirror: false, all: false, headingUp: true, headingMirror: true, hidden: [], route: false, myFaction: null,
    // Which players to draw (the ⚙ panel): party, allies (and anyone whose
    // side is unknown), enemies.
    showParty: true, showAllies: true, showEnemies: true };

  // Saved prefs brought up to date, ready to assign over the defaults.
  const upgradePrefs = (saved) => {
    if (!saved || typeof saved !== "object") return {};
    const p = { ...saved };
    if ((p.v || 1) < 2) delete p.headingMirror;
    // One "players" box before the split: off meant all three off.
    if (p.players === false) Object.assign(p, { showParty: false, showAllies: false, showEnemies: false });
    delete p.players;
    p.v = PREFS_VERSION;
    return p;
  };

  return { KNOWN, UNKNOWN_COLOR: "rgba(255,255,255,0.45)", PREFS_VERSION, DEFAULT_PREFS, upgradePrefs };
})();
