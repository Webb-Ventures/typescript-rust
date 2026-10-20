// An object literal against 'X[] | X | undefined' is matched with the nullish member, so it is
// not elaborated, and the chain skips the member line.
interface Op<T> { v: T }
interface W3 { text?: string | Op<string> }
interface W4 { other?: number }
declare const src: { text: null };
export const y1: W3[] | W3 | undefined = src;
export const y2: W3[] | W3 = src;
export const y3: W4 | W3 | undefined = src;
export const y4: W3 | undefined = src;
export const y5: W3[] | W3 | W4 | undefined = src;
declare function f5(o: { where: W3[] | W3 | undefined }): void;
f5({ where: { text: null } });
declare function f7(o: { where: W3[] | W3 | null }): void;
f7({ where: { text: null } });
