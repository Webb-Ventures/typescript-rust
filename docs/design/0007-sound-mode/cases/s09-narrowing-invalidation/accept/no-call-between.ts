// Nothing can run between the check and the use.
interface Holder {
  name: string | null;
}
function nameLength(o: Holder): number {
  if (o.name !== null) return o.name.length;
  return 0;
}
export const n: number = nameLength({ name: "a" });
