// The Hub window (A2_VIEW "hub", the Abyss Hub build): switches each
// widget — the meter overlay, the Radar — on and off, and holds their
// settings. The DPS Meter tab is the settings panel this bundle already has
// (core.js enterHubWindowMode shows it); the Radar tab edits
// radar_prefs.json, which the Radar window applies as it changes.
window.A2Hub = (() => {
  const invoke = window.__TAURI__?.core?.invoke;
  const listen = window.__TAURI__?.event?.listen;
  const t = (key, fallback) => window.i18n?.t?.(key, fallback) || fallback;
  const esc = (text) => String(text).replace(/[&<>"']/g, (c) => `&#${c.charCodeAt(0)};`);
  const catalog = window.A2RadarCatalog;

  const WIDGETS = [
    { id: "meter", icon: "gauge", name: ["hub.meter.name", "DPS Meter"],
      desc: ["hub.meter.desc", "Damage per second for you and your party, over the game."] },
    { id: "radar", icon: "radar", name: ["hub.radar.name", "Radar"],
      desc: ["hub.radar.desc", "Gather nodes, Hidden Cubes and players around you."] },
    // No settings of its own: no tab.
    { id: "earnings", icon: "coins", name: ["hub.earnings.name", "Earnings"], tab: false,
      desc: ["hub.earnings.desc", "Kinah and Abyss Points earned, with the average per minute and the last minute."] },
  ];
  const TABS = ["widgets", ...WIDGETS.filter((w) => w.tab !== false).map((w) => w.id)];
  const TAB_KEY = "hub.tab";

  let enabled = {};
  let prefs = structuredClone(catalog.DEFAULT_PREFS);
  let labels = {};
  let cards;
  let radarPane;

  // The app icon (src-tauri/icons-hub/source.svg), inline: an <img> path
  // would need the file in the build, where Vite renames assets.
  const LOGO = `<svg viewBox="32 32 960 960" aria-hidden="true">
    <defs><linearGradient id="hubRing" x1="0" y1="0" x2="1" y2="1">
      <stop offset="0" stop-color="#7fd0ff"/><stop offset="0.5" stop-color="#7a6bff"/><stop offset="1" stop-color="#c04dff"/>
    </linearGradient></defs>
    <rect x="32" y="32" width="960" height="960" rx="210" fill="#10142a"/>
    <circle cx="512" cy="512" r="262" fill="#0b0920" stroke="url(#hubRing)" stroke-width="72"/>
    <g stroke="#cfe6ff" stroke-opacity="0.55" stroke-width="20" stroke-linecap="round">
      <line x1="512" y1="512" x2="512" y2="374"/><line x1="512" y1="512" x2="392" y2="581"/><line x1="512" y1="512" x2="632" y2="581"/>
    </g>
    <circle cx="512" cy="512" r="58" fill="#fff"/><circle cx="512" cy="360" r="40" fill="#7fd0ff"/>
    <circle cx="380" cy="588" r="40" fill="#ffcc33"/><circle cx="644" cy="588" r="40" fill="#3ddc84"/>
  </svg>`;

  const shell = () => `
    <aside class="hubSide">
      <div class="hubBrand">${LOGO}<span>Abyss Hub</span></div>
      <nav class="hubNav">
        <button type="button" data-tab="widgets"><i data-lucide="layout-grid"></i><span data-i18n="hub.tabs.widgets">Widgets</span></button>
        ${WIDGETS.filter((w) => w.tab !== false).map((w) => `<button type="button" data-tab="${w.id}"><i data-lucide="${w.icon}"></i><span data-i18n="${w.name[0]}">${w.name[1]}</span></button>`).join("")}
      </nav>
    </aside>
    <section class="hubPane" data-pane="widgets">
      <h1 data-i18n="hub.tabs.widgets">Widgets</h1>
      <p class="hubHint" data-i18n="hub.widgets.hint">Overlays shown over the game. Switch one on to open it; its settings are in its own tab.</p>
      <div class="hubCards"></div>
    </section>
    <section class="hubPane" data-pane="radar">
      <h1 data-i18n="hub.radar.name">Radar</h1>
      <div class="hubRadar"></div>
    </section>`;

  const setTab = (tab) => {
    if (!TABS.includes(tab)) tab = "widgets";
    document.documentElement.dataset.hubTab = tab;
    document.querySelectorAll(".hubNav button").forEach((b) => b.classList.toggle("on", b.dataset.tab === tab));
    try { localStorage.setItem(TAB_KEY, tab); } catch {}
    if (tab === "radar") loadRadar();
  };

  // --- Widgets ---------------------------------------------------------

  const renderCards = () => {
    cards.innerHTML = WIDGETS.map((w) => `
      <div class="hubCard${enabled[w.id] ? " on" : ""}">
        <div class="hubCardIcon"><i data-lucide="${w.icon}"></i></div>
        <div class="hubCardText">
          <div class="hubCardName">${esc(t(...w.name))}</div>
          <div class="hubCardDesc">${esc(t(...w.desc))}</div>
        </div>
        ${w.tab === false ? "" : `<button type="button" class="hubLink" data-goto="${w.id}">${esc(t("hub.settings", "Settings"))}</button>`}
        <label class="hubSwitch" title="${esc(enabled[w.id] ? t("hub.turnOff", "Turn off") : t("hub.turnOn", "Turn on"))}">
          <input type="checkbox" data-widget="${w.id}"${enabled[w.id] ? " checked" : ""} /><span></span>
        </label>
      </div>`).join("");
    window.lucide?.createIcons?.();
  };

  const loadWidgets = async () => {
    try {
      const list = (await invoke("get_hub_widgets")) || [];
      enabled = Object.fromEntries(list.map((w) => [w.id, w.enabled]));
    } catch {}
    renderCards();
  };

  // --- Radar -----------------------------------------------------------

  const saveRadar = () => {
    invoke?.("set_radar_prefs", { prefs }).catch(() => {});
  };

  const loadRadar = async () => {
    try {
      const [saved, savedLabels] = await Promise.all([invoke("get_radar_prefs"), invoke("get_radar_labels")]);
      prefs = { ...structuredClone(catalog.DEFAULT_PREFS), ...catalog.upgradePrefs(saved) };
      labels = savedLabels || {};
    } catch {}
    renderRadar();
  };

  const check = (key, label) =>
    `<label class="hubRow"><input type="checkbox" data-pref="${key}"${prefs[key] ? " checked" : ""} /><span>${esc(label)}</span></label>`;
  const select = (key, label, options) =>
    `<label class="hubRow hubRowSelect"><span>${esc(label)}</span><select data-pref="${key}">` +
    options.map(([value, text]) => `<option value="${value}"${String(prefs[key]) === String(value) ? " selected" : ""}>${esc(text)}</option>`).join("") +
    `</select></label>`;

  const objects = () => {
    const codes = new Set([...Object.keys(catalog.KNOWN), ...Object.keys(labels)].map(Number));
    return [...codes]
      .map((code) => ({ code, ...(labels[String(code)] || catalog.KNOWN[code]) }))
      .filter((m) => m.name)
      .sort((a, b) => a.name.localeCompare(b.name));
  };

  const renderRadar = () => {
    if (!radarPane) return;
    const ranges = [25, 35, 50, 70, 100, 140, 200, 280, 400, 560, 800, 1100, 1600];
    if (!ranges.includes(prefs.range)) ranges.push(prefs.range);
    radarPane.innerHTML = `
      <div class="hubGroup">
        <div class="hubGroupTitle">${esc(t("hub.radar.players", "Players"))}</div>
        ${check("showParty", t("hub.radar.party", "Party members"))}
        ${check("showAllies", t("hub.radar.allies", "Allies"))}
        ${check("showEnemies", t("hub.radar.enemies", "Enemies"))}
        ${select("myFaction", t("hub.radar.faction", "Your faction (when not in a party)"), [
          ["", t("hub.radar.factionUnset", "Not set")], [1, "Elyos"], [2, "Asmodian"]])}
      </div>
      <div class="hubGroup">
        <div class="hubGroupTitle">${esc(t("hub.radar.view", "View"))}</div>
        ${select("headingUp", t("hub.radar.orientation", "Orientation"), [
          ["true", t("hub.radar.facingUp", "Facing up")], ["false", t("hub.radar.northUp", "North up")]])}
        ${select("range", t("hub.radar.range", "Range"), ranges.sort((a, b) => a - b).map((m) => [m, `${m} m`]))}
        ${check("headingMirror", t("hub.radar.mirrorFacing", "Mirror left/right (facing up)"))}
        ${check("mirror", t("hub.radar.mirrorNorth", "Mirror left/right (north up)"))}
        ${select("rotate", t("hub.radar.rotate", "Rotation (north up)"), [0, 90, 180, 270].map((d) => [d, `${d}°`]))}
        ${check("all", t("hub.radar.unknown", "Unknown objects, with their code"))}
      </div>
      <div class="hubGroup">
        <div class="hubGroupTitle">${esc(t("hub.radar.objects", "Objects"))}
          <span class="grow"></span>
          <button type="button" class="hubLink" data-objects="all">${esc(t("hub.radar.all", "All"))}</button>
          <button type="button" class="hubLink" data-objects="none">${esc(t("hub.radar.none", "None"))}</button>
        </div>
        <div class="hubObjects">
          ${objects().map((m) => `<label class="hubRow"><input type="checkbox" data-code="${m.code}"${prefs.hidden.includes(m.code) ? "" : " checked"} />` +
            `<span class="hubDot" style="background:${m.color || catalog.UNKNOWN_COLOR};border-radius:${m.shape === "square" ? "2px" : "50%"}"></span>` +
            `<span>${esc(m.name)}</span><span class="hubDim">#${m.code}</span></label>`).join("")}
        </div>
        <p class="hubHint">${esc(t("hub.radar.namesHint", "Name an object by clicking its row in the radar's list; saved points are in the radar's ⚙ panel."))}</p>
      </div>`;
  };

  const onRadarChange = (e) => {
    const el = e.target;
    if (el.dataset.code) {
      const code = Number(el.dataset.code);
      prefs.hidden = prefs.hidden.filter((c) => c !== code);
      if (!el.checked) prefs.hidden.push(code);
    } else if (el.dataset.pref) {
      const key = el.dataset.pref;
      if (el.type === "checkbox") prefs[key] = el.checked;
      else if (key === "headingUp") prefs[key] = el.value === "true";
      else if (key === "myFaction") prefs[key] = el.value ? Number(el.value) : null;
      else prefs[key] = Number(el.value);
    } else return;
    saveRadar();
  };

  const onRadarClick = (e) => {
    const which = e.target.closest("[data-objects]")?.dataset.objects;
    if (!which) return;
    prefs.hidden = which === "all" ? [] : objects().map((m) => m.code);
    saveRadar();
    renderRadar();
  };

  // --- Start -----------------------------------------------------------

  const init = () => {
    if (!invoke) return;
    document.body.insertAdjacentHTML("afterbegin", shell());
    cards = document.querySelector(".hubCards");
    radarPane = document.querySelector(".hubRadar");
    // i18n has already translated the page by now; later language changes
    // reach these through its own data-i18n pass.
    document.querySelectorAll(".hubSide [data-i18n], .hubPane [data-i18n]").forEach((el) => {
      el.textContent = t(el.dataset.i18n, el.textContent);
    });
    window.lucide?.createIcons?.();

    document.querySelector(".hubNav").addEventListener("click", (e) => {
      const tab = e.target.closest("[data-tab]")?.dataset.tab;
      if (tab) setTab(tab);
    });
    cards.addEventListener("click", (e) => {
      const tab = e.target.closest("[data-goto]")?.dataset.goto;
      if (tab) setTab(tab);
    });
    cards.addEventListener("change", (e) => {
      const id = e.target.dataset.widget;
      if (!id) return;
      enabled[id] = e.target.checked;
      renderCards();
      invoke("set_hub_widget", { id, enabled: e.target.checked }).catch(() => loadWidgets());
    });
    radarPane.addEventListener("change", onRadarChange);
    radarPane.addEventListener("click", onRadarClick);

    listen?.("hub-widgets-changed", loadWidgets);
    listen?.("hub-show-tab", ({ payload }) => setTab(payload));
    // Changes made in the Radar window (zoom, the ⚙ panel) show up here too.
    listen?.("radar-prefs-changed", ({ payload }) => {
      if (!payload?.prefs || payload.source === "hub") return;
      prefs = { ...structuredClone(catalog.DEFAULT_PREFS), ...catalog.upgradePrefs(payload.prefs) };
      // Not while a dropdown is open: rebuilding it would close it.
      if (!radarPane.contains(document.activeElement) || document.activeElement.type === "checkbox") renderRadar();
    });
    window.i18n?.onChange?.(() => { renderCards(); renderRadar(); });

    let tab = "widgets";
    try { tab = localStorage.getItem(TAB_KEY) || tab; } catch {}
    setTab(tab);
    loadWidgets();
    loadRadar();
  };

  return { init };
})();
