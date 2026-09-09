fn main() {
    let mut p = vt100::Parser::new(1, 10, 100);
    p.process(b"abcdefghijklmnopqrstuvwxyz");
    println!("1x10 contents: {:?}", p.screen().contents());
    let mut p = vt100::Parser::new(1, 1, 0);
    p.process("aあb".as_bytes());
    println!("1x1 wide:      {:?}", p.screen().contents());
    let mut p = vt100::Parser::new(3, 10, 100);
    p.process(b"abcdefghijklmno");
    println!("3x10 contents: {:?}", p.screen().contents());
}
