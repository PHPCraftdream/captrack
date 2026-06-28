macro_rules! noisy {
    () => {
        Vec::with_capacity(99)
    };
}
fn main() {
    let _ = noisy!();
}
