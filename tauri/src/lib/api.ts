// Typed wrappers around Tauri's `invoke` IPC bridge. All backend calls go
// through this module — gives us a single place to add caching, mocking for
// tests, or telemetry later, and keeps `invoke<T>(...)` ceremony out of
// every component.

import { invoke, convertFileSrc } from '@tauri-apps/api/core';
import type {
  AddToSteamResult,
  Collection,
  ConfigData,
  DownloadProgress,
  DriveInfo,
  FolderCapacity,
  GameEntry,
  MoveProgress,
  DepStatus,
  LanPeer,
  NewGame,
  PeerGame,
  PeerSource,
  PlaySession,
  ProtonVersion,
  GuidedInstallResult,
  GoOfflineReport,
  GoOnlineReport,
  SyncStatus,
  UploadSnapshot,
  SearchCandidate,
  RawConflictDetails,
  SaveRevision,
  PullResult,
  ManifestPath,
  ManifestStatus,
} from './types';

/** Status of the companion Spool Backup Decky plugin (mirrors the Rust
 *  `DeckyPluginInfo` in `decky_install.rs`). Defined here rather than in
 *  types.ts because it's only consumed through the two api methods below. */
export interface DeckyPluginInfo {
  /** This platform can install the plugin at all (Linux/SteamOS only). */
  supported: boolean;
  /** A copy already exists in ~/homebrew/plugins/spool-backup. */
  installed: boolean;
  /** Version from the installed plugin's package.json, if any. */
  installedVersion: string | null;
  /** Version of the plugin embedded in this Spool build. */
  bundledVersion: string;
  /** Decky Loader itself appears installed (~/homebrew exists). */
  deckyPresent: boolean;
}

/** Outcome of an install attempt (mirrors the Rust `DeckyInstallOutcome`). The
 *  files always landed when this resolves; `loaderRestarted` distinguishes
 *  "fully live now" from "copied but needs a reboot/Decky restart to load". */
export interface DeckyInstallOutcome {
  loaderRestarted: boolean;
}

