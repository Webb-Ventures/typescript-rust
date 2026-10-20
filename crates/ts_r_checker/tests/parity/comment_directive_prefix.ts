// `@ts-ignore` and `@ts-expect-error` match as prefixes (`// @ts-ignore-line`).
export {};
// @ts-ignore-line
const a: string = 1;
// @ts-ignorefoo
const b: string = 1;
// @ts-ignore: why
const c: string = 1;
// @ts-expect-error-x
const d: string = 1;
// @ts-expect-errorx
const e: string = 1;
/* @ts-ignore-me */
const f: string = 1;
//@ts-ignore
const g: string = 1;
// @ts-ignore-line
const h = 1;
// @ts-expect-error-x
const i = 1;
