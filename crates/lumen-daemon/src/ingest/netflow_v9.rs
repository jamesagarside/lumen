//! NetFlow v9 + IPFIX (template-based) parser.
//!
//! NetFlow v9 and IPFIX (RFC 7011) share a model: an exporter
//! periodically sends *templates* that describe record formats, then
//! sends *data* records that reference a template by ID. Receivers
//! cache templates per `(exporter address, observation domain)` and
//! decode incoming data records against the cached template.
//!
//! Wire-level differences between v9 and IPFIX are small enough that
//! one parser handles both:
//!   - header layout (v9 has packet count; IPFIX has total length)
//!   - flowset/set IDs (v9 templates = id 0, IPFIX templates = id 2)
//!   - field IDs that matter to us are the same numbers
//!
//! v1 scope: IPv4 + IPv6, the eight or so fields that map cleanly to
//! `lumen_core::Flow`. Options templates and enterprise-specific
//! IPFIX fields are ignored (parsed and skipped, not errored on).

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use lumen_core::{Flow, FlowEndpoint, FlowSource, Protocol};
use thiserror::Error;
use tracing::{debug, trace};

// ── NetFlow / IPFIX wire constants ──────────────────────────────────────────

const VERSION_V9: u16 = 9;
const VERSION_IPFIX: u16 = 10;

const V9_HEADER_LEN: usize = 20;
const IPFIX_HEADER_LEN: usize = 16;

// Flowset / Set IDs.
const V9_TEMPLATE_FLOWSET: u16 = 0;
const V9_OPTIONS_TEMPLATE_FLOWSET: u16 = 1;
const IPFIX_TEMPLATE_SET: u16 = 2;
const IPFIX_OPTIONS_TEMPLATE_SET: u16 = 3;
const FIRST_DATA_FLOWSET: u16 = 256;

// Field IDs we care about. These are shared between v9 and IPFIX.
// Anything not in this list is parsed and skipped (we still need the
// byte count to advance through the record).
const FIELD_IN_BYTES: u16 = 1;
const FIELD_IN_PKTS: u16 = 2;
const FIELD_PROTOCOL: u16 = 4;
const FIELD_L4_SRC_PORT: u16 = 7;
const FIELD_IPV4_SRC_ADDR: u16 = 8;
const FIELD_L4_DST_PORT: u16 = 11;
const FIELD_IPV4_DST_ADDR: u16 = 12;
const FIELD_LAST_SWITCHED: u16 = 21;
const FIELD_FIRST_SWITCHED: u16 = 22;
const FIELD_IPV6_SRC_ADDR: u16 = 27;
const FIELD_IPV6_DST_ADDR: u16 = 28;
// IPFIX-only flow start/end (absolute timestamps in ms or seconds).
const FIELD_FLOW_START_MS: u16 = 152;
const FIELD_FLOW_END_MS: u16 = 153;

// ── Errors ──────────────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum ParseError {
    #[error("datagram too short for header (got {0} bytes)")]
    TooShortForHeader(usize),
    #[error("unsupported version {0} (expected 9 or 10)")]
    UnsupportedVersion(u16),
    #[error("malformed flowset at offset {offset}: {reason}")]
    BadFlowset { offset: usize, reason: String },
}

// ── Template cache ──────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct TemplateField {
    field_type: u16,
    length: u16,
}

#[derive(Debug, Clone)]
struct Template {
    fields: Vec<TemplateField>,
    /// Sum of field lengths = bytes per data record. Cached so the
    /// data-set parser can count records without re-summing.
    record_size: usize,
}

/// Key into the cache. `observation_domain` is per-exporter — a
/// single router can have multiple flow exporters with different
/// templates, distinguished by this ID. We narrow to the source
/// socket address (IP only — port can vary) and observation domain.
#[derive(Debug, Hash, Eq, PartialEq, Clone)]
struct TemplateKey {
    peer: IpAddr,
    observation_domain: u32,
    template_id: u16,
}

#[derive(Default, Debug)]
pub struct TemplateCache {
    templates: HashMap<TemplateKey, Template>,
}

impl TemplateCache {
    pub fn new() -> Self {
        Self::default()
    }

    #[allow(dead_code)] // used by tests + future /metrics gauge
    pub fn len(&self) -> usize {
        self.templates.len()
    }

    fn insert(&mut self, key: TemplateKey, template: Template) {
        self.templates.insert(key, template);
    }

    fn get(&self, key: &TemplateKey) -> Option<&Template> {
        self.templates.get(key)
    }
}

// ── Parse entry point ───────────────────────────────────────────────────────

