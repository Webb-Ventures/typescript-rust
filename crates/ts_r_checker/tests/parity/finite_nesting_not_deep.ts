// A finite nesting of existing instantiations is compared in full, not cut off as deeply
// nested.
type Cb<T> = {noAlias: () => T}["noAlias"];
declare const x: Cb<Cb<Cb<Cb<number>>>>;
declare let y: Cb<Cb<Cb<Cb<string>>>>;
y = x;
