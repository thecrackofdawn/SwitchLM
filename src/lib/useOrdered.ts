import { ref, watch } from "vue";

/**
 * Keeps a locally (localStorage) ordered view of a source list so the user can
 * drag-reorder rows and have the order persist across reloads — without touching
 * the backend. Items not yet in the stored order are appended at the end, in their
 * natural order.
 *
 * Bind `ordered` to <draggable v-model="ordered" item-key="..."> and call
 * `commit()` on the drag's @end to persist the new order.
 */
export function useOrdered<T>(storageKey: string, source: () => T[], getId: (item: T) => string) {
  function loadOrder(): string[] {
    try {
      const raw = localStorage.getItem(storageKey);
      return raw ? (JSON.parse(raw) as string[]) : [];
    } catch {
      return [];
    }
  }

  function sortByOrder(items: T[]): T[] {
    const rank = new Map<string, number>();
    loadOrder().forEach((id, i) => rank.set(id, i));
    return [...items].sort((a, b) => {
      const ia = rank.get(getId(a));
      const ib = rank.get(getId(b));
      if (ia === undefined && ib === undefined) return 0;
      if (ia === undefined) return 1; // unknown ids sink to the end
      if (ib === undefined) return -1;
      return ia - ib;
    });
  }

  const ordered = ref<T[]>(sortByOrder(source()));

  // Re-sort when the underlying source changes (added/removed items), keeping the saved order.
  watch(source, (items) => {
    ordered.value = sortByOrder(items);
  }, { deep: true });

  // Persist the current order after a drag.
  function commit() {
    localStorage.setItem(storageKey, JSON.stringify((ordered.value as T[]).map(getId)));
  }

  return { ordered, commit };
}
