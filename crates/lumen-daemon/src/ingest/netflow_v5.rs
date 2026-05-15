//! NetFlow v5 datagram parser.
//!
//! Wire format (RFC-equivalent — Cisco proprietary): a 24-byte header
//! followed by N × 48-byte flow records, all big-endian. We only emit
//! `Flow` records for IPv4 (NetFlow v5 is IPv4-only by design).

use std::net::{IpAddr, Ipv4Addr};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use lumen_core::{Flow, FlowEndpoint, FlowSource, Protocol};
use thiserror::Error;

const HEADER_LEN: usize = 24;
const RECORD_LEN: usize = 48;
const VERSION: u16 = 5;

#[derive(Debug, Error)]
pub enum ParseError {
    #[error("datagram too short for header (got {0} bytes, need {HEADER_LEN})")]
    TooShortForHeader(usize),
    #[error("unexpected version {got} (expected {VERSION})")]
    BadVersion { got: u16 },
    #[error("datagram length {actual} does not match header count {expected_records} (expected {expected})")]
    LengthMismatch {
        actual: usize,
        expected: usize,
        expected_records: u16,
    },
    #[error("nanosecond field {0} out of range")]
    BadNanos(u32),
}

/// Parse one NetFlow v5 datagram. Returns one `Flow` per record contained.
pub fn parse(datagram: &[u8]) -> Result<Vec<Flow>, ParseError> {
    if datagram.len() < HEADER_LEN {
        return Err(ParseError::TooShortForHeader(datagram.len()));
    }

    let version = u16::from_be_bytes([datagram[0], datagram[1]]);
    if version != VERSION {
        return Err(ParseError::BadVersion { got: version });
    }

    let count = u16::from_be_bytes([datagram[2], datagram[3]]);
    let sys_uptime_ms = u32::from_be_bytes(datagram[4..8].try_into().unwrap());
    let unix_secs = u32::from_be_bytes(datagram[8..12].try_into().unwrap());
    let unix_nsecs = u32::from_be_bytes(datagram[12..16].try_into().unwrap());

    if unix_nsecs >= 1_000_000_000 {
        return Err(ParseError::BadNanos(unix_nsecs));
    }

    let expected_len = HEADER_LEN + (count as usize) * RECORD_LEN;
    if datagram.len() != expected_len {
        return Err(ParseError::LengthMismatch {
            actual: datagram.len(),
            expected: expected_len,
            expected_records: count,
        });
    }

    let exporter_now = UNIX_EPOCH + Duration::new(unix_secs as u64, unix_nsecs);
    let exporter_uptime = Duration::from_millis(sys_uptime_ms as u64);

    let mut flows = Vec::with_capacity(count as usize);
    for i in 0..count as usize {
        let offset = HEADER_LEN + i * RECORD_LEN;
        let record = &datagram[offset..offset + RECORD_LEN];
        flows.push(parse_record(record, exporter_now, exporter_uptime));
    }

    Ok(flows)
}

fn parse_record(record: &[u8], exporter_now: SystemTime, exporter_uptime: Duration) -> Flow {
    debug_assert_eq!(record.len(), RECORD_LEN);

    let src_addr = Ipv4Addr::from(<[u8; 4]>::try_from(&record[0..4]).unwrap());
    let dst_addr = Ipv4Addr::from(<[u8; 4]>::try_from(&record[4..8]).unwrap());
    let packets = u32::from_be_bytes(record[16..20].try_into().unwrap()) as u64;
    let octets = u32::from_be_bytes(record[20..24].try_into().unwrap()) as u64;
    let first_uptime_ms = u32::from_be_bytes(record[24..28].try_into().unwrap());
    let last_uptime_ms = u32::from_be_bytes(record[28..32].try_into().unwrap());
    let src_port = u16::from_be_bytes(record[32..34].try_into().unwrap());
    let dst_port = u16::from_be_bytes(record[34..36].try_into().unwrap());
    let proto = record[38];

    Flow {
        source: FlowSource::NetflowV5,
        src: FlowEndpoint {
            ip: IpAddr::V4(src_addr),
            port: src_port,
        },
        dst: FlowEndpoint {
            ip: IpAddr::V4(dst_addr),
            port: dst_port,
        },
        protocol: Protocol(proto),
        bytes: octets,
        packets,
        start: uptime_to_walltime(first_uptime_ms, exporter_now, exporter_uptime),
        end: uptime_to_walltime(last_uptime_ms, exporter_now, exporter_uptime),
    }
}

