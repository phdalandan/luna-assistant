const GIGABYTE = 1_000_000_000;

export function formatSize(bytes: number): string {
  return `${(bytes / GIGABYTE).toFixed(1)} GB`;
}

export function formatTime(timestamp: number): string {
  return new Date(timestamp).toLocaleTimeString([], {
    hour: "numeric",
    minute: "2-digit",
  });
}
