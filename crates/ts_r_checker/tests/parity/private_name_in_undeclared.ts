class Foo {
  #a = 1;
  m(v: object) {
    return #a in v && #b in v;
  }
}
