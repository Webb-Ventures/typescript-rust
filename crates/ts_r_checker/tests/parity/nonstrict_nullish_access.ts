// @filename: tsconfig.json
{ "compilerOptions": { "strict": false } }
// @filename: a.ts
declare const u: undefined;
u.x;
null.y;
undefined.z;
const v = this.w;
declare let n: number | undefined;
n.toFixed();
export {};
