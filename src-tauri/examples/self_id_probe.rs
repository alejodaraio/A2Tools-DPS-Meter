//! Investigation tool: which packets carry the local player's entity id?
//!
//! The meter binds "who am I" from the `33 36` self record, which only arrives
//! on zone load. This walks a raw packet log (same framing + LZ4 bundles as
//! `StreamProcessor::consume_stream`) and reports, per opcode, how often the
//! given entity id shows up — to find a self-only packet that arrives often.
//!
//!   cargo run --example self_id_probe -- <packets.txt> selfrecords
//!   cargo run --example self_id_probe -- <packets.txt> probe <id> [from_ts] [to_ts]

use std::collections::{BTreeMap, HashMap, HashSet};

fn varint(b: &[u8], at: usize) -> Option<(u32, usize)> {
    let (mut v, mut shift, mut n) = (0u32, 0, 0);
    loop {
        let x = *b.get(at + n)? as u32;
        n += 1;
        v |= (x & 0x7F) << shift;
        if x & 0x80 == 0 {
            return Some((v, n));
        }
        shift += 7;
        if shift >= 32 {
            return None;
        }
    }
}

fn enc_varint(mut v: u32) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let b = (v & 0x7F) as u8;
        v >>= 7;
        if v == 0 {
            out.push(b);
            return out;
        }
        out.push(b | 0x80);
    }
}

fn decode_hex(hex: &str) -> Option<Vec<u8>> {
    if hex.len() % 2 != 0 {
        return None;
    }
    (0..hex.len()).step_by(2).map(|i| u8::from_str_radix(&hex[i..i + 2], 16).ok()).collect()
}

/// Split a stream buffer into packets; returns bytes consumed.
fn frame(buf: &[u8], out: &mut Vec<Vec<u8>>) -> usize {
    let mut off = 0;
    while off < buf.len() {
        if buf[off] == 0 {
            off += 1;
            continue;
        }
        let Some((len, llen)) = varint(buf, off) else {
            if off + 5 > buf.len() { break; }
            off += 1;
            continue;
        };
        if len <= 3 {
            off += 1;
            continue;
        }
        let total = (len - 3) as usize;
        if total > 65535 {
            off += 1;
            continue;
        }
        if off + total > buf.len() {
            if total > 16384 { off += 1; continue; }
            break;
        }
        let is_bundle = llen + 1 < total && buf[off + llen] == 0xFF && buf[off + llen + 1] == 0xFF;
        if is_bundle {
            let size = total + 1;
            if off + size > buf.len() { break; }
            unbundle(&buf[off + llen..off + size], out);
            off += size;
        } else {
            out.push(buf[off..off + total].to_vec());
            off += total;
        }
    }
    off
}

fn unbundle(p: &[u8], out: &mut Vec<Vec<u8>>) {
    if p.len() < 7 { return; }
    let size = u32::from_le_bytes([p[2], p[3], p[4], p[5]]) as usize;
    if size == 0 || size > 1_000_000 { return; }
    let Ok(d) = lz4_flex::decompress(&p[6..], size) else { return };
    let mut off = 0;
    while off < d.len() {
        if d[off] == 0 { off += 1; continue; }
        let Some((len, llen)) = varint(&d, off) else { break };
        if len <= 3 { off += 1; continue; }
        let end = off + (len - 3) as usize;
        if end > d.len() { break; }
        let inner = &d[off..end];
        if inner.len() > llen + 1 && inner[llen] == 0xFF && inner[llen + 1] == 0xFF {
            unbundle(&inner[llen..], out);
        } else {
            out.push(inner.to_vec());
        }
        off = end;
    }
}

struct Pkt { ts: String, key: String, data: Vec<u8> }

fn packets(path: &str) -> Vec<Pkt> {
    let text = std::fs::read_to_string(path).expect("read capture");
    let mut bufs: HashMap<String, Vec<u8>> = HashMap::new();
    let mut all = Vec::new();
    for line in text.lines() {
        if line.is_empty() || line.starts_with('#') { continue; }
        let mut it = line.splitn(3, '|');
        let (Some(ts), Some(key), Some(hex)) = (it.next(), it.next(), it.next()) else { continue };
        let Some(bytes) = decode_hex(hex) else { continue };
        let buf = bufs.entry(key.to_string()).or_default();
        buf.extend_from_slice(&bytes);
        let mut out = Vec::new();
        let used = frame(buf, &mut out);
        buf.drain(..used);
        for data in out {
            all.push(Pkt { ts: ts.to_string(), key: key.to_string(), data });
        }
    }
    all
}

