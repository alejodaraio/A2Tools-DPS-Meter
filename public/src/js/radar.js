// Radar window: world objects around the player, from `get_radar`
// (see src-tauri/src/combat/radar.rs for where the data comes from).
//
// Two kinds of marker: objects in view right now (filled), and spawn points
// remembered from earlier sightings on this map (hollow) — the server only
// sends objects within ~60 m, so everything further out is from memory and
// may already have been gathered by someone else.
//
// Coordinates are game units (cm). Two orientations:
// - heading-up (default): the character's facing (`02 38`, ~1 Hz, degrees:
//   0° = +X, 90° = +Y) points up, eased between updates so the disc turns
//   rather than jumps. Handedness matches the north-up view unmirrored (+Y is
//   90° counter-clockwise from +X), which is what matched the minimap —
//   assuming Unreal's +Y-to-the-right instead turned the disc the wrong way;
// - north-up: which world axis is minimap north is not known, so this one is
//   rotated in 90° steps and mirrored by hand. Its calibration also places the
//   "N" on the rim in heading-up.
// Each mode keeps its own mirror; all of it is remembered per machine.

(() => {
  const invoke = window.__TAURI__?.core?.invoke;

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
  };
  const UNKNOWN_COLOR = "rgba(255,255,255,0.45)";
  const SWATCHES = ["#5aa9ff", "#ff3b3b", "#e8f4ff", "#3ddc84", "#ffcc33", "#c77dff", "#ff8a3d"];

  // Names the user gave codes from the radar (radar_labels.json, via the
  // backend so they survive dev/installed builds); they win over KNOWN.
  let labels = {};
  const info = (code) => labels[String(code)] || KNOWN[code];
  const esc = (t) => String(t).replace(/[&<>"']/g, (c) => `&#${c.charCodeAt(0)};`);
  const loadLabels = async () => {
    try { labels = (await invoke("get_radar_labels")) || {}; } catch {}
  };
  const RANGES_M = [50, 100, 200, 400, 800, 1600];
  // Zoom (mouse wheel, + / −): finer steps, about ×1.4 each.
  const ZOOM_M = [25, 35, 50, 70, 100, 140, 200, 280, 400, 560, 800, 1100, 1600];
  // The route's area is the range: re-plan once the zooming stops, not on
  // every wheel notch.
  const ROUTE_REPLAN_MS = 800;
  // Fast enough not to add to the ~1 Hz facing's own lag.
  const POLL_MS = 100;
  const STALE_MS = 5000;

  // headingMirror defaults on: in-game, facing-up only matched with the
  // mirror on. `v` bumps re-apply a changed default once over saved prefs.
  const PREFS_VERSION = 2;
  // `hidden`: codes unticked in the ⚙ panel.
  const prefs = { range: 100, rotate: 0, mirror: false, all: false, headingUp: true, headingMirror: true, hidden: [], route: false };
  const applySaved = (saved) => {
    if (!saved || typeof saved !== "object") return;
    if ((saved.v || 1) < 2) delete saved.headingMirror;
    Object.assign(prefs, saved, { v: PREFS_VERSION });
  };
  // Kept by the backend (radar_prefs.json) so the dev server and the
  // installed build — different origins, different localStorage — agree.
  // localStorage is only the first-run source and an offline fallback.
  try { applySaved(JSON.parse(localStorage.getItem("radar.prefs") || "null")); } catch {}
  const loadPrefs = async () => {
    try {
      const saved = await invoke("get_radar_prefs");
      if (saved) applySaved(saved);
      else savePrefs();
    } catch {}
  };
  const savePrefs = () => {
    try { localStorage.setItem("radar.prefs", JSON.stringify(prefs)); } catch {}
    invoke?.("set_radar_prefs", { prefs }).catch(() => {});
  };

  const canvas = document.getElementById("scope");
  const ctx = canvas.getContext("2d");
  const statusEl = document.getElementById("status");
  const listEl = document.getElementById("list");
  const rangeBtn = document.getElementById("rangeBtn");
  const rotateBtn = document.getElementById("rotateBtn");
  const modeBtn = document.getElementById("modeBtn");
  const mirrorBtn = document.getElementById("mirrorBtn");
  const allBtn = document.getElementById("allBtn");
  const routeBtn = document.getElementById("routeBtn");
  const routeInfoEl = document.getElementById("routeInfo");

  let snapshot = null;
  // Packet capture needs it; in the radar-only build nothing else would say so.
  let isAdmin = true;
  // Heading drawn right now, chasing snapshot.selfHeading.
  let shownHeading = null;

  const headingUp = () => prefs.headingUp && snapshot?.selfHeading != null;

  const syncButtons = () => {
    rangeBtn.textContent = `${prefs.range} m`;
    rotateBtn.textContent = `⟳ ${prefs.rotate}°`;
    rotateBtn.style.display = prefs.headingUp ? "none" : "";
    modeBtn.textContent = prefs.headingUp ? "▲" : "N";
    modeBtn.classList.toggle("on", prefs.headingUp);
    mirrorBtn.classList.toggle("on", prefs.headingUp ? prefs.headingMirror : prefs.mirror);
    allBtn.classList.toggle("on", prefs.all);
    routeBtn.classList.toggle("on", prefs.route);
  };

  let replanTimer = null;
  const setRange = (m) => {
    if (m === prefs.range) return;
    prefs.range = m;
    clearTimeout(replanTimer);
    replanTimer = setTimeout(() => { route = null; updateRoute(); draw(); }, ROUTE_REPLAN_MS);
    savePrefs(); syncButtons(); draw();
  };
  // Next of the coarse steps above the current range (which zoom may have
  // left between two of them), wrapping around.
  rangeBtn.addEventListener("click", () => {
    setRange(RANGES_M.find((m) => m > prefs.range) ?? RANGES_M[0]);
  });
  // dir < 0 zooms in (shorter range).
  const zoom = (dir) => {
    let i = ZOOM_M.findIndex((m) => m >= prefs.range);
    if (i < 0) i = ZOOM_M.length - 1;
    // Between two steps: zooming in goes to the lower one.
    if (ZOOM_M[i] !== prefs.range && dir < 0) i -= 1;
    else i += dir;
    setRange(ZOOM_M[Math.max(0, Math.min(ZOOM_M.length - 1, i))]);
  };
  document.querySelector(".scope").addEventListener("wheel", (e) => {
    if (!filterPanel.hidden) return;
    e.preventDefault();
    zoom(e.deltaY < 0 ? -1 : 1);
  }, { passive: false });
  document.getElementById("zoomIn").addEventListener("click", () => zoom(-1));
  document.getElementById("zoomOut").addEventListener("click", () => zoom(1));
  rotateBtn.addEventListener("click", () => {
    prefs.rotate = (prefs.rotate + 90) % 360;
    savePrefs(); syncButtons(); draw();
  });
  mirrorBtn.addEventListener("click", () => {
    if (prefs.headingUp) prefs.headingMirror = !prefs.headingMirror;
    else prefs.mirror = !prefs.mirror;
    savePrefs(); syncButtons(); draw();
  });
  modeBtn.addEventListener("click", () => {
    prefs.headingUp = !prefs.headingUp;
    savePrefs(); syncButtons(); draw();
  });
  allBtn.addEventListener("click", () => {
    prefs.all = !prefs.all;
    route = null;
    savePrefs(); syncButtons(); draw();
  });
  // Clicking it on always plans afresh from where we stand.
  routeBtn.addEventListener("click", () => {
    prefs.route = !prefs.route;
    route = null;
    updateRoute();
    savePrefs(); syncButtons(); draw();
  });
  document.getElementById("closeBtn").addEventListener("click", () => {
    invoke?.("close_tool_window");
  });
  // Frameless window: drag from the header, like the overlay.
  document.querySelector(".header").addEventListener("mousedown", (e) => {
    if (e.button !== 0 || e.target.closest("button")) return;
    invoke?.("start_drag");
  });

  // World delta (cm) -> screen vector. Screen y grows downward.
  const toScreenNorth = (dx, dy) => {
    let sx = dx;
    let sy = -dy;
    for (let r = 0; r < prefs.rotate; r += 90) [sx, sy] = [-sy, sx];
    if (prefs.mirror) sx = -sx;
    return [sx, sy];
  };

  // Facing up: forward = (cos h, sin h), right = (sin h, -cos h), so +Y is to
  // the left when facing +X — same handedness as north-up without mirror.
  const toScreenHeading = (dx, dy) => {
    const h = (shownHeading * Math.PI) / 180;
    const fwd = dx * Math.cos(h) + dy * Math.sin(h);
    let right = dx * Math.sin(h) - dy * Math.cos(h);
    if (prefs.headingMirror) right = -right;
    return [right, -fwd];
  };

  const toScreen = (dx, dy) => (headingUp() ? toScreenHeading(dx, dy) : toScreenNorth(dx, dy));

  // The world direction north-up puts at the top: one of the four axes.
  const northWorld = () =>
    [[1, 0], [0, 1], [-1, 0], [0, -1]].find(([x, y]) => {
      const [sx, sy] = toScreenNorth(x, y);
      return Math.abs(sx) < 1e-6 && sy < 0;
    });

  // Where something is, as a word: a compass point north-up, an arrow
  // relative to the facing heading-up.
  const direction = (sx, sy) => {
    const deg = (Math.atan2(sx, -sy) * 180) / Math.PI;
    const i = Math.round(((deg + 360) % 360) / 45) % 8;
    return headingUp()
      ? ["↑", "↗", "→", "↘", "↓", "↙", "←", "↖"][i]
      : ["N", "NE", "E", "SE", "S", "SW", "W", "NW"][i];
  };

  // Distances from an estimated position are rough.
  const approx = () => (snapshot?.selfPosEstimated ? "~" : "");

  const ago = (ms) => {
    const s = Math.max(0, (Date.now() - ms) / 1000);
    if (s < 90) return `${Math.round(s)}s`;
    if (s < 5400) return `${Math.round(s / 60)}m`;
    if (s < 172800) return `${Math.round(s / 3600)}h`;
    return `${Math.round(s / 86400)}d`;
  };

  // Remembered points of this map, plus live objects that are not on one (the
  // map is not known yet, so nothing has been remembered for them).
  const markers = () => {
    const pts = (snapshot.points || []).map((p) => ({
      code: p.code, kind: p.kind, pos: p.pos, live: p.live,
      lastSeenMs: p.lastSeenMs, lastDepletedMs: p.lastDepletedMs,
    }));
    const near = (a, b) => Math.hypot(a.x - b.x, a.y - b.y) < 200;
    for (const o of snapshot.objects) {
      if (!pts.some((p) => p.code === o.code && near(p.pos, o.pos))) {
        pts.push({ code: o.code, kind: o.kind, pos: o.pos, live: true, lastSeenMs: o.seenMs, lastDepletedMs: 0 });
      }
    }
    return pts;
  };

  const shown = (code) => (prefs.all || info(code)) && !prefs.hidden.includes(code);

  // Route: a loop through every point the radar shows (the ⚙ panel picks
  // which) within the range around where it was switched on — a farming
  // circuit that never runs out: after the last stop comes the first again.
  // The area stays put as we walk so the loop keeps its shape; the range
  // button, the ⚙ panel or a teleport plan a new one from where we stand.
  // Shortest closed tour by nearest neighbour, then 2-opt; a node seen live
  // that is not on it yet is slotted in where it costs least.
  //
  // A stop is done when we reach it (live) or come close enough to have seen
  // it spawn and it did not (objects enter view at ~57 m or more, see
  // position_estimate.rs) — someone gathered it. Done stops and points
  // depleted lately stay on the loop but are passed over for ROUTE_SKIP_MS:
  // respawn times are unknown. A node in view is always worth going to.
  // When every stop is passed over, the loop still leads on to the next one.
  const ROUTE_ABSENT_M = 45;
  const ROUTE_REACHED_M = 6;
  // A node in view closer than the next stop is gathered on the way, loop
  // order or not, in the area or not — up to this far (objects show up at
  // ~57 m, so this is about "in view").
  const ROUTE_DETOUR_M = 50;
  const ROUTE_SKIP_MS = 10 * 60 * 1000;
  // A jump this big between polls is a teleport: the old plan started
  // somewhere else.
  const ROUTE_TELEPORT_M = 200;
  // Planning time on a click: 1600 points converge in ~0.9 s, and stopping
  // at 0.3 s costs ~2% of the length.
  const ROUTE_BUDGET_MS = 300;
  // { map, center, loop: [{ key, code, pos, live, lastDepletedMs }], next,
  // detour }: `loop` in walking order, `next` the index we are heading for,
  // `detour` a node in view to take first (or null).
  let route = null;
  const routeDone = new Map(); // key -> when it was done
  let routeLastMe = null;

  const pointKey = (p) => `${p.code}:${Math.round(p.pos.x / 100)}:${Math.round(p.pos.y / 100)}`;
  const dist = (a, b) => Math.hypot(a.x - b.x, a.y - b.y);

  // Worth walking to: in view, or neither found empty nor gathered lately.
  const stopReady = (st, now) => {
    if (st.live) return true;
    const done = routeDone.get(st.key);
    if (done && now - done < ROUTE_SKIP_MS) return false;
    return !(st.lastDepletedMs > 0 && now - st.lastDepletedMs < ROUTE_SKIP_MS);
  };

  const routeArea = (center) => {
    const reach = prefs.range * 100;
    return markers()
      .filter((p) => shown(p.code) && dist(center, p.pos) <= reach)
      .map((p) => ({ key: pointKey(p), code: p.code, pos: p.pos, live: p.live, lastDepletedMs: p.lastDepletedMs }));
  };

  // Shortest closed tour through every stop.
  function planLoop(stops) {
    if (stops.length < 3) return stops.slice();
    const left = stops.slice();
    const path = [left.pop()];
    while (left.length) {
      const at = path[path.length - 1].pos;
      let bi = 0;
      let bd = Infinity;
      for (let i = 0; i < left.length; i++) {
        const d = dist(at, left[i].pos);
        if (d < bd) { bd = d; bi = i; }
      }
      path.push(left.splice(bi, 1)[0]);
    }
    // 2-opt: swap edges (i, i+1) and (j, j+1) for (i, j) and (i+1, j+1) by
    // reversing path[i+1..j], when that shortens the loop.
    const deadline = performance.now() + ROUTE_BUDGET_MS;
    const n = path.length;
    let improved = true;
    while (improved && performance.now() < deadline) {
      improved = false;
      for (let i = 0; i < n - 2 && performance.now() < deadline; i++) {
        for (let j = i + 2; j < n; j++) {
          if (i === 0 && j === n - 1) continue; // the same edge pair
          const a = path[i].pos;
          const b = path[i + 1].pos;
          const c = path[j].pos;
          const d = path[(j + 1) % n].pos;
          if (dist(a, c) + dist(b, d) < dist(a, b) + dist(c, d) - 1) {
            for (let l = i + 1, r = j; l < r; l++, r--) [path[l], path[r]] = [path[r], path[l]];
            improved = true;
          }
        }
      }
    }
    return path;
  }

  // Into the loop where it adds the least walking; returns its index.
  function insertStop(loop, stop) {
    if (loop.length < 2) { loop.push(stop); return loop.length - 1; }
    let best = 0;
    let bestCost = Infinity;
    for (let i = 0; i < loop.length; i++) {
      const a = loop[i].pos;
      const b = loop[(i + 1) % loop.length].pos;
      const cost = dist(a, stop.pos) + dist(stop.pos, b) - dist(a, b);
      if (cost < bestCost) { bestCost = cost; best = i + 1; }
    }
    loop.splice(best, 0, stop);
    return best;
  }

  // The nearest stop worth going to, else the nearest one.
  const nearestStop = (loop, me, now) => {
    let best = -1;
    let bestD = Infinity;
    for (const readyOnly of [true, false]) {
      loop.forEach((st, i) => {
        const d = dist(me, st.pos);
        if ((!readyOnly || stopReady(st, now)) && d < bestD) { bestD = d; best = i; }
      });
      if (best >= 0) break;
    }
    return Math.max(0, best);
  };

  function updateRoute() {
    const me = snapshot?.selfPos;
    if (!prefs.route || !me) {
      if (!prefs.route) route = null;
      return;
    }
    if (route && (route.map !== snapshot.map || (routeLastMe && dist(me, routeLastMe) > ROUTE_TELEPORT_M * 100))) {
      route = null;
    }
    routeLastMe = me;
    const now = Date.now();
    if (!route) {
      const loop = planLoop(routeArea(me));
      route = { map: snapshot.map, center: me, loop, next: nearestStop(loop, me, now), detour: null };
      return;
    }
    const { loop } = route;
    const area = routeArea(route.center);
    const byKey = new Map(area.map((a) => [a.key, a]));
    for (const st of loop) {
      const a = byKey.get(st.key);
      st.live = !!a?.live;
      st.lastDepletedMs = a ? a.lastDepletedMs : st.lastDepletedMs;
    }
    const onLoop = new Set(loop.map((st) => st.key));
    for (const a of area) {
      if (!a.live || onLoop.has(a.key)) continue;
      const at = insertStop(loop, a);
      if (loop.length > 1 && at <= route.next) route.next++;
    }
    if (!loop.length) return;
    route.next %= loop.length;

    const errM = snapshot.selfPosEstimated ? snapshot.selfPosErrorM || 0 : 0;
    let nextDone = false;
    let reachedOther = -1;
    loop.forEach((st, i) => {
      const dM = dist(me, st.pos) / 100;
      const done = st.live
        ? dM < ROUTE_REACHED_M + Math.min(errM, 5)
        : dM < Math.max(15, ROUTE_ABSENT_M - errM);
      if (!done) return;
      routeDone.set(st.key, now);
      if (i === route.next) nextDone = true;
      else if (st.live) reachedOther = i;
    });
    // Gone to another node than the next one (merely passing an empty point
    // does not count): the loop carries on from there.
    if (reachedOther >= 0 && !nextDone) {
      route.next = reachedOther;
      nextDone = true;
    }
    // Head for the next stop worth going to, in loop order. With none, move
    // on by one each time the current one is done, around and around.
    if (nextDone || !stopReady(loop[route.next], now)) {
      const n = loop.length;
      let k = -1;
      for (let step = 1; step <= n; step++) {
        const i = (route.next + step) % n;
        if (stopReady(loop[i], now)) { k = i; break; }
      }
      if (k >= 0 && !(k === route.next && nextDone)) route.next = k;
      else if (nextDone) route.next = (route.next + 1) % n;
    }

    // On the way: the nearest node in view that is closer than the next stop
    // and not just done.
    const nextD = dist(me, loop[route.next].pos);
    route.detour = null;
    let detourD = Math.min(nextD, ROUTE_DETOUR_M * 100);
    for (const p of markers()) {
      if (!p.live || !shown(p.code)) continue;
      const key = pointKey(p);
      if (key === loop[route.next].key) continue;
      const done = routeDone.get(key);
      if (done && now - done < ROUTE_SKIP_MS) continue;
      const d = dist(me, p.pos);
      if (d < detourD) {
        detourD = d;
        route.detour = { key, code: p.code, pos: p.pos, live: true, lastDepletedMs: p.lastDepletedMs };
      }
    }
  }

  // Loop stops from the next one on, in walking order.
  const routeAhead = () => {
    if (!route?.loop.length) return [];
    const { loop, next } = route;
    return loop.map((_, i) => loop[(next + i) % loop.length]);
  };

  // Visiting number of each stop worth going to: 1 = next.
  const routeNumbers = () => {
    const now = Date.now();
    const numbers = new Map();
    if (route?.detour) numbers.set(route.detour.key, 1);
    for (const st of routeAhead()) {
      if (numbers.has(st.key)) continue;
      if (stopReady(st, now) || !numbers.size) numbers.set(st.key, numbers.size + 1);
    }
    return numbers;
  };

  const loopLengthM = () => {
    const loop = route?.loop || [];
    let total = 0;
    for (let i = 0; i < loop.length && loop.length > 1; i++) total += dist(loop[i].pos, loop[(i + 1) % loop.length].pos);
    return total / 100;
  };

  const visibleObjects = () => {
    if (!snapshot?.selfPos) return [];
    const me = snapshot.selfPos;
    return markers()
      .filter((o) => shown(o.code))
      .map((o) => {
        const dx = o.pos.x - me.x;
        const dy = o.pos.y - me.y;
        const [sx, sy] = toScreen(dx, dy);
        return {
          ...o,
          sx, sy,
          distM: Math.hypot(dx, dy) / 100,
          dzM: (o.pos.z - me.z) / 100,
          info: info(o.code),
        };
      })
      // The range is what the radar shows: nothing beyond it, on the disc or
      // in the list.
      .filter((o) => o.distM <= prefs.range)
      .sort((a, b) => a.distM - b.distM);
  };

  function draw() {
    const dpr = window.devicePixelRatio || 1;
    const w = canvas.clientWidth;
    const h = canvas.clientHeight;
    if (canvas.width !== Math.round(w * dpr) || canvas.height !== Math.round(h * dpr)) {
      canvas.width = Math.round(w * dpr);
      canvas.height = Math.round(h * dpr);
    }
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, w, h);

    const cx = w / 2;
    const cy = h / 2;
    const radius = Math.max(10, Math.min(w, h) / 2 - 12);
    const pxPerM = radius / prefs.range;

    // Rings at quarter range, labelled.
    ctx.strokeStyle = "rgba(255,255,255,0.12)";
    ctx.fillStyle = "rgba(255,255,255,0.35)";
    ctx.font = "10px system-ui, sans-serif";
    ctx.lineWidth = 1;
    for (let i = 1; i <= 4; i++) {
      ctx.beginPath();
      ctx.arc(cx, cy, (radius * i) / 4, 0, Math.PI * 2);
      ctx.stroke();
      if (i % 2 === 0) ctx.fillText(`${Math.round((prefs.range * i) / 4)}`, cx + 3, cy - (radius * i) / 4 + 11);
    }
    ctx.beginPath();
    ctx.moveTo(cx - radius, cy); ctx.lineTo(cx + radius, cy);
    ctx.moveTo(cx, cy - radius); ctx.lineTo(cx, cy + radius);
    ctx.stroke();

    // "N" on the rim, wherever north-up's calibration says north is.
    const north = northWorld();
    if (north) {
      const [nx, ny] = toScreen(north[0], north[1]);
      const len = Math.hypot(nx, ny) || 1;
      ctx.fillStyle = "rgba(255,255,255,0.6)";
      ctx.fillText("N", cx + (nx / len) * (radius + 6) - 3, cy + (ny / len) * (radius + 6) + 4);
    }

    // Estimated position (no position from the server lately — any class but
    // a Chanter with its mantra up): a haze the size of the likely error.
    if (snapshot?.selfPosEstimated && snapshot.selfPosErrorM) {
      const r = Math.min(radius, snapshot.selfPosErrorM * pxPerM);
      ctx.fillStyle = "rgba(255,255,255,0.08)";
      ctx.strokeStyle = "rgba(255,255,255,0.3)";
      ctx.setLineDash([3, 3]);
      ctx.beginPath();
      ctx.arc(cx, cy, r, 0, Math.PI * 2);
      ctx.fill();
      ctx.stroke();
      ctx.setLineDash([]);
    }

    if (route?.loop.length && snapshot?.selfPos) drawRoute(cx, cy, radius, pxPerM);

    // Self: an arrow along the facing when it is known, else a dot.
    ctx.fillStyle = "#fff";
    ctx.beginPath();
    if (shownHeading != null) {
      const h = (shownHeading * Math.PI) / 180;
      const [fx, fy] = toScreen(Math.cos(h), Math.sin(h));
      const a = Math.atan2(fy, fx);
      const tip = 8;
      const back = 5;
      ctx.moveTo(cx + Math.cos(a) * tip, cy + Math.sin(a) * tip);
      ctx.lineTo(cx + Math.cos(a + 2.5) * back, cy + Math.sin(a + 2.5) * back);
      ctx.lineTo(cx + Math.cos(a - 2.5) * back, cy + Math.sin(a - 2.5) * back);
      ctx.closePath();
    } else {
      ctx.arc(cx, cy, 3.5, 0, Math.PI * 2);
    }
    ctx.fill();

    const objs = visibleObjects();
    for (const o of objs) {
      const len = Math.hypot(o.sx, o.sy) || 1;
      const rPx = o.distM * pxPerM;
      const px = cx + (o.sx / len) * rPx;
      const py = cy + (o.sy / len) * rPx;
      const color = o.info?.color || UNKNOWN_COLOR;
      ctx.globalAlpha = o.live ? 1 : 0.7;
      ctx.fillStyle = color;
      ctx.strokeStyle = color;
      ctx.lineWidth = 1.5;
      ctx.beginPath();
      // Live: filled. Remembered: an outline.
      const size = o.live ? 5 : 4;
      if (o.info?.shape === "square") ctx.rect(px - size, py - size, size * 2, size * 2);
      else ctx.arc(px, py, size, 0, Math.PI * 2);
      if (o.live) ctx.fill();
      else ctx.stroke();
      ctx.lineWidth = 1;
      // No label right on top of the player, where they would pile up.
      if (o.live || o.distM > prefs.range * 0.08) {
        const label = o.info?.name || String(o.code);
        ctx.fillStyle = "rgba(255,255,255,0.85)";
        ctx.fillText(`${label} ${approx()}${Math.round(o.distM)}m`, px + 7, py + 3);
      }
      ctx.globalAlpha = 1;
    }

    renderList(objs);
    renderStatus(objs);
    renderRouteInfo();
  }

  // The loop, clipped to the disc: the leg from the player to the next stop
  // solid, the loop dashed, the stops worth going to numbered in order. A leg
  // leaving the disc shows which way to go.
  function drawRoute(cx, cy, radius, pxPerM) {
    const me = snapshot.selfPos;
    const at = (pos) => {
      const [sx, sy] = toScreen(pos.x - me.x, pos.y - me.y);
      return [cx + (sx / 100) * pxPerM, cy + (sy / 100) * pxPerM];
    };
    const ahead = routeAhead();
    const pts = ahead.map((st) => at(st.pos));
    ctx.save();
    ctx.beginPath();
    ctx.arc(cx, cy, radius, 0, Math.PI * 2);
    ctx.clip();
    ctx.lineJoin = "round";
    if (pts.length > 1) {
      ctx.strokeStyle = "rgba(90,169,255,0.45)";
      ctx.lineWidth = 1.5;
      ctx.setLineDash([6, 4]);
      ctx.beginPath();
      pts.forEach(([x, y], i) => (i === 0 ? ctx.moveTo(x, y) : ctx.lineTo(x, y)));
      ctx.closePath();
      ctx.stroke();
      ctx.setLineDash([]);
    }
    // Via the detour, if any: solid to it, thinner on to the loop.
    const head = route.detour ? at(route.detour.pos) : pts[0];
    ctx.strokeStyle = "#5aa9ff";
    if (route.detour) {
      ctx.lineWidth = 1.5;
      ctx.beginPath();
      ctx.moveTo(head[0], head[1]);
      ctx.lineTo(pts[0][0], pts[0][1]);
      ctx.stroke();
    }
    ctx.lineWidth = 2.5;
    ctx.beginPath();
    ctx.moveTo(cx, cy);
    ctx.lineTo(head[0], head[1]);
    ctx.stroke();
    // Where to go now: a ring around it.
    ctx.lineWidth = 1.5;
    ctx.beginPath();
    ctx.arc(head[0], head[1], 9, 0, Math.PI * 2);
    ctx.stroke();
    ctx.font = "bold 10px system-ui, sans-serif";
    ctx.fillStyle = "#5aa9ff";
    const numbers = routeNumbers();
    ahead.forEach((st, i) => {
      const n = numbers.get(st.key);
      if (!n || n > 9) return;
      const [x, y] = pts[i];
      if (Math.hypot(x - cx, y - cy) < radius - 6) ctx.fillText(String(n), x - 15, y - 6);
    });
    if (route.detour) ctx.fillText("1", head[0] - 15, head[1] - 6);
    ctx.restore();
  }

  function renderRouteInfo() {
    if (!prefs.route || !snapshot?.selfPos) {
      routeInfoEl.hidden = true;
      return;
    }
    routeInfoEl.hidden = false;
    if (!route?.loop.length) {
      routeInfoEl.textContent = `Route: no points within ${prefs.range} m yet`;
      return;
    }
    const me = snapshot.selfPos;
    const next = route.detour || route.loop[route.next];
    const dx = next.pos.x - me.x;
    const dy = next.pos.y - me.y;
    const [sx, sy] = toScreen(dx, dy);
    const km = loopLengthM() / 1000;
    const len = km >= 1 ? `${km.toFixed(1)} km` : `${Math.round(km * 1000)} m`;
    const now = Date.now();
    const ready = route.loop.filter((st) => stopReady(st, now)).length;
    const name = info(next.code)?.name || `#${next.code}`;
    routeInfoEl.textContent =
      `Loop ${route.loop.length} nodes (${ready} ready) · ${len} lap · next ${name} ` +
      `${direction(sx, sy)} ${approx()}${Math.round(Math.hypot(dx, dy) / 100)} m`;
  }

  function renderList(objs) {
    if (!objs.length) {
      listEl.innerHTML = `<div class="empty">${snapshot?.selfPos ? `Nothing within ${prefs.range} m` : ""}</div>`;
      return;
    }
    const numbers = route ? routeNumbers() : new Map();
    listEl.innerHTML = objs
      .slice(0, 12)
      .map((o) => {
        const color = o.info?.color || UNKNOWN_COLOR;
        const name = o.info ? esc(o.info.name) : `#${o.code} <span class="dim">k${o.kind}</span>`;
        const dz = Math.abs(o.dzM) >= 5 ? ` ${o.dzM > 0 ? "▲" : "▼"}${Math.round(Math.abs(o.dzM))}` : "";
        // In view, or when it was last seen / last seen gathered out.
        const stop = numbers.get(pointKey(o));
        const badge = stop ? `<span class="stop">${stop}</span>` : "";
        const state = o.live
          ? "live"
          : o.lastDepletedMs >= o.lastSeenMs && o.lastDepletedMs > 0
            ? `gathered ${ago(o.lastDepletedMs)} ago`
            : `seen ${ago(o.lastSeenMs)} ago`;
        const dot = (o.live
          ? `background:${color}`
          : `border:1.5px solid ${color}`) +
          (o.info?.shape === "square" ? ";border-radius:1px" : "");
        return `<div class="row" data-code="${o.code}"><span class="dot" style="${dot}"></span>` +
          `<span class="name">${badge}${name} <span class="dim">${state}</span></span>` +
          `<span class="dim">${direction(o.sx, o.sy)}${dz}</span>` +
          `<span>${approx()}${Math.round(o.distM)} m</span></div>`;
      })
      .join("");
  }

  function renderStatus() {
    if (!invoke) {
      statusEl.textContent = "No backend";
    } else if (!snapshot) {
      statusEl.textContent = "Waiting for data…";
    } else if (!snapshot.selfPos) {
      // Capture often works without admin too (it depends on how Npcap was
      // installed), so only blame it while nothing is coming in.
      statusEl.textContent = isAdmin ? "Waiting for your position…" : "No data yet — try running as administrator";
    } else if (snapshot.map == null) {
      statusEl.textContent = "Map unknown — saved points show once it is recognised";
    } else if (snapshot.selfPosEstimated) {
      // Exact again on any buff on yourself, a skill or a dodge.
      statusEl.textContent = `Estimated ±${Math.round(snapshot.selfPosErrorM || 0)} m — dodge or use a skill to fix it`;
    } else if (Date.now() - snapshot.selfPosMs > STALE_MS) {
      statusEl.textContent = `Position ${Math.round((Date.now() - snapshot.selfPosMs) / 1000)} s old`;
    } else {
      statusEl.textContent = "";
    }
  }

  async function poll() {
    try {
      snapshot = await invoke("get_radar");
    } catch {
      snapshot = null;
    }
    const target = snapshot?.selfHeading;
    if (target == null) {
      shownHeading = null;
      turn = null;
    } else if (shownHeading == null) {
      shownHeading = target;
    } else if (target !== lastTarget) {
      // A new facing: turn to it quickly. The server sends facing only once a
      // second, so the disc already trails the character by up to that much;
      // spreading the turn over the gap (tried first) added up to another
      // second on top and drifted out of sync while turning — disorienting.
      turn = { from: shownHeading, delta: ((target - shownHeading + 540) % 360) - 180, start: performance.now() };
    }
    lastTarget = target;
    updateRoute();
    draw();
  }

  // Facing interpolation (see poll): where the current turn started and how
  // far it goes. Tuned in game: 200 ms ease-out felt abrupt, the whole ~1 s
  // gap lagged into dizziness; 450 ms easing in and out sat between but
  // felt slow to align (2026-10-21), so 300 ms.
  let turn = null;
  const TURN_MS = 300;
  let lastTarget = null;

  function animate(now) {
    if (turn) {
      const t = Math.min(1, (now - turn.start) / TURN_MS);
      const eased = (1 - Math.cos(Math.PI * t)) / 2;
      shownHeading = (turn.from + turn.delta * eased + 360) % 360;
      if (t >= 1) turn = null;
      draw();
    }
    requestAnimationFrame(animate);
  }

  // Label editor: click a row in the list to name its code. The list is
  // rebuilt on every poll, so this listens on mousedown (a click would need
  // the same element under both press and release).
  const editor = document.getElementById("editor");
  const editorCode = document.getElementById("editorCode");
  const editorName = document.getElementById("editorName");
  const editorSwatches = document.getElementById("editorSwatches");
  const editorShape = document.getElementById("editorShape");
  let editing = null;

  const paintEditor = () => {
    editorSwatches.innerHTML = SWATCHES.map((c) =>
      `<button class="swatch${c === editing.color ? " on" : ""}" data-color="${c}" style="background:${c}"></button>`
    ).join("");
    editorShape.textContent = editing.shape === "square" ? "■ square" : "● dot";
  };

  listEl.addEventListener("mousedown", (e) => {
    const row = e.target.closest(".row");
    if (!row || e.button !== 0) return;
    const code = Number(row.dataset.code);
    const current = info(code);
    editing = {
      code,
      color: current?.color || SWATCHES[0],
      shape: current?.shape === "square" ? "square" : "dot",
    };
    editorCode.textContent = `#${code}`;
    editorName.value = current?.name || "";
    paintEditor();
    editor.hidden = false;
    editorName.focus();
  });
  editorSwatches.addEventListener("click", (e) => {
    const c = e.target.closest(".swatch")?.dataset.color;
    if (c) { editing.color = c; paintEditor(); }
  });
  editorShape.addEventListener("click", () => {
    editing.shape = editing.shape === "square" ? "dot" : "square";
    paintEditor();
  });
  const saveLabel = async (label) => {
    try {
      labels = await invoke("set_radar_label", { code: editing.code, label });
    } catch (err) {
      console.error("[radar] set_radar_label", err);
    }
    editor.hidden = true;
    if (!filterPanel.hidden) renderFilter();
    draw();
  };
  document.getElementById("editorSave").addEventListener("click", () => {
    const name = editorName.value.trim();
    if (!name) return;
    saveLabel({ name, color: editing.color, shape: editing.shape });
  });
  document.getElementById("editorRemove").addEventListener("click", () => saveLabel(null));
  document.getElementById("editorCancel").addEventListener("click", () => { editor.hidden = true; });
  editorName.addEventListener("keydown", (e) => {
    if (e.key === "Enter") document.getElementById("editorSave").click();
    if (e.key === "Escape") editor.hidden = true;
  });

  // ⚙ panel: every registered marker (built-in + named), each with a box to
  // show or hide it on the radar.
  const filterBtn = document.getElementById("filterBtn");
  const filterPanel = document.getElementById("filterPanel");
  const filterList = document.getElementById("filterList");

  const registered = () => {
    const codes = new Set([...Object.keys(KNOWN), ...Object.keys(labels)].map(Number));
    return [...codes]
      .map((code) => ({ code, ...info(code) }))
      .sort((a, b) => a.name.localeCompare(b.name));
  };

  const renderFilter = () => {
    filterList.innerHTML = registered()
      .map((m) => {
        const radius = m.shape === "square" ? "1px" : "50%";
        return `<label class="frow"><input type="checkbox" data-code="${m.code}"` +
          `${prefs.hidden.includes(m.code) ? "" : " checked"} />` +
          `<span class="dot" style="background:${m.color};border-radius:${radius}"></span>` +
          `<span class="name">${esc(m.name)}</span><span class="dim">#${m.code}</span></label>`;
      })
      .join("");
  };

  filterBtn.addEventListener("click", () => {
    filterPanel.hidden = !filterPanel.hidden;
    filterBtn.classList.toggle("on", !filterPanel.hidden);
    if (!filterPanel.hidden) renderFilter();
  });
  filterList.addEventListener("change", (e) => {
    const code = Number(e.target.dataset.code);
    if (!code) return;
    prefs.hidden = prefs.hidden.filter((c) => c !== code);
    if (!e.target.checked) prefs.hidden.push(code);
    route = null;
    savePrefs(); draw();
  });
  document.getElementById("filterAll").addEventListener("click", () => {
    prefs.hidden = [];
    route = null;
    savePrefs(); renderFilter(); draw();
  });
  document.getElementById("filterNone").addEventListener("click", () => {
    prefs.hidden = registered().map((m) => m.code);
    route = null;
    savePrefs(); renderFilter(); draw();
  });

  syncButtons();
  window.addEventListener("resize", draw);
  draw();
  if (invoke) {
    loadLabels().then(draw);
    loadPrefs().then(() => { syncButtons(); draw(); });
    invoke("is_admin").then((v) => { isAdmin = v !== false; draw(); }).catch(() => {});
    poll();
    setInterval(poll, POLL_MS);
    requestAnimationFrame(animate);
  }
})();
