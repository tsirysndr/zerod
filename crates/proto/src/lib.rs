pub mod v1alpha1 {
    include!("generated/zerod.v1alpha1.rs");
}

pub const FILE_DESCRIPTOR_SET: &[u8] =
    include_bytes!("generated/zerod_descriptor.bin");
