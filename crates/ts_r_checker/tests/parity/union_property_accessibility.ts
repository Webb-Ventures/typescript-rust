// A union's property needs a shared declaration when one member's is private or protected.
export {};
class Default { member = ""; }
class Public { public member = ""; }
class Protected { protected member = ""; }
class Private { private member = 1; }
declare var v5: Default | Public;
declare var v6: Default | Protected;
declare var v8: Public | Protected;
declare var v10: Protected | Private;
v5.member;
v6.member;
v8.member;
v10.member;
class Foo { protected foo = 0; }
class Bar { protected foo = 0; }
type _3 = (Foo & Bar)['foo'];
type _4 = (Foo | Bar)['foo'];
type _5 = (Foo | (Foo & Bar))['foo'];
type _6 = ({ a: 1 } | { b: 2 })['a'];
type _7 = ({ a: 1 } | { a: 2 })['a'];
type _8 = (string | { a: 1 })['a'];
