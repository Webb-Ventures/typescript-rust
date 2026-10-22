// Spreading a tuple with a rest element into an array literal keeps the rest element as
// an array: the literal is '[number, string, boolean, ...boolean[]]'.
declare const sbb_: [string, boolean, ...boolean[]];
let k6: [number, string, boolean, boolean, ...boolean[]];
k6 = [1, ...sbb_];
const v = [1, ...sbb_] as const;
k6 = v;
let k8: [number, string, boolean, boolean, ...boolean[]] = [1, ...sbb_];
let k9: [number, string] = [1, ...sbb_];
