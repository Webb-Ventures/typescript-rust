import type { Box } from "./shapes";

declare module "./shapes" {
  interface Box {
    label?: string;
  }
}

export function describe(b: Box): string {
  return b.label ?? `${b.min.x},${b.min.y}`;
}
