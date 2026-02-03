pub mod ahand {
    pub mod v1 {
        include!(concat!(env!("OUT_DIR"), "/ahand.v1.rs"));
    }
}

pub use ahand::v1::*;