export const api = {
  // Library
  listGames: (): Promise<GameEntry[]> => invoke('list_games'),
  /** All recorded play sessions across devices, oldest first. Optionally
   *  scoped to one game by name. Feeds the cross-device activity timeline. */
  listPlaySessions: (gameName?: string): Promise<PlaySession[]> =>
    invoke('list_play_sessions', { gameName: gameName ?? null }),
  addGame: (newGame: NewGame): Promise<GameEntry> => invoke('add_game', { newGame }),
  updateGame: (entry: GameEntry): Promise<GameEntry> => invoke('update_game', { entry }),
  removeGame: (id: string): Promise<boolean> => invoke('remove_game', { id }),
  /** Removes the library entry AND deletes its install folder from disk. */
  deleteGameFromDisk: (id: string): Promise<void> => invoke('delete_game_from_disk', { id }),
  /** Deletes the install folder from disk but KEEPS the library entry (dimmed,
   *  Play disabled). Re-adding the game reuses this same entry. */
  uninstallGame: (id: string): Promise<void> => invoke('uninstall_game', { id }),

  // Library folders / drives + move install
  /** Mounted drives with free space, for the Settings drive picker. */
  listDrives: (): Promise<DriveInfo[]> => invoke('list_drives'),
  /** Available bytes on the filesystem holding `path` (0 = unknown). */
  folderFreeSpace: (path: string): Promise<number> =>
    invoke('folder_free_space', { path }),
  /** Total + available bytes on the filesystem holding `path` (both 0 =
   *  unknown). Drives the per-folder capacity bar in Settings → Library. */
  folderCapacity: (path: string): Promise<FolderCapacity> =>
    invoke('folder_capacity', { path }),
  /** Creates the chosen library folder on disk; returns its canonical path. */
  prepareLibraryFolder: (path: string): Promise<string> =>
    invoke('prepare_library_folder', { path }),
  /** The `<app_data>/lan-games` fallback dir LAN installs use when no library
   *  folders exist — the install-location prompt's "Use Spool's data folder"
   *  option registers it as one. */
  defaultLanInstallDir: (): Promise<string> => invoke('default_lan_install_dir'),
  /** Moves game `id`'s install into `destFolder`; resolves with the updated
   *  entry. Progress streams via the `move:progress` event. */
  moveGameInstall: (id: string, destFolder: string): Promise<GameEntry> =>
    invoke('move_game_install', { id, destFolder }),
  /** Requests cancellation of the in-flight move for `gameId`. */
  cancelMove: (gameId: string): Promise<boolean> =>
    invoke('cancel_move', { gameId }),
  /** Snapshot of the active move (if any), for a UI mounting mid-transfer. */
  currentMove: (): Promise<MoveProgress | null> => invoke('current_move'),

  // Config
  getConfig: (): Promise<ConfigData> => invoke('get_config'),
  updateConfig: (data: ConfigData): Promise<ConfigData> => invoke('update_config', { data }),
  detectUmuRun: (): Promise<string> => invoke('detect_umu_run'),
  /** Current library collections (sidebar). */
  listCollections: (): Promise<Collection[]> => invoke('list_collections'),
  /** Persist the full collections array; returns the saved list and broadcasts
   *  `collections:changed` to other windows. */
  setCollections: (collections: Collection[]): Promise<Collection[]> =>
    invoke('set_collections', { collections }),
  appPlatform: (): Promise<string> => invoke('app_platform'),
  checkDependencies: (): Promise<DepStatus[]> => invoke('check_dependencies'),

  // Gamepad presence — drives the "switch to Gamepad layout?" prompt.
  anyGamepadConnected: (): Promise<boolean> => invoke('any_gamepad_connected'),

  // Decky plugin installer (SteamOS / Linux)
  deckyPluginStatus: (): Promise<DeckyPluginInfo> => invoke('decky_plugin_status'),
  installDeckyPlugin: (): Promise<DeckyInstallOutcome> => invoke('install_decky_plugin'),

  // Proton / Linux launch
  listProtonVersions: (): Promise<ProtonVersion[]> => invoke('list_proton_versions'),
  installProtonDeps: (gameId: string, verbs: string): Promise<string> =>
    invoke('install_proton_deps', { gameId, verbs }),
  /** Run another Windows `.exe` (e.g. a game patch / update installer) inside a
   *  game's Proton prefix and wait for it to exit (Linux). */
  runExeInPrefix: (gameId: string, exePath: string): Promise<string> =>
    invoke('run_exe_in_prefix', { gameId, exePath }),

  /** Run a Windows `setup.exe` through Proton with the install folder mounted as
   *  a Wine drive (Linux). Resolves when the installer process exits. */
  runGuidedInstaller: (
    setupExe: string,
    gameName: string,
    installDirOverride?: string,
    protonVersionOverride?: string,
  ): Promise<GuidedInstallResult> =>
    invoke('run_guided_installer', { setupExe, gameName, installDirOverride, protonVersionOverride }),

  // Ludusavi — Add Game flow
  searchGames: (query: string): Promise<SearchCandidate[]> => invoke('search_games', { query }),
  searchByExe: (exePath: string): Promise<SearchCandidate[]> =>
    invoke('search_by_exe', { exePath }),
  openLudusaviGui: (): Promise<void> => invoke('open_ludusavi_gui'),
  setCloudWebdav: (
    url: string,
    username: string,
    password: string,
    provider: string
  ): Promise<void> => invoke('set_cloud_webdav', { url, username, password, provider }),

  // SteamGridDB
  fetchCover: (gameId: string): Promise<string | null> => invoke('fetch_cover', { gameId }),
  fetchHero: (gameId: string): Promise<string | null> => invoke('fetch_hero', { gameId }),

  // Steam Store metadata (description, developer, publisher, genres, release
  // date). Resolves true when any empty field was populated.
  fetchMetadata: (gameId: string): Promise<boolean> => invoke('fetch_metadata', { gameId }),

  // Steam shortcut
  addSpoolToSteam: (): Promise<AddToSteamResult> => invoke('add_spool_to_steam'),
  addToSteam: (gameId: string): Promise<AddToSteamResult> => invoke('add_to_steam', { gameId }),
  // Remove a game's Spool-managed Steam shortcut. Resolves true when a shortcut
  // was present to remove, false when the game wasn't on Steam.
  removeFromSteam: (gameId: string): Promise<boolean> => invoke('remove_from_steam', { gameId }),
  // True when Steam currently has a game running — Add-to-Steam restarts Steam,
  // so callers warn before closing a running game.
  steamGameRunning: (): Promise<boolean> => invoke('steam_game_running'),
  // Rebuild the "Spool" Steam library collection from the managed shortcuts.
  syncSpoolSteamCollection: (): Promise<void> => invoke('sync_spool_steam_collection'),

  // Open a file/folder with the OS default handler. Goes through Rust (not
  // the opener plugin) so it can strip the AppImage environment before
  // spawning the host file manager on Linux — see system_open.rs / issue #95.
  openPath: (path: string): Promise<void> => invoke('open_path', { path }),
  // Open an external link in the default browser. WebKitGTK has no handler for
  // a link that asks for a new window, so `<a target="_blank">` clicks are
  // routed here instead (issue #493); the backend accepts http/https only and
  // rejects anything else. See externalLinks.ts for the click interception.
  openUrl: (url: string): Promise<void> => invoke('open_url', { url }),

  // Armoury Crate launcher
  generateArmouryLauncher: (gameId: string): Promise<string> =>
    invoke('generate_armoury_launcher', { gameId }),

  // Windows registry compat-flag probe — true when the OS has the
  // exe flagged "always run as administrator" via AppCompatFlags.
  getRunAsAdminInRegistry: (exePath: string): Promise<boolean> =>
    invoke('get_run_as_admin_in_registry', { exePath }),

  // Cloud control-plane reachability (rclone remote probe)
  currentSyncStatus: (): Promise<SyncStatus> => invoke('current_sync_status'),
  refreshSyncStatus: (): Promise<SyncStatus> => invoke('refresh_sync_status'),

  /** Prepare for offline play (pull every game's cloud saves, freshen the
   *  ludusavi manifest, pre-download the Proton runtime on Linux), then turn
   *  offline mode ON. Long-running; progress streams via `offline:prep`. */
  goOffline: (): Promise<GoOfflineReport> => invoke('go_offline'),
  /** Turn offline mode OFF, re-probe the remote, and upload/reconcile the
   *  saves from offline sessions. Progress streams via `offline:prep`. */
  goOnline: (): Promise<GoOnlineReport> => invoke('go_online'),

  // Cloud OAuth authentication
  checkCloudRemoteExists: (provider: string): Promise<boolean> =>
    invoke('check_cloud_remote_exists', { provider }),
  connectCloudOAuth: (provider: string): Promise<void> =>
    invoke('connect_cloud_oauth', { provider }),
  cancelCloudOAuth: (): Promise<void> => invoke('cancel_cloud_oauth'),

  // LAN
  listLanPeers: (): Promise<LanPeer[]> => invoke('list_lan_peers'),
  fetchPeerGames: (addr: string, port: number): Promise<PeerGame[]> =>
    invoke('fetch_peer_games', { addr, port }),
  startPeerInstall: (
    peerAddr: string,
    peerPort: number,
    gameId: string,
  ): Promise<string> =>
    invoke('start_peer_install', { peerAddr, peerPort, gameId }),
  currentPeerDownload: (): Promise<DownloadProgress | null> =>
    invoke('current_peer_download'),
  cancelPeerInstall: (installToken: string): Promise<boolean> =>
    invoke('cancel_peer_install', { installToken }),
  listActiveUploads: (): Promise<UploadSnapshot[]> => invoke('list_active_uploads'),
  cancelUpload: (sessionId: string): Promise<boolean> =>
    invoke('cancel_upload', { sessionId }),
  getManifestStatus: (gameId: string): Promise<ManifestStatus> =>
    invoke('get_manifest_status', { gameId }),
  prepareManifest: (gameId: string): Promise<ManifestStatus> =>
    invoke('prepare_manifest', { gameId }),

  // Run workflow
  /**
   * Launch a game through the run workflow. `steal` overrides a *suspended*
   * play-state lock held by another sleeping device (the "Play here instead"
   * path) — the server refuses to steal a live, actively-played lock.
   */
  launchGame: (gameId: string, steal = false): Promise<void> =>
    invoke('launch_game', { gameId, steal }),
  manualBackup: (
    gameId: string,
  ): Promise<{ game_count: number; bytes_total: number; cloud_synced: boolean; game_name: string }> =>
    invoke('manual_backup', { gameId }),
  manualRestore: (gameId: string): Promise<{ game_count: number }> =>
    invoke('manual_restore', { gameId }),
  /**
   * Pull cloud saves down to this device and restore them to disk, without
   * launching the game ("Sync now"). Pull-only — never uploads. The `outcome`
   * tells the caller what happened: `pulled` (cloud was ahead, now applied),
   * `up_to_date`, `local_newer` (local is ahead, left untouched), or
   * `unconfigured` (no cloud remote). A true divergence rejects with a "cloud
   * sync conflict" error so the caller can open the conflict modal.
   */
  pullCloudSaves: (gameId: string): Promise<PullResult> =>
    invoke('pull_cloud_saves', { gameId }),
  /** List the save revisions ludusavi retains for a game, newest first. */
  listSaveRevisions: (gameId: string): Promise<SaveRevision[]> =>
    invoke('list_save_revisions', { gameId }),
  /**
   * Roll back to an earlier save revision. Restores the chosen backup, then
   * pins it as the new tip (immediate cloud-synced backup) so it isn't
   * clobbered by the next pre-launch restore. Blocked while a game is running.
   */
  restoreSaveRevision: (
    gameId: string,
    backupName: string,
  ): Promise<{ game_count: number }> =>
    invoke('restore_save_revision', { gameId, backupName }),
  /**
   * Refresh a game's save-backup stats (revision count + latest-backup time)
   * from ludusavi's real backup store. Fire-and-forget from the detail view;
   * the backend emits `library:changed` only when something actually changed.
   */
  refreshSaveMetadata: (gameId: string): Promise<void> =>
    invoke('refresh_save_metadata', { gameId }),
  /**
   * Classify a picked save folder into a portable ludusavi template (preview
   * for the Saves editor). Returns a placeholder token like
   * `<winLocalAppData>/MyGame` when the folder sits in a known location, or the
   * literal path otherwise.
   */
  deriveSaveTemplate: (gameId: string, pickedPath: string): Promise<string> =>
    invoke('derive_save_template', { gameId, pickedPath }),
  /**
   * Best directory to open the save-folder picker at for a game — deep inside
   * its Proton prefix (the user profile, where AppData / Documents / Saved Games
   * live) when applicable, else the install folder or home. `null` when nothing
   * suitable exists yet (e.g. a Proton game that hasn't been launched once).
   */
  savePickerStartDir: (gameId: string): Promise<string | null> =>
    invoke('save_picker_start_dir', { gameId }),
  /**
   * Whether a Proton game's Wine prefix has been generated yet (its user
   * profile exists). `false` means the game hasn't been launched once, so its
   * save folder doesn't exist yet — the Saves editor hints to play it first.
   * Always `true` for native games / on Windows.
   */
  prefixReady: (gameId: string): Promise<boolean> =>
    invoke('prefix_ready', { gameId }),
  /**
   * Track a custom save location for a non-manifest game: persist it, register
   * it with ludusavi so the next session backs it up, and replicate the
   * definition to your other devices so you only pick the folder once. `files`
   * are ludusavi templates (see `deriveSaveTemplate`); `registry` is usually [].
   */
  setCustomSave: (
    gameId: string,
    files: string[],
    registry: string[] = [],
  ): Promise<void> => invoke('set_custom_save', { gameId, files, registry }),
  /** Stop tracking a custom save location and remove the replicated definition. */
  clearCustomSave: (gameId: string): Promise<void> =>
    invoke('clear_custom_save', { gameId }),
  /**
   * Every manifest-declared save location for an added game, tagged and flagged
   * for this device's launch mode — backs the Saves editor's override picker.
   * Empty when the game isn't in the ludusavi manifest.
   */
  manifestSaveLocations: (gameId: string): Promise<ManifestPath[]> =>
    invoke('manifest_save_locations', { gameId }),
  /**
   * Narrow which manifest save locations sync for a game (e.g. exclude
   * settings/config). Persists the exclusions, re-derives ludusavi's override,
   * forces a fresh backup so the change takes effect immediately, and replicates
   * the intent to your other devices. Passing no exclusions clears the override.
   */
  setManifestOverride: (
    gameId: string,
    excludedTags: string[],
    excludedPaths: string[],
  ): Promise<void> =>
    invoke('set_manifest_override', { gameId, excludedTags, excludedPaths }),
  /** Clear a manifest override — back to syncing the full manifest entry. */
  clearManifestOverride: (gameId: string): Promise<void> =>
    invoke('clear_manifest_override', { gameId }),
  /**
   * Resolve a cloud-sync conflict in-app by picking which copy wins, then
   * land the reconciled saves. `side` is `'local'` (keep this device, upload)
   * or `'cloud'` (keep the cloud, download). Throws if the resolve fails —
   * the caller surfaces that and keeps the "Open Ludusavi" fallback.
   */
  resolveCloudConflict: (
    gameId: string,
    side: 'local' | 'cloud',
  ): Promise<{ game_count: number }> =>
    invoke('resolve_cloud_conflict', { gameId, side }),
  getCloudConflictDetails: (gameId: string): Promise<RawConflictDetails> =>
    invoke('get_cloud_conflict_details', { gameId }),

  // Lifecycle — pulls + clears the game id queued by a startup `--run` invocation.
  takePendingRun: (): Promise<string | null> => invoke('take_pending_run'),

  // Game-Mode splash — signals that the splash's `run:phase` listener is wired
  // so the attached `--run` workflow can start without racing the first phases.
  notifySplashReady: (): Promise<void> => invoke('notify_splash_ready'),

  // KDE on-screen keyboard — WebKitGTK doesn't drive the Wayland text-input
  // protocol on focus, so the layout calls these when a text field gains/loses
  // focus to ask KWin directly. No-op off a KDE session.
  showVirtualKeyboard: (): Promise<void> => invoke('show_virtual_keyboard'),
  hideVirtualKeyboard: (): Promise<void> => invoke('hide_virtual_keyboard'),
} as const;

/**
 * Turn an absolute filesystem path (from a `GameEntry`) into a URL that the
 * webview can load via the `asset:` protocol. Returns `null` for null/missing
 * input so callers can use it directly in template expressions.
 */
export function assetUrl(path: string | null | undefined): string | null {
  if (!path) return null;
  return convertFileSrc(path);
}

/**
 * Build a URL to a peer's cover/hero artwork, served over plain HTTP by the
 * peer's in-process file server (`/games/{id}/cover` and `/hero`). Used for
 * sidebar rows / detail pages backed by a `PeerSource` rather than local art
 * on disk. The webview loads these directly (the app ships with no CSP), so no
 * download-and-cache round-trip is needed just to preview a peer game.
 */
export function peerAssetUrl(source: PeerSource, kind: 'cover' | 'hero'): string {
  return `http://${source.addr}:${source.file_server_port}/games/${source.source_game_id}/${kind}`;
}
