# pkarr-publish

Edit and republish a directory of [pkarr] records.

Each name is a pair of files in one directory:

- `<name>.pkarr`: the signed record, in the pkarr `SignedPacket::as_bytes`
  layout. This is the format iroh-share exports.
- `<name>.key`: the 32-byte Ed25519 signing seed. It is needed only where the
  name is edited, and must not be readable by other users.

Republishing needs only the signed record, so a server can keep a name alive
without ever holding its key. If the server is compromised, an attacker can
stop publishing, but cannot change the name.

```sh
pkarr-publish list         # names, public keys, current versions
pkarr-publish show <name>  # the records as zone-style text
pkarr-publish edit <name>  # edit in $EDITOR, sign, publish once
pkarr-publish daemon       # republish every record until stopped
```

The directory is the current one, or `--dir` / `PKARR_DIR`.

## Records

One record per line, with owners relative to the name (`@` is the name itself):

```dns
@ 300 IN TXT "203.0.113.1:60125"
@ 300 IN TXT "198.51.100.2:33445"
www 300 IN CNAME example.com.
```

Lines starting with `;` are ignored. The whole packet must fit in 1000 bytes.

## Creating and editing a name

`edit` needs `<name>.key`. If there is no `<name>.pkarr` yet, it starts with
no records. A key is just 32 random bytes:

```sh
head -c 32 /dev/urandom > foo.key && chmod 600 foo.key
pkarr-publish edit foo
```

`edit` signs with the current time as the sequence number, writes
`<name>.pkarr`, and publishes it once. A name that has only `<name>.pkarr`
is read-only on that machine.

To take over a name from iroh-share, export it with
`iroh-share names export-name <label> name.zip`. The archive contains
`<public key>.key` and `<public key>.pkarr`; rename them to `<name>.key` and
`<name>.pkarr`.

## Republishing

`daemon` republishes every `.pkarr` file every ten minutes, retries failures
after thirty seconds, and picks up new or changed files within ten seconds. It
publishes exactly what is in the directory and never reads records from the
DHT, so to change a name on a server, copy the new `.pkarr` file there.

A signed record does not expire. A server keeps republishing the version it
has, so update or stop servers when a name changes.

## License

This project is licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or
  https://www.apache.org/licenses/LICENSE-2.0)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or
  https://opensource.org/licenses/MIT)

at your option.

[pkarr]: https://pkarr.org
