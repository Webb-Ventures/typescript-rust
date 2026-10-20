// With a string index on the target, an index signature of type any accepts any non-primitive source.
interface Document {
  [key: string]: any;
}
type WithoutId<T> = Omit<T, "_id">;
declare function insert(d: WithoutId<any>): void;
declare const doc: Document;
insert(doc);
declare const t: { [k: string]: any; [k: symbol]: any };
export const a: { [k: string]: any; [k: number]: string } = doc;
export const b: { [k: string]: number; [k: symbol]: any } = doc;
export const c: { [k: string]: any; [k: symbol]: any } = "x";
export const d: { [k: number]: any; [k: symbol]: any } = doc;
