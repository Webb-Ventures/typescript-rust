// panic() marks a bug, not an error callers handle, so it is unchecked.
import { panic } from "@ts-r.dev/guard";

export function half(n: number): number {
  if (n % 2 !== 0) panic("n must be even");
  return n / 2;
}
