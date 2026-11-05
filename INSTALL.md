# Installing tamis

## Homebrew (macOS/Linux)

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
