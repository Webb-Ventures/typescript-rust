// TypeScript infers the predicate for this callback, so it is proved by construction.
const mixed: (string | number)[] = ["a", 1];
export const strings: string[] = mixed.filter((v) => typeof v === "string");
