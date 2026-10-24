// Earnings widget: kinah / Abyss Points earned since the counter started
// (`get_earnings`; the radar tracker counts every rise of the inventory
// totals, see `inventory_changed` in radar.rs), the average per minute over
// that time, and what came in during the last minute.
(() => {
  const invoke = window.__TAURI__?.core?.invoke;
  const POLL_MS = 500;
  // Earned totals over the last minute, for the "last min" figure.
  const WINDOW_MS = 60_000;
  const samples = [];

  const rowsEl = document.getElementById("rows");
  const sinceEl = document.getElementById("since");

  const fmtNum = (n) => n.toLocaleString("en-US");
  const fmtShort = (n) => (n >= 1e6 ? `${(n / 1e6).toFixed(1)}M` : n >= 1e4 ? `${Math.round(n / 1e3)}k` : String(Math.round(n)));

  const row = (cls, label, earned, perMin, lastMin) =>
    `<span class="label ${cls}">${label}</span><span class="value">+${fmtNum(earned)}</span>` +
    `<span class="rate">avg ${fmtShort(perMin)}/min · last min +${fmtShort(lastMin)}</span>`;

  function render(e) {
    if (!e?.sinceMs) {
      samples.length = 0;
      sinceEl.textContent = "";
      rowsEl.innerHTML = `<span class="waiting">Counting from the next kinah or AP you get.</span>`;
      return;
    }
    const now = Date.now();
    const last = samples[samples.length - 1];
    // Lower than before: the counter was reset.
    if (last && (e.kinah < last.k || e.abyssPoints < last.a)) samples.length = 0;
    samples.push({ t: now, k: e.kinah, a: e.abyssPoints });
    while (samples.length > 1 && samples[1].t <= now - WINDOW_MS) samples.shift();
    const base = samples[0];
    const minutes = Math.max(now - e.sinceMs, 60_000) / 60_000;
    rowsEl.innerHTML =
      row("kinah", "Kinah", e.kinah, e.kinah / minutes, e.kinah - base.k) +
      row("ap", "AP", e.abyssPoints, e.abyssPoints / minutes, e.abyssPoints - base.a);
    const min = Math.floor((now - e.sinceMs) / 60_000);
    sinceEl.textContent = min >= 60 ? `${Math.floor(min / 60)} h ${min % 60} min` : `${min} min`;
  }

  async function poll() {
    let e = null;
    try { e = await invoke("get_earnings"); } catch {}
    render(e);
  }

  document.getElementById("resetBtn").addEventListener("click", async () => {
    try { await invoke("reset_radar_earnings"); } catch {}
    poll();
  });
  document.getElementById("closeBtn").addEventListener("click", () => {
    invoke?.("close_tool_window");
  });
  // Frameless window: drag from the header.
  document.querySelector(".header").addEventListener("mousedown", (e) => {
    if (e.button !== 0 || e.target.closest("button")) return;
    invoke?.("start_drag");
  });

  render(null);
  if (invoke) {
    poll();
    setInterval(poll, POLL_MS);
  }
})();
