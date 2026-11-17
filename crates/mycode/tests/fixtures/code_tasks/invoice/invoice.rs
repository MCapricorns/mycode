#[path = "policy.rs"]
mod policy;

pub fn invoice(units: u32, rate: u32) -> u32 {
    units.max(1) * rate + policy::delivery_fee(units)
}
