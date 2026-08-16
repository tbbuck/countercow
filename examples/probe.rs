//! Subscribe to an arbitrary EventPipe provider and report what actually arrives.
//!
//! Investigation tool for deciding what countercow could show beyond counters: it prints each
//! event's frequency, the field list from its metadata, and a sample of decoded values — so the
//! question "can we tell what is causing GC" gets an answer from the runtime rather than from
//! documentation.
//!
//! ```text
//! cargo run --example probe -- <pid> <provider> <keywords-hex> [seconds] [event-name-filter]
//! ```
//!
//! Useful starting points:
//!   Microsoft-Windows-DotNETRuntime 0x1        GC events
//!   Microsoft-Windows-DotNETRuntime 0x4000     lock contention
//!   Microsoft-Windows-DotNETRuntime 0x8000     exceptions

use std::collections::BTreeMap;
use std::io::Read;
use std::time::{Duration, Instant};

use countercow::ipc::commands::{self, Provider, TraceConfig};
use countercow::ipc::discovery;
use countercow::nettrace::blocks::NettraceParser;
use countercow::nettrace::metadata::{EventMetadata, FieldType};
use countercow::nettrace::payload::{decode_flat, Value};

struct EventStats {
    count: usize,
    fields: Vec<String>,
    /// First successfully decoded payload, as an illustration.
    sample: Option<String>,
    decode_errors: usize,
}

fn describe_fields(metadata: &EventMetadata) -> Vec<String> {
    fn walk(prefix: &str, ty: &FieldType, name: &str, out: &mut Vec<String>) {
        match ty {
            FieldType::Object(children) => {
                for child in children {
                    walk(&format!("{prefix}{name}."), &child.ty, &child.name, out);
                }
            }
            FieldType::Scalar(code) => out.push(format!("{prefix}{name}:{code:?}")),
            FieldType::Array(_) => out.push(format!("{prefix}{name}:Array")),
            FieldType::Unknown(code) => out.push(format!("{prefix}{name}:Unknown({code})")),
        }
    }

    let mut out = Vec::new();
    for field in &metadata.fields {
        walk("", &field.ty, &field.name, &mut out);
    }
    out
}

