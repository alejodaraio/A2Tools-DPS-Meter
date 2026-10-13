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
  const POLL_MS = 250;
  const STALE_MS = 5000;

  // headingMirror defaults on: in-game, facing-up only matched with the
  // mirror on. `v` bumps re-apply a changed default once over saved prefs.
  const PREFS_VERSION = 2;
  // `hidden`: codes unticked in the ⚙ panel.
  const prefs = { range: 100, rotate: 0, mirror: false, all: false, headingUp: true, headingMirror: true, hidden: [] };
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
  };

  rangeBtn.addEventListener("click", () => {
    const i = RANGES_M.indexOf(prefs.range);
    prefs.range = RANGES_M[(i + 1) % RANGES_M.length];
    savePrefs(); syncButtons(); draw();
  });
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

  const visibleObjects = () => {
    if (!snapshot?.selfPos) return [];
    const me = snapshot.selfPos;
    return markers()
      .filter((o) => (prefs.all || info(o.code)) && !prefs.hidden.includes(o.code))
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
      if (i % 2 === 0) ctx.fillText(`${(prefs.range * i) / 4}`, cx + 3, cy - (radius * i) / 4 + 11);
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
  }

  function renderList(objs) {
    if (!objs.length) {
      listEl.innerHTML = `<div class="empty">${snapshot?.selfPos ? `Nothing within ${prefs.range} m` : ""}</div>`;
      return;
    }
    listEl.innerHTML = objs
      .slice(0, 12)
      .map((o) => {
        const color = o.info?.color || UNKNOWN_COLOR;
        const name = o.info ? esc(o.info.name) : `#${o.code} <span class="dim">k${o.kind}</span>`;
        const dz = Math.abs(o.dzM) >= 5 ? ` ${o.dzM > 0 ? "▲" : "▼"}${Math.round(Math.abs(o.dzM))}` : "";
        // In view, or when it was last seen / last seen gathered out.
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
          `<span class="name">${name} <span class="dim">${state}</span></span>` +
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
    draw();
  }

  // Facing interpolation (see poll): where the current turn started and how
  // far it goes. Tuned in game: 200 ms ease-out felt abrupt, the whole ~1 s
  // gap lagged into dizziness; 450 ms easing in and out sits between.
  let turn = null;
  const TURN_MS = 450;
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
    savePrefs(); draw();
  });
  document.getElementById("filterAll").addEventListener("click", () => {
    prefs.hidden = [];
    savePrefs(); renderFilter(); draw();
  });
  document.getElementById("filterNone").addEventListener("click", () => {
    prefs.hidden = registered().map((m) => m.code);
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
