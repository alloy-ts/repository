mod branch;
mod commit;
mod config;
mod diff;
mod dirwalk;
mod error;
mod index;
mod mailmap;
mod merge;
mod notes;
mod object;
mod pathspec;
mod reference;
mod remote;
mod repository;
mod revision;
mod submodule;
mod worktree;

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
