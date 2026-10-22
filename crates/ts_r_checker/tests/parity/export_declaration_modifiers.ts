// Modifiers on export declarations: the keyword rules first (TS1275), then TS1193 / TS1120.
const a = 1, b = 2, c = 3;
accessor export { a };
accessor export default a;
declare export { b };
export export { c };