/// Parse one NetFlow v9 or IPFIX datagram. Templates discovered in
/// this datagram are added to `cache`; data records are decoded
/// against templates already in the cache (templates seen in the
/// same packet apply to data records *after* them).
///
/// Datagrams whose data records reference templates we haven't yet
/// seen are silently skipped — the exporter will re-send the
/// template soon. This is standard behaviour; logging a debug line
/// helps diagnose if it persists.
pub fn parse(
    datagram: &[u8],
    cache: &mut TemplateCache,
    peer: SocketAddr,
) -> Result<Vec<Flow>, ParseError> {
    if datagram.len() < 2 {
        return Err(ParseError::TooShortForHeader(datagram.len()));
    }
    let version = u16::from_be_bytes([datagram[0], datagram[1]]);
    match version {
        VERSION_V9 => parse_v9(datagram, cache, peer.ip()),
        VERSION_IPFIX => parse_ipfix(datagram, cache, peer.ip()),
        v => Err(ParseError::UnsupportedVersion(v)),
    }
}

fn parse_v9(
    datagram: &[u8],
    cache: &mut TemplateCache,
    peer: IpAddr,
) -> Result<Vec<Flow>, ParseError> {
    if datagram.len() < V9_HEADER_LEN {
        return Err(ParseError::TooShortForHeader(datagram.len()));
    }
    let sys_uptime_ms = u32::from_be_bytes(datagram[4..8].try_into().unwrap());
    let unix_secs = u32::from_be_bytes(datagram[8..12].try_into().unwrap());
    let observation_domain = u32::from_be_bytes(datagram[16..20].try_into().unwrap());

    let exporter_now = UNIX_EPOCH + Duration::from_secs(unix_secs as u64);
    let exporter_uptime = Duration::from_millis(sys_uptime_ms as u64);

    walk_flowsets(
        &datagram[V9_HEADER_LEN..],
        V9_HEADER_LEN,
        cache,
        peer,
        observation_domain,
        VERSION_V9,
        Some((exporter_now, exporter_uptime)),
    )
}

fn parse_ipfix(
    datagram: &[u8],
    cache: &mut TemplateCache,
    peer: IpAddr,
) -> Result<Vec<Flow>, ParseError> {
    if datagram.len() < IPFIX_HEADER_LEN {
        return Err(ParseError::TooShortForHeader(datagram.len()));
    }
    let total_length = u16::from_be_bytes(datagram[2..4].try_into().unwrap()) as usize;
    let export_time = u32::from_be_bytes(datagram[4..8].try_into().unwrap());
    let observation_domain = u32::from_be_bytes(datagram[12..16].try_into().unwrap());

    let exporter_now = UNIX_EPOCH + Duration::from_secs(export_time as u64);
    let end = total_length.min(datagram.len());

    walk_flowsets(
        &datagram[IPFIX_HEADER_LEN..end],
        IPFIX_HEADER_LEN,
        cache,
        peer,
        observation_domain,
        VERSION_IPFIX,
        // IPFIX has absolute flow start/end fields; the relative
        // uptime trick from v9 doesn't apply here.
        Some((exporter_now, Duration::ZERO)),
    )
}

#[allow(clippy::too_many_arguments)]
fn walk_flowsets(
    mut bytes: &[u8],
    mut absolute_offset: usize,
    cache: &mut TemplateCache,
    peer: IpAddr,
    observation_domain: u32,
    version: u16,
    timing: Option<(SystemTime, Duration)>,
) -> Result<Vec<Flow>, ParseError> {
    let mut flows = Vec::new();
    while bytes.len() >= 4 {
        let flowset_id = u16::from_be_bytes([bytes[0], bytes[1]]);
        let length = u16::from_be_bytes([bytes[2], bytes[3]]) as usize;
        if length < 4 || length > bytes.len() {
            return Err(ParseError::BadFlowset {
                offset: absolute_offset,
                reason: format!("length {length} out of range (have {})", bytes.len()),
            });
        }
        let body = &bytes[4..length];

        let is_template = (version == VERSION_V9 && flowset_id == V9_TEMPLATE_FLOWSET)
            || (version == VERSION_IPFIX && flowset_id == IPFIX_TEMPLATE_SET);
        let is_options = (version == VERSION_V9 && flowset_id == V9_OPTIONS_TEMPLATE_FLOWSET)
            || (version == VERSION_IPFIX && flowset_id == IPFIX_OPTIONS_TEMPLATE_SET);

        if is_template {
            parse_templates(body, cache, peer, observation_domain, absolute_offset)?;
        } else if is_options {
            // Options templates describe metadata-about-the-exporter
            // records, not flow records. We don't act on them but
            // still need to advance past the flowset.
            trace!(offset = absolute_offset, "skipping options template");
        } else if flowset_id >= FIRST_DATA_FLOWSET {
            let template_id = flowset_id;
            let key = TemplateKey {
                peer,
                observation_domain,
                template_id,
            };
            if let Some(template) = cache.get(&key).cloned() {
                parse_data_set(body, &template, &mut flows, timing)?;
            } else {
                debug!(
                    template_id,
                    peer = %peer,
                    "data set references unknown template — dropping"
                );
            }
        } else {
            trace!(
                flowset_id,
                offset = absolute_offset,
                "unknown flowset id, skipping"
            );
        }

        bytes = &bytes[length..];
        absolute_offset += length;
    }
    Ok(flows)
}

