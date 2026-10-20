// @target: es2022
// Parentheses around an asserted optional chain stay only where the chain would continue
// (ecosystem: zod src/v4/core/to-json-schema.ts).
declare const p: any, q: any;
export const a = (p?.u as string) ?? 't';
export const b = (p?.u as any).x;
export const c = (p?.u as any)();
export const d = (p?.u!) ?? 1;
export const e = (p?.u as any) + 1;
export const f = (p?.u satisfies any)?.x;
export const g = (p?.u as any)[0];
export const h = (p?.u as any)`t`;
export const i = (q.u as any)?.x;
export const j = new (p?.u as any)();
export const k = (<any>p?.u)?.[0];
