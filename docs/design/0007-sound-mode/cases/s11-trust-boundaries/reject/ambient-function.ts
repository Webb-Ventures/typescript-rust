// declare function in a sound file asserts a function exists with this signature.
declare function nativeHash(input: string): number; // @expect TSR1103
export const h: number = nativeHash("a");