fn parse_templates(
    mut body: &[u8],
    cache: &mut TemplateCache,
    peer: IpAddr,
    observation_domain: u32,
    absolute_offset: usize,
) -> Result<(), ParseError> {
    // A template flowset can pack multiple templates back-to-back.
    while body.len() >= 4 {
        let template_id = u16::from_be_bytes([body[0], body[1]]);
        let field_count = u16::from_be_bytes([body[2], body[3]]) as usize;
        let needed = 4 + field_count * 4;
        if body.len() < needed {
            return Err(ParseError::BadFlowset {
                offset: absolute_offset,
                reason: format!(
                    "template id {template_id} needs {needed} bytes, have {}",
                    body.len()
                ),
            });
        }
        let mut fields = Vec::with_capacity(field_count);
        let mut record_size = 0usize;
        for i in 0..field_count {
            let off = 4 + i * 4;
            let field_type = u16::from_be_bytes([body[off], body[off + 1]]);
            let length = u16::from_be_bytes([body[off + 2], body[off + 3]]);
            record_size += length as usize;
            fields.push(TemplateField { field_type, length });
        }
        let key = TemplateKey {
            peer,
            observation_domain,
            template_id,
        };
        debug!(
            template_id,
            fields = field_count,
            record_size,
            "cached template"
        );
        cache.insert(
            key,
            Template {
                fields,
                record_size,
            },
        );
        body = &body[needed..];
    }
    Ok(())
}

fn parse_data_set(
    body: &[u8],
    template: &Template,
    out: &mut Vec<Flow>,
    timing: Option<(SystemTime, Duration)>,
) -> Result<(), ParseError> {
    if template.record_size == 0 {
        return Ok(());
    }
    let record_count = body.len() / template.record_size;
    for i in 0..record_count {
        let record = &body[i * template.record_size..(i + 1) * template.record_size];
        if let Some(flow) = decode_record(record, template, timing) {
            out.push(flow);
        }
    }
    Ok(())
}

