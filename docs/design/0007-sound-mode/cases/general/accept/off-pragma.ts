// @ts-r off
// A file at the off level is plain TypeScript.
const raw: any = JSON.parse("{}");
const el = document.getElementById("app")!;
export const out: string = (raw as { name: string }).name + el.tagName;
