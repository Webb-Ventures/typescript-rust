// An excess property in an element of an array literal under a type assertion is reported
// alone, at the property.
<{ id: number; }[]>[{ foo: "s" }];
<{ id: number; }[]>[{ foo: "s" }, {}];
<{ id: number; }>{ foo: "s" };
[{ foo: "s", id: "x" }] as { id: number }[];