fn opcode(p: &[u8]) -> Option<(u8, u8, usize)> {
    let (_, l) = varint(p, 0)?;
    Some((*p.get(l)?, *p.get(l + 1)?, l + 2))
}

fn ms(ts: &str) -> i64 {
    chrono::DateTime::parse_from_rfc3339(ts).map(|d| d.timestamp_millis()).unwrap_or(0)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path = &args[1];
    let pk = packets(path);
    eprintln!("{} packets", pk.len());

    match args[2].as_str() {
        "selfrecords" => {
            // Scan for 33 36 <varint id> anywhere (the same scan the app does).
            for p in &pk {
                let d = &p.data;
                for i in 0..d.len().saturating_sub(8) {
                    if d[i] == 0x33 && d[i + 1] == 0x36 {
                        if let Some((id, l)) = varint(d, i + 2) {
                            let m2 = i + 2 + l + 4;
                            if (1..10_000_000).contains(&id) && m2 + 1 < d.len() && d[m2] & 1 == 1 {
                                let n = d[m2 + 1] as usize;
                                if let Some(name) = d.get(m2 + 2..m2 + 2 + n).and_then(|s| std::str::from_utf8(s).ok()) {
                                    println!("{} {} self record id={} name={:?}", p.ts, p.key, id, name);
                                }
                            }
                        }
                    }
                }
            }
        }
        "probe" => {
            let id: u32 = args[3].parse().unwrap();
            let from = args.get(4).map(|s| s.as_str()).unwrap_or("");
            let to = args.get(5).map(|s| s.as_str()).unwrap_or("~");
            let needle = enc_varint(id);

            #[derive(Default)]
            struct S {
                total: usize,
                first_is_id: usize,
                first_vals: HashSet<u32>,
                contains: usize,
                offsets: BTreeMap<usize, usize>,
                times: Vec<i64>,
                sample: Option<String>,
            }
            let mut stats: BTreeMap<(String, u8, u8), S> = BTreeMap::new();
            for p in pk.iter().filter(|p| p.ts.as_str() >= from && p.ts.as_str() < to) {
                let Some((a, b, body)) = opcode(&p.data) else { continue };
                let s = stats.entry((p.key.clone(), a, b)).or_default();
                s.total += 1;
                if let Some((v, _)) = varint(&p.data, body) {
                    s.first_vals.insert(v);
                    if v == id {
                        s.first_is_id += 1;
                    }
                }
                if let Some(pos) = p.data.windows(needle.len()).position(|w| w == needle.as_slice()) {
                    s.contains += 1;
                    *s.offsets.entry(pos).or_default() += 1;
                    s.times.push(ms(&p.ts));
                    if s.sample.is_none() {
                        s.sample = Some(p.data.iter().take(48).map(|x| format!("{:02X}", x)).collect::<Vec<_>>().join(" "));
                    }
                }
            }
            let mut rows: Vec<_> = stats.iter().filter(|(_, s)| s.first_is_id > 0).collect();
            rows.sort_by_key(|(_, s)| std::cmp::Reverse(s.first_is_id));
            println!("opcodes whose FIRST varint after the opcode is {id} (window {from}..{to}):");
            println!("{:<14} {:>5} {:>6} {:>6} {:>8} {:>9}  offsets / sample", "key op", "total", "first", "vals", "median_s", "max_gap_s");
            for ((key, a, b), s) in rows {
                let mut t = s.times.clone();
                t.sort();
                let mut gaps: Vec<i64> = t.windows(2).map(|w| w[1] - w[0]).collect();
                gaps.sort();
                let med = gaps.get(gaps.len() / 2).copied().unwrap_or(0) as f64 / 1000.0;
                let max = gaps.last().copied().unwrap_or(0) as f64 / 1000.0;
                println!(
                    "{:<8} {:02X} {:02X} {:>5} {:>6} {:>6} {:>8.2} {:>9.2}  {:?}\n    {}",
                    key, a, b, s.total, s.first_is_id, s.first_vals.len(), med, max,
                    s.offsets.iter().take(4).collect::<Vec<_>>(),
                    s.sample.as_deref().unwrap_or("")
                );
            }
        }
        "vals" => {
            // Distinct first-varint values per opcode (entity ids when that slot
            // is an actor), for the opcodes listed as hex pairs, e.g. 4A36 2B38.
            for op in &args[3..] {
                let a = u8::from_str_radix(&op[0..2], 16).unwrap();
                let b = u8::from_str_radix(&op[2..4], 16).unwrap();
                let mut vals: BTreeMap<u32, (usize, String, String)> = BTreeMap::new();
                for p in &pk {
                    let Some((x, y, body)) = opcode(&p.data) else { continue };
                    if (x, y) != (a, b) { continue; }
                    if let Some((v, _)) = varint(&p.data, body) {
                        let e = vals.entry(v).or_insert((0, p.ts.clone(), String::new()));
                        e.0 += 1;
                        e.2 = p.ts.clone();
                    }
                }
                println!("{op}:");
                for (v, (n, first, last)) in vals {
                    println!("  {v:>8}  x{n:<5} {}..{}", &first[11..23], &last[11..23]);
                }
            }
        }
        "shapes" => {
            // Distinct packet shapes for one opcode: (length, bytes after the id).
            let op = &args[3];
            let a = u8::from_str_radix(&op[0..2], 16).unwrap();
            let b = u8::from_str_radix(&op[2..4], 16).unwrap();
            let mut shapes: BTreeMap<(usize, String), usize> = BTreeMap::new();
            for p in &pk {
                let Some((x, y, body)) = opcode(&p.data) else { continue };
                if (x, y) != (a, b) { continue; }
                let Some((_, idl)) = varint(&p.data, body) else { continue };
                let (len, _) = varint(&p.data, 0).unwrap();
                let tail: String = p.data[body + idl..].iter().map(|x| format!("{:02X}", x)).collect::<Vec<_>>().join(" ");
                *shapes.entry((p.data.len(), format!("len={} tail={}", len, tail))).or_default() += 1;
            }
            for ((n, s), c) in shapes.iter().take(40) {
                println!("x{c:<4} bytes={n:<3} {s}");
            }
            println!("{} distinct shapes", shapes.len());
        }
        "dealt" => {
            // 04 38 records dealt BY <id>: <target> <switch> <flag> <actor> <skill u32>.
            // Counts per skill, over every 04 38 found anywhere in a packet (not
            // just at the front), to compare with what the parser recorded.
            let id: u32 = args[3].parse().unwrap();
            let from = args.get(4).map(|s| s.as_str()).unwrap_or("");
            let to = args.get(5).map(|s| s.as_str()).unwrap_or("~");
            let mut front: BTreeMap<u32, usize> = BTreeMap::new();
            let mut embedded: BTreeMap<u32, usize> = BTreeMap::new();
            let mut samples: BTreeMap<u32, Vec<String>> = BTreeMap::new();
            for p in pk.iter().filter(|p| p.ts.as_str() >= from && p.ts.as_str() < to) {
                let d = &p.data;
                let Some((_, llen)) = varint(d, 0) else { continue };
                for i in 0..d.len().saturating_sub(10) {
                    if d[i] != 0x04 || d[i + 1] != 0x38 { continue; }
                    let mut o = i + 2;
                    let mut next = || -> Option<u32> { let (v, l) = varint(d, o)?; o += l; Some(v) };
                    let (Some(_t), Some(sw), Some(_f), Some(actor)) = (next(), next(), next(), next()) else { continue };
                    if actor != id || !(4..=7).contains(&(sw & 0x0F)) { continue; }
                    let Some(sk) = d.get(o..o + 4) else { continue };
                    let skill = u32::from_le_bytes([sk[0], sk[1], sk[2], sk[3]]);
                    if i == llen {
                        *front.entry(skill).or_default() += 1;
                    } else {
                        *embedded.entry(skill).or_default() += 1;
                    }
                    let s = samples.entry(skill).or_default();
                    if s.len() < 2 {
                        s.push(d[i..d.len().min(i + 44)].iter().map(|x| format!("{:02X}", x)).collect::<Vec<_>>().join(" "));
                    }
                }
            }
            let skills: HashSet<u32> = front.keys().chain(embedded.keys()).copied().collect();
            let mut skills: Vec<u32> = skills.into_iter().collect();
            skills.sort();
            println!("04 38 dealt by {id} ({from}..{to}): skill  front  embedded");
            for s in skills {
                println!("  {s:>10} {:>6} {:>9}", front.get(&s).unwrap_or(&0), embedded.get(&s).unwrap_or(&0));
                for x in samples.get(&s).into_iter().flatten() {
                    println!("       {x}");
                }
            }
        }
        "scalars" => {
            // Power scalar per hit dealt by <id>, decoded like parsing_damage:
            // <target> <switch> <flag> <actor> <skill u32> <uid u8> <type> <skip>
            // then [0 pad] <scalar> <damage>. Prints the timeline of changes.
            let id: u32 = args[3].parse().unwrap();
            let from = args.get(4).map(|s| s.as_str()).unwrap_or("");
            let to = args.get(5).map(|s| s.as_str()).unwrap_or("~");
            let mut last: Option<u32> = None;
            let mut counts: BTreeMap<u32, usize> = BTreeMap::new();
            let mut by_skill: BTreeMap<u32, BTreeMap<u32, usize>> = BTreeMap::new();
            let verbose = std::env::var("VERBOSE").is_ok();
            for p in pk.iter().filter(|p| p.ts.as_str() >= from && p.ts.as_str() < to) {
                let d = &p.data;
                for i in 0..d.len().saturating_sub(10) {
                    if d[i] != 0x04 || d[i + 1] != 0x38 { continue; }
                    let mut o = i + 2;
                    let next = |o: &mut usize| -> Option<u32> { let (v, l) = varint(d, *o)?; *o += l; Some(v) };
                    let (Some(_t), Some(sw), Some(_f), Some(actor)) = (next(&mut o), next(&mut o), next(&mut o), next(&mut o)) else { continue };
                    let and = sw & 0x0F;
                    if actor != id || !(4..=7).contains(&and) { continue; }
                    let Some(sk) = d.get(o..o + 4) else { continue };
                    let skill = u32::from_le_bytes([sk[0], sk[1], sk[2], sk[3]]);
                    o += 4 + 1;
                    let Some(dtype) = next(&mut o) else { continue };
                    o += match and { 5 => 12, 6 => 10, 7 => 14, _ => 8 };
                    let (Some(mut a), Some(mut b)) = (next(&mut o), next(&mut o)) else { continue };
                    if a == 0 { if let Some(c) = next(&mut o) { a = b; b = c; } }
                    let first_is_dmg = (1_000..=5_000_000).contains(&a) && b <= 25 && and == 6 && dtype == 3;
                    let (scalar, dmg) = if first_is_dmg { (None, a) } else { ((1_000..=200_000).contains(&a).then_some(a), b) };
                    if let Some(s) = scalar {
                        *counts.entry(s).or_default() += 1;
                        *by_skill.entry(skill / 10000).or_default().entry(s).or_default() += 1;
                        if verbose {
                            println!("  {} {skill} {s} {dmg}", &p.ts[11..23]);
                        }
                        if last != Some(s) {
                            println!("{} skill={skill} scalar {:?} -> {s}  (dmg {dmg})", &p.ts[11..23], last);
                            last = Some(s);
                        }
                    }
                }
            }
            println!("--- scalar counts: {counts:?}");
            for (sk, m) in &by_skill {
                println!("    skill {sk}xxxx: {m:?}");
            }
        }
        "around" => {
            // Packets carrying <id> (as a varint anywhere) within [t - before_ms,
            // t + after_ms] of each HH:MM:SS.mmm time given, skipping the chatty
            // combat opcodes. `around <id> <before_ms> <after_ms> <t1> [t2 ...]`
            let id: u32 = args[3].parse().unwrap();
            let before: i64 = args[4].parse().unwrap();
            let after: i64 = args[5].parse().unwrap();
            let needle = enc_varint(id);
            let skip: [(u8, u8); 5] = [(0x2B, 0x38), (0x02, 0x38), (0x06, 0x38), (0x4A, 0x36), (0x04, 0x38)];
            let tod = |ts: &str| -> i64 {
                let h: i64 = ts[0..2].parse().unwrap_or(0);
                let m: i64 = ts[3..5].parse().unwrap_or(0);
                let s: f64 = ts[6..].parse().unwrap_or(0.0);
                (h * 3600 + m * 60) * 1000 + (s * 1000.0) as i64
            };
            for t in &args[6..] {
                let center = tod(t);
                println!("=== around {t}");
                for p in &pk {
                    let pt = tod(&p.ts[11..23]);
                    if pt < center - before || pt > center + after { continue; }
                    let Some((a, b, _)) = opcode(&p.data) else { continue };
                    if skip.contains(&(a, b)) { continue; }
                    if !p.data.windows(needle.len()).any(|w| w == needle.as_slice()) { continue; }
                    let hex: String = p.data.iter().take(40).map(|x| format!("{:02X}", x)).collect::<Vec<_>>().join(" ");
                    println!("  {} {:+5}ms {a:02X} {b:02X} len={:<4} {hex}", &p.ts[11..23], pt - center, p.data.len());
                }
            }
        }
        "buffs" => {
            // Buff add (2A 38) / remove (2C 38) on <id>:
            //   2A 38 <target> <v> <v> <seq> <effect u32> <duration_ms u32> <4 x 00> <u64 ms> <caster> ...
            //   2C 38 <target> 01 00 <seq> 01
            let id: u32 = args[3].parse().unwrap();
            let names: HashMap<String, String> = std::fs::read_to_string("../src/data/i18n/skills/en.json")
                .ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
            let name_of = |effect: u32| -> String {
                let code = effect / 10;
                names.get(&code.to_string()).cloned()
                    .or_else(|| names.get(&(code - code % 10000).to_string()).cloned())
                    .unwrap_or_else(|| "?".into())
            };
            let mut open: HashMap<u32, (String, u32, u32)> = HashMap::new(); // seq -> (ts, effect, dur)
            let (mut adds, mut removes, mut matched) = (0, 0, 0);
            for p in &pk {
                let d = &p.data;
                let Some((a, b, body)) = opcode(d) else { continue };
                let mut o = body;
                let next = |o: &mut usize| -> Option<u32> { let (v, l) = varint(d, *o)?; *o += l; Some(v) };
                if (a, b) == (0x2A, 0x38) {
                    let (Some(target), Some(_v1), Some(_v2), Some(seq)) = (next(&mut o), next(&mut o), next(&mut o), next(&mut o)) else { continue };
                    if target != id { continue; }
                    let Some(e) = d.get(o..o + 8) else { continue };
                    let effect = u32::from_le_bytes([e[0], e[1], e[2], e[3]]);
                    let dur = u32::from_le_bytes([e[4], e[5], e[6], e[7]]);
                    let caster = varint(d, o + 8 + 4 + 8).map(|(v, _)| v).unwrap_or(0);
                    adds += 1;
                    println!("{} +  seq={seq:<6} effect={effect:<10} {:<28} dur={:>6}ms caster={caster}", &p.ts[11..23], name_of(effect), dur);
                    open.insert(seq, (p.ts[11..23].to_string(), effect, dur));
                } else if (a, b) == (0x2C, 0x38) && d.len() <= 12 {
                    let (Some(target), Some(_one), Some(_zero), Some(seq)) = (next(&mut o), next(&mut o), next(&mut o), next(&mut o)) else { continue };
                    if target != id { continue; }
                    removes += 1;
                    if let Some((t0, effect, dur)) = open.remove(&seq) {
                        matched += 1;
                        println!("{} -  seq={seq:<6} effect={effect:<10} {:<28} (added {t0}, dur {dur}ms)", &p.ts[11..23], name_of(effect));
                    } else {
                        println!("{} -  seq={seq:<6} (no matching add)", &p.ts[11..23]);
                    }
                }
            }
            println!("--- adds={adds} removes={removes} matched={matched} still_open={}", open.len());
        }
        "findval" => {
            // Where does a value (e.g. an NPC code) appear, as varint or u32 LE,
            // and which entity varints sit near it? `findval <value> [near_id]`.
            let val: u32 = args[3].parse().unwrap();
            let near: Option<u32> = args.get(4).and_then(|s| s.parse().ok());
            let pats = [("varint", enc_varint(val)), ("u32le", val.to_le_bytes().to_vec())];
            let mut by_op: BTreeMap<(String, u8, u8, &str), (usize, usize, String)> = BTreeMap::new();
            for p in &pk {
                let Some((a, b, _)) = opcode(&p.data) else { continue };
                for (enc, pat) in &pats {
                    if let Some(pos) = p.data.windows(pat.len()).position(|w| w == pat.as_slice()) {
                        let e = by_op.entry((p.key.clone(), a, b, *enc)).or_insert((0, 0, String::new()));
                        e.0 += 1;
                        if let Some(id) = near {
                            let idb = enc_varint(id);
                            if p.data.windows(idb.len()).any(|w| w == idb.as_slice()) { e.1 += 1; }
                        }
                        if e.2.is_empty() {
                            let s = pos.saturating_sub(12);
                            e.2 = format!("@{pos}: {}", p.data[s..p.data.len().min(pos + 16)].iter().map(|x| format!("{:02X}", x)).collect::<Vec<_>>().join(" "));
                        }
                    }
                }
            }
            for ((key, a, b, enc), (n, with_id, sample)) in by_op {
                println!("{key} {a:02X} {b:02X} {enc:<6} x{n:<5} with_id={with_id:<5} {sample}");
            }
        }
        _ => eprintln!("unknown mode"),
    }
}
