// Written by `cargo run -p rune --example regenerate_dxbc`, or by
// `RUNE_REGENERATE_DXBC=1 cargo test -p rune --lib uber_dxbc_is_fresh`, on Windows.
pub(super) const UBER_DXBC_WGSL_BLAKE3: &str = "ae5eb871ac64cf6a5f234a00ba9ec01030ab97633c34ad31456b95cfb945618c";
pub(super) const UBER_DXBC_ABI_FNV64: u64 = 0xb53b72e11574bdc1;
#[cfg(test)]
pub(super) const UBER_DXBC_HLSL_BLAKE3: &str = "19a8f9d70d4305c4ffd27ad169ccda84275eafffbf61ee4b59412bda7ba6ff05";
