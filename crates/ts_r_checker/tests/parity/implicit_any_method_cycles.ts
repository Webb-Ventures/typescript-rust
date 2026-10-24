type Foo = { [P in "bar"] };

class It {
  next() {
    return { done: false as const, value: v };
  }
  [Symbol.iterator]() {
    return this;
  }
}
for (var v of new It()) { }

class Self {
  [Symbol.iterator]() {
    return w;
  }
}
for (var w of new Self()) { }
