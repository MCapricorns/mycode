#[path = "workspace/invoice.rs"]
mod candidate;

#[test]
fn invoice_obeys_zero_and_delivery_boundaries() {
    for (units, rate, expected) in [(0, 7, 0), (1, 7, 7), (9, 2, 18), (10, 2, 20), (11, 2, 27), (20, 3, 65)] {
        assert_eq!(candidate::invoice(units, rate), expected, "units={units}, rate={rate}");
    }
}
