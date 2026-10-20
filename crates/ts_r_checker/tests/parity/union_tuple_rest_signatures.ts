declare const swapped: ((a: number, b: string) => void) | ((...args: [string, number]) => void);
swapped(1, "x");
declare const later: ((a: number, b: string) => void) | ((...args: [number, number]) => void);
later(1, "x");
declare const offset: ((a: boolean, b: string, c: number) => void) | ((a: boolean, ...args: [string, string]) => void);
offset(true, "x", 1);
declare const matching: ((a: number, b: string) => void) | ((...args: [number, string]) => void);
matching(1, "x");
