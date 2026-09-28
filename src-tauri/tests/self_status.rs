//! The `4A 36` self-status tick binds the local player when the meter starts
//! mid-zone, before any `33 36` self record (which only comes on zone load).

use std::collections::HashMap;
use std::sync::Arc;

use a2tools_dps_meter_lib::capture::packet_accumulator::PacketAccumulator;
use a2tools_dps_meter_lib::capture::stream_processor::StreamProcessor;
use a2tools_dps_meter_lib::combat::data_storage::DataStorage;
use a2tools_dps_meter_lib::i18n::lookup::{NpcLookup, SkillLookup};

/// The dominant live shape: length 0x11 (17 - 3 = 14 bytes), opcode `4A 36`,
/// entity 12978 as varint `B2 65`, then nine zero bytes.
const TICK_12978: [u8; 14] = [0x11, 0x4A, 0x36, 0xB2, 0x65, 0, 0, 0, 0, 0, 0, 0, 0, 0];

fn processor(storage: &Arc<DataStorage>) -> StreamProcessor {
    StreamProcessor::new(storage.clone(), Arc::new(SkillLookup::new()), Arc::new(NpcLookup::new()))
}

#[test]
fn self_status_tick_binds_the_local_player() {
    let storage = Arc::new(DataStorage::new());
    let mut p = processor(&storage);
    assert_eq!(p.consume_stream(&TICK_12978), TICK_12978.len());
    assert_eq!(storage.local_player_id(), Some(12978));
}

#[test]
fn self_status_tick_does_not_override_an_existing_binding() {
    let storage = Arc::new(DataStorage::new());
    storage.set_local_player_id(Some(4099));
    processor(&storage).consume_stream(&TICK_12978);
    assert_eq!(storage.local_player_id(), Some(4099));
}

/// Ground truth from a live capture: the meter was started mid-zone, so the
/// first `33 36` self record (entity 12978) only arrived at 21:16:41 after a
/// teleport. Replaying just the part before it must already know who you are.
///
///   A2_SELF_STATUS_CAPTURE=/path/to/packets_20260927_211445.txt \
///   cargo test --test self_status -- --nocapture
#[test]
fn binds_before_the_first_self_record_in_a_real_capture() {
    let Ok(path) = std::env::var("A2_SELF_STATUS_CAPTURE") else {
        eprintln!("A2_SELF_STATUS_CAPTURE unset — skipping");
        return;
    };
    let storage = Arc::new(DataStorage::new());
    let mut p = processor(&storage);
    let text = std::fs::read_to_string(&path).expect("capture readable");
    let mut streams: HashMap<String, PacketAccumulator> = HashMap::new();
    let mut bound_at = None;
    for line in text.lines() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.splitn(3, '|');
        let (Some(ts), Some(key), Some(hex)) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        if ts >= "2026-09-27T21:16:41" {
            break;
        }
        let Some(bytes) = (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).ok())
            .collect::<Option<Vec<u8>>>()
        else {
            continue;
        };
        let acc = streams.entry(key.to_string()).or_insert_with(PacketAccumulator::new);
        acc.append(&bytes);
        let used = p.consume_stream(acc.snapshot());
        if used > 0 {
            acc.discard_bytes(used);
        }
        if bound_at.is_none() && storage.local_player_id().is_some() {
            bound_at = Some(ts.to_string());
        }
    }
    println!("bound at {bound_at:?}");
    assert_eq!(storage.local_player_id(), Some(12978));
}
