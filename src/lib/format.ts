export function fmtTokens(n?: number | null): string {
  if (n == null) return "未知";
  if (n >= 1_000_000) return `${(n / 1_000_000).toLocaleString()}M`;
  if (n >= 1000) return `${Math.round(n / 1000)}K`;
  return String(n);
}
