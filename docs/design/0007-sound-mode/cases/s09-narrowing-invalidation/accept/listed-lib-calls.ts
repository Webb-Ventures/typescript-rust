// String methods and Math are on the list of calls that can't mutate objects.
interface Holder {
  name: string | null;
}
function score(o: Holder): number {
  if (o.name !== null) {
    const t = o.name.trim();
    return o.name.length + t.length + Math.max(1, 2);
  }
  return 0;
}
export const s: number = score({ name: " a " });
