// Active time: how long someone was actually attacking. A pause between two
// hits of ACTIVE_GAP_MS or more doesn't count — the same rule as the backend's
// `ACTIVE_GAP_MS` in data_storage.rs, and how the game's own combat analysis
// measures DPS. Dividing by first-to-last hit instead read ~25% low on a live
// fight with one 14 s pause.
(function initActiveTime(global) {
  const ACTIVE_GAP_MS = 2000;

  // From raw hit times (ms, any order). Returns total active ms plus the pauses
  // that were left out, so the UI can show how the time was spent.
  const fromHits = (timestamps) => {
    const hits = (Array.isArray(timestamps) ? timestamps : [])
      .map(Number)
      .filter(Number.isFinite)
      .sort((a, b) => a - b);
    let activeMs = 0;
    const pauses = [];
    for (let i = 1; i < hits.length; i++) {
      const gap = hits[i] - hits[i - 1];
      if (gap < ACTIVE_GAP_MS) activeMs += gap;
      else pauses.push(gap);
    }
    const spanMs = hits.length > 1 ? hits[hits.length - 1] - hits[0] : 0;
    return { activeMs, spanMs, pauses };
  };

  // From [start, end] spans (as the backend sends them). Overlapping spans — a
  // summon hitting alongside its owner, cleaving two targets — count once.
  const fromSpans = (spans) => {
    const list = (Array.isArray(spans) ? spans : [])
      .filter((s) => Array.isArray(s) && s.length >= 2)
      .map(([s, e]) => [Number(s), Number(e)])
      .filter(([s, e]) => Number.isFinite(s) && Number.isFinite(e))
      .sort((a, b) => a[0] - b[0]);
    let total = 0;
    let cur = null;
    for (const [s, e] of list) {
      if (cur && s <= cur[1]) {
        cur[1] = Math.max(cur[1], e);
      } else {
        if (cur) total += cur[1] - cur[0];
        cur = [s, e];
      }
    }
    if (cur) total += cur[1] - cur[0];
    return total;
  };

  // DPS over active time; below one second of activity, divide by one second
  // like the backend does, so a single hit doesn't read as infinite DPS.
  const dps = (damage, activeMs) => (Number(damage) || 0) / (Math.max(1000, Number(activeMs) || 0) / 1000);

  global.activeTime = { ACTIVE_GAP_MS, fromHits, fromSpans, dps };
})(window);
