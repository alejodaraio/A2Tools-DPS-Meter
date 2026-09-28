// Personal bests: your best active-time DPS against each NPC, per character,
// from the saved fight summaries (FightSummary.localName / localDps /
// localActiveMs, computed in FightRecord::local_stats).
(function initPersonalBests(global) {
  // A fight only counts with at least this much active time: a couple of hits
  // inside one second read as their whole damage per second (seen in real
  // history: a 2-hit "72k DPS" against the melee scarecrow).
  const MIN_ACTIVE_MS = 10000;

  const eligible = (fight) =>
    !!fight &&
    !fight.isLive &&
    Number(fight.mobCode) > 0 &&
    typeof fight.localName === "string" &&
    fight.localName !== "" &&
    Number(fight.localDps) > 0 &&
    Number(fight.localActiveMs) >= MIN_ACTIVE_MS;

  const keyOf = (fight) => `${fight.mobCode}|${fight.localName}`;

  // Best fight per NPC + character, as a Map keyed "mobCode|name".
  const bestByKey = (fights) => {
    const best = new Map();
    (Array.isArray(fights) ? fights : []).forEach((fight) => {
      if (!eligible(fight)) return;
      const current = best.get(keyOf(fight));
      if (!current || Number(fight.localDps) > Number(current.localDps)) best.set(keyOf(fight), fight);
    });
    return best;
  };

  // The best saved fight for this NPC and character, or null.
  const best = (fights, mobCode, name) =>
    bestByKey(fights).get(`${Number(mobCode)}|${name}`) || null;

  // Ids of the fights that currently hold a personal best.
  const bestIds = (fights) => new Set([...bestByKey(fights).values()].map((f) => f.id));

  global.personalBests = { MIN_ACTIVE_MS, best, bestIds };
})(window);
