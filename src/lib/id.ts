// Auto-generate a short, unique internal id for new Model/Profile entries.
// The id is an implementation detail (cross-ref key) and is never shown to the user,
// so a random slug is fine and works regardless of the (possibly non-Latin) name.
export function genId(prefix: string, existing: Iterable<string>): string {
  const taken = new Set(existing);
  for (let i = 0; i < 8; i++) {
    const id = `${prefix}_${Math.random().toString(36).slice(2, 8)}`;
    if (!taken.has(id)) return id;
  }
  return `${prefix}_${Date.now().toString(36)}`;
}
