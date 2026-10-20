// string[] to (string | number)[]: a number could be pushed into the string array.
const strings: string[] = ["a"];
const mixed: (string | number)[] = strings; // @expect TSR0501
export const n: number = mixed.length;
