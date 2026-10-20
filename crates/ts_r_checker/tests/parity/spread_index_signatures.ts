// A spread keeps its index signatures when nothing precedes or follows it;
// members before or after drop them.
type H = { [key: string]: string };
declare const h: H;
declare function hs(): H;
const a = { ...h };
a["host"];
const b = { ...hs() };
b["x"] = "y";
const c = { ...h, extra: 1 };
const c2: number = c.extra;
c["z"];
interface Env { [k: string]: string | undefined; TZ?: string }
declare const env: Env;
const d = { ...env };
d["q"];
const e = { x: 1, ...h };
const e2: string = e.x;
declare const nh: { [k: number]: boolean };
const f = { ...nh };
const f2: string = f[0];
