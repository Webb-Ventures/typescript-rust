// Mentioning @ts-ignore in prose or in a string is not a directive.
// The old code used a @ts-ignore here; see the migration notes.
const note: string = "// @ts-ignore";
export const len: number = note.length;
