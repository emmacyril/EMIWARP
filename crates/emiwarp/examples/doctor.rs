fn main() {
    let inv = emiwarp::discover();
    print!("{}", inv.report());
    println!("\nsuggested provider: {:?}", inv.suggested_provider());
    println!("usable harnesses: {}", inv.usable_harnesses().count());
}
