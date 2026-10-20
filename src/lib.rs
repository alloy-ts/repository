mod commit;
mod error;
mod object;
mod reference;
mod remote;
mod repository;

use napi_derive::napi;

#[napi]
pub fn add(left: u32, right: u32) -> u32 {
    left + right
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_works() {
        let result = add(2, 2);
        assert_eq!(result, 4);
    }
}
