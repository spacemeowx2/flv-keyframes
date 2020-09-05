use serde::{Serialize, Deserialize};

#[derive(Serialize, Deserialize, Debug)]
pub struct Patch {
    pub origin_pos: usize,
    pub origin_size: usize,
    pub patched: Vec<u8>,
}
