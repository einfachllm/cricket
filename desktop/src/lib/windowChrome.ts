/// Custom-chrome window controls for the frameless sidecar window.
///
/// Every call goes through a dynamic import and an availability guard, the
/// same pattern `initBackendUrl()` uses: under plain `npm run dev` in a
/// browser (and in jsdom) there is no Tauri runtime, so the controls render
/// inert instead of throwing.

/// True only when running inside the Tauri webview.
export function windowControlsAvailable(): boolean {
  return typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;
}

async function currentWindow(): Promise<{
  minimize: () => Promise<void>;
  toggleMaximize: () => Promise<void>;
  close: () => Promise<void>;
  setAlwaysOnTop: (alwaysOnTop: boolean) => Promise<void>;
} | null> {
  if (!windowControlsAvailable()) return null;
  const { getCurrentWindow } = await import('@tauri-apps/api/window');
  return getCurrentWindow();
}

export async function minimizeWindow(): Promise<void> {
  await (await currentWindow())?.minimize();
}

export async function toggleMaximizeWindow(): Promise<void> {
  await (await currentWindow())?.toggleMaximize();
}

export async function closeWindow(): Promise<void> {
  await (await currentWindow())?.close();
}

/// The window is pinned (always-on-top) straight from `tauri.conf.json`, so
/// the toggle starts on and this only ever needs to flip it.
export async function setAlwaysOnTop(on: boolean): Promise<boolean> {
  const win = await currentWindow();
  if (!win) return false;
  await win.setAlwaysOnTop(on);
  return true;
}
