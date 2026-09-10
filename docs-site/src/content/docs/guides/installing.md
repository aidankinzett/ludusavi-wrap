---
title: Install Spool
description: Download and install Spool on Windows or a Linux handheld, and add your first game.
---

Spool runs on **Windows** and **Linux**, including the gaming-handheld distros
(Bazzite, CachyOS, SteamOS / Steam Deck) and ARM64 handhelds running
[Armada](https://armadaos.dev/). The library, save sync, LAN sharing, and cloud
sync work the same on all of them.

## Download

Grab the latest build from the
[Releases page](https://github.com/aidankinzett/Spool/releases):

- **Windows** — the `Spool_<version>_x64-setup.exe` installer.
- **Linux (x86_64)** — the `Spool_amd64.AppImage`.
- **Linux (ARM64)** — the `Spool_aarch64.AppImage`.

Both platforms update themselves in place — when a new version is released,
Spool prompts to download and apply it on the next launch.

## Install

### Windows

Run the `…-setup.exe` installer and follow the prompts. Spool installs per-user
and adds a Start Menu entry.

### Linux

Run the AppImage:

```bash
./Spool_amd64.AppImage      # x86_64
./Spool_aarch64.AppImage    # ARM64
```

If your browser cleared the executable bit on download, mark it runnable first
with `chmod +x Spool_*.AppImage`.

To install it properly — drop the AppImage into `~/Applications` and add a
launcher entry (with icon) so Spool shows up in your desktop's application menu
(KDE Plasma, GNOME, etc.) — run the installer script instead:

```bash
curl -fsSL https://raw.githubusercontent.com/aidankinzett/Spool/master/scripts/install-appimage.sh | bash
```

It picks the AppImage matching your machine's architecture, downloads the latest
release, registers the launcher entry, and installs the icons. Re-run it anytime
to reinstall (the AppImage also self-updates in place), or pass `--uninstall` to
remove the AppImage and launcher entry.

On a Steam Deck or other handheld, do this from Desktop Mode the first time. To
launch your library from Game Mode without dropping to the desktop, install the
[Decky plugin](/decky/overview/).

:::note[Running Windows games on Linux]
The Linux build launches Windows `.exe` games through **Proton** using
[umu-launcher](https://github.com/Open-Wine-Components/umu-launcher) (`umu-run`).
It's the one dependency Spool doesn't bundle. On **Bazzite** it's already
installed; on most other distros it's a one-line package install; on **SteamOS /
Steam Deck** it needs a home-directory build because the root is read-only. See
[Installing umu-launcher](/guides/installing-umu/) for per-distro steps.
Settings → Compatibility also checks whether it's present and links the guide.

On **ARM64** no distro packages umu-launcher, but it runs fine there and
[Heroic](https://heroicgameslauncher.com/) downloads its own copy — Spool finds
that automatically, so installing Heroic is usually enough. x86_64 Linux games
also work on distros that run them through FEX: Armada registers FEX
system-wide, so Spool launches them like any other native game.
:::

## How Spool runs

Spool lives in your system tray. Closing the library window hides it to the tray
rather than quitting, so it stays ready to launch games with no cold-start delay.
Quit from the tray menu's **Quit Spool** item.

## Add your first game

1. Open Spool and choose **Add Game**.
2. Drop in (or browse for) the game's `.exe`.
3. Spool identifies the game, suggests cover art, and shows ranked matches so its
   saves can be tracked. Pick the right match and add it — or add it without save
   tracking if you'd rather not.

![Spool's Add Game flow after picking an executable, showing ranked ludusavi matches to confirm which game's saves to track.](../../../assets/screenshots/add-game.png)

Once a game is in your library, launching it from Spool restores the latest save
before play and backs it up when you quit.

## Next steps

- [Set up cloud save sync](/guides/cloud-saves/) so your saves follow you between
  devices.
- [Transfer games over your LAN](/guides/lan-transfers/) instead of
  re-downloading them.
