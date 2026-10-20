// Asserting a key exists.
function lookup(cache: ReadonlyMap<string, number>, key: string): number {
  return cache.get(key)!; // @expect TSR0301
}
export const v: number = lookup(new Map<string, number>(), "a");
