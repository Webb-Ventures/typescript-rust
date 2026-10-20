// A default instead of asserting the key exists.
function lookup(map: ReadonlyMap<string, number>, key: string): number {
  return map.get(key) ?? 0;
}
export const v: number = lookup(new Map([["a", 1]]), "a");
