// An inline predicate in a filter callback is checked too.
type Media = { readonly kind: "book" } | { readonly kind: "dvd" } | { readonly kind: "cd" };
type Book = Extract<Media, { kind: "book" }>;
const items: Media[] = [{ kind: "cd" }];
export const books: Book[] = items.filter((m): m is Book => m.kind !== "dvd"); // @expect TSR0701
