---
title: Install
---

Release binaries are currently built for Linux only (x86_64 and aarch64).
macOS and Windows are tracked in
[#22](https://github.com/hugoh/tamis/issues/22).

## Homebrew (Linux)

```sh
brew install hugoh/tap/tamis
```

## Debian/Ubuntu (apt)

```sh
curl -1sLf 'https://dl.cloudsmith.io/public/hugoh-oss/tamis/setup.deb.sh' | sudo -E bash
sudo apt install tamis
```

## RHEL/Fedora (dnf)

```sh
curl -1sLf 'https://dl.cloudsmith.io/public/hugoh-oss/tamis/setup.rpm.sh' | sudo -E bash
sudo dnf install tamis
```

Package repository hosting is graciously provided by
[Cloudsmith](https://cloudsmith.com).

## Package files

Download the `.deb` or `.rpm` from the
[releases page](https://github.com/hugoh/tamis/releases) and install it
with your package manager:

```sh
# Debian/Ubuntu
sudo apt install ./tamis_*.deb

# RHEL/Fedora
sudo dnf install ./tamis_*.rpm
```

### What the packages install

The deb and the rpm hold the same files:

| Path | What |
|---|---|
| `/usr/bin/tamis` | the program |
| `/usr/lib/systemd/system/tamis.service` | the systemd unit, installed disabled |
| `/usr/lib/sysusers.d/tamis.conf` | creates the `tamis` user (home `/var/lib/tamis`) |
| `/usr/share/doc/tamis/README.systemd.md` | the setup steps |

Installing writes no configuration and no password, and starts nothing. To
finish, follow
[Running as a service](../user-guide/running-as-a-service/#from-the-deb-or-rpm-package).
Upgrading restarts the service if it is running, and removing the package stops
it; your configuration under `/etc/tamis` and the state under `/var/lib/tamis`
are left alone.

## mise

```sh
mise use -g github:hugoh/tamis
```

See [INSTALL.md](../install/) for installation instructions.

## Cargo

```sh
cargo install tamis
```

This builds from source, so it needs a Rust toolchain and a C compiler (SQLite
is bundled). The crate ships the web interface's files, so no `mise` step is
needed. It also works on macOS, which the release binaries don't cover yet.

## From source

The web interface's files are built, not committed, so use the `mise` tasks:

```sh
git clone https://github.com/hugoh/tamis
cd tamis
mise install
mise run build
```

A plain `cargo build --release` of a checkout also works, but run
`mise run vendor-web` first:
without the files it embeds, the web interface refuses to start; see
[Development](../contributing/).
