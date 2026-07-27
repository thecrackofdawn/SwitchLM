import { getCurrentWindow, LogicalSize } from "@tauri-apps/api/window";

/** px between adjacent pipeline children — matches RouteLine.vue `.pipeline { gap: 10px }`. */
const PIPELINE_GAP = 10;
/** Extra px so later badge-text changes (usage poll) don't re-introduce a wrap. */
const SAFETY_MARGIN = 24;

/**
 * Pure policy. Given the widest pipeline's natural (unwrapped) width and its current
 * rendered (client) width, return the window width to set — or null when no grow is
 * needed. Grow-only; capped at availWidth; null when content already fits.
 *
 * Returns null (no-op) when:
 *   - naturalWidth <= clientWidth  (content fits — spec req. 2), OR
 *   - the capped target is not larger than currentWindowWidth (no shrink / cap below current).
 */
export function computeTargetWidth(
  naturalWidth: number,
  clientWidth: number,
  currentWindowWidth: number,
  availWidth: number,
  margin: number = SAFETY_MARGIN,
): number | null {
  const overflow = naturalWidth - clientWidth;
  if (overflow <= 0) return null; // fits → no grow (spec req. 2)
  const needed = currentWindowWidth + overflow + margin;
  const target = Math.min(needed, availWidth);
  return target > currentWindowWidth ? target : null;
}

/** Natural (unwrapped) width of a `.pipeline` = sum of its direct children + gaps. */
function naturalWidthOf(el: HTMLElement): number {
  const kids = Array.from(el.children) as HTMLElement[];
  const sum = kids.reduce((acc, c) => acc + c.getBoundingClientRect().width, 0);
  return sum + PIPELINE_GAP * Math.max(0, kids.length - 1);
}

let done = false; // one-shot per session (module survives Dashboard remounts)

/**
 * Impure glue. Measures the widest pipeline and, if the window is too narrow, grows it
 * (grow-only, capped at screen.availWidth). Fires exactly once per session; no-ops on
 * later calls or when no pipelines are present. Errors are swallowed — the fit is
 * best-effort and must never break the UI.
 */
export async function fitWindowToRoutes(pipelineEls: HTMLElement[]): Promise<void> {
  if (done) return;
  done = true; // consume the one-shot whether or not pipelines exist
  if (pipelineEls.length === 0) return;
  try {
    const widest = pipelineEls
      .map((el) => ({ natural: naturalWidthOf(el), client: el.clientWidth }))
      .reduce((a, b) => (b.natural > a.natural ? b : a));
    // All values are CSS px → unit-consistent, no physical/logical conversion:
    //   - naturalWidth / clientWidth: getBoundingClientRect / clientWidth
    //   - current width / height: documentElement.clientWidth/Height (webview inner)
    //   - cap: window.screen.availWidth (taskbar-aware)
    const target = computeTargetWidth(
      widest.natural,
      widest.client,
      document.documentElement.clientWidth,
      window.screen.availWidth,
    );
    if (target != null) {
      await getCurrentWindow().setSize(
        new LogicalSize(Math.round(target), Math.round(document.documentElement.clientHeight)),
      );
    }
  } catch (err) {
    // Resize is best-effort — never let it surface as a user-visible error.
    console.warn("startup window fit failed:", err);
  }
}