/// Decode one fixed-length data record against its template. Returns
/// None if the record is missing required fields (src/dst IP +
/// protocol) — that's not an error, it's a template that doesn't
/// give us enough to populate a Flow (e.g. an exporter sending
/// router-stats-only templates we shouldn't try to coerce).
fn decode_record(
    record: &[u8],
    template: &Template,
    timing: Option<(SystemTime, Duration)>,
) -> Option<Flow> {
    let mut src_ip: Option<IpAddr> = None;
    let mut dst_ip: Option<IpAddr> = None;
    let mut src_port: u16 = 0;
    let mut dst_port: u16 = 0;
    let mut protocol: u8 = 0;
    let mut bytes: u64 = 0;
    let mut packets: u64 = 0;
    let mut first_switched_ms: Option<u32> = None;
    let mut last_switched_ms: Option<u32> = None;
    let mut flow_start_abs_ms: Option<u64> = None;
    let mut flow_end_abs_ms: Option<u64> = None;

    let mut cursor = 0usize;
    for field in &template.fields {
        let len = field.length as usize;
        if cursor + len > record.len() {
            return None;
        }
        let slice = &record[cursor..cursor + len];
        match field.field_type {
            FIELD_IN_BYTES => bytes = read_uint(slice),
            FIELD_IN_PKTS => packets = read_uint(slice),
            FIELD_PROTOCOL => protocol = read_uint(slice) as u8,
            FIELD_L4_SRC_PORT => src_port = read_uint(slice) as u16,
            FIELD_L4_DST_PORT => dst_port = read_uint(slice) as u16,
            FIELD_IPV4_SRC_ADDR if len == 4 => {
                src_ip = Some(IpAddr::V4(Ipv4Addr::from(
                    <[u8; 4]>::try_from(slice).unwrap(),
                )));
            }
            FIELD_IPV4_DST_ADDR if len == 4 => {
                dst_ip = Some(IpAddr::V4(Ipv4Addr::from(
                    <[u8; 4]>::try_from(slice).unwrap(),
                )));
            }
            FIELD_IPV6_SRC_ADDR if len == 16 => {
                src_ip = Some(IpAddr::V6(Ipv6Addr::from(
                    <[u8; 16]>::try_from(slice).unwrap(),
                )));
            }
            FIELD_IPV6_DST_ADDR if len == 16 => {
                dst_ip = Some(IpAddr::V6(Ipv6Addr::from(
                    <[u8; 16]>::try_from(slice).unwrap(),
                )));
            }
            FIELD_FIRST_SWITCHED if len == 4 => {
                first_switched_ms = Some(u32::from_be_bytes(slice.try_into().unwrap()));
            }
            FIELD_LAST_SWITCHED if len == 4 => {
                last_switched_ms = Some(u32::from_be_bytes(slice.try_into().unwrap()));
            }
            FIELD_FLOW_START_MS if len == 8 => {
                flow_start_abs_ms = Some(u64::from_be_bytes(slice.try_into().unwrap()));
            }
            FIELD_FLOW_END_MS if len == 8 => {
                flow_end_abs_ms = Some(u64::from_be_bytes(slice.try_into().unwrap()));
            }
            _ => { /* unknown / unused field; skip */ }
        }
        cursor += len;
    }

    let src_ip = src_ip?;
    let dst_ip = dst_ip?;

    let (start, end) = resolve_timing(
        first_switched_ms,
        last_switched_ms,
        flow_start_abs_ms,
        flow_end_abs_ms,
        timing,
    );

    let template_is_ipfix_shaped = template
        .fields
        .iter()
        .any(|f| f.field_type == FIELD_FLOW_START_MS);
    Some(Flow {
        source: if template_is_ipfix_shaped {
            FlowSource::Ipfix
        } else {
            FlowSource::NetflowV9
        },
        src: FlowEndpoint {
            ip: src_ip,
            port: src_port,
        },
        dst: FlowEndpoint {
            ip: dst_ip,
            port: dst_port,
        },
        protocol: Protocol(protocol),
        bytes,
        packets,
        start,
        end,
    })
}

/// NetFlow / IPFIX bytes/packets are sometimes 4-byte, sometimes
/// 8-byte (configurable per exporter). Read either size as u64.
fn read_uint(slice: &[u8]) -> u64 {
    let mut acc: u64 = 0;
    for b in slice {
        acc = (acc << 8) | (*b as u64);
    }
    acc
}

