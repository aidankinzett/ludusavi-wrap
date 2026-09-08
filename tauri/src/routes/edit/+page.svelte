<script lang="ts">
  /**
   * Edit game dialog — opens as a child WebviewWindow from the detail
   * pane's Edit button. Three tabs (Identity / Install / Launch) covering
   * the fields we have in `GameEntry` today. Other tabs from the design
   * (Saves, Sharing, runner / env vars / window mode under Launch) will
   * land when the underlying schema does.
   *
   * Flow:
   *   - on mount, read `?id=` from URL, load the entry via list_games,
   *     snapshot it into the form state
   *   - tab switcher with the game's accent colour underlining the active
   *     tab + Save button background, same per-game tint as the detail
   *   - "Remove from library" button in the footer is destructive; falls
   *     back to a confirm prompt before calling remove_game
   *   - Save → call update_game with the merged entry → library:changed
   *     fires automatically → window closes
   *   - Cancel → close without saving (form state is discarded)
   */
  import { onMount, onDestroy } from 'svelte';
  import { SvelteSet } from 'svelte/reactivity';
  import { Download, Folder, FolderInput, Play, RefreshCw, Trash2 } from '@lucide/svelte';
  import { open as openDialog } from '@tauri-apps/plugin-dialog';
  import { getCurrentWindow } from '@tauri-apps/api/window';
  import { listen } from '@tauri-apps/api/event';
  import { api, assetUrl } from '$lib/api';
  import { fmtCatalog, absDateTime } from '$lib/format';
  import { toasts } from '$lib/toasts.svelte';
  import { removeGameDialog } from '$lib/removeGame.svelte';
  import { moveInstallDialog } from '$lib/moveInstall.svelte';
  import type { GameEntry, ManifestOverride, ProtonVersion, ManifestStatus } from '$lib/types';
  import AppChrome from '$lib/components/AppChrome.svelte';
  import MonoLabel from '$lib/components/MonoLabel.svelte';
  import CatalogId from '$lib/components/CatalogId.svelte';
  import Btn from '$lib/components/Btn.svelte';
  import TextField from '$lib/components/TextField.svelte';
  import Toggle from '$lib/components/Toggle.svelte';
  import Select, { type SelectOption } from '$lib/components/Select.svelte';
  import EditRow from '$lib/components/EditRow.svelte';
  import SavesPanel from '$lib/components/SavesPanel.svelte';
  import { gamepadScope } from '$lib/gamepad';

  type Tab = 'identity' | 'install' | 'launch' | 'saves' | 'sharing';

  let original = $state<GameEntry | null>(null);
  let form = $state<GameEntry | null>(null);
  let tab = $state<Tab>('identity');
  let saving = $state(false);
  let error = $state<string | null>(null);
  let manifestStatus = $state<ManifestStatus>('nomanifest');
  let preparingManifest = $state(false);

  // Proton launch (Linux only). Populated on mount.
  let isLinux = $state(false);
  let protonVersions = $state<ProtonVersion[]>([]);
  let depsInstalling = $state(false);

  // "Run an installer" helper — one-shot, nothing persisted.
  let runExePath = $state('');
  let runningExe = $state(false);

  // Whether a Proton game's Wine prefix exists yet (false → not launched once,
  // so its save folder doesn't exist). Defaults true so native/Windows games
  // never show the "launch first" hint. Fetched on mount, passed to SavesPanel.
  let prefixReady = $state(true);

  // Proton picker: Auto + each detected build.
  const protonOptions = $derived<SelectOption[]>([
    { value: '', label: 'Auto (newest installed)' },
    ...protonVersions.map((p) => ({ value: p.path, label: p.name })),
  ]);

  const VERB_PRESETS = [
    { verb: 'vcrun2022', label: 'Visual C++ 2022' },
    { verb: 'dotnet48', label: '.NET 4.8' },
    { verb: 'dotnet6', label: '.NET 6' },
    { verb: 'xna40', label: 'XNA 4.0' },
    { verb: 'physx', label: 'PhysX' },
    { verb: 'd3dcompiler_47', label: 'D3D Compiler 47' },
  ] as const;

  let depsChecked = new SvelteSet<string>();
  let depsCustomEnabled = $state(false);
  let depsCustom = $state('');
  const effectiveDeps = $derived(
    [
      ...depsChecked,
      ...(depsCustomEnabled ? depsCustom.trim().split(/\s+/).filter(Boolean) : []),
    ].join(' '),
  );

  function togglePreset(verb: string, checked: boolean) {
    if (checked) depsChecked.add(verb);
    else depsChecked.delete(verb);
  }

  const BRAND_SPOOL = '#d7c9a0';
  const accent = $derived(form?.accent_color ?? BRAND_SPOOL);
  const cover = $derived(assetUrl(form?.cover_image_path));
  // Stable key for a manifest override so the dirty-compare and the save path
  // agree. Empty/absent → '' (no override); arrays sorted so order can't show a
  // false change.
  function overrideKey(ov: ManifestOverride | null | undefined): string {
    if (!ov || (ov.excluded_tags.length === 0 && ov.excluded_paths.length === 0)) return '';
    return JSON.stringify({
      t: [...ov.excluded_tags].sort(),
      p: [...ov.excluded_paths].sort(),
    });
  }

  const dirty = $derived.by(() => {
    if (!form || !original) return false;
    // Cheap shallow compare on the editable fields.
    return (
      form.game_name !== original.game_name ||
      form.exe_path !== original.exe_path ||
      (form.game_folder_path ?? '') !== (original.game_folder_path ?? '') ||
      form.run_as_admin !== original.run_as_admin ||
      form.lan_shared !== original.lan_shared ||
      (form.proton_version_path ?? '') !== (original.proton_version_path ?? '') ||
      (form.wine_prefix_path ?? '') !== (original.wine_prefix_path ?? '') ||
      (form.launch_args ?? '') !== (original.launch_args ?? '') ||
      // Saves tab: the manifest override stages here and commits on save.
      overrideKey(form.manifest_override) !== overrideKey(original.manifest_override)
    );
  });

  // Sharing tab gating: a game can only be flagged for LAN sharing if it
  // has a real install folder for the server to stream from. Uses the live
  // form value (sharing is about what you're about to save).
  const hasFolder = $derived(
    !!form && (form.game_folder_path ?? '').length > 0,
  );

  // Whether the configured executable is a Windows `.exe`. On Linux these
  // launch through Proton automatically (no toggle — issue #80); the Proton
  // version / prefix / deps controls only make sense for such games.
  const exeIsWindows = $derived(
    (form?.exe_path ?? '').toLowerCase().endsWith('.exe'),
  );

  // Tracks whether Windows itself has the exe flagged for elevation via
  // AppCompatFlags. When true, the per-entry Run-As-Admin toggle is
  // informational only — the launch will elevate regardless of the
  // entry's setting. Re-checked whenever exe_path changes.
  let registryRunAsAdmin = $state(false);
  $effect(() => {
    const exe = form?.exe_path ?? '';
    if (!exe) {
      registryRunAsAdmin = false;
      return;
    }
    // `exe_path` is a bound text field, so without this we'd fire one IPC per
    // keystroke. Debounce, and let the effect's cleanup cancel the pending
    // timer and invalidate an already-dispatched probe — so a fast typist
    // can't land an older exe's result over a newer one (resolve-order race).
    let cancelled = false;
    const timer = setTimeout(() => {
      api
        .getRunAsAdminInRegistry(exe)
        .then((v) => {
          if (!cancelled) registryRunAsAdmin = v;
        })
        .catch((e) => console.error('[edit] registry probe failed:', e));
    }, 250);
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  });

  // Re-check whether the Proton prefix exists whenever the exe (or platform)
  // changes — a native↔.exe edit flips whether a prefix is involved at all, so
  // a one-shot mount fetch would leave the Saves tab's "launch first" hint
  // stale. Native/Windows games have no prefix → always ready.
  $effect(() => {
    const exe = form?.exe_path ?? '';
    const id = form?.id;
    if (!isLinux || !exe.toLowerCase().endsWith('.exe') || !id) {
      prefixReady = true;
      return;
    }
    // Same debounce + stale-guard as the registry probe: the effect re-runs on
    // every exe keystroke (the `.exe` test reads it), so coalesce the IPC and
    // drop a resolve that lands after the input moved on.
    let cancelled = false;
    const timer = setTimeout(() => {
      api
        .prefixReady(id)
        .then((v) => {
          if (!cancelled) prefixReady = v;
        })
        .catch((e) => console.error('[edit] prefixReady probe failed:', e));
    }, 250);
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  });

  onMount(async () => {
    try {
      const params = new URLSearchParams(window.location.search);
      const id = params.get('id');
      if (!id) {
        error = 'No game id in URL — close and reopen from the detail page.';
        return;
      }
      const all = await api.listGames();
      const found = all.find((g) => g.id === id);
      if (!found) {
        error = `Game ${id} not found in library.`;
        return;
      }
      original = found;
      form = { ...found };
      // Coerce nullable launch fields to '' so inputs/selects bind cleanly;
      // converted back to null on save. dirty-compare uses `?? ''` so this
      // doesn't register as a change.
      form.proton_version_path ??= '';
      form.wine_prefix_path ??= '';
      form.launch_args ??= '';

      // Linux-only Proton settings. (prefixReady is handled by its own $effect,
      // which re-probes whenever the exe changes.)
      try {
        isLinux = (await api.appPlatform()) === 'linux';
        if (isLinux) protonVersions = await api.listProtonVersions();
      } catch (e) {
        console.error('[edit] proton init failed:', e);
      }

      // "Move install…" rewrites the entry's paths out of band while this
      // window may stay open; without this, a later Save would post the stale
      // snapshot and repoint the entry at the deleted old location.
      listen('library:changed', () => {
        void syncOutOfBandPaths().catch((e) => console.error('[edit] path sync failed:', e));
      }).then((fn) => { if (!destroyed) unlistenLibChanged = fn; else fn(); });

      // Fetch initial manifest status
      api.getManifestStatus(form.id).then((status) => {
        manifestStatus = status;
      }).catch(console.error);

      // Listen for manifest status changes
      listen('lan:manifest-status-changed', (event) => {
        if (form && event.payload === form.id) {
          api.getManifestStatus(form.id).then((status) => {
            manifestStatus = status;
          }).catch(console.error);
        }
      }).then((fn) => { if (!destroyed) unlistenManifestChanged = fn; else fn(); });
    } catch (e) {
      error = String(e);
    }
  });

  let destroyed = false;
  let unlistenLibChanged: (() => void) | null = null;
  let unlistenManifestChanged: (() => void) | null = null;
  onDestroy(() => {
    destroyed = true;
    unlistenLibChanged?.();
    unlistenManifestChanged?.();
  });

  // Mirror out-of-band path changes (a move) into the form — but only fields
  // the user hasn't edited, so in-progress changes survive. `original` is
  // patched too so the dirty-compare baseline stays correct, same pattern as
  // refetchCover's artwork patch.
  async function syncOutOfBandPaths() {
    if (!form || !original) return;
    const all = await api.listGames();
    const next = all.find((g) => g.id === form!.id);
    if (!next || !form || !original) return;
    if ((form.game_folder_path ?? '') === (original.game_folder_path ?? '')) {
      form.game_folder_path = next.game_folder_path;
      original.game_folder_path = next.game_folder_path;
    }
    if (form.exe_path === original.exe_path) {
      form.exe_path = next.exe_path;
      original.exe_path = next.exe_path;
    }
    if ((form.wine_prefix_path ?? '') === (original.wine_prefix_path ?? '')) {
      form.wine_prefix_path = next.wine_prefix_path ?? '';
      original.wine_prefix_path = next.wine_prefix_path;
    }
    if (form.install_size_mb === original.install_size_mb) {
      form.install_size_mb = next.install_size_mb;
      original.install_size_mb = next.install_size_mb;
    }
  }

  async function triggerPrepareManifest() {
    if (!form || preparingManifest) return;
    preparingManifest = true;
    try {
      manifestStatus = await api.prepareManifest(form.id);
      toasts.show({
        kind: 'ok',
        label: 'LAN · MANIFEST',
        title: 'Manifest prepared',
        sub: 'Blake3 hashing manifest is ready for peers.',
      });
    } catch (e) {
      toasts.show({
        kind: 'bad',
        label: 'LAN · FAILED',
        title: "Couldn't prepare manifest",
        sub: String(e),
      });
    } finally {
      preparingManifest = false;
    }
  }

  async function browseExe() {
    if (!form) return;
    const picked = await openDialog({
      title: 'Pick the game executable',
      multiple: false,
      filters: [
        { name: 'Executable', extensions: ['exe', ''] },
        { name: 'All files', extensions: ['*'] },
      ],
    });
    if (typeof picked === 'string') {
      form.exe_path = picked;
    }
  }

  async function browseFolder() {
    if (!form) return;
    const picked = await openDialog({
      title: 'Pick the install folder',
      directory: true,
      multiple: false,
    });
    if (typeof picked === 'string') {
      form.game_folder_path = picked;
    }
  }

  async function installDeps() {
    if (!form || depsInstalling) return;
    const verbs = effectiveDeps.trim();
    if (!verbs) return;
    depsInstalling = true;
    try {
      await api.installProtonDeps(form.id, verbs);
      toasts.show({
        kind: 'ok',
        label: 'PROTON',
        title: 'Dependencies installed',
        sub: verbs,
        catalog: fmtCatalog(form.catalog_number),
      });
    } catch (e) {
      toasts.show({
        kind: 'bad',
        label: 'PROTON · DEPS',
        title: "Couldn't install dependencies",
        sub: String(e),
      });
    } finally {
      depsInstalling = false;
    }
  }

  async function browseRunExe() {
    const picked = await openDialog({
      title: 'Pick the executable to run in this prefix',
      multiple: false,
      filters: [
        { name: 'Executable', extensions: ['exe'] },
        { name: 'All files', extensions: ['*'] },
      ],
    });
    if (typeof picked === 'string') runExePath = picked;
  }

  async function runInstaller() {
    if (!form || runningExe) return;
    const exe = runExePath.trim();
    if (!exe) return;
    runningExe = true;
    try {
      const msg = await api.runExeInPrefix(form.id, exe);
      toasts.show({
        kind: 'ok',
        label: 'PROTON',
        title: msg,
        sub: exe,
        catalog: fmtCatalog(form.catalog_number),
      });
    } catch (e) {
      toasts.show({
        kind: 'bad',
        label: 'PROTON · RUN',
        title: "Couldn't run the executable",
        sub: String(e),
      });
    } finally {
      runningExe = false;
    }
  }

  async function browsePrefix() {
    if (!form) return;
    const picked = await openDialog({
      title: 'Pick the Wine prefix folder',
      directory: true,
      multiple: false,
    });
    if (typeof picked === 'string') {
      form.wine_prefix_path = picked;
    }
  }

  async function refetchCover() {
    if (!form) return;
    try {
      await api.fetchCover(form.id);
      toasts.show({
        kind: 'info',
        label: 'COVER',
        title: 'Cover refreshed',
        sub: 'Pulled the latest artwork.',
        catalog: fmtCatalog(form.catalog_number),
      });
      // Pull the entry again so we see the new path + accent immediately.
      // Patch ONLY the artwork fields — replacing the whole form would wipe
      // any in-progress edits (title, paths, launch settings). Update
      // `original` too so the dirty-compare baseline stays correct. (#271)
      const all = await api.listGames();
      const next = all.find((g) => g.id === form!.id);
      if (next && form) {
        form.cover_image_path = next.cover_image_path;
        form.hero_image_path = next.hero_image_path;
        form.accent_color = next.accent_color;
        if (original) {
          original.cover_image_path = next.cover_image_path;
          original.hero_image_path = next.hero_image_path;
          original.accent_color = next.accent_color;
        }
      }
    } catch (e) {
      toasts.show({
        kind: 'bad',
        label: 'COVER · FAILED',
        title: "Couldn't refresh cover",
        sub: String(e),
      });
    }
  }

  async function save() {
    if (!form) return;
    saving = true;
    try {
      const payload = $state.snapshot(form);
      // Empty optional launch fields persist as null, not "".
      payload.proton_version_path = payload.proton_version_path || null;
      payload.wine_prefix_path = payload.wine_prefix_path || null;
      payload.launch_args = payload.launch_args || null;
      await api.updateGame(payload);
      // The manifest override is written out-of-band (it's a RUNTIME_FIELD, so
      // updateGame's overlay preserves rather than writes it). Commit the staged
      // value here — this is also what kicks off the forced backup + its toasts.
      if (overrideKey(form.manifest_override) !== overrideKey(original?.manifest_override)) {
        const ov = form.manifest_override;
        if (ov && (ov.excluded_tags.length > 0 || ov.excluded_paths.length > 0)) {
          await api.setManifestOverride(form.id, ov.excluded_tags, ov.excluded_paths);
        } else {
          await api.clearManifestOverride(form.id);
        }
      }
      await getCurrentWindow().close();
    } catch (e) {
      error = String(e);
      toasts.show({
        kind: 'bad',
        label: 'EDIT · FAILED',
        title: "Couldn't save changes",
        sub: String(e),
      });
    } finally {
      saving = false;
    }
  }

  async function cancel() {
    await getCurrentWindow().close();
  }

  // Open the three-option remove chooser (remove from disk / from library /
  // from disk and library). On success the edit window closes itself. Uses the
  // persisted `original` entry (not the editable `form`) so the folder it acts
  // on is the saved one, not an unsaved edit.
  function remove() {
    if (!original) return;
    removeGameDialog.request(original, {
      onDone: () => void getCurrentWindow().close(),
    });
  }

  const tabs: { id: Tab; label: string }[] = [
    { id: 'identity', label: 'Identity' },
    { id: 'install', label: 'Install' },
    { id: 'launch', label: 'Launch' },
    { id: 'saves', label: 'Saves' },
    { id: 'sharing', label: 'Sharing' },
  ];

  // Bumpers cycle the edit tabs, like switching tabs on a console.
  function switchTab(dir: -1 | 1) {
    const ids = tabs.map((t) => t.id);
    const i = ids.indexOf(tab);
    tab = ids[(i + dir + ids.length) % ids.length];
  }

  function editButton(btn: string) {
    if (btn === 'LeftTrigger') switchTab(-1);
    else if (btn === 'RightTrigger') switchTab(1);
  }
