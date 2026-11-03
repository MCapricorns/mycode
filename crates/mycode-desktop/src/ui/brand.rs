//! One randomly selected wordmark gradient for the lifetime of the application.
use std::collections::hash_map::RandomState;
use std::hash::BuildHasher as _;
use std::sync::LazyLock;

// The six palettes are authored in assets/wordmark.svg.
const PALETTE_COUNT: u64 = 6;
static PALETTE: LazyLock<u64> =
    LazyLock::new(|| RandomState::new().hash_one("MYCode") % PALETTE_COUNT);

pub(super) fn wordmark_asset() -> String {
    format!("brand/wordmark-{}.png", *PALETTE)
}
