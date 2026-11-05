//! Zone-style DNS records, one per line, converted to and from DNS packets.
//!
//! Owners are relative to the pkarr name (`@` is the name itself). Zone
//! directives such as `$INCLUDE` are rejected, so text from elsewhere cannot
//! read files.
use anyhow::{bail, ensure, Context, Result};
use hickory_proto::{
    op::{Message, MessageType},
    rr::{rdata::NULL, Name, RData, Record, RecordType},
    serialize::txt::{Parser, RDataParser},
};

/// Parses the records in `text` into a DNS packet for the name `key`.
///
/// Empty lines and lines starting with `;` are ignored.
pub fn packet(key: &[u8; 32], text: &str) -> Result<Vec<u8>> {
    let origin = origin(key)?;
    let mut packet = Message::new();
    packet.set_message_type(MessageType::Response);
    for (index, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with(';') {
            continue;
        }
        let parse = || -> Result<Vec<Record>> {
            let mut fields = line.split_whitespace();
            let owner = fields.next().context("missing owner")?;
            ensure!(!owner.starts_with('$'), "zone directives are not supported");
            let ttl: u32 = fields
                .next()
                .context("missing TTL")?
                .parse()
                .context("TTL must be seconds")?;
            ensure!(
                fields
                    .next()
                    .is_some_and(|class| class.eq_ignore_ascii_case("IN")),
                "expected IN class"
            );
            let kind = fields.next().context("missing record type")?;
            let name = if owner == "@" {
                origin.clone()
            } else {
                Name::parse(owner, Some(&origin))?
            };
            ensure!(
                origin.zone_of(&name),
                "record owner must be within this pkarr name"
            );
            if kind.eq_ignore_ascii_case("URI") {
                let priority: u16 = fields.next().context("missing URI priority")?.parse()?;
                let weight: u16 = fields.next().context("missing URI weight")?.parse()?;
                let value = fields.collect::<Vec<_>>().join(" ");
                let RData::TXT(target) = RData::try_from_str(RecordType::TXT, &value)? else {
                    unreachable!()
                };
                ensure!(!target.txt_data().is_empty(), "missing URI target");
                let mut bytes = priority.to_be_bytes().to_vec();
                bytes.extend(weight.to_be_bytes());
                for part in target.txt_data() {
                    bytes.extend_from_slice(part);
                }
                return Ok(vec![Record::from_rdata(
                    name,
                    ttl,
                    RData::Unknown {
                        code: RecordType::Unknown(256),
                        rdata: NULL::with(bytes),
                    },
                )]);
            }
            let (_, records) =
                Parser::new(format!("$TTL 300\n{line}\n"), None, Some(origin.clone())).parse()?;
            Ok(records
                .values()
                .flat_map(|set| set.records_without_rrsigs().cloned())
                .collect())
        };
        let records = parse().with_context(|| format!("DNS record on line {}", index + 1))?;
        for record in records {
            packet.add_answer(record);
        }
    }
    if packet.answers().is_empty() {
        bail!("enter at least one DNS record");
    }
    let bytes = packet.to_vec()?;
    ensure!(
        bytes.len() <= 1000,
        "DNS records exceed the 1000-byte pkarr packet limit"
    );
    Ok(bytes)
}

/// Renders a DNS packet as the zone-style lines [`packet`] accepts.
pub fn text(key: &[u8; 32], bytes: &[u8]) -> Result<String> {
    use std::fmt::Write;
    let origin = origin(key)?;
    let message = Message::from_vec(bytes)?;
    let mut text = String::new();
    for record in message.answers() {
        let name = record.name();
        ensure!(
            origin.zone_of(name),
            "record owner must be within this pkarr name"
        );
        let owner = if *name == origin {
            "@".to_owned()
        } else {
            let labels = name.num_labels() - origin.num_labels();
            let mut relative = Name::from_labels(name.iter().take(labels.into()))?;
            relative.set_fqdn(false);
            relative.to_string()
        };
        let ttl = record.ttl();
        match record.data() {
            RData::Unknown {
                code: RecordType::Unknown(256),
                rdata,
            } => {
                let (header, target) = rdata
                    .anything()
                    .split_first_chunk::<4>()
                    .context("URI record is too short")?;
                let priority = u16::from_be_bytes([header[0], header[1]]);
                let weight = u16::from_be_bytes([header[2], header[3]]);
                let target = quote(target);
                writeln!(text, "{owner} {ttl} IN URI {priority} {weight} {target}")?;
            }
            RData::TXT(txt) => {
                let parts: Vec<_> = txt.txt_data().iter().map(|part| quote(part)).collect();
                writeln!(text, "{owner} {ttl} IN TXT {}", parts.join(" "))?;
            }
            data => writeln!(text, "{owner} {ttl} IN {} {data}", record.record_type())?,
        }
    }
    Ok(text)
}