</script>

<div
  class="flex h-screen flex-col bg-bg-0 text-ink-0"
  use:gamepadScope={{ onBack: () => history.back(), onButton: editButton }}
  style:--gp-focus={accent}
>
  <AppChrome sub="EDIT · ENTRY" {accent} onback={() => history.back()} />

  {#if error && !form}
    <main class="flex flex-1 flex-col items-center justify-center gap-3 px-6 text-center">
      <p class="text-bad">{error}</p>
      <Btn variant="ghost" onclick={cancel}>Close</Btn>
    </main>
  {:else if !form}
    <main class="flex flex-1 items-center justify-center">
      <p class="font-mono text-[10px] uppercase tracking-[0.12em] text-ink-3">Loading…</p>
    </main>
  {:else}
    <main class="flex flex-1 flex-col overflow-hidden">
      <!-- Header: cover thumb + catalog + name -->
      <header class="flex items-center gap-3.5 border-b border-line-1 px-5 py-4">
        <div
          class="size-[50px_70px] h-[70px] w-[50px] shrink-0 overflow-hidden rounded-sm border border-line-1 bg-bg-2"
        >
          {#if cover}
            <img src={cover} alt={form.game_name} class="h-full w-full object-cover" />
          {:else}
            <div
              class="flex h-full w-full items-center justify-center"
              style:background="linear-gradient(160deg, #2a2622 0%, #0a0807 100%)"
            ></div>
          {/if}
        </div>
        <div class="min-w-0 flex-1">
          <div class="flex items-center gap-2">
            <CatalogId id={fmtCatalog(form.catalog_number)} {accent} />
            <MonoLabel size={10}>
              <span style:color={accent}>EDITING</span>
            </MonoLabel>
          </div>
          <div
            class="font-display mt-1 truncate text-[18px] font-semibold"
            style:letter-spacing="-0.012em"
            title={form.game_name}
          >
            {form.game_name}
          </div>
        </div>
      </header>

      <!-- Tab bar -->
      <div class="flex gap-0 border-b border-line-1 bg-bg-1 px-5">
        {#each tabs as t (t.id)}
          {@const active = tab === t.id}
          <button
            type="button"
            onclick={() => (tab = t.id)}
            data-gp-autofocus={active ? '' : undefined}
            class="cursor-pointer border-b-2 px-3.5 py-2.5 text-[12.5px] transition-colors"
            style:border-color={active ? accent : 'transparent'}
            style:color={active ? 'var(--color-ink-0)' : 'var(--color-ink-2)'}
            style:font-weight={active ? 500 : 400}
          >
            {t.label}
          </button>
        {/each}
      </div>

      <!-- Tab content -->
      <div class="flex-1 overflow-y-auto px-5 py-4">
        {#snippet field(label: string, helper: string, control: import('svelte').Snippet)}
          <EditRow {label} {helper} children={control} />
        {/snippet}

        {#if tab === 'identity'}
          {@render field(
            'Title',
            "What shows in the library and on the detail page.",
            identityTitle,
          )}
          {@render field(
            'Cover art',
            'Refetch artwork to update both the image and the accent colour.',
            identityCover,
          )}

          {#snippet identityTitle()}
            <TextField bind:value={form!.game_name} full />
          {/snippet}
          {#snippet identityCover()}
            <div class="flex flex-wrap gap-1.5">
              <Btn variant="ghost" onclick={refetchCover}>
                {#snippet icon()}<RefreshCw size={14} />{/snippet}
                Refetch artwork
              </Btn>
            </div>
          {/snippet}
        {:else if tab === 'install'}
          {@render field('Install folder', 'Where the game lives on disk.', installFolder)}
          {@render field('Executable', 'The file Spool launches.', installExe)}
          {@render field('Added on', 'When this entry first appeared in your library.', installAdded)}

          {#snippet installFolder()}
            {#if form!.installed}
              <div class="flex flex-col gap-1.5">
                <div class="flex gap-1.5">
                  <TextField
                    bind:value={form!.game_folder_path as unknown as string}
                    placeholder="(unset)"
                    mono
                    full
                  />
                  <Btn variant="ghost" onclick={browseFolder}>
                    {#snippet icon()}<Folder size={14} />{/snippet}
                    Browse
                  </Btn>
                </div>
                {#if original?.game_folder_path}
                  <div>
                    <Btn
                      variant="ghost"
                      onclick={() =>
                        moveInstallDialog.request(original!, {
                          onDone: () => void getCurrentWindow().close(),
                        })}
                    >
                      {#snippet icon()}<FolderInput size={14} />{/snippet}
                      Move to another drive…
                    </Btn>
                  </div>
                {/if}
              </div>
            {:else}
              <!-- Uninstalled: install paths are owned by Reinstall, not the
                   editor — editing them here would be silently discarded on save
                   (replace() keeps the cleared paths while installed=false). -->
              <span class="text-[12px] text-ink-3"
                >Not installed — use Reinstall to set the install location.</span
              >
            {/if}
          {/snippet}
          {#snippet installExe()}
            {#if form!.installed}
              <div class="flex gap-1.5">
                <TextField bind:value={form!.exe_path} mono full />
                <Btn variant="ghost" onclick={browseExe}>
                  {#snippet icon()}<Folder size={14} />{/snippet}
                  Browse
                </Btn>
              </div>
            {:else}
              <span class="text-[12px] text-ink-3">Not installed — reinstall to set the executable.</span>
            {/if}
          {/snippet}
          {#snippet installAdded()}
            <span class="font-mono text-[11.5px] text-ink-2">
              {absDateTime(form!.added_at)}
            </span>
          {/snippet}
        {:else if tab === 'launch'}
          {#if !isLinux}
            {@render field(
              'Run as administrator',
              registryRunAsAdmin
                ? 'Already enabled by Windows for this exe — Spool will elevate either way.'
                : 'Required by some games (mostly older / DRM-laden). Off by default. Triggers a UAC prompt at launch.',
              launchRunAs,
            )}
          {/if}
          {#if isLinux && exeIsWindows}
            {@render field(
              'Proton version',
              'This Windows game launches through Proton automatically on Linux. Choose which Proton build to use; Auto picks the newest installed.',
              protonSelect,
            )}
            {@render field(
              'Wine prefix',
              'Override the per-game prefix folder. Leave blank for the Spool default.',
              prefixField,
            )}
            {@render field(
              'Install dependencies',
              'Install Windows runtime packages into this prefix via winetricks. Needs UMU or GE-Proton.',
              depsRow,
            )}
            {@render field(
              'Run an installer',
              "Run another Windows .exe (e.g. a game patch or update installer) inside this game's Proton prefix. Launch the game once first so the prefix exists.",
              runExeRow,
            )}
          {/if}
          {@render field(
            'Launch arguments',
            'Extra command-line arguments passed after the executable.',
            argsField,
          )}

          {#snippet launchRunAs()}
            <div class="flex flex-col items-end gap-1.5">
              <Toggle bind:checked={form!.run_as_admin} aria-label="Run as administrator" />
              {#if registryRunAsAdmin}
                <span
                  class="font-mono inline-flex items-center gap-1 rounded-[3px] px-1.5 py-0.5 text-[9.5px] uppercase tracking-[0.08em]"
                  style:background="rgba(126,198,255,0.10)"
                  style:color="var(--color-info)"
                  title="Windows AppCompatFlags layers has RUNASADMIN set for this exe"
                >
                  Registry
                </span>
              {/if}
            </div>
          {/snippet}
          {#snippet protonSelect()}
            <Select
              bind:value={
                () => form!.proton_version_path ?? '',
                (v) => (form!.proton_version_path = v)
              }
              options={protonOptions}
            />
          {/snippet}
          {#snippet prefixField()}
            <div class="flex gap-1.5">
              <TextField
                bind:value={
                  () => form!.wine_prefix_path ?? '',
                  (v) => (form!.wine_prefix_path = v)
                }
                mono
                full
                placeholder="Spool default"
              />
              <Btn variant="ghost" onclick={browsePrefix}>
                {#snippet icon()}<Folder size={14} />{/snippet}
                Browse
              </Btn>
            </div>
          {/snippet}
          {#snippet argsField()}
            <TextField
              bind:value={
                () => form!.launch_args ?? '',
                (v) => (form!.launch_args = v)
              }
              mono
              full
              placeholder="--windowed -nolauncher"
            />
          {/snippet}
          {#snippet depsRow()}
            <div class="flex flex-col gap-2">
              <div class="grid grid-cols-2 gap-1.5">
                {#each VERB_PRESETS as p (p.verb)}
                  <label class="flex cursor-pointer items-start gap-2 rounded-[4px] border border-line-1 bg-bg-1 px-2.5 py-2 hover:bg-bg-2">
                    <input
                      type="checkbox"
                      checked={depsChecked.has(p.verb)}
                      onchange={(e) => togglePreset(p.verb, e.currentTarget.checked)}
                      class="mt-0.5 shrink-0 accent-[var(--color-spool)]"
                    />
                    <div class="min-w-0">
                      <div class="font-mono truncate text-[11px] font-medium text-ink-0">{p.verb}</div>
                      <div class="truncate text-[10px] text-ink-3">{p.label}</div>
                    </div>
                  </label>
                {/each}
              </div>
              <label class="flex cursor-pointer items-center gap-2 rounded-[4px] border border-line-1 bg-bg-1 px-2.5 py-2 hover:bg-bg-2">
                <input
                  type="checkbox"
                  bind:checked={depsCustomEnabled}
                  class="shrink-0 accent-[var(--color-spool)]"
                />
                <span class="font-mono text-[11px] font-medium text-ink-0">custom</span>
                {#if depsCustomEnabled}
                  <input
                    bind:value={depsCustom}
                    placeholder="e.g. dotnet7 d3dcompiler_47"
                    class="font-mono ml-1 min-w-0 flex-1 rounded-[3px] border border-line-2 bg-bg-2 px-2 py-0.5 text-[11px] text-ink-0 outline-none placeholder:text-ink-3 focus:border-line-3"
                  />
                {/if}
              </label>
              <div class="flex items-center justify-between">
                {#if effectiveDeps}
                  <span class="font-mono text-[10px] text-ink-3 truncate">→ {effectiveDeps}</span>
                {:else}
                  <span></span>
                {/if}
                <Btn variant="ghost" onclick={installDeps} disabled={depsInstalling || !effectiveDeps}>
                  {#snippet icon()}<Download size={14} />{/snippet}
                  {depsInstalling ? 'Installing…' : 'Install'}
                </Btn>
              </div>
            </div>
          {/snippet}
          {#snippet runExeRow()}
            <div class="flex flex-col gap-1.5">
              <div class="flex gap-1.5">
                <TextField
                  bind:value={runExePath}
                  mono
                  full
                  placeholder="/path/to/update-installer.exe"
                />
                <Btn variant="ghost" onclick={browseRunExe} disabled={runningExe}>
                  {#snippet icon()}<Folder size={14} />{/snippet}
                  Browse
                </Btn>
              </div>
              <div class="flex justify-end">
                <Btn
                  variant="ghost"
                  onclick={runInstaller}
                  disabled={runningExe || !runExePath.trim()}
                >
                  {#snippet icon()}<Play size={14} />{/snippet}
                  {runningExe ? 'Running…' : 'Run'}
                </Btn>
              </div>
            </div>
          {/snippet}
        {:else if tab === 'saves'}
          <SavesPanel
            gameId={form.id}
            catalogNumber={form.catalog_number}
            savePaths={form.save_paths}
            usesProton={isLinux && exeIsWindows}
            {prefixReady}
            customSave={form.custom_save}
            manifestOverride={form.manifest_override}
            onChange={(cs) => (form!.custom_save = cs)}
            onOverrideChange={(ov) => (form!.manifest_override = ov)}
          />
        {:else if tab === 'sharing'}
          {@render field(
            'Share over LAN',
            hasFolder
              ? "When on, other Spool devices on your network can install this game from you."
              : "Set an install folder on the Install tab first — there's nothing to stream without it.",
            sharingToggle,
          )}
          {@render field(
            'Visible to peers',
            'Peers see the title, catalog number, developer, and install size. They never see local file paths.',
            sharingVisibility,
          )}
          {@render field(
            'Blake3 manifest',
            'The hashing manifest peers use to verify files and resume downloads. Prepared automatically for shared games; the button re-runs it by hand.',
            sharingManifest,
          )}

          {#snippet sharingToggle()}
            <div class="flex flex-col gap-1.5">
              <Toggle
                bind:checked={form!.lan_shared}
                disabled={!hasFolder}
                aria-label="Share this game over LAN"
              />
              {#if !hasFolder && form!.lan_shared}
                <span class="font-mono text-[10px] uppercase tracking-[0.12em] text-warn">
                  Folder required
                </span>
              {/if}
            </div>
          {/snippet}
          {#snippet sharingVisibility()}
            <span class="text-[11.5px] text-ink-2">
              Title · Catalog # · Developer · Publisher · Genres · Install size · Save metadata
            </span>
          {/snippet}
          {#snippet sharingManifest()}
            <div class="flex items-center gap-3">
              <span class="font-mono text-[11px] uppercase tracking-[0.08em] inline-flex items-center gap-1.5">
                {#if manifestStatus === 'generated'}
                  <span class="inline-block size-2 rounded-full" style:background="var(--color-ok)"></span>
                  <span style:color="var(--color-ok)">Generated</span>
                {:else if manifestStatus === 'generating'}
                  <span class="inline-block size-2 rounded-full animate-pulse" style:background="var(--color-warn)"></span>
                  <span style:color="var(--color-warn)">Generating…</span>
                {:else}
                  <span class="inline-block size-2 rounded-full" style:background="var(--color-ink-3)"></span>
                  <span class="text-ink-3">No manifest</span>
                {/if}
              </span>
              <Btn
                variant="ghost"
                onclick={triggerPrepareManifest}
                disabled={preparingManifest || !hasFolder}
              >
                {#snippet icon()}
                  <RefreshCw size={14} class={preparingManifest || manifestStatus === 'generating' ? 'animate-spin' : ''} />
                {/snippet}
                {preparingManifest || manifestStatus === 'generating' ? 'Preparing…' : 'Prepare manifest'}
              </Btn>
            </div>
          {/snippet}
        {/if}
      </div>

      <!-- Footer -->
      <footer class="flex items-center gap-2 border-t border-line-1 bg-black/20 px-5 py-3">
        <Btn variant="danger" onclick={remove}>
          {#snippet icon()}<Trash2 size={14} />{/snippet}
          Remove…
        </Btn>
        <div class="flex-1"></div>
        <Btn variant="ghost" onclick={cancel}>Cancel</Btn>
        <button
          type="button"
          onclick={save}
          disabled={!dirty || saving}
          class="font-sans inline-flex h-8 min-w-[120px] items-center justify-center gap-1.5 rounded-sm border-none px-3 text-[13px] font-medium transition-opacity"
          class:cursor-pointer={dirty && !saving}
          class:cursor-not-allowed={!dirty || saving}
          class:opacity-50={!dirty || saving}
          style:background={accent}
          style:color="#0b0c0e"
        >
          {saving ? 'Saving…' : 'Save changes'}
        </button>
      </footer>
    </main>
  {/if}
</div>
