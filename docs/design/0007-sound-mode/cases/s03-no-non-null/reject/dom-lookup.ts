// Asserting a DOM lookup succeeded.
const el = document.getElementById("app")!; // @expect TSR0301
export const tag: string = el.tagName;