/// The name of `key` as a fully qualified domain name.
fn origin(key: &[u8; 32]) -> Result<Name> {
    Ok(format!("{}.", z32::encode(key)).parse()?)
}

/// Quotes a character string, escaping what the zone parser would interpret.
fn quote(bytes: &[u8]) -> String {
    let mut quoted = String::from('"');
    for &byte in bytes {
        match byte {
            b'"' | b'\\' => {
                quoted.push('\\');
                quoted.push(byte.into());
            }
            0x20..=0x7e => quoted.push(byte.into()),
            _ => quoted.push_str(&format!("\\{byte:03}")),
        }
    }
    quoted.push('"');
    quoted
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rendered_records_parse_back_to_the_same_packet() -> Result<()> {
        let key = &[5; 32];
        let text = "@ 300 IN A 192.0.2.1\n@ 300 IN AAAA 2001:db8::1\n@ 300 IN MX 10 mail.example.com.\n@ 300 IN TXT \"hello world\"\nwww 300 IN CNAME example.com.\n_sip._tcp 300 IN SRV 0 1 443 example.com.\n@ 300 IN HTTPS 1 example.com. port=8443\n@ 300 IN TXT \"a\\\"b\" \"c d\"\n";
        let uri = "_https._tcp 300 IN URI 0 0 \"https://example.com/path?q=1\"\n";
        let bytes = packet(key, &(text.to_owned() + uri))?;
        let rendered = super::text(key, &bytes)?;
        assert_eq!(packet(key, &rendered)?, bytes, "{rendered}");
        assert!(rendered.contains("www 300 IN CNAME"), "{rendered}");
        Ok(())
    }
    #[test]
    fn parses_common_records_and_uri_with_relative_owners() -> Result<()> {
        let key = &[5; 32];
        let text = "@ 300 IN A 192.0.2.1\n@ 300 IN AAAA 2001:db8::1\n@ 300 IN MX 10 mail.example.com.\n@ 300 IN TXT \"hello world\"\nwww 300 IN CNAME example.com.\n_sip._tcp 300 IN SRV 0 1 443 example.com.\n@ 300 IN HTTPS 0 example.com.\n";
        let uri = "_https._tcp 300 IN URI 0 0 \"https://example.com/path?q=1\"\n";
        let bytes = packet(key, &(text.to_owned() + uri))?;
        let decoded = Message::from_vec(&bytes)?;
        let names: Vec<_> = decoded
            .answers()
            .iter()
            .map(|rr| rr.name().to_string())
            .collect();
        let key = z32::encode(key);
        assert_eq!(names.len(), 8);
        assert!(names.iter().all(|name| name.ends_with(&format!("{key}."))));
        assert!(names.contains(&format!("_https._tcp.{key}.")));
        Ok(())
    }
    #[test]
    fn rejects_bad_oversized_and_external_records() {
        let key = &[5; 32];
        for text in [
            "",
            "$INCLUDE /etc/passwd",
            "@ 300 IN A nonsense",
            "example.com. 300 IN A 192.0.2.1",
        ] {
            assert!(packet(key, text).is_err(), "accepted {text}");
        }
        let large = (0..40)
            .map(|n| format!("n{n} 300 IN TXT \"{}\"\n", "x".repeat(100)))
            .collect::<String>();
        assert!(packet(key, &large).is_err());
    }
}
