// Storing an external value as unknown and narrowing it by hand is sound.
const raw: unknown = JSON.parse('{"v":1}');
let v: unknown = undefined;
if (typeof raw === "object" && raw !== null && "v" in raw) {
  v = raw.v;
}
export const isNumber: boolean = typeof v === "number";
