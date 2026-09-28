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
        _ => eprintln!("unknown mode"),
    }
}
