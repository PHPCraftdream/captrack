fn main() {
    let _v: Vec<u32> = Vec::with_capacity(8);
}
#[cfg(test)]
mod tests {
    #[test]
    fn t() {
        let _v: Vec<u32> = Vec::with_capacity(99); // should be skipped by default
    }
}
