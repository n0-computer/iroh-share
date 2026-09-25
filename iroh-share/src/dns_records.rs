//! Parse one zone-style DNS record per line without filesystem directives.
use anyhow::{bail, ensure, Context, Result};
use hickory_proto::{
    op::{Message, MessageType},
    rr::{rdata::NULL, Name, RData, Record, RecordType},
    serialize::txt::{Parser, RDataParser},
};
use iroh_share_proto::NameKey;

pub fn packet(key: NameKey, text: &str) -> Result<Vec<u8>> {
    let origin: Name = format!("{key}.").parse()?;
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_common_records_and_uri_with_relative_owners() -> Result<()> {
        let key = NameKey([5; 32]);
        let text = "@ 300 IN A 192.0.2.1\n@ 300 IN AAAA 2001:db8::1\n@ 300 IN MX 10 mail.example.com.\n@ 300 IN TXT \"hello world\"\nwww 300 IN CNAME example.com.\n_sip._tcp 300 IN SRV 0 1 443 example.com.\n@ 300 IN HTTPS 0 example.com.\n";
        let uri = iroh_share_proto::redirect_records(&"https://example.com/path?q=1".parse()?);
        let bytes = packet(key, &(text.to_owned() + &uri))?;
        let decoded = simple_dns::Packet::parse(&bytes)?;
        assert_eq!(decoded.answers.len(), 8);
        assert!(decoded
            .answers
            .iter()
            .all(|rr| rr.name.to_string().ends_with(&key.to_string())));
        assert!(decoded
            .answers
            .iter()
            .any(|rr| rr.name.to_string() == format!("_https._tcp.{key}")));
        Ok(())
    }
    #[test]
    fn rejects_bad_oversized_and_external_records() {
        let key = NameKey([5; 32]);
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
