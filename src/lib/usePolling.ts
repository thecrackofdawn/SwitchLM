import { onUnmounted, watch } from "vue";

/**
 * Repeatedly calls `fn` every `intervalMs` while the host component is mounted,
 * stopping automatically on unmount. The first call happens after the interval
 * (not immediately), so a refresh already triggered in `onMounted` is not
 * duplicated right away.
 *
 * `intervalMs` may be a number or a `() => number` getter. When it is a getter,
 * the timer is re-armed (clear + restart) whenever the value changes — e.g. once
 * settings finish loading after mount, or when the user changes the interval.
 *
 * Errors are swallowed — background polling must never spam the UI with toasts.
 * Callers that surface failures (e.g. a manual "刷新" button) keep their own
 * try/catch around the same store action.
 *
 * There is no `<keep-alive>` in the app shell, so views go fully dormant while
 * the user is on another tab; this interval is cleared on unmount and re-armed
 * on the next mount, so tab-hopping never leaks a dangling timer.
 */
export function usePolling(
  fn: () => void | Promise<void>,
  intervalMs: number | (() => number),
): void {
  const getMs = typeof intervalMs === "number" ? () => intervalMs : intervalMs;
  let handle: number | null = null;
  const stop = () => {
    if (handle != null) {
      clearInterval(handle);
      handle = null;
    }
  };
  const arm = () => {
    stop();
    handle = window.setInterval(() => {
      Promise.resolve(fn()).catch(() => {});
    }, getMs());
  };
  arm();
  if (typeof intervalMs !== "number") {
    watch(intervalMs, arm);
  }
  onUnmounted(stop);
}
