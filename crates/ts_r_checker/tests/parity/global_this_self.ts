// `typeof globalThis` has a readonly `globalThis` property that is the object itself.
const x = globalThis.globalThis;
const y: number = globalThis.globalThis.globalThis.parseInt("1");
const z: string = x.Math.PI;
const g: typeof globalThis = globalThis.globalThis;
globalThis.globalThis = globalThis;
