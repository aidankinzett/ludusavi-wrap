<script lang="ts">
  import { onMount, tick } from 'svelte';
  import {
    Check,
    Cloud,
    CloudOff,
    Cpu,
    Download,
    Folder,
    Gamepad2,
    Grid2x2,
    HardDrive,
    KeyRound,
    Layers,
    Library,
    MonitorSmartphone,
    RefreshCcw,
    Shield,
    Sparkles,
    Wifi,
  } from '@lucide/svelte';
  import { goto } from '$app/navigation';
  import { getVersion } from '@tauri-apps/api/app';
  import { appLocalDataDir } from '@tauri-apps/api/path';
  import { getCurrentWindow } from '@tauri-apps/api/window';
  import { emit, listen } from '@tauri-apps/api/event';
  import { api, type DeckyPluginInfo } from '$lib/api';
  import { toasts } from '$lib/toasts.svelte';
  import { confirmSteamRestart } from '$lib/steamRestart';
  import { isNewerVersion } from '$lib/format';
  import { checkForUpdateInteractive } from '$lib/updater';
  import type { ConfigData, DepStatus, FolderCapacity, LanPeer, OfflinePrepProgress, ProtonVersion, SyncStatus } from '$lib/types';
  import AppChrome from '$lib/components/AppChrome.svelte';
  import MonoLabel from '$lib/components/MonoLabel.svelte';
  import Btn from '$lib/components/Btn.svelte';
  import Pill from '$lib/components/Pill.svelte';
  import TextField from '$lib/components/TextField.svelte';
  import NumberField from '$lib/components/NumberField.svelte';
  import Toggle from '$lib/components/Toggle.svelte';
  import SettingsCard from '$lib/components/SettingsCard.svelte';
  import SettingsRow from '$lib/components/SettingsRow.svelte';
  import LibraryStorage from '$lib/components/LibraryStorage.svelte';
  import Segmented from '$lib/components/Segmented.svelte';
  import Slider from '$lib/components/Slider.svelte';
  import Select, { type SelectOption } from '$lib/components/Select.svelte';
  import ButtonLegend from '$lib/components/ButtonLegend.svelte';
  import type { GpButton } from '$lib/components/GamepadButton.svelte';
  import { uiMode } from '$lib/uiMode.svelte';
  import { gamepadScope, inputMode } from '$lib/gamepad';

  let config = $state<ConfigData | null>(null);
  let error = $state<string | null>(null);
  let activeGroup = $state('general');
  let navEl = $state<HTMLElement>();
  let peers = $state<LanPeer[]>([]);
  let appVersion = $state<string | null>(null);
  let syncStatus = $state<SyncStatus | null>(null);

  // "Check for updates" button: 'idle' before a click, 'checking' during the
  // round-trip, then a terminal result that drives the inline status line.
  let updateCheck = $state<'idle' | 'checking' | 'available' | 'up-to-date' | 'error'>('idle');
  let updateCheckMsg = $state<string | null>(null);

  async function onCheckForUpdates() {
    if (updateCheck === 'checking') return;
    updateCheck = 'checking';
    updateCheckMsg = null;
    const result = await checkForUpdateInteractive();
    updateCheck = result.status;
    if (result.status === 'available') {
      updateCheckMsg = `v${result.version} is available — see the install prompt.`;
    } else if (result.status === 'up-to-date') {
      updateCheckMsg = "You're on the latest version.";
    } else {
      updateCheckMsg = `Couldn't check for updates: ${result.error}`;
    }
  }

  // WebDAV URL/username/password are local state, NOT bound to `config`. They
  // reach the backend only via connectWebdav() → setCloudWebdav (which persists
  // them server-side), so a partial edit can never silently ride along on an
  // unrelated field's save or be lost on close. Seeded from config on load and
  // re-seeded after a successful connect. (#295)
  let webdavUrl = $state('');
  let webdavUsername = $state('');
  let webdavPassword = $state('');
  let webdavConnecting = $state(false);

  // Offline mode: the toggle drives the long-running go_offline / go_online
  // sweeps (save pulls, Proton runtime download, reconcile uploads), with live
  // progress from the `offline:prep` event. Disabled while one is in flight.
  let offlineBusy = $state<'idle' | 'going-offline' | 'going-online'>('idle');
  let offlinePrep = $state<OfflinePrepProgress | null>(null);

  async function setOfflineMode(on: boolean) {
    if (!config || offlineBusy !== 'idle') return;
    offlineBusy = on ? 'going-offline' : 'going-online';
    offlinePrep = null;
    try {
      if (on) {
        const r = await api.goOffline();
        config.offline_mode = true;
        const synced = r.pulled.length + r.up_to_date + r.local_newer.length;
        const issues = [
          r.conflicts.length &&
            `${r.conflicts.length} cloud conflict${r.conflicts.length === 1 ? '' : 's'} (${r.conflicts.join(', ')})`,
          r.errors.length && `${r.errors.length} game${r.errors.length === 1 ? '' : 's'} failed to sync`,
          r.proton_runtime.startsWith('failed') && `Proton runtime download failed`,
        ].filter(Boolean);
        toasts.show({
          kind: issues.length ? 'warn' : 'ok',
          label: 'OFFLINE MODE',
          title: issues.length ? 'Offline, with warnings' : 'Ready to play offline',
          sub: issues.length
            ? issues.join(' · ')
            : r.cloud_configured
              ? `Saves are current for ${synced} game${synced === 1 ? '' : 's'}${r.proton_runtime === 'ready' ? ' · Proton runtime downloaded' : ''}.`
              : 'Cloud sync is off — saves keep backing up locally.',
        });
      } else {
        const r = await api.goOnline();
        config.offline_mode = false;
        const issues = [
          r.conflicts.length &&
            `${r.conflicts.length} cloud conflict${r.conflicts.length === 1 ? '' : 's'} (${r.conflicts.join(', ')})`,
          r.errors.length && `${r.errors.length} game${r.errors.length === 1 ? '' : 's'} failed to sync`,
        ].filter(Boolean);
        toasts.show({
          kind: issues.length ? 'warn' : 'ok',
          label: 'OFFLINE MODE',
          title: issues.length ? 'Back online, with warnings' : 'Back online',
          sub: issues.length
            ? issues.join(' · ')
            : !r.reachable
              ? 'Cloud remote not reachable yet — saves will sync on the next launch.'
              : r.uploaded.length
                ? `Uploaded saves for ${r.uploaded.join(', ')}.`
                : 'Everything was already in sync.',
        });
      }
    } catch (e) {
      toasts.show({
        kind: 'bad',
        label: 'OFFLINE MODE',
        title: on ? "Couldn't go offline" : "Couldn't go back online",
        sub: String(e),
      });
    } finally {
      offlineBusy = 'idle';
      offlinePrep = null;
      try {
        syncStatus = await api.currentSyncStatus();
      } catch (e) {
        console.error('[settings] currentSyncStatus failed:', e);
      }
    }
  }

  let isLinux = $state(false);
  let protonVersions = $state<ProtonVersion[]>([]);
  let deps = $state<DepStatus[]>([]);
  let depsLoading = $state(false);

  let deckyPlugin = $state<DeckyPluginInfo | null>(null);
  let deckyInstalling = $state(false);

  const OAUTH_PROVIDERS = ['google-drive', 'onedrive', 'dropbox', 'box'];
  let oauthConnecting = $state(false);
  let remoteExists = $state(false);

  const CLOUD_PROVIDER_OPTIONS: SelectOption[] = [
    { value: '', label: 'Disabled' },
    { value: 'custom', label: 'Custom (rclone remote)' },
    { value: 'google-drive', label: 'Google Drive' },
    { value: 'onedrive', label: 'OneDrive' },
    { value: 'dropbox', label: 'Dropbox' },
    { value: 'box', label: 'Box' },
    { value: 'ftp', label: 'FTP' },
    { value: 'smb', label: 'SMB' },
    { value: 'webdav', label: 'WebDAV' },
  ];
  // Default-Proton picker: Auto + each detected build.
  const protonOptions = $derived<SelectOption[]>([
    { value: '', label: 'Auto (newest installed)' },
    ...protonVersions.map((p) => ({ value: p.path, label: p.name })),
  ]);

  onMount(() => {
    let unlisten: (() => void) | undefined;
    let unlistenPeers: (() => void) | undefined;
    let unlistenOfflinePrep: (() => void) | undefined;
    let destroyed = false;

    // Register the live-status listener up front, independent of the slow data
    // fetch below. Registering it at the *end* of setup() meant a quick unmount
    // during the await chain would run the cleanup before `unlisten` was
    // assigned, leaking a global subscription that keeps firing into a dead
    // component. The destroyed guard unlistens a handle that resolves after an
    // already-unmounted teardown. (#294)
    listen<SyncStatus>('sync:status-changed', (ev) => {
      syncStatus = ev.payload;
    })
      .then((fn) => {
        if (destroyed) fn();
        else unlisten = fn;
      })
      .catch((e) => console.error('[settings] sync listener failed:', e));

    // Keep the discovered-peers list live. Without this it's a one-shot fetch
    // at mount, so peers that appear or drop off while Settings is open never
    // update. `lan:peers-changed` is a bare signal — re-fetch on it. Same
    // destroyed-guard as the sync listener above. (#389)
    listen<null>('lan:peers-changed', () => {
      api
        .listLanPeers()
        .then((p) => {
          if (!destroyed) peers = p;
        })
        .catch((e) => console.error('[settings] listLanPeers (peers-changed) failed:', e));
    })
      .then((fn) => {
        if (destroyed) fn();
        else unlistenPeers = fn;
      })
      .catch((e) => console.error('[settings] peers listener failed:', e));

    // Live progress for the offline-mode prepare/reconcile sweeps. Same
    // destroyed-guard as the sync listener above.
    listen<OfflinePrepProgress>('offline:prep', (ev) => {
      if (offlineBusy !== 'idle') offlinePrep = ev.payload;
    })
      .then((fn) => {
        if (destroyed) fn();
        else unlistenOfflinePrep = fn;
      })
      .catch((e) => console.error('[settings] offline prep listener failed:', e));

    const setup = async () => {
      // Only the core config is fatal — the whole page is bound to it. Loading
      // it is the one step that gates the error screen. (#372)
      try {
        config = await api.getConfig();
        seedWebdavFields();
      } catch (e) {
        error = String(e);
        return;
      }

      // Land controller/keyboard focus on the active settings group once the
      // sidebar is in the DOM. The nav scope's initial autofocus runs before
      // config loads (only the chrome Back button exists then), so it would
      // otherwise settle on Back. Mouse users are left undisturbed.
      await tick();
      if (inputMode() === 'gamepad' || inputMode() === 'keyboard') {
        navEl?.querySelector<HTMLElement>('[data-gp-autofocus]')?.focus();
      }

      // Secondary loads degrade gracefully: each failing one leaves a safe
      // default and the rest of the page usable, so a misconfigured machine
      // (e.g. an unreachable rclone remote or broken Proton detection) doesn't
      // lock the user out of the very controls they need to fix it. (#372)
      try {
        peers = await api.listLanPeers();
      } catch (e) {
        console.error('[settings] listLanPeers failed:', e);
      }
      try {
        appVersion = await getVersion();
      } catch (e) {
        console.error('[settings] getVersion failed:', e);
      }
      try {
        syncStatus = await api.currentSyncStatus();
      } catch (e) {
        console.error('[settings] currentSyncStatus failed:', e);
      }
      try {
        isLinux = (await api.appPlatform()) === 'linux';
      } catch (e) {
        console.error('[settings] appPlatform failed:', e);
      }
      if (isLinux) {
        try {
          protonVersions = await api.listProtonVersions();
        } catch (e) {
          console.error('[settings] listProtonVersions failed:', e);
        }
        try {
          deps = await api.checkDependencies();
        } catch (e) {
          console.error('[settings] checkDependencies failed:', e);
        }
        try {
          deckyPlugin = await api.deckyPluginStatus();
        } catch (e) {
          console.error('[settings] deckyPluginStatus failed:', e);
        }
      }
      if (config && OAUTH_PROVIDERS.includes(config.cloud_provider)) {
        try {
          remoteExists = await api.checkCloudRemoteExists(config.cloud_provider);
        } catch (e) {
          console.error('[settings] checkCloudRemoteExists failed:', e);
        }
      }
    };

    setup();

    return () => {
      destroyed = true;
      unlisten?.();
      unlistenPeers?.();
      unlistenOfflinePrep?.();
    };
  });

  async function persist(): Promise<boolean> {
    if (!config) return false;
    try {
      // Don't reassign `config` from the response. updateConfig echoes back
      // the snapshot it was sent, so overwriting the live object with the
      // resolved (now-stale) echo would silently discard any field the user
      // edited while this round-trip was in flight — every field commits via
      // its own async persist(). The local `config` is already the source of
      // truth the snapshot was built from. (#272)
      await api.updateConfig($state.snapshot(config));
      return true;
    } catch (e) {
      // A save failure is transient (e.g. a config.yaml write error projecting
      // cloud settings) — toast it rather than setting the fatal `error`, which
      // would discard the whole Settings UI for a recoverable hiccup.
      toasts.show({
        kind: 'bad',
        label: 'SETTINGS · SAVE',
        title: "Couldn't save settings",
        sub: String(e),
      });
      return false;
    }
  }

  async function setUiMode(mode: ConfigData['ui_mode']) {
    if (!config) return;
    config.ui_mode = mode;
    const ok = await persist();
    if (!ok) return;
    await uiMode.init(mode);
    await emit('config:ui-mode-changed');
    const win = getCurrentWindow();
    if (win.label !== 'main') {
      if (uiMode.resolved === 'touch') {
        toasts.show({ kind: 'ok', label: 'DISPLAY', title: 'Gamepad mode on', sub: 'Switched the library to the gamepad layout.' });
        await win.close();
      }
    } else if (uiMode.resolved === 'desktop') {
      await goto('/');
    }
  }

  async function refreshDeps() {
    depsLoading = true;
    try { deps = await api.checkDependencies(); } finally { depsLoading = false; }
  }

  let detectingUmu = $state(false);
  // `detect_umu_run` persists the path it finds on the backend, so mirror the
  // result into the live config rather than calling persist() and writing the
  // stale local value back over it.
  async function detectUmuRun() {
    if (!config) return;
    detectingUmu = true;
    try {
      const found = await api.detectUmuRun();
      if (found) {
        config.umu_run_path = found;
        toasts.show({ kind: 'ok', label: 'COMPATIBILITY', title: 'Found umu-run', sub: found });
      } else {
        toasts.show({
          kind: 'warn',
          label: 'COMPATIBILITY',
          title: 'No umu-run found',
          sub: "Install umu-launcher or Heroic, or enter the path manually.",
        });
      }
      await refreshDeps();
    } catch (e) {
      toasts.show({ kind: 'bad', label: 'COMPATIBILITY', title: 'Detection failed', sub: String(e) });
    } finally {
      detectingUmu = false;
    }
  }

  let addingToSteam = $state(false);
  async function addSpoolToSteam() {
    if (!(await confirmSteamRestart())) return;
    addingToSteam = true;
    try {
      const r = await api.addSpoolToSteam();
      const extras = r.extras_placed.length ? ` · ${r.extras_placed.join(', ')} art placed` : '';
      const sub = r.steam_restarted
        ? `Restarting Steam — launch Spool from your library${extras}. In Game Mode it opens fullscreen.`
        : `Restart Steam, then launch Spool from your library${extras}. In Game Mode it opens fullscreen.`;
      toasts.show({
        kind: 'ok',
        label: 'STEAM',
        title: 'Added Spool to Steam',
        sub,
      });
    } catch (e) {
      toasts.show({ kind: 'bad', label: 'STEAM · FAILED', title: "Couldn't add to Steam", sub: String(e) });
    } finally {
      addingToSteam = false;
    }
  }

  let syncingCollection = $state(false);
  async function syncSpoolCollection() {
    syncingCollection = true;
    try {
      await api.syncSpoolSteamCollection();
      toasts.show({
        kind: 'ok',
        label: 'STEAM',
        title: 'Rebuilt Spool collection',
        sub: 'Restart Steam to see the “Spool” collection. TabMaster can turn it into a tab via its Collection filter.',
      });
    } catch (e) {
      toasts.show({ kind: 'bad', label: 'STEAM · FAILED', title: "Couldn't rebuild collection", sub: String(e) });
    } finally {
      syncingCollection = false;
    }
  }

  async function installDeckyPlugin() {
    deckyInstalling = true;
    try {
      let outcome;
      try {
        outcome = await api.installDeckyPlugin();
      } catch (e) {
        toasts.show({ kind: 'bad', label: 'DECKY', title: "Couldn't install plugin", sub: String(e) });
        return;
      }
      // Install succeeded — a status-refresh failure here must not be reported
      // as an install failure.
      try {
        deckyPlugin = await api.deckyPluginStatus();
      } catch (e) {
        console.error('[settings] deckyPluginStatus failed after install:', e);
      }
      if (outcome.loaderRestarted) {
        toasts.show({ kind: 'ok', label: 'DECKY', title: 'Backup plugin installed', sub: 'Decky was restarted — the Spool Backup plugin is now active.' });
      } else {
        toasts.show({ kind: 'warn', label: 'DECKY', title: 'Backup plugin installed', sub: "The plugin was copied but Decky didn't restart — reboot or restart Decky to load it." });
      }
    } finally {
      deckyInstalling = false;
    }
  }

  // Copy the persisted WebDAV URL/username into the local edit fields. Called on
  // load and after a successful connect (which re-fetches config). (#295)
  function seedWebdavFields() {
    if (!config) return;
    webdavUrl = config.cloud_webdav_url ?? '';
    webdavUsername = config.cloud_webdav_username ?? '';
  }

  async function connectWebdav() {
    if (!config) return;
    webdavConnecting = true;
    try {
      await api.setCloudWebdav(webdavUrl.trim(), webdavUsername.trim(), webdavPassword, 'other');
      webdavPassword = '';
      config = await api.getConfig();
      seedWebdavFields();
      toasts.show({ kind: 'ok', label: 'CLOUD', title: 'WebDAV connected', sub: 'Saves will sync to this remote.' });
    } catch (e) {
      toasts.show({ kind: 'bad', label: 'CLOUD · WEBDAV', title: "Couldn't connect", sub: String(e) });
    } finally {
      webdavConnecting = false;
    }
  }

  // ── Library folders (per-drive install roots) ──────────────────────────────
  // path → total/available bytes for the folder's drive, refreshed when the
  // configured folder list changes. Drives the capacity bars in LibraryStorage.
  let libFolderCapacity = $state<Record<string, FolderCapacity>>({});

  // Generation counter: only the latest refresh writes to libFolderCapacity.
  // Rapid folder add/remove can leave two refreshes in flight; without this a
  // slow earlier run (a sleeping drive) could resolve last and overwrite the
  // newer map with capacity for folders that no longer exist.
  let capSeq = 0;

  async function refreshLibFolderCapacity() {
    if (!config) return;
    const seq = ++capSeq;
    const folders = config.library_folders;
    const entries = await Promise.all(
      folders.map(async (f) => {
        try {
          return [f.path, await api.folderCapacity(f.path)] as const;
        } catch {
          return [f.path, { total_bytes: 0, available_bytes: 0, mount_point: '' }] as const;
        }
      }),
    );
    if (seq === capSeq) libFolderCapacity = Object.fromEntries(entries);
  }

  // Keep capacity figures fresh as the configured folder list changes.
  $effect(() => {
    if (config?.library_folders) void refreshLibFolderCapacity();
  });

  // Prepare + dedupe + persist a new library folder. Returns true on success so
  // LibraryStorage can close its add panel. Toasts on dedupe / failure.
  async function addLibraryFolder(raw: string): Promise<boolean> {
    if (!config) return false;
    const trimmed = raw.trim();
    if (!trimmed) return false;
    try {
      const canonical = await api.prepareLibraryFolder(trimmed);
      if (config.library_folders.some((f) => f.path === canonical)) {
        toasts.show({ kind: 'ok', label: 'LIBRARY', title: 'Already a library folder', sub: canonical });
        return true;
      }
      // Restore the prior list if the save fails, so the UI doesn't drift from
      // what's on disk (persist() toasts the error and returns false).
      const prev = config.library_folders;
      config.library_folders = [...prev, { path: canonical, label: null, default_install: false }];
      if (!(await persist())) {
        config.library_folders = prev;
        return false;
      }
      // The $effect watching `library_folders` refreshes the capacity map.
      return true;
    } catch (e) {
      toasts.show({ kind: 'bad', label: 'LIBRARY', title: "Couldn't add folder", sub: String(e) });
      return false;
    }
  }

  async function removeLibFolder(path: string) {
    if (!config) return;
    const prev = config.library_folders;
    config.library_folders = prev.filter((f) => f.path !== path);
    if (!(await persist())) config.library_folders = prev;
  }

  /** The folder new installs (LAN downloads) land in — the flagged one, else
   * the first. Mirrors `ConfigData::default_install_folder` in Rust. */
  const defaultInstallFolder = $derived(
    config?.library_folders.find((f) => f.default_install) ?? config?.library_folders[0] ?? null,
  );

  async function setDefaultLibFolder(path: string) {
    if (!config) return;
    const prev = config.library_folders;
    config.library_folders = prev.map((f) => ({ ...f, default_install: f.path === path }));
    if (!(await persist())) config.library_folders = prev;
  }

  function onLanPortCommit() {
    if (!config) return;
    if (!Number.isFinite(config.lan_share_port) || config.lan_share_port < 1024) config.lan_share_port = 47632;
    if (config.lan_share_port > 65535) config.lan_share_port = 65535;
    persist();
  }

  function onRetentionCommit() {
    if (!config) return;
    if (!Number.isFinite(config.save_retention_full)) config.save_retention_full = 5;
    // Floor of 3, not 1: at full=1 ludusavi overwrites a single in-place
    // backup, so a mid-backup kill can truncate the only copy.
    config.save_retention_full = Math.min(10, Math.max(3, Math.round(config.save_retention_full)));
    persist();
  }

  async function connectOAuth() {
    oauthConnecting = true;
    try {
      await api.connectCloudOAuth(config!.cloud_provider);
      remoteExists = true;
      syncStatus = await api.refreshSyncStatus();
      toasts.show({ kind: 'ok', label: 'CLOUD', title: 'Connected', sub: 'Authenticated — saves will sync.' });
    } catch (e) {
      toasts.show({ kind: 'bad', label: 'CLOUD · OAUTH', title: "Couldn't connect", sub: String(e) });
    } finally {
      oauthConnecting = false;
    }
  }

  async function cancelOAuth() {
    try { await api.cancelCloudOAuth(); } catch (e) { console.warn('[oauth] cancel failed:', e); }
    oauthConnecting = false;
  }

  // "Enabled" = the user has picked a provider (drives the on/off toggle and
  // reveals the sub-fields). Distinct from "configured" below.
  const cloudEnabled = $derived(!!config?.cloud_provider);
  // "Configured" = a remote is actually connected, not merely selected — so the
  // status pill / nav dot don't claim a working sync the moment the toggle is
  // flipped (which only defaults the provider to webdav). Each provider needs
  // its own connection detail: custom → an rclone remote name, webdav → a URL,
  // OAuth → an authorised remote (`remoteExists`). (#389)
  const cloudConfigured = $derived.by(() => {
    const p = config?.cloud_provider;
    if (!p) return false;
    if (p === 'custom') return !!config?.cloud_remote;
    if (p === 'webdav') return !!config?.cloud_webdav_url;
    return remoteExists;
  });
  const cloudOnline = $derived(syncStatus?.reachability === 'online');
  const deckOk = $derived(
    !isLinux || (!!deckyPlugin?.installed && deps.every(d => d.found))
  );
  const lanOk = $derived(config?.lan_share_enabled);

  type NavGroup = {
    id: string;
    title: string;
    sub: string;
    platform?: 'linux';
    status: 'ok' | 'warn' | 'off';
  };

  const NAV_GROUPS = $derived<NavGroup[]>([
    {
      id: 'general',
      title: 'General',
      sub: 'Display & artwork',
      status: 'ok',
    },
    {
      id: 'library',
      title: 'Library',
      sub: 'Folders & installed games',
      status: 'ok',
    },
    {
      id: 'saves',
      title: 'Saves',
      sub: 'Backups & cloud sync',
      status: cloudOnline ? 'ok' : cloudConfigured ? 'warn' : 'off',
    },
    ...(isLinux
      ? [{
          id: 'deck',
          title: 'Steam Deck & Linux',
          sub: 'Decky · Proton',
          platform: 'linux' as const,
          status: (deckOk ? 'ok' : 'warn') as 'ok' | 'warn',
        }]
      : []),
    {
      id: 'network',
      title: 'Network',
      sub: 'LAN sharing · device',
      status: lanOk ? 'ok' : 'off',
    },
    {
      id: 'advanced',
      title: 'Advanced',
      sub: 'Debug & reset',
      status: 'off' as const,
    },
  ]);

  const GROUP_BLURB: Record<string, string> = {
    general: 'Display behaviour and the look of your cover-art shelf.',
    library: 'Install roots for your games, and everything installed across them.',
    saves: 'How saves are backed up locally and mirrored to the cloud.',
    deck: 'Bring Spool into SteamOS Game Mode, and run Windows games via Proton.',
    network: 'Share game installs across your home network.',
    advanced: 'Diagnostics and escape hatches.',
  };

  const GROUP_TITLE: Record<string, string> = {
    general: 'General',
    library: 'Library',
    saves: 'Saves',
    deck: 'Steam Deck & Linux',
    network: 'Network',
    advanced: 'Advanced',
  };

  // Bumpers cycle the settings groups, like switching tabs on a console.
  function switchGroup(dir: -1 | 1) {
    const ids = NAV_GROUPS.map((g) => g.id);
    const i = ids.indexOf(activeGroup);
    if (i === -1) return;
    activeGroup = ids[(i + dir + ids.length) % ids.length];
  }

  function settingsButton(btn: string) {
    if (btn === 'LeftTrigger') switchGroup(-1);
    else if (btn === 'RightTrigger') switchGroup(1);
  }

  const settingsLegend: { button: GpButton; label: string }[] = [
    { button: 'a', label: 'Select' },
    { button: 'b', label: 'Back' },
    { button: 'lb', label: 'Prev' },
    { button: 'rb', label: 'Next' },
  ];
