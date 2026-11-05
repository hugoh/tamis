# Installing tamis

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

## mise

```sh
mise use -g github:hugoh/tamis
```
