function f<T>(thing: T & (0 | 1 | 2)) {
  "key" in thing;
}