</script>

<div
  class="flex h-screen flex-col bg-bg-0 text-ink-0"
  use:gamepadScope={{ onBack: () => history.back(), onButton: settingsButton }}
>
  <AppChrome sub="SETTINGS" onback={() => history.back()} />

  {#if error}
    <div class="flex-1 flex items-center justify-center p-8">
      <div class="rounded-md border border-bad/40 bg-bad/10 p-4 text-sm text-bad max-w-lg">
        <div class="mb-1 font-medium">Failed to load settings</div>
        <code class="font-mono text-[11px] opacity-80">{error}</code>
      </div>
    </div>
  {:else if !config}
    <div class="flex-1 flex items-center justify-center">
      <p class="font-mono text-[11px] uppercase tracking-[0.12em] text-ink-3">Loading…</p>
    </div>
  {:else}
    <div class="flex flex-1 min-h-0" style="display:grid; grid-template-columns: 264px 1fr">

      <!-- ── Sidebar ── -->
      <nav bind:this={navEl} class="overflow-y-auto border-r border-line-1 bg-bg-1 flex flex-col" style="padding: 16px 12px">
        <div class="flex flex-col gap-[3px]">
          {#each NAV_GROUPS as group (group.id)}
            {@const isActive = activeGroup === group.id}
            <button
              onclick={() => (activeGroup = group.id)}
              data-gp-autofocus={isActive ? '' : undefined}
              class="flex items-center gap-[11px] w-full text-left px-[10px] py-[9px] transition-colors"
              style:background={isActive ? 'var(--color-bg-3)' : 'transparent'}
              style:border-left={isActive ? '2px solid var(--color-spool)' : '2px solid transparent'}
            >
              <!-- Icon slot -->
              <span class="flex shrink-0 text-ink-2 w-[22px] justify-center" style:color={isActive ? 'var(--color-spool)' : undefined}>
                {#if group.id === 'general'}
                  <MonitorSmartphone size={14} />
                {:else if group.id === 'library'}
                  <Library size={14} />
                {:else if group.id === 'saves'}
                  <Layers size={14} />
                {:else if group.id === 'deck'}
                  <Gamepad2 size={14} />
                {:else if group.id === 'network'}
                  <Wifi size={14} />
                {:else}
                  <HardDrive size={14} />
                {/if}
              </span>

              <div class="flex-1 min-w-0 flex flex-col gap-[2px]">
                <span
                  class="text-[13px] truncate"
                  style:font-weight={isActive ? '600' : '500'}
                  style:color={isActive ? 'var(--color-ink-0)' : 'var(--color-ink-1)'}
                >{group.title}</span>
                <div class="flex items-center gap-1.5">
                  <span class="text-[10.5px] text-ink-3 truncate">{group.sub}</span>
                  {#if group.platform === 'linux'}
                    <span class="font-mono text-[8px] uppercase tracking-[0.1em] text-ink-3 border border-line-2 rounded-[3px] px-1 leading-[12px]">Linux</span>
                  {/if}
                </div>
              </div>

              <!-- Status dot -->
              {#if group.status !== 'off'}
                <span
                  class="size-[6px] rounded-full shrink-0"
                  style:background={group.status === 'ok' ? 'var(--color-ok)' : 'var(--color-warn)'}
                ></span>
              {/if}
            </button>
          {/each}
        </div>

        <div class="flex-1"></div>

        <div class="flex flex-col gap-[3px] border-t border-dashed border-line-1 pt-[12px] pb-1 px-2">
          <MonoLabel size={9}>{appVersion ? `v${appVersion}` : '—'}</MonoLabel>
        </div>
      </nav>

      <!-- ── Content pane ── -->
      <div class="overflow-y-auto" style="padding: 26px 34px 60px">
        <div class="mx-auto" style="max-width: 700px">

          <!-- Pane header -->
          <div class="mb-[22px]">
            <h1 class="font-display text-[27px] font-bold tracking-[-0.02em] mb-[5px]">
              {GROUP_TITLE[activeGroup] ?? activeGroup}
            </h1>
            <p class="text-[13px] leading-[1.55] text-ink-2 max-w-[520px]">
              {GROUP_BLURB[activeGroup] ?? ''}
            </p>
          </div>

          <!-- ════ GENERAL ════ -->
          {#if activeGroup === 'general'}
            <div class="flex flex-col gap-4">

              <!-- Display & input -->
              <SettingsCard title="Display" helper="Auto-detects a touchscreen or handheld. Gamepad mode grows targets and turns on controller navigation — good for a Deck, Ally, or a PC on the TV.">
                {#snippet icon()}<MonitorSmartphone size={14} />{/snippet}
                <SettingsRow
                  label="Layout"
                  helper={`Gamepad mode: bigger targets, controller navigation, couch-friendly spacing. Currently rendering: ${uiMode.resolved === 'touch' ? 'Gamepad' : 'Desktop'}.`}
                >
                  {#snippet control()}
                    <div class="flex justify-end">
                      <Segmented
                        value={config!.ui_mode}
                        onchange={(v) => setUiMode(v as ConfigData['ui_mode'])}
                        options={[
                          { value: 'auto', label: 'Auto' },
                          { value: 'desktop', label: 'Desktop' },
                          { value: 'touch', label: 'Gamepad' },
                        ]}
                      />
                    </div>
                  {/snippet}
                </SettingsRow>
              </SettingsCard>

              <!-- Updates -->
              <SettingsCard
                title="Updates"
                helper="Spool checks for new versions automatically while it's running. You can also check right now."
              >
                {#snippet icon()}<Download size={14} />{/snippet}
                <SettingsRow
                  label="App version"
                  status={updateCheck === 'available'
                    ? 'warn'
                    : updateCheck === 'up-to-date'
                      ? 'ok'
                      : updateCheck === 'error'
                        ? 'warn'
                        : undefined}
                  helper={updateCheckMsg ??
                    (appVersion ? `Currently running v${appVersion}.` : 'Currently running Spool.')}
                >
                  {#snippet control()}
                    <div class="flex justify-end">
                      <Btn
                        variant={updateCheck === 'available' ? 'primary' : 'ghost'}
                        onclick={onCheckForUpdates}
                        disabled={updateCheck === 'checking'}
                      >
                        {#snippet icon()}<RefreshCcw size={14} />{/snippet}
                        {updateCheck === 'checking' ? 'Checking…' : 'Check for updates'}
                      </Btn>
                    </div>
                  {/snippet}
                </SettingsRow>
              </SettingsCard>

              <!-- Cover artwork -->
              <SettingsCard
                title="Cover artwork"
                helper="Cover, hero, and logo art is fetched when you add a game — from Steam's official artwork first, with SteamGridDB as a fallback."
              >
                {#snippet icon()}<Grid2x2 size={14} />{/snippet}
                {#snippet right()}
                  <Pill kind={config!.steamgriddb_enabled ? 'ok' : 'off'}>
                    {config!.steamgriddb_enabled ? 'On' : 'Off'}
                  </Pill>
                {/snippet}

                <!-- SteamGridDB toggle -->
                <div class="border-b border-dashed border-line-1">
                  <div class="flex items-center gap-[14px] px-[18px] py-[14px]">
                    <div class="flex-1">
                      <div class="text-[13px] font-medium text-ink-0">Use SteamGridDB</div>
                      <div class="mt-[3px] text-[11.5px] text-ink-2">
                        {config.steamgriddb_enabled
                          ? 'Authenticated — art fetches on every game import.'
                          : 'Disabled — covers use generated placeholders.'}
                      </div>
                    </div>
                    <Toggle bind:checked={config!.steamgriddb_enabled} onchange={persist} />
                  </div>
                  {#if config.steamgriddb_enabled}
                    <div class="bg-bg-0 pb-1">
                      <SettingsRow label="API key" helper="Required for art fetches. Your key is never sent to anyone but SteamGridDB.">
                        {#snippet extras()}
                          <TextField bind:value={config!.steamgriddb_api_key} placeholder="API key" mono masked full oncommit={persist} />
                          <a href="https://www.steamgriddb.com/profile/preferences/api" target="_blank" rel="noopener noreferrer">
                            <Btn variant="ghost">
                              {#snippet icon()}<KeyRound size={14} />{/snippet}
                              Get a key
                            </Btn>
                          </a>
                        {/snippet}
                      </SettingsRow>
                    </div>
                  {/if}
                </div>
              </SettingsCard>

            </div>

          <!-- ════ LIBRARY ════ -->
          {:else if activeGroup === 'library'}
            <LibraryStorage
              folders={config!.library_folders}
              capacity={libFolderCapacity}
              onAddFolder={addLibraryFolder}
              onRemoveFolder={removeLibFolder}
              onSetDefaultFolder={setDefaultLibFolder}
            />

          <!-- ════ SAVES ════ -->
          {:else if activeGroup === 'saves'}
            <div class="flex flex-col gap-4">

              <!-- Backups -->
              <SettingsCard
                title="Backups"
                helper="Spool backs up each game's saves locally before and after every session — no setup needed. The backup engine (ludusavi) ships bundled."
              >
                {#snippet icon()}<Layers size={14} />{/snippet}
                {#snippet right()}<Pill kind="info">ludusavi · bundled</Pill>{/snippet}

                <SettingsRow
                  label="Revisions to keep"
                  helper="How many save backups to retain per game. More gives more rollback points at the cost of disk and cloud upload. 3–10."
                >
                  {#snippet control()}
                    <div style="max-width: 280px">
                      <Slider
                        min={3}
                        max={10}
                        step={1}
                        bind:value={config!.save_retention_full}
                        oncommit={onRetentionCommit}
                      />
                    </div>
                  {/snippet}
                </SettingsRow>

              </SettingsCard>

              <!-- Cloud sync -->
              <SettingsCard
                title="Cloud sync"
                helper="Mirror your save backups to a cloud remote so they follow you between devices. Powered by the bundled rclone."
              >
                {#snippet icon()}<Cloud size={14} />{/snippet}
                {#snippet right()}
                  {#if cloudOnline}
                    <Pill kind="ok">Syncing</Pill>
                  {:else if cloudConfigured}
                    <Pill kind="warn">Offline</Pill>
                  {:else}
                    <Pill kind="off">Local only</Pill>
                  {/if}
                {/snippet}

                <!-- Cloud toggle + sub-fields -->
                <div class="border-b border-dashed border-line-1">
                  <div class="flex items-center gap-[14px] px-[18px] py-[14px]">
                    <div class="flex-1">
                      <div class="text-[13px] font-medium text-ink-0">Sync saves to the cloud</div>
                      <div class="mt-[3px] text-[11.5px] text-ink-2">
                        {cloudEnabled
                          ? `Provider: ${config.cloud_provider}.`
                          : 'Off — saves are backed up locally only.'}
                      </div>
                    </div>
                    <Toggle
                      checked={cloudEnabled}
                      onchange={(v) => {
                        if (!config) return;
                        config.cloud_provider = v ? 'webdav' : '';
                        persist();
                      }}
                    />
                  </div>

                  {#if cloudEnabled}
                    <div class="bg-bg-0 pb-1">
                      <SettingsRow label="Provider" helper="Choose a cloud storage provider or Custom for a custom rclone remote name.">
                        {#snippet extras()}
                          <Select
                            bind:value={config!.cloud_provider}
                            options={CLOUD_PROVIDER_OPTIONS}
                            placeholder="Disabled"
                            onchange={async () => {
                              if (oauthConnecting) await cancelOAuth();
                              remoteExists = false;
                              await persist();
                              const provider = config?.cloud_provider;
                              if (provider && OAUTH_PROVIDERS.includes(provider)) {
                                try {
                                  const exists = await api.checkCloudRemoteExists(provider);
                                  // Guard against a rapid second switch while this
                                  // probe was in flight, which would otherwise apply
                                  // the old provider's result to the new selection.
                                  if (config?.cloud_provider === provider) remoteExists = exists;
                                } catch (e) {
                                  console.error('[settings] checkCloudRemoteExists failed:', e);
                                }
                              }
                            }}
                          />
                        {/snippet}
                      </SettingsRow>

                      {#if OAUTH_PROVIDERS.includes(config!.cloud_provider)}
                        <SettingsRow
                          label="Authentication"
                          helper={remoteExists
                            ? 'Authenticated — your account is connected.'
                            : 'Not connected — click Connect to open a browser and authorise rclone.'}
                          status={remoteExists ? 'ok' : 'warn'}
                        >
                          {#snippet extras()}
                            {#if oauthConnecting}
                              <span class="font-mono text-[11px] text-ink-2 animate-pulse">Waiting for browser…</span>
                              <Btn variant="ghost" onclick={cancelOAuth}>Cancel</Btn>
                            {:else}
                              <Btn
                                variant={remoteExists ? 'ghost' : 'primary'}
                                onclick={connectOAuth}
                              >
                                {#snippet icon()}<Cloud size={14} />{/snippet}
                                {remoteExists ? 'Reconnect' : 'Connect'}
                              </Btn>
                            {/if}
                          {/snippet}
                        </SettingsRow>
                      {/if}

                      {#if config.cloud_provider === 'custom'}
                        <SettingsRow label="Remote" helper="rclone remote name (e.g. bazzite, gdrive, b2)">
                          {#snippet control()}
                            <TextField bind:value={config!.cloud_remote} placeholder="bazzite" mono oncommit={persist} />
                          {/snippet}
                        </SettingsRow>
                      {/if}

                      {#if config.cloud_provider === 'webdav'}
                        <SettingsRow label="WebDAV URL" helper="e.g. https://nextcloud.example.com/remote.php/dav/files/me">
                          {#snippet control()}
                            <TextField bind:value={webdavUrl} placeholder="https://host/webdav" mono full />
                          {/snippet}
                        </SettingsRow>
                        <SettingsRow label="Username">
                          {#snippet control()}
                            <TextField bind:value={webdavUsername} placeholder="username" mono />
                          {/snippet}
                        </SettingsRow>
                        <SettingsRow label="Password" helper="Stored obscured by rclone, never saved in Spool's config.">
                          {#snippet extras()}
                            <TextField bind:value={webdavPassword} masked placeholder="password" mono full />
                            <Btn
                              variant="primary"
                              onclick={connectWebdav}
                              disabled={webdavConnecting || !webdavUrl || !webdavUsername}
                            >
                              {#snippet icon()}<Check size={14} />{/snippet}
                              {webdavConnecting ? 'Connecting…' : 'Connect'}
                            </Btn>
                          {/snippet}
                        </SettingsRow>
                      {/if}

                      <SettingsRow
                        label="Remote folder"
                        helper="Base folder on the remote. Saves go to &lt;folder&gt;/ludusavi-backup; Spool's cross-device session state to &lt;folder&gt;/_spool."
                      >
                        {#snippet control()}
                          <TextField bind:value={config!.cloud_base_path} placeholder="Spool" mono oncommit={persist} />
                        {/snippet}
                      </SettingsRow>

                      <p class="px-3 pt-1 pb-2 text-[11px] leading-relaxed text-ink-2">
                        Syncing large saves over the internet can be slow. For
                        faster transfers on your home network, you can sync to a
                        self-hosted remote (e.g. a Raspberry Pi or NAS) —
                        <a
                          href="https://spool.kinzett.io/guides/sftp-remote/"
                          target="_blank"
                          rel="noopener noreferrer"
                          class="text-ink-1 underline underline-offset-2 hover:text-ink-0"
                        >learn how</a>.
                      </p>

                    </div>
                  {/if}
                </div>
              </SettingsCard>

              <!-- Offline mode -->
              <SettingsCard
                title="Offline mode"
                helper="For travel or a spotty connection. Going offline gets everything ready first, then pauses all cloud sync so game launches never wait on the network."
              >
                {#snippet icon()}<CloudOff size={14} />{/snippet}
                {#snippet right()}
                  {#if offlineBusy !== 'idle'}
                    <Pill kind="warn">{offlineBusy === 'going-offline' ? 'Preparing…' : 'Syncing…'}</Pill>
                  {:else if config?.offline_mode}
                    <Pill kind="warn">Offline</Pill>
                  {:else}
                    <Pill kind="off">Online</Pill>
                  {/if}
                {/snippet}

                <div class="flex items-center gap-[14px] px-[18px] py-[14px]">
                  <div class="flex-1">
                    <div class="text-[13px] font-medium text-ink-0">
                      {config.offline_mode ? 'Offline — cloud sync paused' : 'Prepare for offline play'}
                    </div>
                    <div class="mt-[3px] text-[11.5px] text-ink-2">
                      {#if offlineBusy !== 'idle'}
                        <span class="animate-pulse">
                          {offlinePrep
                            ? `${offlinePrep.detail}${offlinePrep.total ? ` (${offlinePrep.current}/${offlinePrep.total})` : ''}`
                            : offlineBusy === 'going-offline'
                              ? 'Preparing…'
                              : 'Checking the cloud remote…'}
                        </span>
                      {:else if config.offline_mode}
                        Saves keep backing up locally. Turn off to upload them and resume syncing.
                      {:else}
                        Downloads the latest cloud saves for every game{isLinux ? ' and the Proton runtime' : ''} before switching.
                      {/if}
                    </div>
                  </div>
                  <Toggle
                    checked={config.offline_mode}
                    disabled={offlineBusy !== 'idle'}
                    onchange={(v) => setOfflineMode(v)}
                  />
                </div>
              </SettingsCard>

            </div>

          <!-- ════ STEAM DECK & LINUX ════ -->
          {:else if activeGroup === 'deck'}
            <div class="flex flex-col gap-4">

              <!-- Add Spool to Steam -->
              {#if isLinux}
                <SettingsCard
                  title="Add to Steam"
                  helper="Adds Spool as a non-Steam shortcut so you can launch your library from Steam — including SteamOS Game Mode, where it opens fullscreen."
                >
                  {#snippet icon()}<Gamepad2 size={14} />{/snippet}
                  <SettingsRow
                    label="Steam shortcut"
                    helper="Creates a “Spool” entry in your Steam library with its cover art. Restart Steam afterwards to see it."
                  >
                    {#snippet extras()}
                      <Btn variant="primary" onclick={addSpoolToSteam} disabled={addingToSteam}>
                        {#snippet icon()}<Gamepad2 size={14} />{/snippet}
                        {addingToSteam ? 'Adding…' : 'Add to Steam'}
                      </Btn>
                    {/snippet}
                  </SettingsRow>
                  <SettingsRow
                    label="Spool collection"
                    helper="Builds a “Spool” library collection from your Steam-added games. Steam shows it natively; TabMaster can turn it into a tab. Restart Steam afterwards."
                  >
                    {#snippet extras()}
                      <Btn onclick={syncSpoolCollection} disabled={syncingCollection}>
                        {#snippet icon()}<Gamepad2 size={14} />{/snippet}
                        {syncingCollection ? 'Rebuilding…' : 'Rebuild collection'}
                      </Btn>
                    {/snippet}
                  </SettingsRow>
                </SettingsCard>
              {/if}

              <!-- Game Mode companion (Decky) -->
              {#if isLinux}
                {@const dp = deckyPlugin}
                {@const needsUpdate = !!dp?.installed && isNewerVersion(dp.bundledVersion, dp.installedVersion ?? '')}
                <SettingsCard
                  title="Game Mode companion"
                  helper="A Decky Loader plugin that backs up saves even when a game is force-quit, and lets you browse your library right from the Quick Access menu."
                >
                  {#snippet icon()}<Gamepad2 size={14} />{/snippet}
                  {#snippet right()}
                    <Pill kind={dp?.installed ? (needsUpdate ? 'warn' : 'ok') : 'off'}>
                      {dp?.installed ? (needsUpdate ? 'Update' : 'Installed') : 'Not installed'}
                    </Pill>
                  {/snippet}

                  <SettingsRow
                    label="Plugin"
                    status={dp?.installed ? (needsUpdate ? 'warn' : 'ok') : 'warn'}
                    helper={!dp
                      ? 'Checking…'
                      : !dp.deckyPresent
                        ? "Decky Loader isn't detected (~/homebrew). Install Decky first, then come back."
                        : dp.installed
                          ? needsUpdate
                            ? `Installed v${dp.installedVersion ?? '?'} · bundled v${dp.bundledVersion}. An update is available.`
                            : `Installed v${dp.installedVersion ?? dp.bundledVersion} — up to date.`
                          : `Not installed. Bundled v${dp.bundledVersion}. Installing asks for your password and restarts Decky.`}
                  >
                    {#snippet extras()}
                      <Btn variant={needsUpdate || !dp?.installed ? 'primary' : 'ghost'} onclick={installDeckyPlugin} disabled={deckyInstalling || !dp}>
                        {#snippet icon()}<Sparkles size={14} />{/snippet}
                        {deckyInstalling ? 'Installing…' : dp?.installed ? (needsUpdate ? 'Update plugin' : 'Reinstall') : 'Install plugin'}
                      </Btn>
                    {/snippet}
                  </SettingsRow>

                  {#if dp?.installed}
                    <div class="border-t border-dashed border-line-1 bg-bg-0">
                      <div class="px-[18px] pt-[11px] pb-1">
                        <MonoLabel size={9.5}>In Game Mode, this plugin can</MonoLabel>
                      </div>
                      <div class="flex items-start gap-3 px-[18px] py-3">
                        <span class="flex mt-[1px] shrink-0 w-6 h-6 rounded-[5px] bg-white/[0.04] items-center justify-center text-spool">
                          <Shield size={13} />
                        </span>
                        <div class="flex-1 min-w-0">
                          <div class="text-[12.5px] font-medium text-ink-0">Backup on forced quit</div>
                          <div class="mt-[2px] text-[11px] leading-[1.5] text-ink-2 max-w-[480px]">Quick Access → Exit Game can kill Spool before it saves. The plugin runs the backup from outside the game's process tree as a safety net.</div>
                        </div>
                      </div>
                      <div class="flex items-start gap-3 px-[18px] py-3">
                        <span class="flex mt-[1px] shrink-0 w-6 h-6 rounded-[5px] bg-white/[0.04] items-center justify-center text-spool">
                          <Grid2x2 size={13} />
                        </span>
                        <div class="flex-1 min-w-0">
                          <div class="text-[12.5px] font-medium text-ink-0">Browse library in Game Mode</div>
                          <div class="mt-[2px] text-[11px] leading-[1.5] text-ink-2 max-w-[480px]">See your cover-art shelf and launch games from the Quick Access panel — no Desktop Mode round-trip.</div>
                        </div>
                      </div>
                      <div class="flex items-start gap-3 px-[18px] py-3">
                        <span class="flex mt-[1px] shrink-0 w-6 h-6 rounded-[5px] bg-white/[0.04] items-center justify-center text-spool">
                          <Download size={13} />
                        </span>
                        <div class="flex-1 min-w-0">
                          <div class="text-[12.5px] font-medium text-ink-0">LAN downloads in Game Mode</div>
                          <div class="mt-[2px] text-[11px] leading-[1.5] text-ink-2 max-w-[480px]">Discover peers and pull game installs over your local network straight from the panel.</div>
                        </div>
                      </div>
                    </div>
                  {/if}
                </SettingsCard>

                <!-- Compatibility -->
                <SettingsCard
                  title="Compatibility (Proton)"
                  helper="Run Windows games on Linux via Proton. umu-run manages the runtime; each game gets its own prefix."
                >
                  {#snippet icon()}<Cpu size={14} />{/snippet}
                  {#snippet right()}
                    <Pill kind={deps.length > 0 && deps.every(d => d.found) ? 'ok' : 'warn'}>
                      {deps.filter(d => d.found).length}/{deps.length} deps
                    </Pill>
                  {/snippet}

                  <SettingsRow label="Default Proton" helper="Used when a game doesn't pick its own version. Auto chooses the newest installed.">
                    {#snippet extras()}
                      <Select
                        bind:value={config!.default_proton_path}
                        options={protonOptions}
                        onchange={persist}
                      />
                    {/snippet}
                  </SettingsRow>

                  <SettingsRow
                    label="umu-run path"
                    helper="Leave blank to search /usr/bin, PATH, then Heroic's bundled copy. Set it to use a specific umu-run."
                  >
                    {#snippet control()}
                      <TextField
                        bind:value={config!.umu_run_path}
                        placeholder="/usr/bin/umu-run"
                        mono
                        full
                        oncommit={persist}
                      />
                    {/snippet}
                    {#snippet extras()}
                      <Btn variant="ghost" onclick={detectUmuRun} disabled={detectingUmu}>
                        {#snippet icon()}<RefreshCcw size={14} />{/snippet}
                        {detectingUmu ? 'Detecting…' : 'Detect'}
                      </Btn>
                    {/snippet}
                  </SettingsRow>

                  <!-- Dependency doctor -->
                  {#if deps.length > 0}
                    <div class="border-t border-dashed border-line-1 px-[18px] py-[14px]">
                      <div class="mb-3 flex items-center justify-between">
                        <MonoLabel size={10}>Dependencies</MonoLabel>
                        <button
                          onclick={refreshDeps}
                          disabled={depsLoading}
                          class="flex items-center gap-1 font-mono text-[10px] uppercase tracking-[0.1em] text-ink-3 hover:text-ink-1 disabled:opacity-40"
                        >
                          <RefreshCcw size={10} class={depsLoading ? 'animate-spin' : ''} />
                          {depsLoading ? 'Scanning…' : 'Rescan'}
                        </button>
                      </div>
                      <div class="flex flex-col gap-3">
                        {#each deps as dep (dep.name)}
                          <div class="flex flex-col gap-0.5">
                            <div class="flex items-center gap-2">
                              <span class="size-[6px] shrink-0 rounded-full" style:background={dep.found ? 'var(--color-ok)' : 'var(--color-warn)'}></span>
                              <span class="font-mono text-[12px] text-ink-0">{dep.name}</span>
                              {#if dep.found}
                                <span
                                  class="rounded-[3px] px-1.5 py-px font-mono text-[9.5px] uppercase tracking-[0.08em]"
                                  style:background={dep.source === 'bundled' ? 'rgba(126,198,255,0.12)' : 'rgba(120,220,160,0.12)'}
                                  style:color={dep.source === 'bundled' ? 'var(--color-info)' : 'var(--color-ok)'}
                                >{dep.source}</span>
                              {/if}
                            </div>
                            {#if dep.found}
                              <p class="ml-4 font-mono text-[10.5px] text-ink-3 break-all">{dep.path}</p>
                            {:else}
                              <div class="ml-4 flex flex-col gap-0.5">
                                <span class="text-[11px] text-warn">Not found</span>
                                {#if dep.install_hint}
                                  <code class="rounded-[3px] bg-black/30 px-2 py-1 font-mono text-[10.5px] text-ink-2 select-all">{dep.install_hint}</code>
                                {/if}
                                {#if dep.install_docs_url}
                                  <a
                                    href={dep.install_docs_url}
                                    target="_blank"
                                    rel="noopener noreferrer"
                                    class="text-[11px] text-info hover:underline"
                                  >Install instructions →</a>
                                {/if}
                              </div>
                            {/if}
                          </div>
                        {/each}
                      </div>
                    </div>
                  {/if}
                </SettingsCard>
              {/if}

            </div>

          <!-- ════ NETWORK ════ -->
          {:else if activeGroup === 'network'}
            <div class="flex flex-col gap-4">

              <!-- LAN sharing -->
              <SettingsCard
                title="LAN sharing"
                helper="Discovers other Spool instances on your local network and shares game installs over HTTP."
              >
                {#snippet icon()}<Wifi size={14} />{/snippet}
                {#snippet right()}
                  <Pill kind={config!.lan_share_enabled ? 'ok' : 'off'}>
                    {config!.lan_share_enabled ? `${peers.length} peer${peers.length === 1 ? '' : 's'}` : 'Off'}
                  </Pill>
                {/snippet}

                <div class="border-b border-dashed border-line-1">
                  <div class="flex items-center gap-[14px] px-[18px] py-[14px]">
                    <div class="flex-1">
                      <div class="text-[13px] font-medium text-ink-0">Share installs over LAN</div>
                      <div class="mt-[3px] text-[11.5px] text-ink-2">
                        {config.lan_share_enabled
                          ? `Listening on :${config.lan_share_port} · ${peers.length} peer${peers.length === 1 ? '' : 's'} visible`
                          : 'Off — your installs stay private.'}
                      </div>
                    </div>
                    <Toggle bind:checked={config!.lan_share_enabled} onchange={persist} />
                  </div>

                  {#if config.lan_share_enabled}
                    <div class="bg-bg-0 pb-1">
                      <SettingsRow label="Port" helper="TCP port peers connect to. Falls back to a random port if taken.">
                        {#snippet control()}
                          <NumberField
                            min={1024}
                            max={65535}
                            width="6rem"
                            bind:value={config!.lan_share_port}
                            oncommit={onLanPortCommit}
                          />
                        {/snippet}
                      </SettingsRow>

                      <SettingsRow
                        label="Install location"
                        helper={defaultInstallFolder
                          ? `Downloads from peers install into ${defaultInstallFolder.path}.`
                          : 'Downloads from peers install into Spool’s data folder. Add a library folder to choose a drive.'}
                      >
                        {#snippet control()}
                          <Btn variant="ghost" onclick={() => (activeGroup = 'library')}>
                            {#snippet icon()}<HardDrive size={14} />{/snippet}
                            Library folders
                          </Btn>
                        {/snippet}
                      </SettingsRow>

                      <SettingsRow
                        label="Download speed limit"
                        helper={config.lan_download_max_mbps > 0
                          ? `Capped at ${config.lan_download_max_mbps} Mbps across all parallel files.`
                          : 'Unlimited — transfers use whatever bandwidth they can get.'}
                      >
                        {#snippet control()}
                          <NumberField
                            min={0}
                            step={1}
                            width="5rem"
                            suffix="Mbps"
                            bind:value={config!.lan_download_max_mbps}
                            oncommit={() => {
                              if (!config) return;
                              if (!Number.isFinite(config.lan_download_max_mbps) || config.lan_download_max_mbps < 0) {
                                config.lan_download_max_mbps = 0;
                              }
                              persist();
                            }}
                          />
                        {/snippet}
                      </SettingsRow>

                      {#if peers.length > 0}
                        <div class="mx-[18px] mb-3.5 overflow-hidden rounded-sm border border-dashed border-line-2 bg-bg-0">
                          <div class="flex items-center justify-between border-b border-dashed border-line-1 px-3 py-2">
                            <MonoLabel size={9.5}>Discovered peers</MonoLabel>
                            <span class="font-mono text-[10px] uppercase tracking-[0.08em] text-ink-3">
                              UDP · BROADCAST · {config.lan_share_port}
                            </span>
                          </div>
                          {#each peers as peer, i (peer.device_id)}
                            <div
                              class="grid items-center gap-3 px-3 py-2"
                              style="grid-template-columns: 14px 1fr auto auto auto"
                              class:border-b={i < peers.length - 1}
                              class:border-dashed={i < peers.length - 1}
                              class:border-line-1={i < peers.length - 1}
                            >
                              <span class="size-[7px] rounded-full" style:background="var(--color-ok)" style:box-shadow="0 0 8px rgba(126,226,164,0.4)"></span>
                              <div class="flex min-w-0 flex-col gap-px">
                                <span class="truncate text-[12px] text-ink-0">{peer.device_name}</span>
                                <span class="font-mono text-[9.5px] uppercase tracking-[0.06em] text-ink-3">{peer.game_count} games</span>
                              </div>
                              <span class="font-mono text-[10px] text-ink-2">{peer.addr}</span>
                              <span class="font-mono text-[10px] text-ink-3">{peer.last_seen_ago_secs < 5 ? 'now' : `${peer.last_seen_ago_secs}s ago`}</span>
                              <Pill kind="info" soft>peer</Pill>
                            </div>
                          {/each}
                        </div>
                      {/if}
                    </div>
                  {/if}
                </div>
              </SettingsCard>

              <!-- This device -->
              <SettingsCard
                title="This device"
                helper="The label other Spool devices see in their LAN peer list."
              >
                {#snippet icon()}<MonitorSmartphone size={14} />{/snippet}
                <SettingsRow label="Device name">
                  {#snippet control()}
                    <TextField bind:value={config!.device_name} placeholder="Workshop · Desktop" oncommit={persist} />
                  {/snippet}
                </SettingsRow>
              </SettingsCard>

            </div>

          <!-- ════ ADVANCED ════ -->
          {:else if activeGroup === 'advanced'}
            <div class="flex flex-col gap-4">

              <SettingsCard
                title="Debug & reset"
                helper="Escape hatches and maintenance. Resetting loses all settings and starts over — your library and saves stay."
              >
                {#snippet icon()}<HardDrive size={14} />{/snippet}
                <div class="px-[18px] py-[14px] flex flex-col gap-3">
                  <div class="flex gap-2 flex-wrap">
                    <Btn
                      variant="ghost"
                      onclick={() => api.openLudusaviGui().catch(err => toasts.show({ kind: 'bad', label: 'LUDUSAVI', title: 'Could not open settings', sub: String(err) }))}
                    >
                      {#snippet icon()}<Layers size={14} />{/snippet}
                      Open Ludusavi settings
                    </Btn>
                    <Btn
                      variant="ghost"
                      onclick={async () => {
                        try {
                          const dir = await appLocalDataDir();
                          await api.openPath(dir);
                        } catch (e) {
                          toasts.show({ kind: 'bad', label: 'SETTINGS', title: "Couldn't open config folder", sub: String(e) });
                        }
                      }}
                    >
                      {#snippet icon()}<Folder size={14} />{/snippet}
                      Open config folder
                    </Btn>
                    <Btn
                      variant="ghost"
                      onclick={async () => {
                        if (!config) return;
                        try {
                          await navigator.clipboard.writeText(JSON.stringify($state.snapshot(config), null, 2));
                          toasts.show({ kind: 'ok', label: 'SETTINGS', title: 'Copied', sub: 'Config JSON copied to clipboard.' });
                        } catch (e) {
                          toasts.show({ kind: 'bad', label: 'SETTINGS', title: "Couldn't copy", sub: String(e) });
                        }
                      }}
                    >
                      Copy diagnostics
                    </Btn>
                  </div>
                </div>
                <div class="border-t border-dashed border-line-1 px-[18px] py-[14px] text-[11px] text-ink-3">
                  Settings live at
                  <code class="font-mono text-ink-2">%LOCALAPPDATA%\Spool\config.json</code>
                  · Changes save automatically.
                </div>
              </SettingsCard>

            </div>
          {/if}

        </div>
      </div>

    </div>

    <!-- ── Bottom strip ── -->
    <div class="h-[34px] shrink-0 border-t border-line-1 bg-black/20 px-[18px] flex items-center justify-between">
      <div class="flex items-center gap-2 font-mono text-[10px] uppercase tracking-[0.08em] text-ink-2">
        <span class="size-[6px] rounded-full bg-ok"></span>
        All changes saved
      </div>
      {#if uiMode.resolved === 'touch'}
        <ButtonLegend items={settingsLegend} size={16} />
      {:else}
        <span class="font-mono text-[9.5px] text-ink-3 tracking-[0.08em]">%LOCALAPPDATA%\Spool\config.json</span>
      {/if}
    </div>
  {/if}
</div>
