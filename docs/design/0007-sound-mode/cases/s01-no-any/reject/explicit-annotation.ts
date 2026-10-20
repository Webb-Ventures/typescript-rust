// Writing any in a variable annotation.
const raw: any = 1; // @expect TSR0101
export const copy: unknown = raw;
