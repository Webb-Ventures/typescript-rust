// Class statics and enum members merged with a namespace are not in scope as bare names in
// the namespace's body (other enum declarations of the same enum do see the members).
class Clod {
    private static x = 10;
    public static y = 10;
    m() {}
}
namespace Clod {
    var p = Clod.x;
    var q = x;
    var t = y;
    var u = m;
    export var w = 1;
}
namespace Clod {
    var v = w;
}
enum Foo {
    b
}
enum Foo {
    a = b
}
namespace Foo {
    export var x = b
}
function fn() {}
namespace fn {
    export var k = 1;
}
namespace fn {
    var kk = k;
}