fn render(values: &BTreeMap<String, Value>) -> String {
    values
        .iter()
        .map(|(k, v)| {
            let rendered = match v {
                Value::String(s) => s.clone(),
                Value::Int(i) => i.to_string(),
                Value::UInt(u) => u.to_string(),
                Value::Float(f) => format!("{f:.3}"),
                Value::Bool(b) => b.to_string(),
                other => format!("{other:?}"),
            };
            format!("{k}={rendered}")
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let pid: u32 = args
        .next()
        .ok_or("usage: probe <pid> <provider> <keywords-hex> [seconds] [filter]")?
        .parse()?;
    let provider_name = args.next().ok_or("missing provider")?;
    let keywords_arg = args.next().unwrap_or_else(|| "0x1".into());
    let keywords = u64::from_str_radix(keywords_arg.trim_start_matches("0x"), 16)?;
    let seconds: u64 = args.next().unwrap_or_else(|| "5".into()).parse()?;
    let filter = args.next();

    let found = discovery::discover()?;
    let process = found
        .processes
        .iter()
        .find(|p| p.pid == pid)
        .ok_or_else(|| format!("no attachable .NET process with pid {pid}"))?;

    println!(
        "probing {} (pid {}) — provider {provider_name}, keywords 0x{keywords:x}, {seconds}s\n",
        process.name, pid
    );

    let config = TraceConfig {
        // GC events are far higher volume than counters, so give the runtime real headroom.
        circular_buffer_mb: 256,
        providers: vec![Provider {
            name: provider_name.clone(),
            keywords,
            // Verbose, so nothing is filtered out by level.
            level: 5,
            filter_data: String::new(),
        }],
    };

    let session = commands::start_tracing(&process.socket, &config)?;
    let session_id = session.session_id;

    // Stop from another thread: the parser blocks on the socket until the runtime closes it.
    let socket = process.socket.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(seconds));
        let _ = commands::stop_tracing(&socket, session_id);
    });

    let started = Instant::now();
    let mut parser = NettraceParser::new(BlockingRead(session.stream))?;
    let mut stats: BTreeMap<String, EventStats> = BTreeMap::new();
    let mut total = 0usize;
    let mut raw_seen: BTreeMap<i32, usize> = BTreeMap::new();

    while let Ok(Some(batch)) = parser.next_events() {
        for event in batch {
            let Some(metadata) = parser.metadata().get(event.metadata_id) else {
                continue;
            };
            // A numeric filter selects one event id and dumps raw payloads instead: manifest-based
            // providers send no schema, so the only way to confirm a hardcoded layout is to look
            // at the bytes.
            if let Some(filter) = &filter {
                // A comma-separated list of event ids selects those and dumps raw payloads.
                let wanted_ids: Vec<i32> =
                    filter.split(',').filter_map(|part| part.trim().parse().ok()).collect();
                match wanted_ids.is_empty() {
                    false => {
                        if !wanted_ids.contains(&metadata.event_id) {
                            continue;
                        }
                        // A couple of samples per id is enough to confirm a layout.
                        let seen = raw_seen.entry(metadata.event_id).or_insert(0);
                        if *seen < 2 {
                            *seen += 1;
                            println!(
                                "id {} v{} — {} payload bytes",
                                metadata.event_id,
                                metadata.version,
                                event.payload.len()
                            );
                            println!("  hex: {}", hex(&event.payload));
                            for found in utf16_strings(&event.payload) {
                                println!("  utf16 @{}: {:?}", found.0, found.1);
                            }
                            println!();
                        }
                    }
                    true => {
                        if !metadata.event_name.contains(filter.as_str()) {
                            continue;
                        }
                    }
                }
            }

            total += 1;
            // Manifest-based (ETW) providers such as Microsoft-Windows-DotNETRuntime send no
            // event name on the wire — only an id — so key on that and show whatever name there
            // is alongside.
            let key = format!(
                "id {:>3} v{}  {}",
                metadata.event_id,
                metadata.version,
                if metadata.event_name.is_empty() {
                    "(no name on the wire)"
                } else {
                    &metadata.event_name
                }
            );
            let entry = stats.entry(key).or_insert_with(|| EventStats {
                count: 0,
                fields: describe_fields(metadata),
                sample: None,
                decode_errors: 0,
            });
            entry.count += 1;

            match decode_flat(&metadata.fields, &event.payload) {
                Ok(values) => {
                    if entry.sample.is_none() {
                        entry.sample = Some(render(&values.into_iter().collect()));
                    }
                }
                Err(_) => entry.decode_errors += 1,
            }
        }
    }

    println!(
        "{total} events over {:.1}s, {} distinct\n",
        started.elapsed().as_secs_f64(),
        stats.len()
    );

    let mut by_count: Vec<_> = stats.iter().collect();
    by_count.sort_by_key(|(_, stat)| std::cmp::Reverse(stat.count));

    for (name, stat) in by_count {
        let rate = stat.count as f64 / started.elapsed().as_secs_f64();
        println!("{name}  —  {} events ({rate:.0}/s)", stat.count);
        if stat.decode_errors > 0 {
            println!("  UNDECODED: {} of them", stat.decode_errors);
        }
        println!("  fields: {}", stat.fields.join(", "));
        if let Some(sample) = &stat.sample {
            let truncated: String = sample.chars().take(220).collect();
            println!("  sample: {truncated}");
        }
        println!();
    }

    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().take(96).map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(" ")
}

/// Find NUL-terminated UTF-16LE runs in a payload, so a hardcoded field layout can be checked
/// against where the strings actually sit.
fn utf16_strings(bytes: &[u8]) -> Vec<(usize, String)> {
    let mut found = Vec::new();
    let mut offset = 0;

    while offset + 4 <= bytes.len() {
        let mut units = Vec::new();
        let mut cursor = offset;
        while cursor + 2 <= bytes.len() {
            let unit = u16::from_le_bytes([bytes[cursor], bytes[cursor + 1]]);
            if unit == 0 {
                break;
            }
            // Printable ASCII range only, to avoid reading numeric fields as text.
            if !(0x20..0x7f).contains(&unit) {
                units.clear();
                break;
            }
            units.push(unit);
            cursor += 2;
        }
        if units.len() >= 4 {
            found.push((offset, String::from_utf16_lossy(&units)));
            offset = cursor + 2;
        } else {
            offset += 2;
        }
    }
    found
}

/// `read_exact` on a `UnixStream` can return early at a message boundary; the parser needs the
/// full count. Wrapping makes short reads retry rather than surfacing as truncation.
struct BlockingRead(std::os::unix::net::UnixStream);

impl Read for BlockingRead {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.0.read(buf)
    }
}
