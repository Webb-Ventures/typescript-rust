// Union members that are aliased anonymous types sort by their alias name (tsc 7).
export {};
interface Router { _def: { router: true } }
type Zed = { a: 1 };
type Abc = { b: 1 };
const z: null = null as any as Router | Zed;
const a: null = null as any as Router | Abc;
const c: null = null as any as Router | { c: 1 };
type Fn = () => void;
const d: null = null as any as Router | Fn;