/// NetFlow v5 timestamps are exporter-relative milliseconds since boot.
/// Convert to absolute wall time using the header's snapshot of (uptime, walltime).
fn uptime_to_walltime(
    record_uptime_ms: u32,
    exporter_now: SystemTime,
    exporter_uptime: Duration,
) -> SystemTime {
    let record_uptime = Duration::from_millis(record_uptime_ms as u64);
    if record_uptime >= exporter_uptime {
        exporter_now + (record_uptime - exporter_uptime)
    } else {
        exporter_now - (exporter_uptime - record_uptime)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a single-record NetFlow v5 datagram for testing.
    pub(crate) fn synthetic_packet(
        src: Ipv4Addr,
        dst: Ipv4Addr,
        src_port: u16,
        dst_port: u16,
        protocol: u8,
        bytes: u32,
        packets: u32,
    ) -> Vec<u8> {
        let mut buf = Vec::with_capacity(HEADER_LEN + RECORD_LEN);
        // Header
        buf.extend_from_slice(&5u16.to_be_bytes()); // version
        buf.extend_from_slice(&1u16.to_be_bytes()); // count
        buf.extend_from_slice(&60_000u32.to_be_bytes()); // sys_uptime: 60s
        buf.extend_from_slice(&1_700_000_000u32.to_be_bytes()); // unix_secs
        buf.extend_from_slice(&0u32.to_be_bytes()); // unix_nsecs
        buf.extend_from_slice(&0u32.to_be_bytes()); // flow_sequence
        buf.push(0); // engine_type
        buf.push(0); // engine_id
        buf.extend_from_slice(&0u16.to_be_bytes()); // sampling_interval
        debug_assert_eq!(buf.len(), HEADER_LEN);

        // Record
        buf.extend_from_slice(&src.octets()); // src_addr
        buf.extend_from_slice(&dst.octets()); // dst_addr
        buf.extend_from_slice(&[0, 0, 0, 0]); // next_hop
        buf.extend_from_slice(&0u16.to_be_bytes()); // input_iface
        buf.extend_from_slice(&0u16.to_be_bytes()); // output_iface
        buf.extend_from_slice(&packets.to_be_bytes()); // packets
        buf.extend_from_slice(&bytes.to_be_bytes()); // octets
        buf.extend_from_slice(&30_000u32.to_be_bytes()); // first uptime: 30s
        buf.extend_from_slice(&55_000u32.to_be_bytes()); // last uptime: 55s
        buf.extend_from_slice(&src_port.to_be_bytes());
        buf.extend_from_slice(&dst_port.to_be_bytes());
        buf.push(0); // pad1
        buf.push(0); // tcp_flags
        buf.push(protocol);
        buf.push(0); // tos
        buf.extend_from_slice(&0u16.to_be_bytes()); // src_as
        buf.extend_from_slice(&0u16.to_be_bytes()); // dst_as
        buf.push(0); // src_mask
        buf.push(0); // dst_mask
        buf.extend_from_slice(&0u16.to_be_bytes()); // pad2
        debug_assert_eq!(buf.len(), HEADER_LEN + RECORD_LEN);

        buf
    }

    #[test]
    fn parses_a_single_flow() {
        let pkt = synthetic_packet(
            Ipv4Addr::new(10, 0, 0, 1),
            Ipv4Addr::new(8, 8, 8, 8),
            45_678,
            53,
            17,
            128,
            2,
        );
        let flows = parse(&pkt).unwrap();
        assert_eq!(flows.len(), 1);
        let f = &flows[0];
        assert_eq!(f.src.ip, IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)));
        assert_eq!(f.dst.ip, IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)));
        assert_eq!(f.src.port, 45_678);
        assert_eq!(f.dst.port, 53);
        assert_eq!(f.protocol, Protocol::UDP);
        assert_eq!(f.bytes, 128);
        assert_eq!(f.packets, 2);
        assert!(f.end >= f.start);
    }

    #[test]
    fn rejects_short_datagram() {
        let pkt = vec![0u8; 10];
        assert!(matches!(
            parse(&pkt),
            Err(ParseError::TooShortForHeader(10))
        ));
    }

    #[test]
    fn rejects_wrong_version() {
        let mut pkt = synthetic_packet(
            Ipv4Addr::new(1, 1, 1, 1),
            Ipv4Addr::new(2, 2, 2, 2),
            1,
            2,
            6,
            1,
            1,
        );
        pkt[0..2].copy_from_slice(&9u16.to_be_bytes());
        assert!(matches!(
            parse(&pkt),
            Err(ParseError::BadVersion { got: 9 })
        ));
    }

    #[test]
    fn rejects_count_length_mismatch() {
        let mut pkt = synthetic_packet(
            Ipv4Addr::new(1, 1, 1, 1),
            Ipv4Addr::new(2, 2, 2, 2),
            1,
            2,
            6,
            1,
            1,
        );
        pkt[2..4].copy_from_slice(&2u16.to_be_bytes()); // claim 2 records, but datagram has 1
        assert!(matches!(
            parse(&pkt),
            Err(ParseError::LengthMismatch { .. })
        ));
    }
}
