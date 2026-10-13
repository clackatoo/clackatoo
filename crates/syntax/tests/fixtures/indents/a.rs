struct Point {
x: i32,
y: i32,
}

impl Point {
fn new(x: i32,
y: i32) -> Self {
let p = Point {
x,
y,
};
match p.x {
0 => {
p
}
_ => p,
}
}
}

fn f(a: u8) -> u8 {
// a comment
if a > 1 {
return a;
} else {
foo(a,
b)
}
}
