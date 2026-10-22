// A primitive in a union contextual type contributes its apparent type's members.
interface Full { normalize?: (m: { x: string }) => void; other?: (m: { x: string }) => void; }
const a: { f: string | Full } = { f: { normalize: m => m.x } };
const b: { f: string | Full } = { f: { other: m => m.x } };
const c: number | Full = { normalize: m => m.x };
const d: number | { toFixed?: (m: { x: string }) => void } = { toFixed: m => m.x };
export {};
