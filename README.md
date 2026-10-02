# iroh-share

Publish files, folders and names on the content-addressed web.

iroh-share is a small daemon that shares content over [iroh] and announces it on
the Mainline DHT, so anyone with a `https://<hash>.blake3.net` link can find you
and download it, without a server or anyone's permission. It also keeps
`https://<key>.pkarr.net` names pointing at your latest version. You can think
of it as [sendme], but running in the background with a separate user
interface: a desktop app, a terminal UI, or the command line.

For the background, see the blog post [Iroh global content discovery][blog].

Content stays available only while someone announces it, so run the daemon on a
machine that's on all the time: a box in the attic, a NAS, or a small VM.

## Install

Download the latest [release][releases]:

- **macOS** (Apple silicon) and **Windows** (x64): installers that set up the
  daemon for your account, start it at login, and connect the desktop app. See
  the [macOS](packaging/macos/README.md) and [Windows](packaging/windows/README.md)
  notes.
- **Synology NAS**: packages for DSM 7 and DSM 6. See the
  [Synology notes](packaging/synology/README.md).
- **Linux**: static `iroh-share`, `iroh-share-background` and `iroh-share-tui`
  binaries for x64 and arm64 that run on any distribution, including NAS systems
  with an old glibc. For a user service, see the
  [systemd example](packaging/systemd/README.md).

Or build from source:

```sh
cargo install --path iroh-share
cargo install --path iroh-share-gui
cargo install --path iroh-share-tui
```

To *browse* `blake3.net` and `pkarr.net` links, you also need
[Iroh Link Gateway][gateway] and its browser extension.

## Getting started

Start the daemon. On first start it prints a one-time pairing ticket:

```sh
iroh-share daemon
```

Then open the desktop app with `iroh-share-gui` or the terminal UI with
`iroh-share-tui`, and paste the ticket. The installers do this for you. The
frontends can run on another machine than the daemon; see
[pairing and remote control](docs/remote-access.md).

From the command line, sharing and downloading look like this:

```sh
iroh-share share /path/to/directory
iroh-share download 'https://<z32-hash>.blake3.net/' /path/to/target
iroh-share names create website --job 0
```

The shared directory gets a `blake3.net` link, and the name a stable `pkarr.net`
link that follows the directory when its contents change.

## Documentation

- [Using iroh-share](docs/usage.md): the daemon, desktop app and terminal UI,
  publishing, downloading, names, and backing up names.
- [Pairing and remote control](docs/remote-access.md): connecting frontends,
  managing several daemons, and access control.
- [Internals](docs/internals.md): the control protocol, storage, persistence,
  and development.
- [Building an iroh-share UI](iroh-share-proto/UI.md) and the
  [shared UX foundations](iroh-share-proto/UX.md), for frontend authors.

## Status

This is experimental. Sharing works, but there is **no privacy**: anybody can
look up the IP address of whoever shares a given piece of content. The builds
are unsigned, so your OS may ask you to approve them before they run.

## License

Copyright 2026 N0, INC.

This project is licensed under either of

 * Apache License, Version 2.0, ([LICENSE-APACHE](LICENSE-APACHE) or
   https://www.apache.org/licenses/LICENSE-2.0)
 * MIT license ([LICENSE-MIT](LICENSE-MIT) or
   https://opensource.org/licenses/MIT)

at your option.

## Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in this project by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.

[iroh]: https://www.iroh.computer
[sendme]: https://www.iroh.computer/sendme
[blog]: https://www.iroh.computer/blog/iroh-global-content-discovery
[releases]: https://github.com/n0-computer/iroh-share/releases/latest
[gateway]: https://github.com/n0-computer/iroh-content-discovery/releases/latest
