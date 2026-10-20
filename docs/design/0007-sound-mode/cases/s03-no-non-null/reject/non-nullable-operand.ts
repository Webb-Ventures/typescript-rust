// ! on a type that is already non-nullable is still reported (it would lie after a refactor).
const n: number = 1;
export const m: number = n!; // @expect TSR0301