fn resolve_timing(
    first_uptime_ms: Option<u32>,
    last_uptime_ms: Option<u32>,
    flow_start_abs_ms: Option<u64>,
    flow_end_abs_ms: Option<u64>,
    timing: Option<(SystemTime, Duration)>,
) -> (SystemTime, SystemTime) {
    if let (Some(start_ms), Some(end_ms)) = (flow_start_abs_ms, flow_end_abs_ms) {
        return (
            UNIX_EPOCH + Duration::from_millis(start_ms),
            UNIX_EPOCH + Duration::from_millis(end_ms),
        );
    }
    let (exporter_now, exporter_uptime) = timing.unwrap_or((UNIX_EPOCH, Duration::ZERO));
    let to_walltime = |ms: u32| -> SystemTime {
        let record_uptime = Duration::from_millis(ms as u64);
        if record_uptime >= exporter_uptime {
            exporter_now + (record_uptime - exporter_uptime)
        } else {
            exporter_now - (exporter_uptime - record_uptime)
        }
    };
    let start = first_uptime_ms.map(to_walltime).unwrap_or(exporter_now);
    let end = last_uptime_ms.map(to_walltime).unwrap_or(start);
    (start, end)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer() -> SocketAddr {
        "192.0.2.1:9995".parse().unwrap()
    }

    /// Build a v9 datagram containing a template + one data record.
    /// Template: IPV4_SRC, IPV4_DST, SRC_PORT, DST_PORT, PROTOCOL,
    /// IN_BYTES, IN_PKTS (24 bytes per record).
    fn build_v9_packet() -> Vec<u8> {
        let mut p = Vec::new();
        // Header
        p.extend_from_slice(&9u16.to_be_bytes()); // version
        p.extend_from_slice(&2u16.to_be_bytes()); // count (1 template + 1 data record)
        p.extend_from_slice(&60_000u32.to_be_bytes()); // sys_uptime ms
        p.extend_from_slice(&1_700_000_000u32.to_be_bytes()); // unix_secs
        p.extend_from_slice(&0u32.to_be_bytes()); // package_seq
        p.extend_from_slice(&1u32.to_be_bytes()); // source_id (observation domain)
        assert_eq!(p.len(), V9_HEADER_LEN);

        // Template flowset (id 0)
        let template_body_len = 4 /* template_id + field_count */ + 7 * 4 /* fields */;
        let template_flowset_len = 4 + template_body_len;
        p.extend_from_slice(&0u16.to_be_bytes()); // flowset_id (template)
        p.extend_from_slice(&(template_flowset_len as u16).to_be_bytes());
        p.extend_from_slice(&300u16.to_be_bytes()); // template_id
        p.extend_from_slice(&7u16.to_be_bytes()); // field_count
        for &(t, l) in &[
            (FIELD_IPV4_SRC_ADDR, 4u16),
            (FIELD_IPV4_DST_ADDR, 4u16),
            (FIELD_L4_SRC_PORT, 2u16),
            (FIELD_L4_DST_PORT, 2u16),
            (FIELD_PROTOCOL, 1u16),
            (FIELD_IN_BYTES, 8u16),
            (FIELD_IN_PKTS, 4u16),
        ] {
            p.extend_from_slice(&t.to_be_bytes());
            p.extend_from_slice(&l.to_be_bytes());
        }

        // Data flowset (id 300)
        let record_len = 4 + 4 + 2 + 2 + 1 + 8 + 4;
        // Pad to 4-byte boundary — many exporters do, we don't require it.
        let data_flowset_len = 4 + record_len;
        p.extend_from_slice(&300u16.to_be_bytes()); // flowset_id (data, matches template)
        p.extend_from_slice(&(data_flowset_len as u16).to_be_bytes());
        // Record: 10.0.0.5 → 8.8.8.8, src 51234, dst 53, UDP (17), 256 bytes, 4 packets
        p.extend_from_slice(&[10, 0, 0, 5]);
        p.extend_from_slice(&[8, 8, 8, 8]);
        p.extend_from_slice(&51_234u16.to_be_bytes());
        p.extend_from_slice(&53u16.to_be_bytes());
        p.push(17);
        p.extend_from_slice(&256u64.to_be_bytes());
        p.extend_from_slice(&4u32.to_be_bytes());

        p
    }

    #[test]
    fn v9_template_then_data_round_trips() {
        let pkt = build_v9_packet();
        let mut cache = TemplateCache::new();
        let flows = parse(&pkt, &mut cache, peer()).unwrap();
        assert_eq!(flows.len(), 1);
        assert_eq!(cache.len(), 1);
        let f = &flows[0];
        assert_eq!(f.src.ip, IpAddr::V4(Ipv4Addr::new(10, 0, 0, 5)));
        assert_eq!(f.dst.ip, IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)));
        assert_eq!(f.src.port, 51_234);
        assert_eq!(f.dst.port, 53);
        assert_eq!(f.protocol, Protocol::UDP);
        assert_eq!(f.bytes, 256);
        assert_eq!(f.packets, 4);
        assert_eq!(f.source, FlowSource::NetflowV9);
    }

    #[test]
    fn data_without_cached_template_is_silently_dropped() {
        // Build a packet with only a data set (no template).
        let mut p = Vec::new();
        p.extend_from_slice(&9u16.to_be_bytes());
        p.extend_from_slice(&1u16.to_be_bytes());
        p.extend_from_slice(&60_000u32.to_be_bytes());
        p.extend_from_slice(&1_700_000_000u32.to_be_bytes());
        p.extend_from_slice(&0u32.to_be_bytes());
        p.extend_from_slice(&1u32.to_be_bytes());

        p.extend_from_slice(&300u16.to_be_bytes()); // flowset_id
        p.extend_from_slice(&12u16.to_be_bytes()); // length (4 header + 8 body)
        p.extend_from_slice(&[0u8; 8]);

        let mut cache = TemplateCache::new();
        let flows = parse(&p, &mut cache, peer()).unwrap();
        assert_eq!(flows.len(), 0);
    }

    #[test]
    fn unsupported_version_errors() {
        let pkt = vec![0, 99, 0, 0];
        let mut cache = TemplateCache::new();
        assert!(matches!(
            parse(&pkt, &mut cache, peer()),
            Err(ParseError::UnsupportedVersion(99))
        ));
    }

    #[test]
    fn too_short_errors() {
        let pkt = vec![0u8];
        let mut cache = TemplateCache::new();
        assert!(matches!(
            parse(&pkt, &mut cache, peer()),
            Err(ParseError::TooShortForHeader(1))
        ));
    }
}
